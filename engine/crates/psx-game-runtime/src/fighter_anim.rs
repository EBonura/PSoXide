//! One animation implementation for every fighter: which locomotion clip a
//! motor frame asks for, the clock a clip runs on, the phase carried from
//! one gait into the next, and the crossfade a clip change leaves behind.
//!
//! The player and every [`GameEntities`](crate::entities::GameEntities) slot
//! run these same rules. A fighter keeps an [`AnimTrack`] per thing it
//! shows: the state it plays and the crossfade from the one before. The
//! clip's clock is a start tick the fighter owns (the player's is her
//! [`FighterAction::start`](crate::fighter_action::FighterAction::start)):
//! a clip's local tick is the time since that start, so a gait's phase runs
//! on the clock whatever distance the body covered on any one tick.

use crate::character::PlayerAnim;
use psx_engine::{CharacterMotorAnim, SimTick};
use psx_level::CharacterAnimationAction as Action;

/// Generic crossfade window in sim ticks (60 Hz): long enough to soften
/// idle, walk and run cuts, short enough that the matrix lerp's shrink stays
/// invisible.
pub const BLEND_LOCOMOTION_TICKS: u32 = 8;
/// Crossfade window entering an attack or action: snappy, so its startup
/// frames are not softened away.
pub const BLEND_ACTION_TICKS: u32 = 4;
/// Crossfade window when leaving a committed one-shot (attack, intro).
/// Entering one stays snappy so its startup frames read, but a long swing cut
/// to idle in four ticks is the harshest transition in the game; it wants a
/// settle roughly as long as a footfall.
pub const BLEND_ACTION_OUT_TICKS: u32 = 14;
/// Crossfade window between two stepping gaits (walk, run, strafe). Longer
/// than the generic window because gait clips are cook-time aligned to
/// foot-down and the incoming clip is entered at the outgoing clip's phase,
/// so a long fade blends like poses instead of averaging opposite halves of
/// a stride.
pub const BLEND_GAIT_TICKS: u32 = 10;

/// The locomotion state a motor frame's animation intent asks for. The
/// motor reports [`CharacterMotorAnim::Idle`] for a requested move only when
/// it was blocked, so a body carried a sub-unit step still walks.
pub const fn locomotion_anim(anim: CharacterMotorAnim) -> PlayerAnim {
    match anim {
        CharacterMotorAnim::Idle => PlayerAnim::Idle,
        CharacterMotorAnim::Walk => PlayerAnim::Walk,
        CharacterMotorAnim::WalkBackward => PlayerAnim::WalkBackward,
        CharacterMotorAnim::StrafeLeft => PlayerAnim::StrafeLeft,
        CharacterMotorAnim::StrafeRight => PlayerAnim::StrafeRight,
        CharacterMotorAnim::Run => PlayerAnim::Run,
        CharacterMotorAnim::Roll => PlayerAnim::Roll,
        CharacterMotorAnim::Quickstep => PlayerAnim::Quickstep,
        CharacterMotorAnim::DashLeft => PlayerAnim::DashLeft,
        CharacterMotorAnim::DashRight => PlayerAnim::DashRight,
    }
}

/// Whether `action` is a stepping gait: a cyclic clip whose phase means a
/// position in the stride (see [`PlayerAnim::is_gait`]).
pub const fn is_gait(action: Action) -> bool {
    matches!(
        action,
        Action::Walk
            | Action::WalkBackward
            | Action::StrafeLeft
            | Action::StrafeRight
            | Action::Run
            | Action::RangedWalk
            | Action::RangedBackward
            | Action::RangedLeft
            | Action::RangedRight
    )
}

/// Whether `action` is where a gait comes to rest: standing, or a stop clip.
pub const fn is_stop(action: Action) -> bool {
    matches!(
        action,
        Action::Idle
            | Action::WalkWinddown
            | Action::WalkWinddownAlt
            | Action::RunWinddown
            | Action::RunWinddownAlt
    )
}

/// Whether `action` is an attack.
pub const fn is_attack(action: Action) -> bool {
    matches!(
        action,
        Action::LightAttack
            | Action::HeavyAttack
            | Action::VertLightAttack
            | Action::VertHeavyAttack
            | Action::RangedAttack
    )
}

/// Whether the motor drives `action` as a fixed-length evade.
pub const fn is_motor_fixed(action: Action) -> bool {
    matches!(
        action,
        Action::Roll | Action::Backstep | Action::DashLeft | Action::DashRight
    )
}

/// Crossfade length for a change from `from` to `to`, rather than one number
/// for every change. What reads well depends on both ends: an attack must
/// start crisply but settle slowly, and a gait change can afford a long fade
/// only because the clips are phase-matched.
pub const fn blend_ticks(from: Action, to: Action) -> u32 {
    if matches!(to, Action::Fall) {
        0
    } else if matches!(to, Action::Land) {
        2
    } else if matches!(to, Action::HookLaunch) {
        4
    } else if matches!(from, Action::HookLaunch) {
        10
    } else if matches!(from, Action::WalkWindup) && matches!(to, Action::Walk) {
        // The windup reaches the moving walk entry pose. Blending from its
        // clamped endpoint would brake the stride again after preparation.
        0
    } else if is_stop(to) && is_gait(from) {
        4
    } else if is_attack(to) || is_motor_fixed(to) {
        // Entering a committed action: its first frames carry the read.
        BLEND_ACTION_TICKS
    } else if is_attack(from) || matches!(from, Action::Intro) {
        // Leaving one: a long swing needs somewhere to land.
        BLEND_ACTION_OUT_TICKS
    } else if is_gait(from) && is_gait(to) {
        BLEND_GAIT_TICKS
    } else {
        BLEND_LOCOMOTION_TICKS
    }
}

/// Ticks to backdate an incoming gait's clock by so it starts in phase with
/// the outgoing one: the outgoing clip is `local` ticks into its clock and
/// loops every `out_cycle` ticks, the incoming one every `in_cycle`.
/// Cook-time alignment puts foot-down at frame 0 of every gait clip, so equal
/// phase is the same point in the stride and the feet stay in step across
/// the change. Zero when either cycle is unknown.
pub fn gait_phase_carry(local: u32, out_cycle: u32, in_cycle: u32) -> u32 {
    if out_cycle == 0 || in_cycle == 0 {
        return 0;
    }
    // local % out_cycle < out_cycle guarantees the exact quotient is below
    // in_cycle, even when the product needs both MULTU result words.
    psx_math::int32::mul_div_u32(local % out_cycle, in_cycle, out_cycle)
}

/// What a fighter shows: anything that names the animation action it plays,
/// which decides how changes to and from it crossfade.
pub trait AnimState: Copy + PartialEq {
    /// The animation action this state plays.
    fn action(self) -> Action;
}

impl AnimState for PlayerAnim {
    fn action(self) -> Action {
        PlayerAnim::action(self)
    }
}

/// The state a fighter switched away from: its clip-local tick at the
/// switch and the tick the switch happened, from which the crossfade ramps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimSwitch<S> {
    /// Outgoing state.
    pub from: S,
    /// Outgoing clip's local tick at the switch.
    pub local_tick: u32,
    /// Tick of the switch.
    pub at: SimTick,
}

/// The crossfade a fighter shows at one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimFade<S> {
    /// Outgoing state.
    pub from: S,
    /// Outgoing clip's local tick for this tick: it keeps playing while it
    /// fades, except a gait coming to rest, which holds its release pose
    /// (more steps would show after the body stopped).
    pub local_tick: u32,
    /// Q12 weight of the incoming (current) state: `1 << 12` is the
    /// current clip alone, 0 the outgoing pose alone.
    pub alpha_q12: u16,
}

/// One fighter's animation: the state it plays and the switch it is fading
/// from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimTrack<S> {
    /// Current state.
    pub state: S,
    /// Last switch, while its crossfade may still show.
    pub switched: Option<AnimSwitch<S>>,
}

impl<S: AnimState> AnimTrack<S> {
    /// A track showing `state`, with nothing to fade from.
    pub const fn new(state: S) -> Self {
        Self {
            state,
            switched: None,
        }
    }

    /// Switch to `next` at `now`, keeping the outgoing pose for the
    /// crossfade. `start` is the clip clock: the outgoing clip's local tick
    /// is read off it, and it restarts at `now` backdated by `carried` ticks
    /// (see [`gait_phase_carry`]).
    pub fn switch(&mut self, next: S, start: &mut SimTick, now: SimTick, carried: u32) {
        self.switched = Some(AnimSwitch {
            from: self.state,
            local_tick: now.saturating_sub(*start),
            at: now,
        });
        self.state = next;
        *start = SimTick::from_u32(now.as_u32().saturating_sub(carried));
    }

    /// The crossfade showing at `now`, if any. Alpha ramps linearly over
    /// [`blend_ticks`] of the change.
    pub fn fade(&self, now: SimTick) -> Option<AnimFade<S>> {
        let switched = self.switched?;
        let to = self.state.action();
        let from = switched.from.action();
        let duration = blend_ticks(from, to);
        let elapsed = now.saturating_sub(switched.at);
        if elapsed >= duration {
            return None;
        }
        Some(AnimFade {
            from: switched.from,
            local_tick: if is_gait(from) && is_stop(to) {
                switched.local_tick
            } else {
                switched.local_tick.saturating_add(elapsed)
            },
            alpha_q12: ((elapsed << 12) / duration.max(1)) as u16,
        })
    }
}

/// Which joints of a skeleton are its upper body and its legs, for the
/// layers a fighter plays while it moves and acts at once (a shot over a
/// gait, a gait under a wind-up). Read off the skeleton's own parent table,
/// so every rig gets them, whatever its joint count: from the root, the
/// child subtree with the most joints is the upper body (spine, arms and
/// head hang from it), and every other child subtree is a leg. The root
/// itself is in neither. Joints past 31 cannot be masked and are left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyJointMasks {
    /// The upper body's joints; 0 when the skeleton has no such split.
    pub upper_body: u32,
    /// The legs' joints; 0 when the skeleton has no such split.
    pub legs: u32,
}

impl BodyJointMasks {
    /// The masks of a `joint_count`-joint skeleton whose joint `j` has
    /// parent `parent(j)`. A skeleton whose root does not carry an upper body
    /// and at least two legs (a prop, a single chain) has no split: both
    /// masks are 0.
    pub fn from_parents(joint_count: u16, parent: impl Fn(u16) -> Option<u16>) -> Self {
        let count = joint_count.min(32);
        // The child of the root each joint hangs from (u16::MAX: the root,
        // or a joint outside the first root's tree).
        let branch_of = |joint: u16| {
            let mut child = joint;
            let mut steps = 0;
            while let Some(up) = parent(child) {
                if up == 0 {
                    return Some(child);
                }
                child = up;
                steps += 1;
                if steps > 32 {
                    break;
                }
            }
            None
        };
        if count == 0 || parent(0).is_some() {
            return Self::default();
        }
        let mut subtree = [0u32; 32];
        let mut size = [0u8; 32];
        for joint in 1..count {
            if let Some(child) = branch_of(joint) {
                let child = usize::from(child.min(31));
                subtree[child] |= 1 << joint;
                size[child] += 1;
            }
        }
        let branches = size.iter().filter(|&&n| n > 0).count();
        if branches < 3 {
            return Self::default();
        }
        let mut upper = 0;
        for child in 1..32 {
            if size[child] > size[upper] {
                upper = child;
            }
        }
        let all = subtree.iter().fold(0, |mask, branch| mask | branch);
        Self {
            upper_body: subtree[upper],
            legs: all & !subtree[upper],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [PlayerAnim; 35] = [
        PlayerAnim::Idle,
        PlayerAnim::Walk,
        PlayerAnim::WalkBackward,
        PlayerAnim::StrafeLeft,
        PlayerAnim::StrafeRight,
        PlayerAnim::Run,
        PlayerAnim::Roll,
        PlayerAnim::Quickstep,
        PlayerAnim::DashLeft,
        PlayerAnim::DashRight,
        PlayerAnim::LightAttack,
        PlayerAnim::HeavyAttack,
        PlayerAnim::RangedAttack,
        PlayerAnim::RangedAim,
        PlayerAnim::RangedWalk,
        PlayerAnim::RangedBackward,
        PlayerAnim::RangedLeft,
        PlayerAnim::RangedRight,
        PlayerAnim::HookLaunch,
        PlayerAnim::Fall,
        PlayerAnim::Land,
        PlayerAnim::VertLightAttack,
        PlayerAnim::VertHeavyAttack,
        PlayerAnim::Intro,
        PlayerAnim::Death,
        PlayerAnim::HitReact,
        PlayerAnim::Stun,
        PlayerAnim::Parried,
        PlayerAnim::Knockdown,
        PlayerAnim::WalkWindup,
        PlayerAnim::WalkWinddown,
        PlayerAnim::WalkWinddownAlt,
        PlayerAnim::RunWindup,
        PlayerAnim::RunWinddown,
        PlayerAnim::RunWinddownAlt,
    ];

    #[test]
    fn the_action_predicates_agree_with_the_player_states() {
        for anim in ALL {
            assert_eq!(is_gait(anim.action()), anim.is_gait(), "{anim:?}");
            assert_eq!(
                is_motor_fixed(anim.action()),
                anim.is_motor_fixed_action(),
                "{anim:?}"
            );
            assert_eq!(
                is_attack(anim.action()),
                crate::character::player_anim_is_attack(anim),
                "{anim:?}"
            );
        }
    }

    #[test]
    fn a_switch_restarts_the_clock_backdated_by_the_carry() {
        let mut start = SimTick::from_u32(100);
        let mut track = AnimTrack::new(PlayerAnim::Walk);
        track.switch(
            PlayerAnim::StrafeLeft,
            &mut start,
            SimTick::from_u32(130),
            7,
        );
        assert_eq!(start, SimTick::from_u32(123));
        assert_eq!(track.state, PlayerAnim::StrafeLeft);
        let fade = track.fade(SimTick::from_u32(132)).unwrap();
        assert_eq!(fade.from, PlayerAnim::Walk);
        assert_eq!(fade.local_tick, 32);
        assert_eq!(u32::from(fade.alpha_q12), (2 << 12) / BLEND_GAIT_TICKS);
        assert!(track
            .fade(SimTick::from_u32(130 + BLEND_GAIT_TICKS))
            .is_none());
    }

    fn masks(parents: &[Option<u16>]) -> BodyJointMasks {
        BodyJointMasks::from_parents(parents.len() as u16, |joint| parents[usize::from(joint)])
    }

    #[test]
    fn the_body_masks_come_from_the_skeleton_for_every_rig() {
        // Aletha's 26 joints: hips, two three-joint legs, then the spine.
        let mut aletha = [None; 26];
        for (joint, parent) in [(1, 0), (2, 1), (3, 2), (4, 0), (5, 4), (6, 5), (7, 0)] {
            aletha[joint] = Some(parent);
        }
        for parent in aletha.iter_mut().skip(8) {
            *parent = Some(7);
        }
        assert_eq!(
            masks(&aletha),
            BodyJointMasks {
                upper_body: 0x03ff_ff80,
                legs: 0x0000_007e,
            }
        );
        // The light enemy's 22 joints (its cooked parent table): the spine
        // chain with both arms, then two four-joint legs off the root.
        let light: [Option<u16>; 22] = [
            None,
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(3),
            Some(6),
            Some(7),
            Some(8),
            Some(3),
            Some(10),
            Some(11),
            Some(12),
            Some(0),
            Some(14),
            Some(15),
            Some(16),
            Some(0),
            Some(18),
            Some(19),
            Some(20),
        ];
        assert_eq!(
            masks(&light),
            BodyJointMasks {
                upper_body: 0x0000_3ffe,
                legs: 0x003f_c000,
            }
        );
        // A prop's single chain, or a root with two parts, has no split.
        assert_eq!(masks(&[None, Some(0), Some(1)]), BodyJointMasks::default());
        assert_eq!(masks(&[None, Some(0), Some(0)]), BodyJointMasks::default());
        assert_eq!(masks(&[None]), BodyJointMasks::default());
    }

    #[test]
    fn the_gait_carry_keeps_the_same_point_in_the_stride() {
        assert_eq!(gait_phase_carry(45, 40, 60), 7);
        assert_eq!(gait_phase_carry(20, 40, 60), 30);
        assert_eq!(gait_phase_carry(20, 0, 60), 0);
        assert_eq!(gait_phase_carry(20, 40, 0), 0);
    }
}
