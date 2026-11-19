//! Runtime character view and the player animation-state vocabulary,
//! carved out of `editor-playtest`'s `character_runtime` module with
//! the phase-2 model-rendering slice (docs/game-runtime-plan.md).
//! [`RuntimeCharacter`] is the once-at-init decode of the cooked
//! `LevelCharacterRecord`; speed scaling arrives as plain values.

use psx_engine::{Angle, CharacterMotorConfig};
use psx_level::combat_verbs::{self, CombatKits, VerbBindings};
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
    RangedAttack,
    RangedAim,
    RangedWalk,
    RangedBackward,
    RangedLeft,
    RangedRight,
    /// Hook takeoff, held on its last frame through wire travel.
    HookLaunch,
    Fall,
    Land,
    /// Zenith axis: overhead strikes.
    VertLightAttack,
    VertHeavyAttack,
    /// First-spawn intro, played once with control locked out.
    Intro,
    Death,
    /// An unarmoured hit interrupts the previous action until recovery ends.
    HitReact,
    /// A poise break plays the complete stagger and recovery one-shot.
    Stun,
    /// The stagger a parry's riposte deals: the same lock as [`Self::Stun`],
    /// over the Parried clip (the stagger clip until one is authored).
    Parried,
    /// A poise break knocked the fighter down: the knockdown clip, held
    /// until it gets up.
    Knockdown,
    /// Idle-to-walk transition, one shot; Walk begins where it ends.
    WalkWindup,
    /// Walk-to-idle transition, one shot, over the motor's deceleration.
    WalkWinddown,
    /// Mirrored walk-to-idle transition (other foot), for the half-stride phase.
    WalkWinddownAlt,
    /// Idle-to-run transition, one shot; Run begins where it ends.
    RunWindup,
    /// Run-to-idle transition, one shot, over the motor's deceleration.
    RunWinddown,
    /// Mirrored run-to-idle transition (other foot), for the half-stride phase.
    RunWinddownAlt,
}

/// The in-place override an action's flags author, if any: `Some(true)`
/// cancels the clip's root translation, `Some(false)` keeps it, `None` leaves
/// the clip's own calibration. Every fighter reads its actions through this
/// one rule (the player's [`RuntimeCharacter`] and an entity's body record).
pub const fn in_place_override(flags: u8) -> Option<bool> {
    if flags & character_action_flags::IN_PLACE_OVERRIDE == 0 {
        None
    } else {
        Some(flags & character_action_flags::IN_PLACE != 0)
    }
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
            Self::RangedAttack => CharacterAnimationAction::RangedAttack,
            Self::RangedAim => CharacterAnimationAction::RangedAim,
            Self::RangedWalk => CharacterAnimationAction::RangedWalk,
            Self::RangedBackward => CharacterAnimationAction::RangedBackward,
            Self::RangedLeft => CharacterAnimationAction::RangedLeft,
            Self::RangedRight => CharacterAnimationAction::RangedRight,
            Self::HookLaunch => CharacterAnimationAction::HookLaunch,
            Self::Fall => CharacterAnimationAction::Fall,
            Self::Land => CharacterAnimationAction::Land,
            Self::VertLightAttack => CharacterAnimationAction::VertLightAttack,
            Self::VertHeavyAttack => CharacterAnimationAction::VertHeavyAttack,
            Self::Intro => CharacterAnimationAction::Intro,
            Self::Death => CharacterAnimationAction::Death,
            Self::HitReact => CharacterAnimationAction::HitReact,
            Self::Stun => CharacterAnimationAction::Stun,
            Self::Parried => CharacterAnimationAction::Parried,
            Self::Knockdown => CharacterAnimationAction::Knockdown,
            Self::WalkWindup => CharacterAnimationAction::WalkWindup,
            Self::WalkWinddown => CharacterAnimationAction::WalkWinddown,
            Self::WalkWinddownAlt => CharacterAnimationAction::WalkWinddownAlt,
            Self::RunWindup => CharacterAnimationAction::RunWindup,
            Self::RunWinddown => CharacterAnimationAction::RunWinddown,
            Self::RunWinddownAlt => CharacterAnimationAction::RunWinddownAlt,
        }
    }

    /// Whether this state is a stepping gait: a cyclic clip whose phase
    /// means a position in the stride. Idle loops too, but has no stride,
    /// so carrying phase into or out of it is meaningless.
    pub const fn is_gait(self) -> bool {
        matches!(
            self,
            Self::Walk
                | Self::WalkBackward
                | Self::StrafeLeft
                | Self::StrafeRight
                | Self::Run
                | Self::RangedWalk
                | Self::RangedBackward
                | Self::RangedLeft
                | Self::RangedRight
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
        PlayerAnim::LightAttack
            | PlayerAnim::HeavyAttack
            | PlayerAnim::VertLightAttack
            | PlayerAnim::VertHeavyAttack
            | PlayerAnim::RangedAttack
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
    /// Explicit combat permissions and protection windows.
    pub combat_windows: [psx_level::CharacterCombatWindow; psx_level::MAX_CHARACTER_COMBAT_WINDOWS],
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
    /// The authored turn rate `yaw_step` comes from.
    pub turn_speed_degrees_per_second: u16,
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
        Self {
            model: c.model,
            action_clips: c.action_clips,
            action_flags: c.action_flags,
            action_speeds: c.action_speeds,
            action_frame_ranges: c.action_frame_ranges,
            action_pushes: c.action_pushes,
            combat_windows: c.combat_windows,
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
            yaw_step: yaw_step_for(c.turn_speed_degrees_per_second),
            turn_speed_degrees_per_second: c.turn_speed_degrees_per_second,
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

    /// Whether this character authors the requested action.
    ///
    /// Optional clip fallbacks remain useful for rendering, but gameplay
    /// features with distinct mechanics (notably sprinting) use this check so
    /// a fallback clip cannot accidentally enable an unsupported action.
    pub fn supports_action(&self, action: CharacterAnimationAction) -> bool {
        self.action_clip(action).is_some()
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
        in_place_override(self.action_flags(action))
    }

    /// Pick the clip index for an animation state. An unbound action plays
    /// the first bound action of its documented fallback chain
    /// ([`psx_level::combat_verbs::fallback_chain`]), the same chain the
    /// cook and the editor report; past the chain the rest pose (Idle) holds.
    /// Actions the player may not perform at all (an attack without the
    /// melee kit, a dodge without the mobility kit) are refused before they
    /// get here.
    pub fn clip_for(&self, anim: PlayerAnim) -> ModelClipIndex {
        let rest = self
            .action_clip(CharacterAnimationAction::Idle)
            .unwrap_or(ModelClipIndex::ZERO);
        match combat_verbs::resolve(self, anim.action()) {
            Some(action) => self.action_clip(action).unwrap_or(rest),
            None => rest,
        }
    }

    /// The combat kits this character binds, which decide what it may do
    /// ([`CombatKits`]): the same derivation the enemy cook uses.
    pub fn combat_kits(&self) -> CombatKits {
        CombatKits::from_bindings(self)
    }

    /// The motor configuration this character drives.
    ///
    /// Built by [`motion_motor_config`], the builder every enemy's
    /// configuration comes from too. The speeds here are already scaled by
    /// the game's knob (see [`Self::from_record`]), so it scales by one.
    pub fn motor_config(&self) -> CharacterMotorConfig {
        let motion = psx_level::CharacterMotion {
            walk_speed: self.walk_speed,
            run_speed: self.run_speed,
            turn_speed_degrees_per_second: self.turn_speed_degrees_per_second,
            weight_q8: self.weight_q8,
            stamina_max_q12: self.stamina_max_q12,
            sprint_min_q12: self.sprint_min_q12,
            sprint_drain_q12: self.sprint_drain_q12,
            stamina_recover_q12: self.stamina_recover_q12,
            roll_cost_q12: self.roll_cost_q12,
            roll_speed: self.roll_speed,
            roll_active_frames: self.roll_active_frames,
            roll_recovery_frames: self.roll_recovery_frames,
            roll_invulnerable_frames: self.roll_invulnerable_frames,
        };
        let mut config = motion_motor_config(
            self.radius,
            self.height,
            &motion,
            self.supports_action(CharacterAnimationAction::Run),
            (1, 1),
        );
        config.backstep_cost_q12 = self.backstep_cost_q12;
        config.backstep_speed = self.backstep_speed;
        config.backstep_active_frames = self.backstep_active_frames;
        config.backstep_recovery_frames = self.backstep_recovery_frames;
        config.backstep_invulnerable_frames = self.backstep_invulnerable_frames;
        config
    }
}

/// The player character's bindings for the verb rules. Her shot is always
/// the [`CharacterAnimationAction::RangedAttack`] slot: her runtime plays
/// that action for every shot.
impl VerbBindings for RuntimeCharacter {
    fn bound(&self, action: CharacterAnimationAction) -> bool {
        self.supports_action(action)
    }
}

/// Per-frame turn step for an authored turn rate. 4096 q12 / 360 deg = 11
/// q12 per deg, divided by the 60 Hz target is about 0.19 q12 per
/// deg/frame; `(deg * 4096) / (360 * 60)` is exact for the 180 deg/s
/// default (34 quanta per frame).
pub fn yaw_step_for(turn_speed_degrees_per_second: u16) -> Angle {
    Angle::from_q12(((turn_speed_degrees_per_second as u32 * 4096) / (360 * 60)) as u16)
}

/// An authored speed under the game's speed knob `(numerator,
/// denominator)` ([`scaled_player_speed`]); an unset knob (a zeroed table)
/// is one.
pub fn scaled_motion_speed(speed: i32, speed_scale: (i32, i32)) -> i32 {
    match speed_scale {
        (_, 0) => scaled_player_speed(speed, 1, 1),
        (num, den) => scaled_player_speed(speed, num, den),
    }
}

/// The motor configuration of a character: the one builder every fighter's
/// motor configuration comes from, the player's and every enemy's. Speeds
/// scale by `speed_scale` (numerator, denominator), the game's speed knob;
/// a character runs only with a Run action bound (`run_enabled`).
pub fn motion_motor_config(
    radius: i32,
    height: i32,
    motion: &psx_level::CharacterMotion,
    run_enabled: bool,
    speed_scale: (i32, i32),
) -> CharacterMotorConfig {
    let mut config = CharacterMotorConfig::character_with_body(
        radius,
        height,
        scaled_motion_speed(motion.walk_speed, speed_scale),
        scaled_motion_speed(motion.run_speed, speed_scale),
        yaw_step_for(motion.turn_speed_degrees_per_second),
    );
    config.run_enabled = run_enabled;
    config.weight_q8 = motion.weight_q8;
    config.stamina_max_q12 = motion.stamina_max_q12;
    config.sprint_min_q12 = motion.sprint_min_q12;
    config.sprint_drain_q12 = motion.sprint_drain_q12;
    config.stamina_recover_q12 = motion.stamina_recover_q12;
    config.roll_cost_q12 = motion.roll_cost_q12;
    config.roll_speed = motion.roll_speed;
    config.roll_active_frames = motion.roll_active_frames;
    config.roll_recovery_frames = motion.roll_recovery_frames;
    config.roll_invulnerable_frames = motion.roll_invulnerable_frames;
    config
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

/// Outcome of one player damage application (the souls death rule).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlayerDamageOutcome {
    /// Health after the subtraction (floors at zero).
    pub health: u16,
    /// The application killed: the owning game must arm exactly one
    /// death sequence (death animation, input lock, delayed respawn).
    pub died: bool,
}

/// Apply damage to the player: the shared kill decision for every
/// damage source (enemy combat connections and environmental hazards).
/// Health floors at zero; reaching zero kills UNLESS a death sequence
/// is already running (`already_dying`), so overlapping hits during
/// the death countdown can never arm a second respawn.
pub fn apply_player_damage(health: u16, already_dying: bool, damage: u16) -> PlayerDamageOutcome {
    let health = health.saturating_sub(damage);
    PlayerDamageOutcome {
        health,
        died: !already_dying && damage > 0 && health == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stun_plays_the_stun_action() {
        assert_eq!(PlayerAnim::Stun.action(), CharacterAnimationAction::Stun);
    }

    #[test]
    fn player_poise_separates_light_heavy_and_accumulated_hits() {
        let mut poise = crate::poise::Poise::EMPTY;
        assert!(!poise.hit(50, psx_level::CombatStats::DEFAULT.poise, false));
        assert!(poise.hit(50, psx_level::CombatStats::DEFAULT.poise, false));
        let mut poise = crate::poise::Poise::EMPTY;
        assert!(poise.hit(75, psx_level::CombatStats::DEFAULT.poise, false));
        let mut poise = crate::poise::Poise::EMPTY;
        assert!(!poise.hit(75, psx_level::CombatStats::DEFAULT.poise, true));
    }

    #[test]
    fn player_damage_kills_exactly_at_zero() {
        let survived = apply_player_damage(100, false, 99);
        assert_eq!(
            survived,
            PlayerDamageOutcome {
                health: 1,
                died: false
            }
        );
        let killed = apply_player_damage(1, false, 1);
        assert_eq!(
            killed,
            PlayerDamageOutcome {
                health: 0,
                died: true
            }
        );
        let overkill = apply_player_damage(10, false, u16::MAX);
        assert_eq!(
            overkill,
            PlayerDamageOutcome {
                health: 0,
                died: true
            }
        );
    }

    #[test]
    fn player_damage_never_rearms_a_running_death() {
        // A hit landing during the death countdown keeps health floored
        // without starting a second death sequence.
        let during_death = apply_player_damage(0, true, 25);
        assert_eq!(
            during_death,
            PlayerDamageOutcome {
                health: 0,
                died: false
            }
        );
    }

    #[test]
    fn zero_damage_is_not_a_death_event() {
        let nothing = apply_player_damage(0, false, 0);
        assert_eq!(
            nothing,
            PlayerDamageOutcome {
                health: 0,
                died: false
            }
        );
    }
}
