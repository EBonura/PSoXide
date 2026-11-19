//! One action system for every fighter: the clock an action's clip runs on,
//! the lock it holds the fighter in, and the input memory that decides when
//! the next attack or dodge may replace it. The rules read a fighter's
//! [`Sample`] of its character's authored combat windows; with none authored
//! they fall back to the short legacy buffers. Every fighter keeps one
//! [`FighterAction`] in its [`GameEntities`](crate::entities::GameEntities)
//! slot, whoever controls it.

use crate::combat_intent::{CombatAction, CombatIntent};
use crate::combat_timing::{Request, Sample};
use psx_engine::SimTick;
use psx_level::CombatWindowKind as WindowKind;

/// Ticks a dodge pressed while the fighter is free stays remembered.
pub const EVADE_BUFFER_TICKS: u8 = 12;

/// Longest a dodge pressed during a locked action waits for the lock to end.
pub const EVADE_BUFFER_LOCK_CAP_TICKS: u8 = 48;

/// Ticks an attack press outside any authored window stays remembered.
pub const ATTACK_BUFFER_TICKS: u32 = 12;

/// Short, single-action input memory. Early presses expire; holding a button
/// never creates extra attacks, and interruption discards the intention.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AttackBuffer {
    until: u32,
    tag: u8,
}

impl AttackBuffer {
    /// Nothing remembered.
    pub const EMPTY: Self = Self { until: 0, tag: 0 };

    /// Remember `tag`, replacing any older request, for
    /// [`ATTACK_BUFFER_TICKS`] from `now`.
    pub fn request(&mut self, tag: u8, now: u32) {
        self.tag = tag;
        self.until = now.wrapping_add(ATTACK_BUFFER_TICKS);
    }

    /// Forget the request.
    pub fn clear(&mut self) {
        *self = Self::EMPTY;
    }

    /// The remembered request if still fresh; the buffer is empty afterwards.
    pub fn take(&mut self, now: u32) -> Option<u8> {
        let tag = self.tag;
        let fresh = self.until.wrapping_sub(now) as i32 > 0;
        self.clear();
        (tag != 0 && fresh).then_some(tag)
    }
}

/// The attack a fighter's intent presses this tick, as a buffer tag: 1 light,
/// 2 heavy, plus 2 in the vertical (Zenith) stance; 0 for none. A heavy press
/// wins over a light one.
pub fn pressed_strike(intent: &CombatIntent, zenith: bool) -> u8 {
    let stance_offset = if zenith { 2 } else { 0 };
    if intent.pressed(CombatAction::HeavyAttack) {
        2 + stance_offset
    } else if intent.pressed(CombatAction::LightAttack) {
        1 + stance_offset
    } else {
        0
    }
}

/// How an attack runs, in ticks from its start: the telegraph before its
/// first contact, the span contact is live, and the recovery after the last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionTiming {
    /// Ticks before the first contact.
    pub windup: u16,
    /// Ticks contact stays live.
    pub active: u16,
    /// Ticks from the end of contact to the end of the action.
    pub recovery: u16,
    /// The action's authored `Armored` window, as ticks from its start
    /// (first included, first excluded); None when its clip authors none,
    /// and the armour rule falls back to its contact
    /// ([`crate::combatant::armoured`]).
    pub armour: Option<(u16, u16)>,
    /// The action's authored `Invulnerable` window, placed as
    /// [`Self::armour`] is; None when its clip authors none.
    pub invulnerable: Option<(u16, u16)>,
}

impl ActionTiming {
    /// The timing of an action read off its clip: the clip is `total` ticks
    /// long, its phase (Q12 frames, the frame range's start included) is
    /// `start_q12` on its first tick and advances `rate_q12` per tick, and its
    /// authored contact covers frames `first..=last`. None when the clip
    /// does not advance or never reaches that contact: the action then keeps
    /// the timing its record authors.
    pub fn from_clip(
        total: u32,
        start_q12: u32,
        rate_q12: u32,
        (first, last): (u16, u16),
    ) -> Option<Self> {
        let windup = crate::combat::ticks_to_contact(first, last, start_q12, rate_q12)?;
        let end_q12 = (u32::from(last) + 1) << 12;
        let end = end_q12.saturating_sub(start_q12).div_ceil(rate_q12.max(1));
        let end = end.min(u32::from(u16::MAX));
        let clamp = |ticks: u32| ticks.min(u32::from(u16::MAX)) as u16;
        Some(Self {
            windup,
            active: clamp(end.saturating_sub(u32::from(windup))).max(1),
            recovery: clamp(total.saturating_sub(end)),
            armour: None,
            invulnerable: None,
        })
    }

    /// This timing with the action's authored `Armored` and `Invulnerable`
    /// windows, frames `start..end` (first excluded) on the same clip clock
    /// as [`Self::from_clip`], placed as ticks from the action's start.
    pub fn with_windows(
        self,
        start_q12: u32,
        rate_q12: u32,
        armour: Option<(u16, u16)>,
        invulnerable: Option<(u16, u16)>,
    ) -> Self {
        let tick = |frame: u16| {
            let ticks = (u32::from(frame) << 12)
                .saturating_sub(start_q12)
                .div_ceil(rate_q12.max(1));
            ticks.min(u32::from(u16::MAX)) as u16
        };
        let place = |window: Option<(u16, u16)>| {
            window.and_then(|(start, end)| (start < end).then(|| (tick(start), tick(end))))
        };
        Self {
            armour: place(armour),
            invulnerable: place(invulnerable),
            ..self
        }
    }

    /// Whether the action's authored `Armored` window is open `since` ticks
    /// after the action started; None when it authors none.
    pub fn armoured_at(self, since: u16) -> Option<bool> {
        self.armour
            .map(|(start, end)| since >= start && since < end)
    }

    /// Whether the action's authored `Invulnerable` window is open `since`
    /// ticks after the action started; None when it authors none.
    pub fn invulnerable_at(self, since: u16) -> Option<bool> {
        self.invulnerable
            .map(|(start, end)| since >= start && since < end)
    }
}

/// What [`FighterAction::dodge`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DodgeStep {
    /// The fighter dodges this tick.
    pub evade: bool,
    /// An authored dodge window replaced the current action (its lock ends
    /// now and the motor's action must be interrupted).
    pub cancelled: bool,
}

/// One fighter's current action: how long it holds the fighter, and the attack
/// and dodge requests waiting to replace it (its clip's clock is the fighter's
/// [`Body`](crate::fighter_anim::Body)).
#[derive(Clone, Copy, Debug)]
pub struct FighterAction {
    /// The action holds the fighter until this tick.
    pub lock_until: SimTick,
    /// Legacy attack memory, used where no authored window decides.
    pub buffer: AttackBuffer,
    /// Attack request inside the action's authored attack windows.
    pub authored_attack: Request,
    /// Dodge request inside the action's authored dodge windows.
    pub authored_dodge: Request,
    /// Ticks a remembered dodge still waits.
    pub evade_buffer: u8,
}

impl FighterAction {
    /// No action, nothing remembered.
    pub const IDLE: Self = Self {
        lock_until: SimTick::ZERO,
        buffer: AttackBuffer::EMPTY,
        authored_attack: Request::EMPTY,
        authored_dodge: Request::EMPTY,
        evade_buffer: 0,
    };

    /// Whether the action still holds the fighter at `now`.
    pub fn locked(&self, now: SimTick) -> bool {
        self.lock_until > now
    }

    /// Hold the fighter in its action for `ticks` from `now`: the length of
    /// the action's clip at its authored speed and range.
    pub fn hold(&mut self, now: SimTick, ticks: u32) {
        self.lock_until = now.saturating_add(ticks);
    }

    /// Forget every waiting attack and dodge request.
    pub fn clear_requests(&mut self) {
        self.buffer.clear();
        self.authored_attack.clear();
        self.authored_dodge.clear();
        self.evade_buffer = 0;
    }

    /// The attack request that fires this tick for a press `pressed` (a
    /// [`pressed_strike`] tag) during action `action` at clip `sample`, its
    /// clip started at `started` (which run of the action a request is for): inside
    /// an action that authors attack windows, the authored request (armed by
    /// its buffer window, fired in its attack window); otherwise the legacy
    /// buffer, which fires only once the fighter is `free`. `(tag, authored)`.
    pub fn attack_request(
        &mut self,
        action: u8,
        pressed: u8,
        sample: Sample,
        started: SimTick,
        now: SimTick,
        free: bool,
    ) -> Option<(u8, bool)> {
        if sample.window(WindowKind::Attack).is_some() {
            self.buffer.clear();
            return self
                .authored_attack
                .update(
                    action,
                    started.as_u32(),
                    pressed,
                    sample,
                    WindowKind::AttackBuffer,
                    WindowKind::Attack,
                )
                .map(|tag| (tag, true));
        }
        self.authored_attack.clear();
        if pressed != 0 {
            self.buffer.request(pressed, now.as_u32());
        }
        if !free {
            return None;
        }
        self.buffer.take(now.as_u32()).map(|tag| (tag, false))
    }

    /// Whether a dodge pressed (`pressed`) during action `action` at clip
    /// `sample` (started at `started`) happens this tick. Inside an authored dodge window the
    /// authored request decides (and cancels the action when it fires); a
    /// press during a locked action waits for the lock to end, a little past
    /// it at most; a press while `free` dodges at once.
    pub fn dodge(
        &mut self,
        action: u8,
        pressed: bool,
        sample: Sample,
        started: SimTick,
        now: SimTick,
        free: bool,
        delta: u16,
    ) -> DodgeStep {
        let authored = sample.window(WindowKind::Dodge).is_some() && !free;
        let mut cancelled = false;
        let evade = if authored {
            self.evade_buffer = 0;
            cancelled = self
                .authored_dodge
                .update(
                    action,
                    started.as_u32(),
                    u8::from(pressed),
                    sample,
                    WindowKind::DodgeBuffer,
                    WindowKind::Dodge,
                )
                .is_some();
            if cancelled {
                self.lock_until = now;
            }
            cancelled
        } else if pressed && !free {
            // Recovery cancel: a tap during the last stretch of an attack,
            // hit reaction or dodge fires the moment the fighter is free. A
            // tap earlier than the cap expires instead of surprising the
            // player with a roll a second later.
            let lock_left = self.lock_until.saturating_sub(now).saturating_add(2);
            self.evade_buffer = lock_left
                .min(u32::from(EVADE_BUFFER_LOCK_CAP_TICKS))
                .max(u32::from(EVADE_BUFFER_TICKS)) as u8;
            false
        } else if pressed || (self.evade_buffer != 0 && free) {
            // A press while free, or a remembered one now that it is.
            self.evade_buffer = 0;
            true
        } else {
            self.evade_buffer = self
                .evade_buffer
                .saturating_sub(delta.min(u8::MAX as u16) as u8);
            false
        };
        if !authored {
            self.authored_dodge.clear();
        }
        DodgeStep { evade, cancelled }
    }

    /// Whether movement the fighter asks for (`moving`, not a dodge) replaces
    /// a held action at `sample`: only inside its authored movement window.
    /// The lock ends now when it does.
    pub fn movement_cancel(
        &mut self,
        sample: Sample,
        now: SimTick,
        free: bool,
        moving: bool,
    ) -> bool {
        let cancel = !free && moving && sample.active(WindowKind::Movement) == Some(true);
        if cancel {
            self.lock_until = now;
        }
        cancel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::CharacterCombatWindow;

    #[test]
    fn late_press_runs_once_but_early_press_expires() {
        let mut buffer = AttackBuffer::EMPTY;
        buffer.request(1, 100);
        assert_eq!(buffer.take(111), Some(1));
        assert_eq!(buffer.take(111), None);
        buffer.request(2, 100);
        assert_eq!(buffer.take(112), None);
    }

    #[test]
    fn newest_intent_replaces_old_and_interrupt_clears_it() {
        let mut buffer = AttackBuffer::EMPTY;
        buffer.request(1, 100);
        buffer.request(4, 104);
        assert_eq!(buffer.take(110), Some(4));
        buffer.request(2, 120);
        buffer.clear();
        assert_eq!(buffer.take(120), None);
        buffer.request(3, u32::MAX - 4);
        assert_eq!(buffer.take(2), Some(3));
    }

    #[test]
    fn a_legacy_attack_waits_for_the_fighter_to_be_free() {
        let mut action = FighterAction::IDLE;
        let now = SimTick::from_u32(100);
        assert_eq!(
            action.attack_request(6, 1, Sample::LEGACY, SimTick::ZERO, now, false),
            None
        );
        let later = SimTick::from_u32(105);
        assert_eq!(
            action.attack_request(6, 0, Sample::LEGACY, SimTick::ZERO, later, true),
            Some((1, false))
        );
        assert_eq!(
            action.attack_request(6, 0, Sample::LEGACY, SimTick::ZERO, later, true),
            None
        );
    }

    #[test]
    fn an_authored_window_fires_inside_the_lock_and_ends_it_for_a_dodge() {
        let windows = [
            CharacterCombatWindow {
                action: 6,
                kind: WindowKind::Attack,
                start: 4,
                end: 8,
            },
            CharacterCombatWindow {
                action: 6,
                kind: WindowKind::Dodge,
                start: 4,
                end: 8,
            },
        ];
        let mut action = FighterAction::IDLE;
        action.lock_until = SimTick::from_u32(200);
        let inside = Sample::new(&windows, 6, 5 << 12);
        let now = SimTick::from_u32(110);
        assert_eq!(
            action.attack_request(6, 2, inside, SimTick::ZERO, now, false),
            Some((2, true))
        );
        let step = action.dodge(6, true, inside, SimTick::ZERO, now, false, 1);
        assert_eq!(
            step,
            DodgeStep {
                evade: true,
                cancelled: true
            }
        );
        assert!(!action.locked(now));
    }

    #[test]
    fn a_dodge_pressed_late_in_a_lock_fires_when_it_ends_and_an_early_one_expires() {
        let mut action = FighterAction::IDLE;
        action.lock_until = SimTick::from_u32(110);
        let step = action.dodge(
            0,
            true,
            Sample::LEGACY,
            SimTick::ZERO,
            SimTick::from_u32(100),
            false,
            1,
        );
        assert!(!step.evade);
        assert_eq!(action.evade_buffer, EVADE_BUFFER_TICKS);
        let step = action.dodge(
            0,
            false,
            Sample::LEGACY,
            SimTick::ZERO,
            SimTick::from_u32(110),
            true,
            1,
        );
        assert!(step.evade);
        action.lock_until = SimTick::from_u32(1000);
        action.dodge(
            0,
            true,
            Sample::LEGACY,
            SimTick::ZERO,
            SimTick::from_u32(100),
            false,
            1,
        );
        assert_eq!(action.evade_buffer, EVADE_BUFFER_LOCK_CAP_TICKS);
        for tick in 101..1000 {
            action.dodge(
                0,
                false,
                Sample::LEGACY,
                SimTick::ZERO,
                SimTick::from_u32(tick),
                false,
                1,
            );
        }
        assert!(
            !action
                .dodge(
                    0,
                    false,
                    Sample::LEGACY,
                    SimTick::ZERO,
                    SimTick::from_u32(1000),
                    true,
                    1
                )
                .evade
        );
    }

    #[test]
    fn a_clip_times_an_attack_by_its_contact_frames() {
        // One frame a tick from frame 0, contact on frames 10 to 14, 40 ticks.
        assert_eq!(
            ActionTiming::from_clip(40, 0, 1 << 12, (10, 14)),
            Some(ActionTiming {
                windup: 10,
                active: 5,
                recovery: 25,
                armour: None,
                invulnerable: None,
            })
        );
        // Half a frame a tick from frame 2 (a frame range's start).
        assert_eq!(
            ActionTiming::from_clip(60, 2 << 12, 1 << 11, (10, 14)),
            Some(ActionTiming {
                windup: 16,
                active: 10,
                recovery: 34,
                armour: None,
                invulnerable: None,
            })
        );
        // Contact the clip never reaches, or a clip that does not advance.
        assert_eq!(
            ActionTiming::from_clip(40, 20 << 12, 1 << 12, (10, 14)),
            None
        );
        assert_eq!(ActionTiming::from_clip(40, 0, 0, (10, 14)), None);
    }

    #[test]
    fn an_armour_window_sits_on_the_clip_clock() {
        // Half a frame a tick from frame 2: frames 10..14 are ticks 16..24.
        let timing = ActionTiming::from_clip(60, 2 << 12, 1 << 11, (10, 14))
            .unwrap()
            .with_windows(2 << 12, 1 << 11, Some((10, 14)), Some((12, 13)));
        assert_eq!(timing.armour, Some((16, 24)));
        assert_eq!(timing.invulnerable, Some((20, 22)));
        assert_eq!(timing.invulnerable_at(19), Some(false));
        assert_eq!(timing.invulnerable_at(21), Some(true));
        assert_eq!(ActionTiming::default().invulnerable_at(21), None);
        assert_eq!(timing.armoured_at(15), Some(false));
        assert_eq!(timing.armoured_at(16), Some(true));
        assert_eq!(timing.armoured_at(23), Some(true));
        assert_eq!(timing.armoured_at(24), Some(false));
        assert_eq!(ActionTiming::default().armoured_at(16), None);
    }
}
