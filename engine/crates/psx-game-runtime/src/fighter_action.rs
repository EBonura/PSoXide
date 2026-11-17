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

/// What [`FighterAction::dodge`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DodgeStep {
    /// The fighter dodges this tick.
    pub evade: bool,
    /// An authored dodge window replaced the current action (its lock ends
    /// now and the motor's action must be interrupted).
    pub cancelled: bool,
}

/// One fighter's current action: when its clip started, how long it holds the
/// fighter, and the attack and dodge requests waiting to replace it.
#[derive(Clone, Copy, Debug)]
pub struct FighterAction {
    /// Tick the current action's clip started: its local clock's zero.
    pub start: SimTick,
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
        start: SimTick::ZERO,
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
    /// [`pressed_strike`] tag) during action `action` at clip `sample`: inside
    /// an action that authors attack windows, the authored request (armed by
    /// its buffer window, fired in its attack window); otherwise the legacy
    /// buffer, which fires only once the fighter is `free`. `(tag, authored)`.
    pub fn attack_request(
        &mut self,
        action: u8,
        pressed: u8,
        sample: Sample,
        now: SimTick,
        free: bool,
    ) -> Option<(u8, bool)> {
        if sample.window(WindowKind::Attack).is_some() {
            self.buffer.clear();
            return self
                .authored_attack
                .update(
                    action,
                    self.start.as_u32(),
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
    /// `sample` happens this tick. Inside an authored dodge window the
    /// authored request decides (and cancels the action when it fires); a
    /// press during a locked action waits for the lock to end, a little past
    /// it at most; a press while `free` dodges at once.
    pub fn dodge(
        &mut self,
        action: u8,
        pressed: bool,
        sample: Sample,
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
                    self.start.as_u32(),
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
        } else if pressed {
            self.evade_buffer = 0;
            true
        } else if self.evade_buffer != 0 && free {
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
            action.attack_request(6, 1, Sample::LEGACY, now, false),
            None
        );
        let later = SimTick::from_u32(105);
        assert_eq!(
            action.attack_request(6, 0, Sample::LEGACY, later, true),
            Some((1, false))
        );
        assert_eq!(
            action.attack_request(6, 0, Sample::LEGACY, later, true),
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
            action.attack_request(6, 2, inside, now, false),
            Some((2, true))
        );
        let step = action.dodge(6, true, inside, now, false, 1);
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
        let step = action.dodge(0, true, Sample::LEGACY, SimTick::from_u32(100), false, 1);
        assert!(!step.evade);
        assert_eq!(action.evade_buffer, EVADE_BUFFER_TICKS);
        let step = action.dodge(0, false, Sample::LEGACY, SimTick::from_u32(110), true, 1);
        assert!(step.evade);
        action.lock_until = SimTick::from_u32(1000);
        action.dodge(0, true, Sample::LEGACY, SimTick::from_u32(100), false, 1);
        assert_eq!(action.evade_buffer, EVADE_BUFFER_LOCK_CAP_TICKS);
        for tick in 101..1000 {
            action.dodge(0, false, Sample::LEGACY, SimTick::from_u32(tick), false, 1);
        }
        assert!(
            !action
                .dodge(0, false, Sample::LEGACY, SimTick::from_u32(1000), true, 1)
                .evade
        );
    }
}
