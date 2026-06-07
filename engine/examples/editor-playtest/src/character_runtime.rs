use super::*;

/// Animation state machine for the player: idle with no movement,
/// walking for normal movement, running while Circle is held.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum PlayerAnim {
    Idle,
    Walk,
    Run,
    Roll,
    Backstep,
    LightAttack,
    HeavyAttack,
}

impl PlayerAnim {
    pub(super) const fn action(self) -> CharacterAnimationAction {
        match self {
            Self::Idle => CharacterAnimationAction::Idle,
            Self::Walk => CharacterAnimationAction::Walk,
            Self::Run => CharacterAnimationAction::Run,
            Self::Roll => CharacterAnimationAction::Roll,
            Self::Backstep => CharacterAnimationAction::Backstep,
            Self::LightAttack => CharacterAnimationAction::LightAttack,
            Self::HeavyAttack => CharacterAnimationAction::HeavyAttack,
        }
    }

    pub(super) const fn is_motor_fixed_action(self) -> bool {
        matches!(self, Self::Roll | Self::Backstep)
    }
}

pub(super) const fn player_anim_is_attack(anim: PlayerAnim) -> bool {
    matches!(anim, PlayerAnim::LightAttack | PlayerAnim::HeavyAttack)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct RuntimeCheckpoint {
    pub(super) room: RoomIndex,
    pub(super) position: RoomPoint,
    pub(super) yaw: Angle,
    pub(super) checkpoint_id: &'static str,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) struct RuntimeMessageOverlay {
    pub(super) title: &'static str,
    pub(super) body: &'static str,
}

#[derive(Copy, Clone, Debug, Default)]
pub(super) struct EvadeRunIntent {
    pub(super) sprint: bool,
    pub(super) evade: bool,
}

/// Runtime view of the cooked LevelCharacterRecord -- the same
/// fields, decoded into runtime-friendly types. Resolved once
/// at init time so per-frame movement / animation / camera code
/// doesn't keep re-resolving the manifest.
#[derive(Copy, Clone, Debug)]
pub(super) struct RuntimeCharacter {
    /// Index into `MODELS`.
    pub(super) model: ModelIndex,
    pub(super) action_clips: [OptionalModelClipIndex; CHARACTER_ANIMATION_ACTION_COUNT],
    pub(super) action_flags: [u8; CHARACTER_ANIMATION_ACTION_COUNT],
    pub(super) action_speeds: [u16; CHARACTER_ANIMATION_ACTION_COUNT],
    pub(super) action_frame_ranges:
        [psx_level::CharacterActionFrameRange; CHARACTER_ANIMATION_ACTION_COUNT],
    pub(super) visual_offset: [i16; 3],
    pub(super) visual_yaw: i16,
    pub(super) visual_scale_q8: u16,
    pub(super) weight_q8: u16,
    /// Coarse collision cylinder radius. Engine units.
    pub(super) radius: i32,
    /// Coarse collision cylinder height. Engine units.
    pub(super) height: i32,
    pub(super) walk_speed: i32,
    pub(super) run_speed: i32,
    /// Yaw rate translated from degrees/second to PSX angle
    /// units / 60 Hz frame at init time.
    pub(super) yaw_step: Angle,
    pub(super) stamina_max_q12: i32,
    pub(super) sprint_min_q12: i32,
    pub(super) sprint_drain_q12: i32,
    pub(super) stamina_recover_q12: i32,
    pub(super) roll_cost_q12: i32,
    pub(super) roll_speed: i32,
    pub(super) roll_active_frames: u8,
    pub(super) roll_recovery_frames: u8,
    pub(super) roll_invulnerable_frames: u8,
    pub(super) backstep_cost_q12: i32,
    pub(super) backstep_speed: i32,
    pub(super) backstep_active_frames: u8,
    pub(super) backstep_recovery_frames: u8,
    pub(super) backstep_invulnerable_frames: u8,
}

impl RuntimeCharacter {
    /// Resolve the cooked record into the runtime's preferred
    /// units. Yaw is converted from degrees/second to per-frame
    /// quanta (`4096 quanta = full turn`, runtime targets 60 Hz)
    /// up-front so the per-frame update path is just a wrapping
    /// add.
    pub(super) fn from_record(c: &LevelCharacterRecord) -> Self {
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
            visual_offset: c.visual_offset,
            visual_yaw: c.visual_yaw,
            visual_scale_q8: c.visual_scale_q8,
            weight_q8: c.weight_q8,
            radius: c.radius as i32,
            height: c.height as i32,
            walk_speed: scaled_player_speed(c.walk_speed),
            run_speed: scaled_player_speed(c.run_speed),
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

    pub(super) fn action_clip(&self, action: CharacterAnimationAction) -> OptionalModelClipIndex {
        self.action_clips
            .get(action.to_index())
            .copied()
            .unwrap_or(OptionalModelClipIndex::NONE)
    }

    pub(super) fn action_flags(&self, action: CharacterAnimationAction) -> u8 {
        self.action_flags
            .get(action.to_index())
            .copied()
            .unwrap_or(0)
    }

    /// Q8 playback speed (`256 = 1.0x`) for an action, defaulting to
    /// unscaled when the action slot is out of range.
    pub(super) fn action_speed(&self, action: CharacterAnimationAction) -> u16 {
        self.action_speeds
            .get(action.to_index())
            .copied()
            .unwrap_or(psx_level::CHARACTER_ACTION_SPEED_UNSCALED_Q8)
    }

    pub(super) fn action_frame_range(
        &self,
        action: CharacterAnimationAction,
    ) -> psx_level::CharacterActionFrameRange {
        self.action_frame_ranges
            .get(action.to_index())
            .copied()
            .unwrap_or(psx_level::CharacterActionFrameRange::FULL)
    }

    pub(super) fn action_loops(&self, action: CharacterAnimationAction) -> bool {
        self.action_flags(action) & character_action_flags::LOOPING != 0
    }

    pub(super) fn action_in_place_override(
        &self,
        action: CharacterAnimationAction,
    ) -> Option<bool> {
        let flags = self.action_flags(action);
        if flags & character_action_flags::IN_PLACE_OVERRIDE == 0 {
            None
        } else {
            Some(flags & character_action_flags::IN_PLACE != 0)
        }
    }

    /// Pick the clip index for an animation state, with
    /// cheap deterministic fallbacks for unassigned optional actions.
    pub(super) fn clip_for(&self, anim: PlayerAnim) -> ModelClipIndex {
        let idle = self
            .action_clip(CharacterAnimationAction::Idle)
            .unwrap_or(ModelClipIndex::ZERO);
        let walk = self
            .action_clip(CharacterAnimationAction::Walk)
            .unwrap_or(idle);
        match anim.action() {
            CharacterAnimationAction::Idle => idle,
            CharacterAnimationAction::Walk => walk,
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
                .unwrap_or(walk),
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
            CharacterAnimationAction::Turn => idle,
        }
    }

    pub(super) fn motor_config(&self) -> CharacterMotorConfig {
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

pub(super) fn scaled_player_speed(speed: i32) -> i32 {
    let scaled = speed.saturating_mul(PLAYER_SPEED_SCALE_NUM) / PLAYER_SPEED_SCALE_DEN;
    if speed > 0 {
        scaled.max(1)
    } else {
        scaled
    }
}
