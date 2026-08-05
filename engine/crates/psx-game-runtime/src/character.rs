//! Runtime character view and the player animation-state vocabulary,
//! carved out of `editor-playtest`'s `character_runtime` module with
//! the phase-2 model-rendering slice (docs/game-runtime-plan.md).
//! [`RuntimeCharacter`] is the once-at-init decode of the cooked
//! `LevelCharacterRecord`; speed scaling arrives as plain values.

use psx_engine::{Angle, CharacterMotorConfig};
use psx_level::{
    character_action_flags, CharacterAnimationAction, LevelCharacterRecord,
    LevelModelMaterialOverride, ModelClipIndex, ModelIndex, OptionalModelClipIndex,
    CHARACTER_ANIMATION_ACTION_COUNT,
};

/// Animation state machine for the player: idle with no movement,
/// walking for normal movement, running while Circle is held.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum PlayerAnim {
    Idle,
    Walk,
    WalkBackward,
    StrafeLeft,
    StrafeRight,
    Run,
    Roll,
    Quickstep,
    DashLeft,
    DashRight,
    LightAttack,
    HeavyAttack,
    ComboAttack,
    /// First-spawn intro, played once with control locked out.
    Intro,
    Death,
}

impl PlayerAnim {
    /// The cooked animation action this state plays.
    pub const fn action(self) -> CharacterAnimationAction {
        match self {
            Self::Idle => CharacterAnimationAction::Idle,
            Self::Walk => CharacterAnimationAction::Walk,
            Self::WalkBackward => CharacterAnimationAction::WalkBackward,
            Self::StrafeLeft => CharacterAnimationAction::StrafeLeft,
            Self::StrafeRight => CharacterAnimationAction::StrafeRight,
            Self::Run => CharacterAnimationAction::Run,
            Self::Roll => CharacterAnimationAction::Roll,
            // Cooked slot 5 retains its legacy Backstep discriminant.
            Self::Quickstep => CharacterAnimationAction::Backstep,
            Self::DashLeft => CharacterAnimationAction::DashLeft,
            Self::DashRight => CharacterAnimationAction::DashRight,
            Self::LightAttack => CharacterAnimationAction::LightAttack,
            Self::HeavyAttack => CharacterAnimationAction::HeavyAttack,
            Self::ComboAttack => CharacterAnimationAction::ComboAttack,
            Self::Intro => CharacterAnimationAction::Intro,
            Self::Death => CharacterAnimationAction::Death,
        }
    }

    /// Whether this state is a stepping gait: a cyclic clip whose phase
    /// means a position in the stride. Idle loops too, but has no stride,
    /// so carrying phase into or out of it is meaningless.
    pub const fn is_gait(self) -> bool {
        matches!(
            self,
            Self::Walk | Self::WalkBackward | Self::StrafeLeft | Self::StrafeRight | Self::Run
        )
    }

    /// Whether the motor drives this state as a fixed-length action.
    pub const fn is_motor_fixed_action(self) -> bool {
        matches!(
            self,
            Self::Roll | Self::Quickstep | Self::DashLeft | Self::DashRight
        )
    }
}

/// Whether `anim` is one of the attack states.
pub const fn player_anim_is_attack(anim: PlayerAnim) -> bool {
    matches!(
        anim,
        PlayerAnim::LightAttack | PlayerAnim::HeavyAttack | PlayerAnim::ComboAttack
    )
}

/// Player clip-transition crossfade, resolved per frame by the caller.
///
/// The outgoing animation plays on at `local_tick` (its clip-local
/// tick for this render tick, advanced by the caller); `alpha_q12` is
/// the Q12 weight of the INCOMING clip this frame. The renderer shows
/// the pure incoming clip at `1 << 12` and the pure outgoing pose at 0.
#[derive(Copy, Clone, Debug)]
pub struct PlayerAnimBlend {
    /// Outgoing animation state at the switch.
    pub anim: PlayerAnim,
    /// Outgoing clip's clip-local tick for THIS render tick. The
    /// caller advances it across the crossfade so the outgoing clip
    /// keeps playing while it fades out.
    pub local_tick: u32,
    /// Q12 weight of the incoming clip.
    pub alpha_q12: u16,
}

/// Runtime view of the cooked LevelCharacterRecord -- the same
/// fields, decoded into runtime-friendly types. Resolved once
/// at init time so per-frame movement / animation / camera code
/// doesn't keep re-resolving the manifest.
#[derive(Copy, Clone, Debug)]
#[allow(missing_docs)]
pub struct RuntimeCharacter {
    /// Index into the cooked `MODELS` table.
    pub model: ModelIndex,
    pub action_clips: [OptionalModelClipIndex; CHARACTER_ANIMATION_ACTION_COUNT],
    pub action_flags: [u8; CHARACTER_ANIMATION_ACTION_COUNT],
    pub action_speeds: [u16; CHARACTER_ANIMATION_ACTION_COUNT],
    pub action_frame_ranges:
        [psx_level::CharacterActionFrameRange; CHARACTER_ANIMATION_ACTION_COUNT],
    pub action_pushes: [psx_level::CharacterActionPush; CHARACTER_ANIMATION_ACTION_COUNT],
    pub combat_capsule_first: psx_level::CombatCapsuleIndex,
    pub combat_capsule_count: u8,
    pub visual_offset: [i16; 3],
    pub visual_yaw: i16,
    pub visual_scale_q8: u16,
    /// Covering material replacing the model's cooked atlas.
    pub material_override: Option<LevelModelMaterialOverride>,
    pub weight_q8: u16,
    /// Coarse collision cylinder radius. Engine units.
    pub radius: i32,
    /// Coarse collision cylinder height. Engine units.
    pub height: i32,
    pub walk_speed: i32,
    pub run_speed: i32,
    /// Yaw rate translated from degrees/second to PSX angle
    /// units / 60 Hz frame at init time.
    pub yaw_step: Angle,
    pub stamina_max_q12: i32,
    pub sprint_min_q12: i32,
    pub sprint_drain_q12: i32,
    pub stamina_recover_q12: i32,
    pub roll_cost_q12: i32,
    pub roll_speed: i32,
    pub roll_active_frames: u8,
    pub roll_recovery_frames: u8,
    pub roll_invulnerable_frames: u8,
    pub backstep_cost_q12: i32,
    pub backstep_speed: i32,
    pub backstep_active_frames: u8,
    pub backstep_recovery_frames: u8,
    pub backstep_invulnerable_frames: u8,
}

impl RuntimeCharacter {
    /// Resolve the cooked record into the runtime's preferred
    /// units. Yaw is converted from degrees/second to per-frame
    /// quanta (`4096 quanta = full turn`, runtime targets 60 Hz)
    /// up-front so the per-frame update path is just a wrapping
    /// add. Authored speeds scale by `speed_scale_num / speed_scale_den`
    /// (the game's global player-speed knob).
    pub fn from_record(
        c: &LevelCharacterRecord,
        speed_scale_num: i32,
        speed_scale_den: i32,
    ) -> Self {
        // 4096 q12 / 360 deg = 11 q12 per deg, divided by
        // 60 Hz target ≈ 0.19 q12 per deg/frame. We approximate
        // as `(deg * 4096) / (360 * 60)` which is exact for the
        // 180 deg/s default (= 34 quanta/frame).
        let yaw_step_q12 = ((c.turn_speed_degrees_per_second as u32 * 4096) / (360 * 60)) as u16;
        Self {
            model: c.model,
            action_clips: c.action_clips,
            action_flags: c.action_flags,
            action_speeds: c.action_speeds,
            action_frame_ranges: c.action_frame_ranges,
            action_pushes: c.action_pushes,
            combat_capsule_first: c.combat_capsule_first,
            combat_capsule_count: c.combat_capsule_count,
            visual_offset: c.visual_offset,
            visual_yaw: c.visual_yaw,
            visual_scale_q8: c.visual_scale_q8,
            material_override: c.material_override,
            weight_q8: c.weight_q8,
            radius: c.radius as i32,
            height: c.height as i32,
            walk_speed: scaled_player_speed(c.walk_speed, speed_scale_num, speed_scale_den),
            run_speed: scaled_player_speed(c.run_speed, speed_scale_num, speed_scale_den),
            yaw_step: Angle::from_q12(yaw_step_q12),
            stamina_max_q12: c.stamina_max_q12,
            sprint_min_q12: c.sprint_min_q12,
            sprint_drain_q12: c.sprint_drain_q12,
            stamina_recover_q12: c.stamina_recover_q12,
            roll_cost_q12: c.roll_cost_q12,
            roll_speed: c.roll_speed,
            roll_active_frames: c.roll_active_frames,
            roll_recovery_frames: c.roll_recovery_frames,
            roll_invulnerable_frames: c.roll_invulnerable_frames,
            backstep_cost_q12: c.backstep_cost_q12,
            backstep_speed: c.backstep_speed,
            backstep_active_frames: c.backstep_active_frames,
            backstep_recovery_frames: c.backstep_recovery_frames,
            backstep_invulnerable_frames: c.backstep_invulnerable_frames,
        }
    }

    /// The authored clip slot for `action` (may be NONE).
    pub fn action_clip(&self, action: CharacterAnimationAction) -> OptionalModelClipIndex {
        self.action_clips
            .get(action.to_index())
            .copied()
            .unwrap_or(OptionalModelClipIndex::NONE)
    }

    /// The authored flag byte for `action`.
    pub fn action_flags(&self, action: CharacterAnimationAction) -> u8 {
        self.action_flags
            .get(action.to_index())
            .copied()
            .unwrap_or(0)
    }

    /// Q8 playback speed (`256 = 1.0x`) for an action, defaulting to
    /// unscaled when the action slot is out of range.
    pub fn action_speed(&self, action: CharacterAnimationAction) -> u16 {
        self.action_speeds
            .get(action.to_index())
            .copied()
            .unwrap_or(psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8)
    }

    /// The authored playback frame range for `action`.
    pub fn action_frame_range(
        &self,
        action: CharacterAnimationAction,
    ) -> psx_level::CharacterActionFrameRange {
        self.action_frame_ranges
            .get(action.to_index())
            .copied()
            .unwrap_or(psx_level::CharacterActionFrameRange::FULL)
    }

    /// The authored forward push for `action`.
    pub fn action_push(&self, action: CharacterAnimationAction) -> psx_level::CharacterActionPush {
        self.action_pushes
            .get(action.to_index())
            .copied()
            .unwrap_or(psx_level::CharacterActionPush::NONE)
    }

    /// Whether `action` loops.
    pub fn action_loops(&self, action: CharacterAnimationAction) -> bool {
        self.action_flags(action) & character_action_flags::LOOPING != 0
    }

    /// Authored in-place override for `action`, if one is set.
    pub fn action_in_place_override(&self, action: CharacterAnimationAction) -> Option<bool> {
        let flags = self.action_flags(action);
        if flags & character_action_flags::IN_PLACE_OVERRIDE == 0 {
            None
        } else {
            Some(flags & character_action_flags::IN_PLACE != 0)
        }
    }

    /// Pick the clip index for an animation state, with
    /// cheap deterministic fallbacks for unassigned optional actions.
    pub fn clip_for(&self, anim: PlayerAnim) -> ModelClipIndex {
        let idle = self
            .action_clip(CharacterAnimationAction::Idle)
            .unwrap_or(ModelClipIndex::ZERO);
        let walk = self
            .action_clip(CharacterAnimationAction::Walk)
            .unwrap_or(idle);
        match anim.action() {
            CharacterAnimationAction::Idle => idle,
            CharacterAnimationAction::Walk => walk,
            CharacterAnimationAction::WalkBackward => self
                .action_clip(CharacterAnimationAction::WalkBackward)
                .unwrap_or(walk),
            CharacterAnimationAction::StrafeLeft => self
                .action_clip(CharacterAnimationAction::StrafeLeft)
                .unwrap_or(walk),
            CharacterAnimationAction::StrafeRight => self
                .action_clip(CharacterAnimationAction::StrafeRight)
                .unwrap_or(walk),
            CharacterAnimationAction::Run => self
                .action_clip(CharacterAnimationAction::Run)
                .unwrap_or(walk),
            CharacterAnimationAction::Roll => {
                self.action_clip(CharacterAnimationAction::Roll).unwrap_or(
                    self.action_clip(CharacterAnimationAction::Run)
                        .unwrap_or(walk),
                )
            }
            CharacterAnimationAction::Backstep => self
                .action_clip(CharacterAnimationAction::Backstep)
                .unwrap_or(
                    self.action_clip(CharacterAnimationAction::Roll)
                        .unwrap_or(walk),
                ),
            CharacterAnimationAction::LightAttack => self
                .action_clip(CharacterAnimationAction::LightAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::ComboAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::HeavyAttack => self
                .action_clip(CharacterAnimationAction::HeavyAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::LightAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::ComboAttack => self
                .action_clip(CharacterAnimationAction::ComboAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::LightAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::Block => self
                .action_clip(CharacterAnimationAction::Block)
                .unwrap_or(idle),
            CharacterAnimationAction::HitReact => self
                .action_clip(CharacterAnimationAction::HitReact)
                .unwrap_or(idle),
            CharacterAnimationAction::Death => self
                .action_clip(CharacterAnimationAction::Death)
                .unwrap_or(idle),
            // An unauthored intro degrades to idle: the spawn lock is
            // skipped entirely when the clip is missing, so the player
            // never loses control waiting for an animation that is not
            // there.
            CharacterAnimationAction::Intro => self
                .action_clip(CharacterAnimationAction::Intro)
                .unwrap_or(idle),
            CharacterAnimationAction::Turn => idle,
            // Locked-on fast locomotion degrades to the walk-speed strafe
            // set, then plain locomotion, so a character bound before the
            // full moveset existed keeps moving.
            CharacterAnimationAction::RunBackward => self
                .action_clip(CharacterAnimationAction::RunBackward)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::WalkBackward)
                        .to_option()
                })
                .or_else(|| self.action_clip(CharacterAnimationAction::Run).to_option())
                .unwrap_or(walk),
            CharacterAnimationAction::RunStrafeLeft => self
                .action_clip(CharacterAnimationAction::RunStrafeLeft)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::StrafeLeft)
                        .to_option()
                })
                .or_else(|| self.action_clip(CharacterAnimationAction::Run).to_option())
                .unwrap_or(walk),
            CharacterAnimationAction::RunStrafeRight => self
                .action_clip(CharacterAnimationAction::RunStrafeRight)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::StrafeRight)
                        .to_option()
                })
                .or_else(|| self.action_clip(CharacterAnimationAction::Run).to_option())
                .unwrap_or(walk),
            // Lateral evades degrade to the forward roll.
            CharacterAnimationAction::DashLeft => self
                .action_clip(CharacterAnimationAction::DashLeft)
                .to_option()
                .or_else(|| self.action_clip(CharacterAnimationAction::Roll).to_option())
                .unwrap_or(walk),
            CharacterAnimationAction::DashRight => self
                .action_clip(CharacterAnimationAction::DashRight)
                .to_option()
                .or_else(|| self.action_clip(CharacterAnimationAction::Roll).to_option())
                .unwrap_or(walk),
            // Poise break: an unbound stun reads as a long hit reaction.
            CharacterAnimationAction::Stun => self
                .action_clip(CharacterAnimationAction::Stun)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::HitReact)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::StunRecovery => self
                .action_clip(CharacterAnimationAction::StunRecovery)
                .to_option()
                .or_else(|| self.action_clip(CharacterAnimationAction::Stun).to_option())
                .unwrap_or(idle),
            CharacterAnimationAction::HitReactAlt => self
                .action_clip(CharacterAnimationAction::HitReactAlt)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::HitReact)
                        .to_option()
                })
                .unwrap_or(idle),
            // The alternate weapon class falls back to the primary set.
            CharacterAnimationAction::AltLightAttack => self
                .action_clip(CharacterAnimationAction::AltLightAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::LightAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::AltHeavyAttack => self
                .action_clip(CharacterAnimationAction::AltHeavyAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::HeavyAttack)
                        .to_option()
                })
                .unwrap_or(idle),
            CharacterAnimationAction::AltComboAttack => self
                .action_clip(CharacterAnimationAction::AltComboAttack)
                .to_option()
                .or_else(|| {
                    self.action_clip(CharacterAnimationAction::ComboAttack)
                        .to_option()
                })
                .unwrap_or(idle),
        }
    }

    /// The motor configuration this character drives.
    pub fn motor_config(&self) -> CharacterMotorConfig {
        let mut config = CharacterMotorConfig::character_with_body(
            self.radius,
            self.height,
            self.walk_speed,
            self.run_speed,
            self.yaw_step,
        );
        config.weight_q8 = self.weight_q8;
        config.stamina_max_q12 = self.stamina_max_q12;
        config.sprint_min_q12 = self.sprint_min_q12;
        config.sprint_drain_q12 = self.sprint_drain_q12;
        config.stamina_recover_q12 = self.stamina_recover_q12;
        config.roll_cost_q12 = self.roll_cost_q12;
        config.roll_speed = self.roll_speed;
        config.roll_active_frames = self.roll_active_frames;
        config.roll_recovery_frames = self.roll_recovery_frames;
        config.roll_invulnerable_frames = self.roll_invulnerable_frames;
        config.backstep_cost_q12 = self.backstep_cost_q12;
        config.backstep_speed = self.backstep_speed;
        config.backstep_active_frames = self.backstep_active_frames;
        config.backstep_recovery_frames = self.backstep_recovery_frames;
        config.backstep_invulnerable_frames = self.backstep_invulnerable_frames;
        config
    }
}

/// Scale an authored speed by the game's `num / den` player-speed
/// knob, keeping positive speeds at least 1.
pub fn scaled_player_speed(speed: i32, num: i32, den: i32) -> i32 {
    let scaled = speed.saturating_mul(num) / den;
    if speed > 0 {
        scaled.max(1)
    } else {
        scaled
    }
}
