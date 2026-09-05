//! Souls-like melee-combat resolution (the phase-3 combat slice of
//! docs/game-runtime-plan.md): a flat XZ ARC in front of an attacker
//! tested against hurtbox cylinders. Grid-native and integer-only --
//! per-axis early-outs, one exact squared compare, and one octant
//! `atan2` on the survivors -- so a whiffed swing costs a handful of
//! loads and a connected one stays deep inside the budget's 10k-cycle
//! combat line. Deliberately NOT weapon-shape hitboxes: capsule/point
//! vs arc is the phase-3 shape, and rig-attached
//! [`CombatCapsuleRecord`]s are the authored upgrade path. The cooked
//! grip-local `WeaponHitShapeRecord` geometry is authoring-only data
//! today: nothing at runtime reads the shapes (only the hitbox
//! records' active frame windows, below).
//!
//! The player's arc parameters and ACTIVE window come from the cooked
//! contract: the first PLAYER-flagged [`EquipmentRecord`] resolves a
//! [`LevelWeaponRecord`] whose `arc_*`/`damage` fields size the arc
//! and whose hitbox `active_start_frame`/`active_end_frame` windows
//! (character attack-clip animation frames) bound the hit window --
//! windup before, recovery after. An unarmed player falls back to the
//! [`UNARMED`] spec with the whole swing active.
//!
//! [`WeaponHitShapeRecord`]: psx_level::WeaponHitShapeRecord

use psx_engine::{JointWorldTransform, WorldVertex};
use psx_level::{
    combat_capsule_flags, equipment_flags, CharacterAnimationAction, CombatCapsuleRecord,
    EquipmentRecord, LevelWeaponRecord, RoomIndex, WeaponHitboxRecord,
    MAX_CHARACTER_COMBAT_CAPSULES,
};
use psx_math::atan2_q12;

use crate::actor_pose::ActorPoseSnapshot;
use crate::projectiles::{ProjectileDamageChannel, ProjectileVisualStyle};

/// One animated combat capsule in room-local world space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldCombatCapsule {
    /// World-space segment start.
    pub start: [i32; 3],
    /// World-space segment end.
    pub end: [i32; 3],
    /// Capsule radius in engine units.
    pub radius: u16,
}

impl WorldCombatCapsule {
    /// Zeroed fixed-array initializer. A caller tracks the populated length,
    /// so this value is never interpreted as an authored volume.
    pub const EMPTY: Self = Self {
        start: [0; 3],
        end: [0; 3],
        radius: 0,
    };
}

/// Transform one compact joint-local record through an already sampled joint.
/// Callers can reuse the joint transform for multiple volumes on the same bone.
pub fn transform_combat_capsule(
    record: &CombatCapsuleRecord,
    joint: JointWorldTransform,
) -> WorldCombatCapsule {
    WorldCombatCapsule {
        start: transform_joint_local_point(joint, record.start),
        end: transform_joint_local_point(joint, record.end),
        radius: record.radius,
    }
}

/// Transform one authored capsule through the shared per-tick actor pose.
///
/// This is the preferred gameplay entry point: it prevents combat from
/// rebuilding animation phase or presentation transforms separately from the
/// visible body and its equipment sockets.
pub fn transform_actor_combat_capsule(
    record: &CombatCapsuleRecord,
    pose: ActorPoseSnapshot,
) -> Option<WorldCombatCapsule> {
    Some(transform_combat_capsule(
        record,
        pose.joint_world_transform(u16::from(record.joint))?,
    ))
}

/// Animation-timed rig attachment which releases one projectile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthoredProjectileRelease {
    /// Room-local muzzle center sampled from the retained actor pose.
    pub position: [i32; 3],
    /// Projectile swept-sphere radius.
    pub radius: u16,
    /// Displacement per 60 Hz simulation tick.
    pub speed: u16,
    /// Lifetime in 60 Hz simulation ticks.
    pub lifetime_ticks: u16,
    /// AI minimum attack range.
    pub min_range: u16,
    /// AI maximum attack range.
    pub max_range: u16,
    /// Health damage on contact.
    pub damage: u16,
    /// Poise damage on contact.
    pub poise_damage: u16,
    /// Additive render tint.
    pub tint_rgb: [u8; 3],
    /// Typed destination in the dual-vitality system.
    pub damage_channel: ProjectileDamageChannel,
    /// Bounded bolt/trail/impact presentation profile.
    pub visual: ProjectileVisualStyle,
}

/// Readable pre-release charge sampled from the same retained actor pose as
/// the eventual projectile. Rendering this query cannot desynchronise the
/// muzzle from animation because it performs no independent phase advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthoredProjectileCharge {
    /// Room-local animated muzzle center.
    pub position: [i32; 3],
    /// Collision radius used to size the charge flare.
    pub radius: u16,
    /// Charge completion in Q8 (`0..=256`).
    pub progress_q8: u16,
    /// Bounded projectile presentation profile.
    pub visual: ProjectileVisualStyle,
}

fn projectile_visual(record: &CombatCapsuleRecord) -> ProjectileVisualStyle {
    ProjectileVisualStyle {
        core_rgb: record.projectile_core_rgb,
        glow_rgb: record.projectile_glow_rgb,
        impact_rgb: record.projectile_impact_rgb,
        glow_scale_q8: record.projectile_glow_scale_q8.max(256),
        length_ticks: record.projectile_length_ticks.max(1),
        trail_segments: record.projectile_trail_segments.min(6),
        trail_spacing_ticks: record.projectile_trail_spacing_ticks.max(1),
        impact_lifetime_ticks: record.projectile_impact_lifetime_ticks.max(1),
        break_fragment_count: 0,
    }
}

/// Resolve the first projectile emitter active on an actor's retained pose.
///
/// Release windows are inclusive. A one-frame event also stays eligible after
/// its exact marker: low-rate NPC updates may jump from frame 17 to 19, and the
/// attack state's one-shot latch makes the first retained pose at/after frame
/// 18 the deterministic crossing of that event. The query remains retryable if
/// the fixed projectile pool is temporarily full.
pub fn authored_projectile_release(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<ActorPoseSnapshot>,
) -> Option<AuthoredProjectileRelease> {
    authored_projectile_release_pending(capsules, action, pose, 0).map(|(_, release)| release)
}

/// Find an eligible emitter that has not fired in this attack. The returned
/// index is relative to the actor's capsule slice and fits a sixteen-bit mask.
/// The caller marks it only after successful insertion into the projectile pool.
pub fn authored_projectile_release_pending(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<ActorPoseSnapshot>,
    released_mask: u16,
) -> Option<(u8, AuthoredProjectileRelease)> {
    let pose = pose?;
    let frame = (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
    let action = action.to_index() as u8;
    capsules
        .iter()
        .take(MAX_CHARACTER_COMBAT_CAPSULES)
        .enumerate()
        .find_map(|(index, record)| {
            if released_mask & (1u16 << index) != 0 {
                return None;
            }
            let exact_event_crossed = record.active_start_frame == record.active_end_frame
                && frame >= record.active_start_frame;
            let inside_window =
                frame >= record.active_start_frame && frame <= record.active_end_frame;
            if record.flags & combat_capsule_flags::PROJECTILE_EMITTER == 0
                || record.action != action
                || (!inside_window && !exact_event_crossed)
            {
                return None;
            }
            let muzzle = transform_actor_combat_capsule(record, pose)?;
            Some((index as u8, AuthoredProjectileRelease {
                position: muzzle.start,
                radius: muzzle.radius,
                speed: record.projectile_speed,
                lifetime_ticks: record.projectile_lifetime_ticks,
                min_range: record.projectile_min_range,
                max_range: record.projectile_max_range,
                damage: record.damage,
                poise_damage: record.poise_damage,
                tint_rgb: record.projectile_tint_rgb,
                damage_channel: ProjectileDamageChannel::from_raw(record.projectile_damage_channel),
                visual: projectile_visual(record),
            }))
        })
}

/// Resolve the first projectile emitter currently inside its authored charge
/// window. The release frame itself belongs to the bolt/muzzle-flash path.
pub fn authored_projectile_charge(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<ActorPoseSnapshot>,
) -> Option<AuthoredProjectileCharge> {
    let pose = pose?;
    let frame = (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
    let action = action.to_index() as u8;
    capsules
        .iter()
        .take(MAX_CHARACTER_COMBAT_CAPSULES)
        .find_map(|record| {
            if record.flags & combat_capsule_flags::PROJECTILE_EMITTER == 0
                || record.action != action
                || frame < record.projectile_charge_start_frame
                || frame >= record.active_start_frame
            {
                return None;
            }
            let muzzle = transform_actor_combat_capsule(record, pose)?;
            let duration = record
                .active_start_frame
                .saturating_sub(record.projectile_charge_start_frame)
                .max(1);
            let elapsed = frame.saturating_sub(record.projectile_charge_start_frame);
            Some(AuthoredProjectileCharge {
                position: muzzle.start,
                radius: muzzle.radius,
                progress_q8: ((u32::from(elapsed) * 256) / u32::from(duration)).min(256) as u16,
                visual: projectile_visual(record),
            })
        })
}

/// Result of resolving one retained-pose actor attack against another.
///
/// `FallbackRequired` has deliberately narrow semantics: it means either the
/// attacking action has no authored HITBOX at all or the defender has no
/// authored HURTBOX at all. Once both sides are authored, inactive frames,
/// invalid joints, missing snapshots, and geometric separation are
/// authoritative misses and must never invoke a legacy radius/arc test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoredActorContact {
    /// One side of the authored contract is absent; caller may use its
    /// explicitly documented legacy fallback.
    FallbackRequired,
    /// Both sides are authored but do not connect on this exact pose/frame.
    Miss,
    /// First deterministic authored HITBOX/HURTBOX overlap.
    Hit {
        /// Health damage from the attacking HITBOX.
        damage: u16,
        /// Poise damage from the attacking HITBOX.
        poise_damage: u16,
    },
}

/// Resolve one authored attack using only two already-retained actor poses.
///
/// This performs no animation phase reconstruction and allocates nothing. The
/// attack frame comes from `attacker_pose.phase_q12()`, exactly matching body
/// and equipment consumers of that snapshot. Cooked slices are capped at the
/// per-character contract even if malformed caller data is longer.
pub fn resolve_authored_actor_contact(
    attacker_capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    attacker_pose: Option<ActorPoseSnapshot>,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: Option<ActorPoseSnapshot>,
) -> AuthoredActorContact {
    resolve_authored_actor_contact_pending(attacker_capsules, action, attacker_pose,
        defender_capsules, defender_pose, 0).0
}

/// Resolve an unconsumed melee window. Simultaneously active hitboxes share
/// one contact; later separated windows can each connect once. Only commit
/// the returned mask after invulnerability and world occlusion checks pass.
pub fn resolve_authored_actor_contact_pending(
    attacker_capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    attacker_pose: Option<ActorPoseSnapshot>,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: Option<ActorPoseSnapshot>,
    connected_mask: u16,
) -> (AuthoredActorContact, u16) {
    let action = action.to_index() as u8;
    let has_attack_hitbox = attacker_capsules
        .iter()
        .any(|record| record.flags & combat_capsule_flags::HITBOX != 0 && record.action == action);
    let has_defender_hurtbox = defender_capsules
        .iter()
        .any(|record| record.flags & combat_capsule_flags::HURTBOX != 0);
    if !has_attack_hitbox || !has_defender_hurtbox {
        return (AuthoredActorContact::FallbackRequired, 0);
    }
    let (Some(attacker_pose), Some(defender_pose)) = (attacker_pose, defender_pose) else {
        return (AuthoredActorContact::Miss, 0);
    };
    let frame = (attacker_pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
    let mut active = [WorldCombatCapsule::EMPTY; MAX_CHARACTER_COMBAT_CAPSULES];
    let mut damage = [0u16; MAX_CHARACTER_COMBAT_CAPSULES];
    let mut poise_damage = [0u16; MAX_CHARACTER_COMBAT_CAPSULES];
    let mut active_count = 0usize;
    let mut window_mask = 0u16;
    for (index, record) in attacker_capsules.iter().take(MAX_CHARACTER_COMBAT_CAPSULES).enumerate() {
        if connected_mask & (1u16 << index) != 0
            || record.flags & combat_capsule_flags::HITBOX == 0
            || record.action != action
            || frame < record.active_start_frame
            || frame > record.active_end_frame
        {
            continue;
        }
        let Some(capsule) = transform_actor_combat_capsule(record, attacker_pose) else {
            continue;
        };
        window_mask |= 1u16 << index;
        active[active_count] = capsule;
        damage[active_count] = record.damage;
        poise_damage[active_count] = record.poise_damage;
        active_count += 1;
    }
    if active_count == 0 {
        return (AuthoredActorContact::Miss, 0);
    }
    for hurtbox in defender_capsules.iter().take(MAX_CHARACTER_COMBAT_CAPSULES) {
        if hurtbox.flags & combat_capsule_flags::HURTBOX == 0 {
            continue;
        }
        let Some(hurtbox) = transform_actor_combat_capsule(hurtbox, defender_pose) else {
            continue;
        };
        let mut hit = 0usize;
        while hit < active_count {
            if combat_capsules_overlap(&active[hit], &hurtbox) {
                return (AuthoredActorContact::Hit {
                    damage: damage[hit],
                    poise_damage: poise_damage[hit],
                }, window_mask);
            }
            hit += 1;
        }
    }
    (AuthoredActorContact::Miss, 0)
}

/// Four blade-edge samples, clamped to one authored damage window. The trail
/// disappears in gaps and cannot bridge from one swing into the next.
pub fn melee_trail_phases(record: &CombatCapsuleRecord, action: CharacterAnimationAction, phase: u32) -> Option<[u32; 4]> {
    let start = u32::from(record.active_start_frame) << 12;
    let end = (u32::from(record.active_end_frame) + 1) << 12;
    if record.flags & combat_capsule_flags::HITBOX == 0
        || record.action != action.to_index() as u8 || phase < start || phase >= end {
        return None;
    }
    Some(core::array::from_fn(|i| phase.saturating_sub((2 << 12) * i as u32 / 3).max(start)))
}

fn transform_joint_local_point(joint: JointWorldTransform, local: [i16; 3]) -> [i32; 3] {
    let rotate = |row: [i16; 3]| {
        i32::from(row[0])
            .saturating_mul(i32::from(local[0]))
            .saturating_add(i32::from(row[1]).saturating_mul(i32::from(local[1])))
            .saturating_add(i32::from(row[2]).saturating_mul(i32::from(local[2])))
            >> 12
    };
    let WorldVertex { x, y, z } = joint.translation;
    [
        x.saturating_add(rotate(joint.rotation.m[0])),
        y.saturating_add(rotate(joint.rotation.m[1])),
        z.saturating_add(rotate(joint.rotation.m[2])),
    ]
}

/// Six-compare broad phase. Call this before [`combat_capsules_overlap`]; it
/// rejects separated limbs without division or 64-bit segment math.
pub fn combat_capsule_aabbs_overlap(a: &WorldCombatCapsule, b: &WorldCombatCapsule) -> bool {
    let ar = i32::from(a.radius);
    let br = i32::from(b.radius);
    let mut axis = 0usize;
    while axis < 3 {
        let a_min = a.start[axis].min(a.end[axis]).saturating_sub(ar);
        let a_max = a.start[axis].max(a.end[axis]).saturating_add(ar);
        let b_min = b.start[axis].min(b.end[axis]).saturating_sub(br);
        let b_max = b.start[axis].max(b.end[axis]).saturating_add(br);
        if a_max < b_min || b_max < a_min {
            return false;
        }
        axis += 1;
    }
    true
}

/// Capsule/capsule narrow phase after the AABB broad phase. Two bounded
/// alternating projections find the closest points in Q12. This deliberately
/// uses only 32-bit integer math: on PS1 it is substantially cheaper than the
/// closed-form segment solution's 64-bit products and divisions. The AABB
/// rejection and authored 16-volume cap bound the uncommon projection path.
pub fn combat_capsules_overlap(a: &WorldCombatCapsule, b: &WorldCombatCapsule) -> bool {
    if !combat_capsule_aabbs_overlap(a, b) {
        return false;
    }
    let a_delta = sub3(a.end, a.start);
    let b_delta = sub3(b.end, b.start);
    let mut a_point = closest_point_on_segment(b.start, a.start, a_delta);
    let mut b_point = closest_point_on_segment(a_point, b.start, b_delta);
    // One refinement handles the interior/interior skew case. Parallel and
    // endpoint cases have already converged after the first pair.
    a_point = closest_point_on_segment(b_point, a.start, a_delta);
    b_point = closest_point_on_segment(a_point, b.start, b_delta);

    let delta = sub3(a_point, b_point);
    let distance_sq = square_sum_saturating(delta);
    let radii = i32::from(a.radius).saturating_add(i32::from(b.radius));
    distance_sq <= radii.saturating_mul(radii)
}

/// Clip one consecutive pose interval to an inclusive active-frame window.
///
/// When `previous_phase_q12` is absent, only the current pose can participate.
/// With a consecutive pose, this also catches a fast action which enters and
/// leaves the entire active window between two simulation ticks.
pub fn active_frame_sweep_phase_range(
    previous_phase_q12: Option<u32>,
    current_phase_q12: u32,
    active_start_frame: u16,
    active_end_frame: u16,
) -> Option<(u32, u32)> {
    let active_start_q12 = u32::from(active_start_frame) << 12;
    let active_end_q12 = (u32::from(active_end_frame.max(active_start_frame)).saturating_add(1)
        << 12)
        .saturating_sub(1);
    let Some(previous_phase_q12) = previous_phase_q12 else {
        return (current_phase_q12 >= active_start_q12 && current_phase_q12 <= active_end_q12)
            .then_some((current_phase_q12, current_phase_q12));
    };
    if current_phase_q12 < previous_phase_q12 {
        return None;
    }
    let start = previous_phase_q12.max(active_start_q12);
    let end = current_phase_q12.min(active_end_q12);
    (start <= end).then_some((start, end))
}

/// Number of temporal intervals used by the retained-pose melee sweep.
///
/// Nine complete blade samples are still cheap after the swept AABB rejection.
/// The conservative radius between samples is at most one sixteenth of the
/// greatest endpoint displacement over the tick.
const COMBAT_CAPSULE_MOTION_INTERVALS: i32 = 8;

/// Broad phase for a capsule moving from `previous` to `current` in one tick.
///
/// The endpoint paths are linear over the retained-pose interval. The extra
/// radius covers the unsampled time between the nine complete blade poses, so
/// this AABB encloses the whole swept blade rather than only its endpoints.
pub fn combat_capsule_motion_aabbs_overlap(
    previous: &WorldCombatCapsule,
    current: &WorldCombatCapsule,
    target: &WorldCombatCapsule,
) -> bool {
    let radius = i32::from(combat_capsule_motion_radius(previous, current));
    let target_radius = i32::from(target.radius);
    let mut axis = 0usize;
    while axis < 3 {
        let moving_min = previous.start[axis]
            .min(previous.end[axis])
            .min(current.start[axis])
            .min(current.end[axis])
            .saturating_sub(radius);
        let moving_max = previous.start[axis]
            .max(previous.end[axis])
            .max(current.start[axis])
            .max(current.end[axis])
            .saturating_add(radius);
        let target_min = target.start[axis]
            .min(target.end[axis])
            .saturating_sub(target_radius);
        let target_max = target.start[axis]
            .max(target.end[axis])
            .saturating_add(target_radius);
        if moving_max < target_min || target_max < moving_min {
            return false;
        }
        axis += 1;
    }
    true
}

/// Test a capsule's complete retained-pose motion against a target capsule.
///
/// Discrete overlap alone can tunnel when an attack advances far enough that
/// the sword is clear on both simulation ticks but crosses a hurtbox between
/// them. Nine complete interpolated blade capsules sample the interval. Each is
/// expanded by half the greatest endpoint travel between adjacent samples, so
/// every unsampled blade pose is contained by its nearest sample. This covers
/// the full linearly swept blade, including its interior, with only bounded
/// 32-bit integer work.
pub fn combat_capsule_motion_overlaps(
    previous: &WorldCombatCapsule,
    current: &WorldCombatCapsule,
    target: &WorldCombatCapsule,
) -> bool {
    if !combat_capsule_motion_aabbs_overlap(previous, current, target) {
        return false;
    }
    if combat_capsules_overlap(previous, target) || combat_capsules_overlap(current, target) {
        return true;
    }

    let radius = combat_capsule_motion_radius(previous, current);
    let mut sample = 0i32;
    while sample <= COMBAT_CAPSULE_MOTION_INTERVALS {
        let sampled = sample_capsule_motion(previous, current, sample, radius);
        if combat_capsules_overlap(&sampled, target) {
            return true;
        }
        sample += 1;
    }
    false
}

fn combat_capsule_motion_radius(
    previous: &WorldCombatCapsule,
    current: &WorldCombatCapsule,
) -> u16 {
    // L1 distance is a conservative, square-root-free upper bound on endpoint
    // travel. Any point along the blade moves no faster than the faster of its
    // two endpoints under linear interpolation. Half one temporal interval is
    // therefore the furthest an unsampled pose can sit from its nearest sample.
    let longest = point_l1_distance(previous.start, current.start)
        .max(point_l1_distance(previous.end, current.end));
    let diameter_intervals = (COMBAT_CAPSULE_MOTION_INTERVALS as u32).saturating_mul(2);
    let coverage = longest.div_ceil(diameter_intervals.max(1));
    previous
        .radius
        .max(current.radius)
        .saturating_add(coverage.min(u32::from(u16::MAX)) as u16)
}

fn point_l1_distance(a: [i32; 3], b: [i32; 3]) -> u32 {
    a[0].abs_diff(b[0])
        .saturating_add(a[1].abs_diff(b[1]))
        .saturating_add(a[2].abs_diff(b[2]))
}

fn sample_capsule_motion(
    previous: &WorldCombatCapsule,
    current: &WorldCombatCapsule,
    sample: i32,
    radius: u16,
) -> WorldCombatCapsule {
    let interpolate = |from: [i32; 3], to: [i32; 3], axis: usize| {
        let delta = to[axis].saturating_sub(from[axis]);
        let whole = delta / COMBAT_CAPSULE_MOTION_INTERVALS;
        let remainder = delta % COMBAT_CAPSULE_MOTION_INTERVALS;
        from[axis]
            .saturating_add(whole.saturating_mul(sample))
            .saturating_add(remainder.saturating_mul(sample) / COMBAT_CAPSULE_MOTION_INTERVALS)
    };
    WorldCombatCapsule {
        start: [
            interpolate(previous.start, current.start, 0),
            interpolate(previous.start, current.start, 1),
            interpolate(previous.start, current.start, 2),
        ],
        end: [
            interpolate(previous.end, current.end, 0),
            interpolate(previous.end, current.end, 1),
            interpolate(previous.end, current.end, 2),
        ],
        radius,
    }
}

/// Return the earliest Q12 phase at which a swept capsule reaches `target`.
///
/// `sweep.start` is the moving sphere's position before a simulation tick and
/// `sweep.end` is its position afterwards. Each probe keeps the start fixed and
/// shortens only the end, so overlap is monotonic: once the accumulated sweep
/// touches the target, every longer prefix also touches it. Twelve bounded
/// probes therefore find the first engine-visible contact without floating
/// point or 64-bit division. This is shared by projectiles and any later
/// continuous hit-volume query that must not tunnel between ticks.
pub fn combat_capsule_sweep_contact_fraction_q12(
    sweep: &WorldCombatCapsule,
    target: &WorldCombatCapsule,
) -> Option<u16> {
    if !combat_capsules_overlap(sweep, target) {
        return None;
    }
    let start = WorldCombatCapsule {
        start: sweep.start,
        end: sweep.start,
        radius: sweep.radius,
    };
    if combat_capsules_overlap(&start, target) {
        return Some(0);
    }

    // Phase zero is known clear and phases 1..=4096 contain exactly 4096
    // candidates, so this search always finishes in twelve iterations.
    let mut low = 1u16;
    let mut high = 4096u16;
    while low < high {
        let mid = low + (high - low) / 2;
        let prefix = WorldCombatCapsule {
            start: sweep.start,
            end: interpolate3_q12(sweep.start, sweep.end, mid),
            radius: sweep.radius,
        };
        if combat_capsules_overlap(&prefix, target) {
            high = mid;
        } else {
            low = mid.saturating_add(1);
        }
    }
    Some(low)
}

fn interpolate3_q12(start: [i32; 3], end: [i32; 3], phase_q12: u16) -> [i32; 3] {
    let delta = sub3(end, start);
    let phase = i32::from(phase_q12.min(4096));
    [
        start[0].saturating_add(mul_q12(delta[0], phase)),
        start[1].saturating_add(mul_q12(delta[1], phase)),
        start[2].saturating_add(mul_q12(delta[2], phase)),
    ]
}

fn closest_point_on_segment(point: [i32; 3], start: [i32; 3], delta: [i32; 3]) -> [i32; 3] {
    let relative = sub3(point, start);
    let shift = projection_shift(relative, delta);
    let relative = shift3(relative, shift);
    let scaled_delta = shift3(delta, shift);
    let numerator = dot3_saturating(relative, scaled_delta);
    let denominator = dot3_saturating(scaled_delta, scaled_delta);
    let phase = ratio_q12(numerator, denominator);
    [
        start[0].saturating_add(mul_q12(delta[0], phase)),
        start[1].saturating_add(mul_q12(delta[1], phase)),
        start[2].saturating_add(mul_q12(delta[2], phase)),
    ]
}

fn mul_q12(value: i32, phase: i32) -> i32 {
    let whole = value / 4096;
    let fraction = value % 4096;
    whole
        .saturating_mul(phase)
        .saturating_add(fraction.saturating_mul(phase) / 4096)
}

/// Shared right shift which keeps three signed component products inside i32.
/// Applying the same shift to numerator and denominator preserves the ratio.
fn projection_shift(a: [i32; 3], b: [i32; 3]) -> u32 {
    let mut maximum = 0i32;
    let mut axis = 0usize;
    while axis < 3 {
        maximum = maximum.max(a[axis].saturating_abs());
        maximum = maximum.max(b[axis].saturating_abs());
        axis += 1;
    }
    let mut shift = 0u32;
    while maximum > 16_000 {
        maximum >>= 1;
        shift += 1;
    }
    shift
}

fn shift3(value: [i32; 3], shift: u32) -> [i32; 3] {
    [value[0] >> shift, value[1] >> shift, value[2] >> shift]
}

fn ratio_q12(mut numerator: i32, mut denominator: i32) -> i32 {
    if numerator <= 0 || denominator <= 0 {
        return 0;
    }
    if numerator >= denominator {
        return 4096;
    }
    // Keep the Q12 upscale in range without wide arithmetic. Both operands
    // are reduced together, preserving the fraction to better than 1/4096.
    while denominator > i32::MAX / 4096 {
        numerator >>= 1;
        denominator >>= 1;
    }
    numerator.saturating_mul(4096) / denominator.max(1)
}

fn sub3(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[0].saturating_sub(b[0]),
        a[1].saturating_sub(b[1]),
        a[2].saturating_sub(b[2]),
    ]
}

fn dot3_saturating(a: [i32; 3], b: [i32; 3]) -> i32 {
    a[0].saturating_mul(b[0])
        .saturating_add(a[1].saturating_mul(b[1]))
        .saturating_add(a[2].saturating_mul(b[2]))
}

fn square_sum_saturating(value: [i32; 3]) -> i32 {
    value[0]
        .saturating_mul(value[0])
        .saturating_add(value[1].saturating_mul(value[1]))
        .saturating_add(value[2].saturating_mul(value[2]))
}

/// One melee swing volume: a flat arc in the attacker's room, opened
/// `half_angle` PSX angle units to each side of `yaw`, `reach` engine
/// units deep. Positions are room-local (the cooked convention), so
/// the arc only tests targets in the same room; vertical extent is
/// implicit (one room, one combat floor -- the phase-3 scope).
#[derive(Debug, Clone, Copy)]
pub struct MeleeArc {
    /// Room the attacker stands in.
    pub room: RoomIndex,
    /// Attacker origin X, room-local engine units.
    pub x: i32,
    /// Attacker origin Z, room-local engine units.
    pub z: i32,
    /// Facing, PSX angle units (the motor convention: x = sin,
    /// z = cos).
    pub yaw: u16,
    /// Arc depth from the origin, engine units.
    pub reach: i32,
    /// Arc half-width to each side of `yaw`, PSX angle units.
    pub half_angle: u16,
}

/// Resolved player melee attack: arc geometry, hit numbers, and the
/// active window in attack-clip animation frames (`None` = the whole
/// swing is active, the unarmed/authored-window-free fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerMeleeSpec {
    /// Arc depth, engine units.
    pub reach: i32,
    /// Arc half-width, PSX angle units.
    pub half_angle: u16,
    /// Damage per connection.
    pub damage: u16,
    /// Poise damage per connection.
    pub poise_damage: u16,
    /// Active window in animation frames (inclusive), unioned over
    /// the weapon's hitboxes.
    pub active_window: Option<(u16, u16)>,
}

/// Unarmed fallback spec: fist reach barely past two touching body
/// capsules, a narrow 45-degree half-arc, chip damage, and the whole
/// swing active (no cooked window to source).
pub const UNARMED: PlayerMeleeSpec = PlayerMeleeSpec {
    reach: 448,
    half_angle: 512,
    damage: 10,
    poise_damage: 15,
    active_window: None,
};

/// Heavy-attack scaling over the authored (light-attack) numbers:
/// damage x3/2, poise damage x2 -- the souls grammar's slower, more
/// staggering commitment. Runtime policy, deliberately not authored
/// per-weapon in this slice.
pub const HEAVY_DAMAGE_NUM: u32 = 3;
/// See [`HEAVY_DAMAGE_NUM`].
pub const HEAVY_DAMAGE_DEN: u32 = 2;
/// Poise-damage multiplier for heavy attacks.
pub const HEAVY_POISE_MUL: u32 = 2;

impl PlayerMeleeSpec {
    /// This spec with the heavy-attack scaling applied.
    pub fn heavy(self) -> Self {
        Self {
            damage: scale_u16(self.damage, HEAVY_DAMAGE_NUM, HEAVY_DAMAGE_DEN),
            poise_damage: scale_u16(self.poise_damage, HEAVY_POISE_MUL, 1),
            ..self
        }
    }

    /// Whether attack-clip animation frame `frame` is inside the
    /// active window (always true without an authored window).
    pub fn frame_active(&self, frame: u32) -> bool {
        match self.active_window {
            Some((start, end)) => frame >= u32::from(start) && frame <= u32::from(end),
            None => true,
        }
    }
}

fn scale_u16(value: u16, num: u32, den: u32) -> u16 {
    ((u32::from(value)).saturating_mul(num) / den.max(1)).min(u32::from(u16::MAX)) as u16
}

/// Resolve the player's melee spec from the cooked tables: the first
/// PLAYER-flagged equipment record picks the weapon (souls loadout:
/// one right-hand arm), its arc fields size the swing, and its hitbox
/// frame windows union into the active window. Room-agnostic on
/// purpose -- the weapon follows the player across rooms even though
/// the equipment RECORD is pinned to the spawn room (the render path
/// matches: player equipment draws without a room filter). No player
/// equipment (or an all-zero weapon from a hand-rolled manifest)
/// falls back to [`UNARMED`].
pub fn player_melee_spec(
    equipment: &'static [EquipmentRecord],
    weapons: &'static [LevelWeaponRecord],
    weapon_hitboxes: &'static [WeaponHitboxRecord],
) -> PlayerMeleeSpec {
    let mut i = 0usize;
    while i < equipment.len() {
        let record = &equipment[i];
        i += 1;
        if record.flags & equipment_flags::PLAYER == 0 {
            continue;
        }
        let Some(weapon) = weapons.get(record.weapon.to_usize()) else {
            continue;
        };
        // A cooked weapon always carries a positive reach and damage
        // (the cook rejects zeros); a zero-reach record here is a
        // hand-rolled manifest, and the unarmed fallback keeps combat
        // functional instead of authoring-dead.
        if weapon.arc_reach == 0 || weapon.damage == 0 {
            continue;
        }
        return PlayerMeleeSpec {
            reach: i32::from(weapon.arc_reach),
            half_angle: weapon.arc_half_angle,
            damage: weapon.damage,
            poise_damage: weapon.poise_damage,
            active_window: hitbox_active_window(weapon, weapon_hitboxes),
        };
    }
    UNARMED
}

/// Union of a weapon's hitbox active windows (min start, max end) in
/// attack-clip animation frames, or `None` for a hitbox-free weapon.
fn hitbox_active_window(
    weapon: &LevelWeaponRecord,
    weapon_hitboxes: &'static [WeaponHitboxRecord],
) -> Option<(u16, u16)> {
    let first = weapon.hitbox_first.to_usize();
    let hitboxes =
        weapon_hitboxes.get(first..first.saturating_add(weapon.hitbox_count as usize))?;
    let mut window: Option<(u16, u16)> = None;
    let mut i = 0usize;
    while i < hitboxes.len() {
        let hitbox = &hitboxes[i];
        i += 1;
        let end = hitbox.active_end_frame.max(hitbox.active_start_frame);
        window = Some(match window {
            Some((start, stop)) => (start.min(hitbox.active_start_frame), stop.max(end)),
            None => (hitbox.active_start_frame, end),
        });
    }
    window
}

/// Point-blank pass distance: a target whose center is this close to
/// the attacker origin is hit regardless of facing (you cannot whiff
/// a swing through a body you are standing inside).
pub const ARC_POINT_BLANK: i32 = 64;

/// Does `arc` reach the hurtbox cylinder at `(cx, cz)` with
/// `radius`? Integer-only capsule/point-vs-arc: per-axis early-out on
/// `reach + radius`, exact squared distance (radii clamped so the sum
/// of squares stays inside `i32`, the `entities` convention), a
/// point-blank pass, then one octant `atan2` compared against the
/// half-angle. The target's own angular size is absorbed by authored
/// half-angles (souls swings are generous), documented rather than
/// computed.
pub fn arc_hits_circle(arc: &MeleeArc, cx: i32, cz: i32, radius: i32) -> bool {
    let total = arc
        .reach
        .saturating_add(radius.max(0))
        .clamp(0, i32::from(i16::MAX));
    let dx = cx.saturating_sub(arc.x);
    let dz = cz.saturating_sub(arc.z);
    if dx.abs() > total || dz.abs() > total {
        return false;
    }
    let d2 = dx * dx + dz * dz;
    if d2 > total * total {
        return false;
    }
    let point_blank = radius.max(0).saturating_add(ARC_POINT_BLANK);
    if d2 <= point_blank * point_blank {
        return true;
    }
    let to_target = atan2_q12(dx, dz);
    angle_within(to_target, arc.yaw, arc.half_angle)
}

/// Wrapped PSX-angle distance test: is `angle` within `half_angle`
/// units of `center` around the 4096-unit circle?
pub fn angle_within(angle: u16, center: u16, half_angle: u16) -> bool {
    let diff = (i32::from(angle) - i32::from(center)).rem_euclid(4096);
    let diff = if diff > 2048 { 4096 - diff } else { diff };
    diff <= i32::from(half_angle.min(2048))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::{WeaponHitShapeRecord, WeaponHitboxIndex, WeaponIndex};

    fn arc(yaw: u16, reach: i32, half_angle: u16) -> MeleeArc {
        MeleeArc {
            room: RoomIndex(0),
            x: 1000,
            z: 1000,
            yaw,
            reach,
            half_angle,
        }
    }

    fn capsule(start: [i32; 3], end: [i32; 3], radius: u16) -> WorldCombatCapsule {
        WorldCombatCapsule { start, end, radius }
    }

    #[test]
    fn capsule_narrow_phase_handles_crossing_segments_and_separation() {
        let horizontal = capsule([-100, 0, 0], [100, 0, 0], 10);
        let vertical = capsule([0, -100, 0], [0, 100, 0], 10);
        assert!(combat_capsules_overlap(&horizontal, &vertical));

        let separated = capsule([0, -100, 30], [0, 100, 30], 10);
        assert!(!combat_capsules_overlap(&horizontal, &separated));
    }

    #[test]
    fn capsule_narrow_phase_handles_spheres_and_endpoint_contacts() {
        let sphere = capsule([0, 0, 0], [0, 0, 0], 20);
        let segment = capsule([40, 0, 0], [100, 0, 0], 20);
        assert!(combat_capsules_overlap(&sphere, &segment));

        let gap = capsule([41, 0, 0], [100, 0, 0], 20);
        assert!(!combat_capsules_overlap(&sphere, &gap));
    }

    #[test]
    fn capsule_aabb_rejects_before_narrow_phase() {
        let a = capsule([0, 0, 0], [10, 0, 0], 4);
        let b = capsule([100, 0, 0], [110, 0, 0], 4);
        assert!(!combat_capsule_aabbs_overlap(&a, &b));
        assert!(!combat_capsules_overlap(&a, &b));
    }

    #[test]
    fn capsule_motion_catches_a_blade_crossing_between_clear_poses() {
        let previous = capsule([-600, -400, 0], [-600, 400, 0], 12);
        let current = capsule([600, -400, 0], [600, 400, 0], 12);
        let target = capsule([0, -40, 0], [0, 40, 0], 32);

        assert!(!combat_capsules_overlap(&previous, &target));
        assert!(!combat_capsules_overlap(&current, &target));
        assert!(combat_capsule_motion_aabbs_overlap(
            &previous, &current, &target
        ));
        assert!(combat_capsule_motion_overlaps(&previous, &current, &target));
    }

    #[test]
    fn active_frame_sweep_clips_entry_exit_and_a_fully_skipped_window() {
        assert_eq!(
            active_frame_sweep_phase_range(Some(9 << 12), 12 << 12, 10, 20),
            Some((10 << 12, 12 << 12))
        );
        assert_eq!(
            active_frame_sweep_phase_range(Some(19 << 12), 22 << 12, 10, 20),
            Some((19 << 12, (21 << 12) - 1))
        );
        assert_eq!(
            active_frame_sweep_phase_range(Some(9 << 12), 22 << 12, 10, 20),
            Some((10 << 12, (21 << 12) - 1))
        );
        assert_eq!(
            active_frame_sweep_phase_range(Some(2 << 12), 8 << 12, 10, 20),
            None
        );
        assert_eq!(
            active_frame_sweep_phase_range(None, 12 << 12, 10, 20),
            Some((12 << 12, 12 << 12))
        );
    }

    #[test]
    fn capsule_motion_covers_the_blade_interior_and_rejects_separation() {
        // The target is midway along a long blade. Sweeping only the two blade
        // endpoints would miss it; each temporal sample retains the full blade.
        let previous = capsule([-400, -800, 0], [-400, 800, 0], 8);
        let current = capsule([400, -800, 0], [400, 800, 0], 8);
        let middle = capsule([0, 0, 0], [0, 0, 0], 24);
        assert!(combat_capsule_motion_overlaps(&previous, &current, &middle));

        let separated = capsule([0, 0, 500], [0, 0, 500], 24);
        assert!(!combat_capsule_motion_aabbs_overlap(
            &previous, &current, &separated
        ));
        assert!(!combat_capsule_motion_overlaps(
            &previous, &current, &separated
        ));
    }

    #[test]
    fn swept_capsule_reports_first_contact_in_q12() {
        let sweep = capsule([0, 0, 0], [1_000, 0, 0], 10);
        let target = capsule([500, 0, 0], [500, 0, 0], 40);
        let phase = combat_capsule_sweep_contact_fraction_q12(&sweep, &target)
            .expect("sweep reaches target");
        // First contact is x=450, or 45% of the path. Q12 rounding may move
        // the reconstructed position by one engine unit.
        let x = i32::from(phase) * 1_000 / 4096;
        assert!((449..=451).contains(&x), "phase={phase}, x={x}");

        let starts_inside = capsule([490, 0, 0], [1_000, 0, 0], 10);
        assert_eq!(
            combat_capsule_sweep_contact_fraction_q12(&starts_inside, &target),
            Some(0)
        );
        let miss = capsule([0, 100, 0], [1_000, 100, 0], 10);
        assert_eq!(
            combat_capsule_sweep_contact_fraction_q12(&miss, &target),
            None
        );
    }

    #[test]
    fn arc_hits_target_dead_ahead_and_respects_reach() {
        // Yaw 0 faces +Z (x = sin, z = cos).
        let swing = arc(0, 640, 683);
        assert!(arc_hits_circle(&swing, 1000, 1600, 192));
        // reach + radius: 1000 + 640 + 192 = 1832 is the last
        // reachable center.
        assert!(arc_hits_circle(&swing, 1000, 1832, 192));
        assert!(!arc_hits_circle(&swing, 1000, 1833, 192));
    }

    #[test]
    fn arc_misses_target_behind() {
        let swing = arc(0, 640, 683);
        assert!(!arc_hits_circle(&swing, 1000, 400, 192));
    }

    #[test]
    fn arc_edges_follow_the_half_angle() {
        // 60-degree half-arc = 683 PSX units. A target 45 degrees off
        // (both axes equal) is inside; one 90 degrees off (straight
        // +X) is out.
        let swing = arc(0, 1000, 683);
        assert!(arc_hits_circle(&swing, 1400, 1400, 0));
        assert!(!arc_hits_circle(&swing, 1600, 1000, 0));
        // A narrow 10-degree arc (114 units) drops the diagonal.
        let narrow = arc(0, 1000, 114);
        assert!(!arc_hits_circle(&narrow, 1400, 1400, 0));
    }

    #[test]
    fn point_blank_targets_hit_regardless_of_facing() {
        // Target center behind the attacker but overlapping the body.
        let swing = arc(0, 640, 114);
        assert!(arc_hits_circle(&swing, 1000, 900, 192));
    }

    #[test]
    fn angle_wraps_across_zero() {
        assert!(angle_within(4090, 10, 30));
        assert!(angle_within(10, 4090, 30));
        assert!(!angle_within(2048, 0, 683));
    }

    #[test]
    fn heavy_scaling_multiplies_damage_and_poise() {
        let spec = PlayerMeleeSpec {
            reach: 640,
            half_angle: 683,
            damage: 30,
            poise_damage: 40,
            active_window: Some((10, 20)),
        };
        let heavy = spec.heavy();
        assert_eq!(heavy.damage, 45);
        assert_eq!(heavy.poise_damage, 80);
        assert_eq!(heavy.reach, spec.reach);
        assert_eq!(heavy.active_window, spec.active_window);
    }

    #[test]
    fn frame_active_honors_window_and_fallback() {
        let windowed = PlayerMeleeSpec {
            active_window: Some((8, 14)),
            ..UNARMED
        };
        assert!(!windowed.frame_active(7));
        assert!(windowed.frame_active(8));
        assert!(windowed.frame_active(14));
        assert!(!windowed.frame_active(15));
        assert!(UNARMED.frame_active(0));
        assert!(UNARMED.frame_active(999));
    }

    const fn test_weapon(
        arc_reach: u16,
        damage: u16,
        hitbox_first: u16,
        hitbox_count: u16,
    ) -> LevelWeaponRecord {
        LevelWeaponRecord {
            name: "Test Cleaver",
            model: None,
            default_character_socket: "right_hand_grip",
            grip_name: "grip",
            grip_translation: [0; 3],
            grip_rotation_q12: [0; 3],
            hitbox_first: WeaponHitboxIndex(hitbox_first),
            hitbox_count,
            arc_reach,
            arc_half_angle: 683,
            damage,
            poise_damage: 40,
            flags: 0,
        }
    }

    const fn test_hitbox(start: u16, end: u16) -> WeaponHitboxRecord {
        WeaponHitboxRecord {
            name: "Edge",
            shape: WeaponHitShapeRecord::Capsule {
                start: [0; 3],
                end: [0, 512, 0],
                radius: 48,
            },
            active_start_frame: start,
            active_end_frame: end,
            flags: 0,
        }
    }

    static WEAPONS: [LevelWeaponRecord; 1] = [test_weapon(640, 30, 0, 2)];
    static HITBOXES: [WeaponHitboxRecord; 2] = [test_hitbox(10, 16), test_hitbox(14, 22)];
    static PLAYER_EQUIPMENT: [EquipmentRecord; 2] = [
        EquipmentRecord {
            room: RoomIndex(0),
            weapon: WeaponIndex(0),
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            character_socket: "right_hand_grip",
            weapon_grip: "grip",
            model_instance: EquipmentRecord::NO_INSTANCE,
            flags: 0,
        },
        EquipmentRecord {
            room: RoomIndex(3),
            weapon: WeaponIndex(0),
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            character_socket: "right_hand_grip",
            weapon_grip: "grip",
            model_instance: EquipmentRecord::NO_INSTANCE,
            flags: equipment_flags::PLAYER,
        },
    ];
    static ZERO_REACH_WEAPONS: [LevelWeaponRecord; 1] = [test_weapon(0, 30, 0, 0)];

    #[test]
    fn player_spec_resolves_the_player_flagged_weapon_across_rooms() {
        // The PLAYER-flagged record is second and pinned to room 3;
        // resolution must skip the non-player record and ignore rooms.
        let spec = player_melee_spec(&PLAYER_EQUIPMENT, &WEAPONS, &HITBOXES);
        assert_eq!(spec.reach, 640);
        assert_eq!(spec.damage, 30);
        assert_eq!(spec.poise_damage, 40);
        // Two hitbox windows union: (10..16) + (14..22) = (10..22).
        assert_eq!(spec.active_window, Some((10, 22)));
    }

    #[test]
    fn player_spec_falls_back_to_unarmed() {
        // No equipment at all.
        assert_eq!(player_melee_spec(&[], &WEAPONS, &HITBOXES), UNARMED);
        // A hand-rolled zero-reach weapon record.
        assert_eq!(
            player_melee_spec(&PLAYER_EQUIPMENT, &ZERO_REACH_WEAPONS, &HITBOXES),
            UNARMED
        );
    }

    mod authored_contact {
        extern crate std;

        use super::super::*;
        use psx_asset::Animation;
        use psx_engine::{LocalToWorldScale, Mat3I16, ModelPoseTranslation, SimTick};
        use std::{boxed::Box, vec::Vec};

        /// One-joint identity-rotation animation whose every frame holds the
        /// same translation, so a snapshot's world pose is phase-independent
        /// while its raw phase still drives the active-frame windows.
        fn static_one_joint_animation() -> Animation<'static> {
            const ANIMATION_HEADER_SIZE: usize = 8;
            const POSE_RECORD_SIZE: usize = 24;
            const FRAMES: usize = 2;
            let payload_len = ANIMATION_HEADER_SIZE + FRAMES * POSE_RECORD_SIZE;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"PSXA");
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&(FRAMES as u16).to_le_bytes());
            bytes.extend_from_slice(&30u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            for _ in 0..FRAMES {
                for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                for value in [0i16, 0, 0] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            Animation::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("test animation")
        }

        /// Snapshot at `origin` with the given raw Q12 phase (frame = phase
        /// >> 12), matching the retained poses the runtime finalizes against.
        fn pose_at(origin: [i32; 3], phase_q12: u32) -> ActorPoseSnapshot {
            ActorPoseSnapshot::new(
                SimTick::from_u32(1),
                static_one_joint_animation(),
                phase_q12,
                None,
                WorldVertex::new(origin[0], origin[1], origin[2]),
                Mat3I16::IDENTITY,
                LocalToWorldScale::IDENTITY,
                ModelPoseTranslation { x: 0, y: 0, z: 0 },
            )
        }

        const fn capsule_record(
            joint: u8,
            flags: u8,
            action: CharacterAnimationAction,
        ) -> CombatCapsuleRecord {
            CombatCapsuleRecord {
                joint,
                flags,
                action: action.to_index() as u8,
                reserved: 0,
                start: [0, 0, 0],
                end: [0, 100, 0],
                radius: 48,
                active_start_frame: 2,
                active_end_frame: 4,
                damage: 25,
                poise_damage: 35,
                projectile_speed: 0,
                projectile_lifetime_ticks: 0,
                projectile_min_range: 0,
                projectile_max_range: 0,
                projectile_tint_rgb: [0; 3],
                projectile_damage_channel: psx_level::projectile_damage_channel::ZENITH,
                projectile_core_rgb: [0; 3],
                projectile_trail_segments: 0,
                projectile_glow_rgb: [0; 3],
                projectile_length_ticks: 0,
                projectile_impact_rgb: [0; 3],
                projectile_trail_spacing_ticks: 0,
                projectile_charge_start_frame: 0,
                projectile_glow_scale_q8: 0,
                projectile_impact_lifetime_ticks: 0,
                projectile_reserved: 0,
            }
        }

        const ATTACK: CharacterAnimationAction = CharacterAnimationAction::LightAttack;
        const HITBOXES: [CombatCapsuleRecord; 1] =
            [capsule_record(0, combat_capsule_flags::HITBOX, ATTACK)];
        const HURTBOXES: [CombatCapsuleRecord; 1] = [capsule_record(
            0,
            combat_capsule_flags::HURTBOX,
            CharacterAnimationAction::Idle,
        )];
        /// Inside the 2..=4 active window.
        const ACTIVE_PHASE: u32 = 3 << 12;
        /// Before the window opens.
        const INACTIVE_PHASE: u32 = 1 << 12;

        #[test]
        fn projectile_charge_and_exact_release_use_the_retained_pose() {
            const EMITTERS: [CombatCapsuleRecord; 1] = [CombatCapsuleRecord {
                flags: combat_capsule_flags::PROJECTILE_EMITTER,
                start: [20, 30, 40],
                end: [20, 30, 40],
                radius: 36,
                damage: 24,
                poise_damage: 12,
                projectile_speed: 180,
                projectile_lifetime_ticks: 120,
                projectile_min_range: 512,
                projectile_max_range: 4096,
                projectile_tint_rgb: [80, 180, 255],
                active_start_frame: 3,
                active_end_frame: 3,
                projectile_charge_start_frame: 1,
                projectile_damage_channel: psx_level::projectile_damage_channel::ZENITH,
                projectile_core_rgb: [220, 255, 248],
                projectile_glow_rgb: [80, 180, 255],
                projectile_impact_rgb: [120, 220, 255],
                projectile_trail_segments: 3,
                projectile_length_ticks: 2,
                projectile_trail_spacing_ticks: 1,
                projectile_glow_scale_q8: 448,
                projectile_impact_lifetime_ticks: 10,
                ..capsule_record(0, combat_capsule_flags::PROJECTILE_EMITTER, ATTACK)
            }];
            let release = authored_projectile_release(
                &EMITTERS,
                ATTACK,
                Some(pose_at([1000, 2000, 3000], ACTIVE_PHASE)),
            )
            .expect("active emitter releases");
            assert_eq!(release.position, [1020, 2030, 3040]);
            assert_eq!(release.radius, 36);
            assert_eq!(release.speed, 180);
            assert_eq!(release.lifetime_ticks, 120);
            assert_eq!((release.min_range, release.max_range), (512, 4096));
            assert_eq!((release.damage, release.poise_damage), (24, 12));
            assert_eq!(release.tint_rgb, [80, 180, 255]);
            assert_eq!(release.damage_channel, ProjectileDamageChannel::Zenith);
            assert_eq!(release.visual.trail_segments, 3);
            assert!(
                authored_projectile_release(
                    &EMITTERS,
                    ATTACK,
                    Some(pose_at([1000, 2000, 3000], 4 << 12)),
                )
                .is_some(),
                "a skipped exact frame releases on the first retained pose after it"
            );
            let charge = authored_projectile_charge(
                &EMITTERS,
                ATTACK,
                Some(pose_at([1000, 2000, 3000], 2 << 12)),
            )
            .expect("charge window is visible before release");
            assert_eq!(charge.position, [1020, 2030, 3040]);
            assert_eq!(charge.progress_q8, 128);
            assert_eq!(
                authored_projectile_release(
                    &EMITTERS,
                    ATTACK,
                    Some(pose_at([1000, 2000, 3000], INACTIVE_PHASE)),
                ),
                None
            );
        }

        #[test]
        fn volley_markers_release_once_and_survive_skipped_pose_frames() {
            let mut first = capsule_record(0, combat_capsule_flags::PROJECTILE_EMITTER, ATTACK);
            first.active_start_frame = 3;
            first.active_end_frame = 3;
            let mut second = first;
            second.active_start_frame = 9;
            second.active_end_frame = 9;
            second.start = [30, 0, 0];
            let emitters = [first, second];
            let query = |frame: u32, mask| authored_projectile_release_pending(
                &emitters, ATTACK, Some(pose_at([0; 3], frame << 12)), mask);
            assert!(query(2, 0).is_none());
            assert_eq!(query(4, 0).unwrap().0, 0);
            assert_eq!(query(4, 0).unwrap().0, 0, "pool-full retry keeps its marker");
            assert!(query(8, 1).is_none(), "second shot cannot fire early");
            let (index, release) = query(10, 1).unwrap();
            assert_eq!(index, 1);
            assert_eq!(release.position, [30, 0, 0]);
            assert!(query(12, 3).is_none(), "all shots consumed");
            // A slow update crossing both markers drains them in authored order.
            assert_eq!(query(12, 0).unwrap().0, 0);
            assert_eq!(query(12, 1).unwrap().0, 1);
        }

        #[test]
        fn separated_melee_windows_hit_once_each_and_trails_never_bridge_gaps() {
            let mut swings = [capsule_record(0, combat_capsule_flags::HITBOX, ATTACK); 3];
            for (i, record) in swings.iter_mut().enumerate() {
                record.active_start_frame = 3 + i as u16 * 6;
                record.active_end_frame = 5 + i as u16 * 6;
            }
            let defender = Some(pose_at([0; 3], 0));
            let mut used = 0;
            for (i, frame) in [4u32, 10, 16].into_iter().enumerate() {
                let query = |mask| resolve_authored_actor_contact_pending(
                    &swings, ATTACK, Some(pose_at([0; 3], frame << 12)), &HURTBOXES, defender, mask);
                let (hit, window) = query(used);
                assert!(matches!(hit, AuthoredActorContact::Hit { .. }));
                assert_eq!(window, 1 << i);
                used |= window;
                assert_eq!(query(used), (AuthoredActorContact::Miss, 0));
                let phases = melee_trail_phases(&swings[i], ATTACK, frame << 12).unwrap();
                assert!(phases.iter().all(|p| *p >= u32::from(swings[i].active_start_frame) << 12));
                let gap = frame + 2;
                assert!(melee_trail_phases(&swings[i], ATTACK, gap << 12).is_none());
                assert_eq!(resolve_authored_actor_contact_pending(&swings, ATTACK,
                    Some(pose_at([0; 3], gap << 12)), &HURTBOXES, defender, used),
                    (AuthoredActorContact::Miss, 0));
            }
            // Multiple volumes within one swing must not multiply its damage.
            let copies = [swings[0]; 2];
            let (_, window) = resolve_authored_actor_contact_pending(&copies, ATTACK,
                Some(pose_at([0; 3], 4 << 12)), &HURTBOXES, defender, 0);
            assert_eq!(window, 3);
        }

        #[test]
        fn fallback_is_offered_only_while_a_side_is_unauthored() {
            let near = [1000, 2000, 3000];
            let attacker = Some(pose_at(near, ACTIVE_PHASE));
            let defender = Some(pose_at(near, 0));

            // Attacker with no HITBOX at all.
            assert_eq!(
                resolve_authored_actor_contact(&HURTBOXES, ATTACK, attacker, &HURTBOXES, defender),
                AuthoredActorContact::FallbackRequired
            );
            // Attacker authored only for a DIFFERENT action slot.
            const HEAVY_ONLY: [CombatCapsuleRecord; 1] = [capsule_record(
                0,
                combat_capsule_flags::HITBOX,
                CharacterAnimationAction::HeavyAttack,
            )];
            assert_eq!(
                resolve_authored_actor_contact(&HEAVY_ONLY, ATTACK, attacker, &HURTBOXES, defender),
                AuthoredActorContact::FallbackRequired
            );
            // Defender with no HURTBOX at all.
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, attacker, &[], defender),
                AuthoredActorContact::FallbackRequired
            );
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, attacker, &HITBOXES, defender),
                AuthoredActorContact::FallbackRequired
            );

            // Both sides authored and connecting: the authored numbers land.
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, attacker, &HURTBOXES, defender),
                AuthoredActorContact::Hit {
                    damage: 25,
                    poise_damage: 35,
                }
            );
        }

        #[test]
        fn authored_misses_are_authoritative_and_never_fall_back() {
            let near = [1000, 2000, 3000];
            let attacker = Some(pose_at(near, ACTIVE_PHASE));
            let defender = Some(pose_at(near, 0));

            // A missing retained snapshot on either side is a miss.
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, None, &HURTBOXES, defender),
                AuthoredActorContact::Miss
            );
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, attacker, &HURTBOXES, None),
                AuthoredActorContact::Miss
            );
            // A frame outside the authored active window is a miss.
            let early = Some(pose_at(near, INACTIVE_PHASE));
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, early, &HURTBOXES, defender),
                AuthoredActorContact::Miss
            );
            // A hitbox naming a joint the rig does not have is a miss.
            const BAD_JOINT: [CombatCapsuleRecord; 1] =
                [capsule_record(9, combat_capsule_flags::HITBOX, ATTACK)];
            assert_eq!(
                resolve_authored_actor_contact(&BAD_JOINT, ATTACK, attacker, &HURTBOXES, defender),
                AuthoredActorContact::Miss
            );
            // Geometric separation is a miss.
            let far = Some(pose_at([1000, 2000, 30_000], 0));
            assert_eq!(
                resolve_authored_actor_contact(&HITBOXES, ATTACK, attacker, &HURTBOXES, far),
                AuthoredActorContact::Miss
            );
        }
    }

    /// Combat runs on the fixed simulation clock, never on the presented
    /// frame. An attack's active window is expressed in ANIMATION FRAMES,
    /// and the phase that selects the frame is
    /// [`animation_phase_at_tick_q12`], a pure function of
    /// `sim_tick - attack_start_tick`. So the number of simulation ticks a
    /// swing spends inside its active window has to be the same whether the
    /// build presents every visual interval or drops most of them.
    ///
    /// This drives the real [`FrameScheduler`] (the same one `App::run` uses)
    /// at several render costs and compares the tick sets. It is the unit-level
    /// form of the cross-layout stage in `tools/editor_souls_bsp_check.sh`:
    /// before 2026-08-11 that gate scored melee 4 on one guest build and melee
    /// 3 on another, and the shape of the bug it was blamed on would show up
    /// here as an active window that shrinks when rendering gets slower.
    mod tick_authority {
        extern crate std;

        use super::super::{PlayerMeleeSpec, UNARMED};
        use crate::model_rendering::animation_phase_at_tick_q12;
        use psx_asset::Animation;
        use psx_engine::{FrameScheduler, SchedulerAction, SchedulerConfig, VideoHz};
        use std::{boxed::Box, vec::Vec};

        /// Sampled clip rate of the test animation, in Hz. Half the NTSC
        /// simulation rate, so one animation frame spans two ticks.
        const CLIP_HZ: u16 = 30;
        /// Frames in the test attack clip.
        const CLIP_FRAMES: usize = 20;
        /// Authored active window, in animation frames (inclusive).
        const ACTIVE_WINDOW: (u16, u16) = (12, 15);
        /// Simulation ticks the window must cover: 4 frames at 2 ticks each.
        const EXPECT_ACTIVE_TICKS: usize = 8;
        /// Visual pacing under test: render every second simulation tick.
        const VISUAL_INTERVAL: u16 = 2;
        /// Simulation ticks each replay runs for.
        const TICKS: u32 = 400;
        /// Tick the swing starts on.
        const ATTACK_START: u32 = 100;

        /// Identity-pose clip whose only job is to carry a frame count and a
        /// sample rate; the window test reads phase, not joints.
        fn attack_animation() -> Animation<'static> {
            const ANIMATION_HEADER_SIZE: usize = 8;
            const POSE_RECORD_SIZE: usize = 24;
            let payload_len = ANIMATION_HEADER_SIZE + CLIP_FRAMES * POSE_RECORD_SIZE;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"PSXA");
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            bytes.extend_from_slice(&(payload_len as u32).to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&(CLIP_FRAMES as u16).to_le_bytes());
            bytes.extend_from_slice(&CLIP_HZ.to_le_bytes());
            bytes.extend_from_slice(&0u16.to_le_bytes());
            for _ in 0..CLIP_FRAMES {
                for value in [4096i16, 0, 0, 0, 4096, 0, 0, 0, 4096] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
                for value in [0i16, 0, 0] {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            Animation::from_bytes(Box::leak(bytes.into_boxed_slice())).expect("test clip")
        }

        /// What one replay observed.
        struct Replay {
            /// Simulation ticks the scheduler actually ran, in order.
            ticks: Vec<u32>,
            /// Ticks whose attack-clip frame was inside the active window.
            active_ticks: Vec<u32>,
            /// Visual frames presented.
            visual_frames: usize,
        }

        /// Run `TICKS` fixed updates through the real scheduler, charging
        /// `render_cost_vblanks` display VBlanks to every presented frame.
        /// Cost 1 keeps up with the 30 Hz cadence; higher costs overrun it and
        /// force the scheduler's catch-up bursts, exactly as an overloaded
        /// build does on hardware.
        fn replay(render_cost_vblanks: u32, spec: &PlayerMeleeSpec) -> Replay {
            let animation = attack_animation();
            let mut scheduler = FrameScheduler::new(SchedulerConfig::new(), VISUAL_INTERVAL);
            let mut elapsed_vblanks = 0u32;
            let mut out = Replay {
                ticks: Vec::new(),
                active_ticks: Vec::new(),
                visual_frames: 0,
            };
            while (out.ticks.len() as u32) < TICKS {
                match scheduler.next_action(elapsed_vblanks) {
                    SchedulerAction::RunFixedUpdate { tick } => {
                        let tick = tick.as_u32();
                        out.ticks.push(tick);
                        if tick >= ATTACK_START {
                            let phase = animation_phase_at_tick_q12(
                                animation,
                                tick - ATTACK_START,
                                VideoHz::NTSC,
                                false,
                                256,
                                psx_level::CharacterActionFrameRange::FULL,
                            );
                            if spec.frame_active(phase >> 12) {
                                out.active_ticks.push(tick);
                            }
                        }
                        scheduler.complete_fixed_update();
                    }
                    SchedulerAction::RunVisualFrame { .. } => {
                        out.visual_frames += 1;
                        scheduler.complete_visual_frame();
                        elapsed_vblanks =
                            elapsed_vblanks.saturating_add(render_cost_vblanks.max(1));
                    }
                    SchedulerAction::WaitForVBlank => {
                        elapsed_vblanks = elapsed_vblanks.saturating_add(1);
                    }
                }
            }
            out
        }

        #[test]
        fn attack_window_covers_the_same_simulation_ticks_at_every_frame_rate() {
            let spec = PlayerMeleeSpec {
                active_window: Some(ACTIVE_WINDOW),
                ..UNARMED
            };
            let baseline = replay(1, &spec);
            let expected_ticks: Vec<u32> = (0..TICKS).collect();
            assert_eq!(
                baseline.ticks, expected_ticks,
                "the simulation must run every tick exactly once, in order"
            );
            assert_eq!(
                baseline.active_ticks.len(),
                EXPECT_ACTIVE_TICKS,
                "a {}-frame window on a {CLIP_HZ} Hz clip is {EXPECT_ACTIVE_TICKS} ticks at 60 Hz",
                ACTIVE_WINDOW.1 - ACTIVE_WINDOW.0 + 1
            );

            // Costs above VISUAL_INTERVAL overrun the 30 Hz slot, so the
            // scheduler drops visuals and catches the simulation up in bursts.
            for render_cost_vblanks in [3u32, 5, 9] {
                let slow = replay(render_cost_vblanks, &spec);
                assert_eq!(
                    slow.ticks, baseline.ticks,
                    "render cost {render_cost_vblanks} changed which simulation ticks ran"
                );
                assert_eq!(
                    slow.active_ticks, baseline.active_ticks,
                    "render cost {render_cost_vblanks} changed the attack's active ticks; \
                     the hit window is following the presented frame, not the sim clock"
                );
                // Non-vacuity: these replays really are different frame rates.
                assert!(
                    slow.visual_frames < baseline.visual_frames,
                    "render cost {render_cost_vblanks} presented {} frames, not fewer than the \
                     baseline's {}",
                    slow.visual_frames,
                    baseline.visual_frames
                );
            }
        }

        #[test]
        fn dropping_visual_frames_never_dilates_the_attack() {
            let spec = PlayerMeleeSpec {
                active_window: Some(ACTIVE_WINDOW),
                ..UNARMED
            };
            // The window opens and closes on fixed offsets from the swing's
            // start tick, so both edges are frame-rate invariant too.
            let first = ATTACK_START + 2 * u32::from(ACTIVE_WINDOW.0);
            let last = ATTACK_START + 2 * u32::from(ACTIVE_WINDOW.1) + 1;
            for render_cost_vblanks in [1u32, 4, 12] {
                let run = replay(render_cost_vblanks, &spec);
                assert_eq!(run.active_ticks.first().copied(), Some(first));
                assert_eq!(run.active_ticks.last().copied(), Some(last));
            }
        }
    }
}
