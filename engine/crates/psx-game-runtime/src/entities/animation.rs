//! What an entity shows, on the player's animation rules
//! ([`crate::fighter_anim`]): its locomotion clip comes from its motor's
//! animation intent, a gait's phase runs on the clock since the gait began
//! (carried in step from the gait before), and every clip change crossfades.
//! The simulation never reads any of it; [`GameEntities::update_animation`]
//! runs once per presentation tick, after the simulation tick.

use super::*;
use crate::character::PlayerAnim;
use crate::fighter_anim::{self, AnimState, AnimTrack};
use psx_engine::SimTick;

/// One shown clip as a crossfade key: the animation action it plays and the
/// clip with its playback (phase zero; the phase lives on the clock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityAnim {
    /// Animation action the clip plays.
    pub action: CharacterAnimationAction,
    /// Clip and playback, phase zero.
    pub clip: GameEntityClip,
}

impl EntityAnim {
    /// Nothing shown yet: the next clip starts without a crossfade.
    pub const NONE: Self = Self {
        action: CharacterAnimationAction::Idle,
        clip: GameEntityClip {
            clip: u16::MAX,
            phase_ticks: 0,
            one_shot: false,
            speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
            frame_range: CharacterActionFrameRange::FULL,
        },
    };

    /// A looping locomotion clip.
    const fn looping(action: CharacterAnimationAction, clip: u16) -> Self {
        Self {
            action,
            clip: GameEntityClip {
                clip,
                phase_ticks: 0,
                one_shot: false,
                speed_q8: CHARACTER_ACTION_SPEED_UNSCALED_Q8,
                frame_range: CharacterActionFrameRange::FULL,
            },
        }
    }

    /// This clip `phase_ticks` into its playback.
    const fn at(self, phase_ticks: u16) -> GameEntityClip {
        GameEntityClip {
            phase_ticks,
            ..self.clip
        }
    }
}

impl AnimState for EntityAnim {
    fn action(self) -> CharacterAnimationAction {
        self.action
    }
}

/// What an entity shows this tick ([`GameEntities::animation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameEntityAnimation {
    /// The clip its state plays ([`GameEntities::clip_for_state`]).
    pub clip: GameEntityClip,
    /// The clip it is crossfading from, at that clip's tick now, and the Q12
    /// weight of [`Self::clip`] (`1 << 12` shows `clip` alone).
    pub fade: Option<(GameEntityClip, u16)>,
    /// While it moves through an attack: its gait, for its legs, and the Q12
    /// weight of [`Self::clip`] on them (0 shows the gait alone there).
    pub legs: Option<(GameEntityClip, u16)>,
}

impl GameEntityAnimation {
    /// Nothing resolved yet: the default clip, no layer.
    pub const NONE: Self = Self {
        clip: EntityAnim::NONE.clip,
        fade: None,
        legs: None,
    };
}

impl<const N: usize> GameEntities<N> {
    /// The locomotion entity `i` shows: what its motor's latest frame asked
    /// for (the motor reports idle for a requested move only when it was
    /// blocked, so a sub-unit step still walks), or its turn clip while it
    /// turns in place.
    pub(super) fn locomotion_anim(&self, r: &LevelGameEntityRecord, i: usize) -> EntityAnim {
        use CharacterAnimationAction as Action;
        match fighter_anim::locomotion_anim(self.motor_anims[i]) {
            PlayerAnim::Walk => EntityAnim::looping(Action::Walk, r.walk_clip),
            PlayerAnim::WalkBackward => {
                EntityAnim::looping(Action::WalkBackward, r.walk_backward_clip)
            }
            PlayerAnim::StrafeLeft => EntityAnim::looping(Action::StrafeLeft, r.strafe_left_clip),
            PlayerAnim::StrafeRight => {
                EntityAnim::looping(Action::StrafeRight, r.strafe_right_clip)
            }
            PlayerAnim::Run => EntityAnim::looping(Action::Run, r.run_clip),
            _ if self.tactics[i].turning => EntityAnim::looping(Action::Turn, r.turn_clip),
            _ => EntityAnim::looping(Action::Idle, r.idle_clip),
        }
    }

    /// `shown` on entity `i`'s locomotion clock: its phase is the time since
    /// its gait began, when it is the gait the clock runs for.
    pub(super) fn locomotion_clip(&self, shown: EntityAnim, i: usize) -> GameEntityClip {
        let phase = if self.gaits[i].state == shown {
            self.anim_clock.saturating_sub(self.actions[i].start) as u16
        } else {
            0
        };
        shown.at(phase)
    }

    /// Advance every entity's animation to `now` (once per presentation
    /// tick, after the simulation tick): a changed locomotion or shown clip
    /// switches its track, keeping the outgoing pose to crossfade from, and
    /// gait to gait the clock is carried so the feet stay in step.
    /// `cycle(index, clip)` is the clip's length in ticks at its playback,
    /// 0 when unknown (no carry).
    pub fn update_animation(
        &mut self,
        records: &'static [LevelGameEntityRecord],
        now: SimTick,
        mut cycle: impl FnMut(usize, &GameEntityClip) -> u32,
    ) {
        self.anim_clock = now;
        let count = self.count().min(records.len());
        for i in 0..count {
            let gait = self.locomotion_anim(&records[i], i);
            let track = &mut self.gaits[i];
            if track.state == EntityAnim::NONE {
                *track = AnimTrack::new(gait);
                self.actions[i].start = now;
            } else if track.state != gait {
                let from = track.state;
                let carried =
                    if fighter_anim::is_gait(from.action) && fighter_anim::is_gait(gait.action) {
                        fighter_anim::gait_phase_carry(
                            now.saturating_sub(self.actions[i].start),
                            cycle(i, &from.clip),
                            cycle(i, &gait.clip),
                        )
                    } else {
                        0
                    };
                track.switch(gait, &mut self.actions[i].start, now, carried);
            }
            let (action, clip) = self.state_anim(records, i);
            let shown = EntityAnim {
                action,
                clip: GameEntityClip {
                    phase_ticks: 0,
                    ..clip
                },
            };
            let track = &mut self.bodies[i];
            if track.state == EntityAnim::NONE {
                *track = AnimTrack::new(shown);
            } else if track.state != shown {
                track.switch(shown, &mut self.body_starts[i], now, 0);
            }
            // The shown clip's own clock (its state's, or the locomotion
            // clock) is where its phase reads as zero.
            self.body_starts[i] =
                SimTick::from_u32(now.as_u32().wrapping_sub(u32::from(clip.phase_ticks)));
            let fade = self.bodies[i]
                .fade(now)
                .filter(|fade| fade.from != EntityAnim::NONE)
                .map(|fade| (fade.from.at(fade.local_tick as u16), fade.alpha_q12));
            self.shown[i] = GameEntityAnimation {
                clip,
                fade,
                legs: self.moving_legs(i),
            };
        }
    }

    /// What entity `index` shows, as the latest [`Self::update_animation`]
    /// resolved it (resolved once per tick, read by every consumer).
    pub fn animation(&self, index: usize) -> GameEntityAnimation {
        self.shown
            .get(index)
            .copied()
            .filter(|_| index < self.count())
            .unwrap_or(GameEntityAnimation::NONE)
    }

    /// The gait on entity `index`'s legs while it winds up, fires or
    /// recovers on the move (the same states the old leg layer covered):
    /// fading in as it starts to move, out as it stops.
    fn moving_legs(&self, index: usize) -> Option<(GameEntityClip, u16)> {
        let phase = self.attack_phase(index);
        let acting = phase.is_some()
            && (self.selected_attack_is_ranged(index) || phase == Some(AttackPhase::Windup));
        if !acting {
            return None;
        }
        let track = &self.gaits[index];
        let now = self.anim_clock;
        let fade = track.fade(now).filter(|fade| fade.from != EntityAnim::NONE);
        if fighter_anim::is_gait(track.state.action) {
            let weight = match fade {
                // Starting to move: the gait takes over the legs.
                Some(fade) if !fighter_anim::is_gait(fade.from.action) => {
                    (1u16 << 12) - fade.alpha_q12
                }
                _ => 0,
            };
            return Some((self.locomotion_clip(track.state, index), weight));
        }
        // Stopping: the outgoing gait hands the legs back.
        fade.filter(|fade| fighter_anim::is_gait(fade.from.action))
            .map(|fade| (fade.from.at(fade.local_tick as u16), fade.alpha_q12))
    }
}
