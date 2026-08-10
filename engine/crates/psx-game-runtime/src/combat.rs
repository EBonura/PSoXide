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
    equipment_flags, CombatCapsuleRecord, EquipmentRecord, LevelWeaponRecord, RoomIndex,
    WeaponHitboxRecord,
};
use psx_math::atan2_q12;

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
}
