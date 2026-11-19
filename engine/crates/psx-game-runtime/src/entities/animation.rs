//! What an entity shows, on the player's animation rules
//! ([`crate::fighter_anim`]): its locomotion clip comes from its motor's
//! animation intent, a gait's phase runs on the clock since the gait began
//! (carried in step from the gait before), and every clip change crossfades.
//! The simulation never reads any of it; [`GameEntities::update_animation`]
//! runs once per presentation tick, after the simulation tick.

use super::*;
use crate::character::PlayerAnim;
use crate::fighter_anim::{self, AnimState, Body};
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

    /// A body showing nothing yet.
    pub const NO_BODY: Body<Self> = Body::new(Self::NONE, SimTick::ZERO);

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

/// Entities crossfade into a reaction (the default [`AnimState::REACTIONS`]).
impl AnimState for EntityAnim {
    fn action(self) -> CharacterAnimationAction {
        self.action
    }
}

/// What an entity shows this tick ([`GameEntities::animation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameEntityAnimation {
    /// The action [`Self::clip`] plays.
    pub action: CharacterAnimationAction,
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
        action: EntityAnim::NONE.action,
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
            self.gaits[i].local_tick(self.anim_clock) as u16
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
        let delta = now.as_u32().wrapping_sub(self.anim_clock.as_u32());
        self.anim_clock = now;
        let mut footfalls = 0u64;
        let count = self.count().min(records.len());
        for i in 0..count {
            if self.away(i) {
                continue;
            }
            let gait = self.locomotion_anim(&records[i], i);
            let was_gait = fighter_anim::is_gait(self.gaits[i].state.action)
                && self.gaits[i].state != EntityAnim::NONE;
            let track = &mut self.gaits[i];
            if track.state == EntityAnim::NONE {
                *track = Body::new(gait, now);
            } else if track.state != gait {
                let carried = track.gait_carry(gait, now, |state| cycle(i, &state.clip));
                track.switch(gait, now, carried);
            }
            // A gait the motor reports is a step it took (blocked, it
            // reports idle), so a shown gait is a moving one.
            if i < 64
                && fighter_anim::footfall(
                    fighter_anim::is_gait(gait.action),
                    was_gait,
                    self.gaits[i].local_tick(now),
                    delta,
                    cycle(i, &gait.clip),
                )
            {
                footfalls |= 1u64 << i;
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
                track.state = shown;
                track.switched = None;
            } else if track.state != shown {
                track.switch(shown, now, 0);
            }
            // The shown clip's own clock (its state's, or the locomotion
            // clock) is where its phase reads as zero.
            track.start = SimTick::from_u32(now.as_u32().wrapping_sub(u32::from(clip.phase_ticks)));
            let fade = self.bodies[i]
                .fade(now)
                .filter(|fade| fade.from != EntityAnim::NONE)
                .map(|fade| (fade.from.at(fade.local_tick as u16), fade.alpha_q12));
            self.shown[i] = GameEntityAnimation {
                action,
                clip,
                fade,
                legs: self.moving_legs(i),
            };
        }
        self.footfalls = footfalls;
    }

    /// How entity `index` (placed by `record`) is posed this tick: at its
    /// position and facing, playing its shown clip (or `attack`, the retained
    /// attack's action and clip while one is under way) with its crossfade,
    /// and its action's in-place choice read through the player's rule
    /// ([`crate::character::in_place_override`]). The disc and the host duel
    /// harness pose every entity through this.
    pub fn pose_override(
        &self,
        record: &LevelGameEntityRecord,
        index: usize,
        attack: Option<(CharacterAnimationAction, GameEntityClip)>,
    ) -> crate::model_rendering::ModelInstancePoseOverride {
        let animation = self.animation(index);
        let (action, clip) = attack.unwrap_or((animation.action, animation.clip));
        let position = self.position(index);
        let flags = self
            .body(record)
            .action_flags
            .get(action.to_index())
            .copied()
            .unwrap_or(0);
        crate::model_rendering::ModelInstancePoseOverride {
            instance: record.model_instance,
            x: position[0],
            y: position[1],
            z: position[2],
            yaw: self.yaw(index),
            clip: psx_level::OptionalModelClipIndex::some(psx_level::ModelClipIndex(clip.clip)),
            phase_ticks: clip.phase_ticks,
            one_shot: clip.one_shot,
            speed_q8: clip.speed_q8,
            frame_range: clip.frame_range,
            layer: animation.fade.map(|(clip, alpha_q12)| {
                crate::model_rendering::ModelInstancePoseLayer {
                    clip,
                    alpha_q12,
                    joint_mask: u32::MAX,
                }
            }),
            in_place_override: crate::character::in_place_override(flags),
        }
    }

    /// Whether entity `index`'s foot landed at the latest
    /// [`Self::update_animation`] ([`fighter_anim::footfall`], the player's
    /// rule).
    pub fn footfall(&self, index: usize) -> bool {
        index < 64 && self.footfalls & (1u64 << index) != 0
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
