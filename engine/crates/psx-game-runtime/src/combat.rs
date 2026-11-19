//! Animation-timed melee capsules and projectile release markers sampled from
//! the same retained actor poses as rendering. Melee sweeps cover the active
//! portion of consecutive poses; per-window masks prevent duplicate damage.
//! Unauthored actors fall back to a facing arc against cylindrical hurtboxes.
//!
//! Capsule overlap uses bounded 32-bit integer projections and an AABB broad
//! phase, keeping fast weapon motion reliable without heap allocation or wide
//! arithmetic on PlayStation.

use psx_engine::{JointWorldTransform, WorldVertex};
use psx_level::{
    combat_capsule_flags, CharacterAnimationAction, CombatCapsuleRecord,
    MAX_CHARACTER_COMBAT_CAPSULES,
};

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
    pose: &ActorPoseSnapshot,
) -> Option<WorldCombatCapsule> {
    Some(transform_combat_capsule(
        record,
        pose.joint_world_transform(u16::from(record.joint))?,
    ))
}

/// Most authored hurtboxes a bolt tests on one fighter (the cooked fighters
/// author two or three); later ones are ignored. Callers size their target
/// storage from it.
pub const PROJECTILE_HURTBOXES_PER_FIGHTER: usize = 4;

/// A fighter's body cylinder as the capsule a bolt strikes when it has no
/// authored hurtbox: from its feet to its height, as wide as its radius (at
/// most half its height). `None` for a body with no size.
pub fn body_hurt_capsule(
    position: [i32; 3],
    radius: i32,
    height: i32,
) -> Option<WorldCombatCapsule> {
    if radius <= 0 || height <= 0 {
        return None;
    }
    let r = radius.min(height / 2).max(1);
    Some(WorldCombatCapsule {
        start: [position[0], position[1].saturating_add(r), position[2]],
        end: [
            position[0],
            position[1].saturating_add(height).saturating_sub(r),
            position[2],
        ],
        radius: r.min(i32::from(u16::MAX)) as u16,
    })
}

/// What a bolt strikes on one fighter, by one rule for every fighter: each
/// authored HURTBOX capsule on its pose, in capsule order (at most
/// [`PROJECTILE_HURTBOXES_PER_FIGHTER`]), or `body` when none is on a pose.
///
/// Every hurtbox counts, not only the first: a fighter whose first hurtbox
/// is a leg would otherwise let every bolt aimed at its chest fly over it.
pub fn projectile_hurtboxes(
    capsules: &[CombatCapsuleRecord],
    pose: Option<&ActorPoseSnapshot>,
    body: Option<WorldCombatCapsule>,
    mut push: impl FnMut(WorldCombatCapsule),
) {
    let mut pushed = 0usize;
    if let Some(pose) = pose {
        for record in capsules {
            if pushed == PROJECTILE_HURTBOXES_PER_FIGHTER {
                return;
            }
            if record.flags & combat_capsule_flags::HURTBOX == 0 {
                continue;
            }
            if let Some(hurtbox) = transform_actor_combat_capsule(record, pose) {
                push(hurtbox);
                pushed += 1;
            }
        }
    }
    if pushed == 0 {
        if let Some(body) = body {
            push(body);
        }
    }
}

/// Sample the endpoints of an active part of a retained-pose interval. Both
/// animation phase and actor translation are clipped, so movement during an
/// inactive tell or recovery cannot become part of the damaging blade path.
/// The previous pose must belong to the same consecutive action as `current`.
pub fn transform_actor_combat_capsule_sweep(
    record: &CombatCapsuleRecord,
    previous: Option<&ActorPoseSnapshot>,
    current: &ActorPoseSnapshot,
    start_phase: u32,
    end_phase: u32,
) -> Option<(WorldCombatCapsule, Option<WorldCombatCapsule>)> {
    let sample = |pose: &ActorPoseSnapshot, phase| {
        let mut capsule = transform_combat_capsule(
            record,
            pose.joint_world_transform_at(phase, u16::from(record.joint))?,
        );
        if let Some(previous) = previous {
            let from = previous.origin();
            let to = current.origin();
            if from != to && current.phase_q12() > previous.phase_q12() {
                let fraction = ratio_q12(
                    phase
                        .saturating_sub(previous.phase_q12())
                        .min(i32::MAX as u32) as i32,
                    current
                        .phase_q12()
                        .saturating_sub(previous.phase_q12())
                        .min(i32::MAX as u32) as i32,
                ) as u16;
                let origin =
                    interpolate3_q12([from.x, from.y, from.z], [to.x, to.y, to.z], fraction);
                let sampled_origin = pose.origin();
                let offset = sub3(
                    origin,
                    [sampled_origin.x, sampled_origin.y, sampled_origin.z],
                );
                for (axis, delta) in offset.into_iter().enumerate() {
                    capsule.start[axis] = capsule.start[axis].saturating_add(delta);
                    capsule.end[axis] = capsule.end[axis].saturating_add(delta);
                }
            }
        }
        Some(capsule)
    };
    let end = sample(current, end_phase)?;
    let start = previous
        .filter(|_| start_phase < end_phase)
        .and_then(|previous| sample(previous, start_phase));
    Some((end, start))
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
        star_points: 0,
        crystal: false,
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
    pose: Option<&ActorPoseSnapshot>,
) -> Option<AuthoredProjectileRelease> {
    authored_projectile_release_pending(capsules, action, pose, 0).map(|(_, release)| release)
}

/// Find an eligible emitter that has not fired in this attack. The returned
/// index is relative to the actor's capsule slice and fits a sixteen-bit mask.
/// The caller marks it only after successful insertion into the projectile pool.
pub fn authored_projectile_release_pending(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<&ActorPoseSnapshot>,
    released_mask: u16,
) -> Option<(u8, AuthoredProjectileRelease)> {
    let pose = pose?;
    let frame = (pose.phase_q12() >> 12).min(u32::from(u16::MAX)) as u16;
    authored_projectile_release_pending_at_frame(capsules, action, Some(pose), released_mask, frame)
}

/// Use the weapon's event clock while taking muzzle geometry from live locomotion.
pub fn authored_projectile_release_pending_at_frame(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<&ActorPoseSnapshot>,
    released_mask: u16,
    frame: u16,
) -> Option<(u8, AuthoredProjectileRelease)> {
    let pose = pose?;
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
            Some((
                index as u8,
                AuthoredProjectileRelease {
                    position: muzzle.start,
                    radius: muzzle.radius,
                    speed: record.projectile_speed,
                    lifetime_ticks: record.projectile_lifetime_ticks,
                    min_range: record.projectile_min_range,
                    max_range: record.projectile_max_range,
                    damage: record.damage,
                    poise_damage: record.poise_damage,
                    tint_rgb: record.projectile_tint_rgb,
                    damage_channel: ProjectileDamageChannel::from_raw(
                        record.projectile_damage_channel,
                    ),
                    visual: projectile_visual(record),
                },
            ))
        })
}

/// Resolve the first projectile emitter currently inside its authored charge
/// window. The release frame itself belongs to the bolt/muzzle-flash path.
pub fn authored_projectile_charge(
    capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    pose: Option<&ActorPoseSnapshot>,
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
/// Contact is authored geometry only: an action with no HITBOX or a
/// defender with no HURTBOX never connects, and inactive frames, invalid
/// joints, missing snapshots and geometric separation are misses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoredActorContact {
    /// No authored overlap on this exact pose/frame.
    Miss,
    /// First deterministic authored HITBOX/HURTBOX overlap.
    Hit {
        /// Health damage from the attacking HITBOX.
        damage: u16,
        /// Poise damage from the attacking HITBOX.
        poise_damage: u16,
    },
}

/// One authored contact of a swing on a defender: the hitbox's numbers and
/// the strike window it belongs to (every hitbox of the action whose
/// active frames overlap the one that connected).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrikeContact {
    /// Health damage from the attacking HITBOX.
    pub damage: u16,
    /// Poise damage from the attacking HITBOX.
    pub poise_damage: u16,
    /// The strike window, one bit per attacker capsule index.
    pub window: u16,
}

/// One fighter's swing on one tick: every authored hitbox of its action
/// active on this pose, swept from the previous pose of the same clip,
/// sampled once and then tested against any number of defenders
/// ([`Self::contact`]). Every fighter's melee contact goes through it.
#[derive(Clone, Copy)]
pub struct ActiveStrike {
    capsules: [WorldCombatCapsule; MAX_CHARACTER_COMBAT_CAPSULES],
    previous: [Option<WorldCombatCapsule>; MAX_CHARACTER_COMBAT_CAPSULES],
    /// The attacker capsule index of each active volume.
    records: [u8; MAX_CHARACTER_COMBAT_CAPSULES],
    count: u8,
}

impl ActiveStrike {
    /// No active volume.
    pub const NONE: Self = Self {
        capsules: [WorldCombatCapsule::EMPTY; MAX_CHARACTER_COMBAT_CAPSULES],
        previous: [None; MAX_CHARACTER_COMBAT_CAPSULES],
        records: [0; MAX_CHARACTER_COMBAT_CAPSULES],
        count: 0,
    };

    /// Sample the hitboxes of `action` among `attacker_capsules` that are
    /// active on `pose`, each swept from `previous` (the attacker's pose
    /// one tick earlier, same clip; a stale tick or a phase reset drops the
    /// sweep). The caller discards `previous` on a clip change.
    pub fn sample(
        attacker_capsules: &[CombatCapsuleRecord],
        action: CharacterAnimationAction,
        pose: &ActorPoseSnapshot,
        previous: Option<&ActorPoseSnapshot>,
    ) -> Self {
        let action = action.to_index() as u8;
        let phase = pose.phase_q12();
        let previous = previous
            .filter(|old| pose.tick().wrapping_sub(old.tick()) == 1 && old.phase_q12() <= phase);
        let mut strike = Self::NONE;
        for (index, record) in attacker_capsules
            .iter()
            .take(MAX_CHARACTER_COMBAT_CAPSULES)
            .enumerate()
        {
            if record.flags & combat_capsule_flags::HITBOX == 0 || record.action != action {
                continue;
            }
            let Some((start, end)) = active_frame_sweep_phase_range(
                previous.map(|pose| pose.phase_q12()),
                phase,
                record.active_start_frame,
                record.active_end_frame,
            ) else {
                continue;
            };
            let Some((capsule, previous_capsule)) =
                transform_actor_combat_capsule_sweep(record, previous, pose, start, end)
            else {
                continue;
            };
            let slot = usize::from(strike.count);
            strike.capsules[slot] = capsule;
            strike.previous[slot] = previous_capsule;
            strike.records[slot] = index as u8;
            strike.count += 1;
        }
        strike
    }

    /// Whether no hitbox is active this tick.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The active volumes this tick, in capsule order.
    pub fn volumes(&self) -> &[WorldCombatCapsule] {
        &self.capsules[..usize::from(self.count)]
    }

    /// Active volume `index` and the attacker capsule index it samples.
    ///
    /// # Panics
    ///
    /// When `index` is past [`Self::volumes`].
    pub fn volume(&self, index: usize) -> (WorldCombatCapsule, usize) {
        assert!(index < usize::from(self.count), "an active volume");
        (self.capsules[index], usize::from(self.records[index]))
    }

    /// The first active volume (capsule order) that is not in `spent` and
    /// overlaps one of the defender's HURTBOXes on `defender_pose`, swept
    /// over the tick. `attacker_capsules` is the slice [`Self::sample`] read.
    pub fn contact(
        &self,
        attacker_capsules: &[CombatCapsuleRecord],
        defender_capsules: &[CombatCapsuleRecord],
        defender_pose: Option<&ActorPoseSnapshot>,
        spent: u16,
    ) -> Option<StrikeContact> {
        let defender_pose = defender_pose?;
        let mut hurtboxes = [WorldCombatCapsule::EMPTY; MAX_CHARACTER_COMBAT_CAPSULES];
        let mut hurtbox_count = 0;
        let mut hurtboxes_sampled = false;
        for slot in 0..usize::from(self.count) {
            let index = usize::from(self.records[slot]);
            if spent & (1u16 << index) != 0 {
                continue;
            }
            if !hurtboxes_sampled {
                for hurtbox in defender_capsules.iter().take(MAX_CHARACTER_COMBAT_CAPSULES) {
                    if hurtbox.flags & combat_capsule_flags::HURTBOX == 0 {
                        continue;
                    }
                    if let Some(capsule) = transform_actor_combat_capsule(hurtbox, defender_pose) {
                        hurtboxes[hurtbox_count] = capsule;
                        hurtbox_count += 1;
                    }
                }
                hurtboxes_sampled = true;
            }
            let capsule = self.capsules[slot];
            let hit = hurtboxes[..hurtbox_count].iter().any(|hurtbox| {
                self.previous[slot].map_or_else(
                    || combat_capsules_overlap(&capsule, hurtbox),
                    |old| combat_capsule_motion_overlaps(&old, &capsule, hurtbox),
                )
            });
            if !hit {
                continue;
            }
            let record = &attacker_capsules[index];
            // Several blade volumes may describe one strike. The window is
            // every volume overlapping this one in frames, never a separate
            // later swing which happens to be crossed by the same interval.
            let mut window = 0u16;
            for (other_index, other) in attacker_capsules
                .iter()
                .take(MAX_CHARACTER_COMBAT_CAPSULES)
                .enumerate()
            {
                if other.flags & combat_capsule_flags::HITBOX != 0
                    && other.action == record.action
                    && other.active_start_frame <= record.active_end_frame
                    && record.active_start_frame <= other.active_end_frame
                {
                    window |= 1u16 << other_index;
                }
            }
            return Some(StrikeContact {
                damage: record.damage,
                poise_damage: record.poise_damage,
                window,
            });
        }
        None
    }
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
    attacker_pose: Option<&ActorPoseSnapshot>,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: Option<&ActorPoseSnapshot>,
) -> AuthoredActorContact {
    resolve_authored_actor_contact_pending(
        attacker_capsules,
        action,
        attacker_pose,
        defender_capsules,
        defender_pose,
        0,
    )
    .0
}

/// Resolve an unconsumed melee window. Simultaneously active hitboxes share
/// one contact; later separated windows can each connect once. Only commit
/// the returned mask after invulnerability and world occlusion checks pass.
pub fn resolve_authored_actor_contact_pending(
    attacker_capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    attacker_pose: Option<&ActorPoseSnapshot>,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: Option<&ActorPoseSnapshot>,
    connected_mask: u16,
) -> (AuthoredActorContact, u16) {
    resolve_authored_actor_contact_swept_pending(
        attacker_capsules,
        action,
        attacker_pose,
        None,
        defender_capsules,
        defender_pose,
        connected_mask,
    )
}

/// Resolve enemy blade motion between consecutive retained poses, clipped to
/// each authored strike window. The caller must discard the previous pose on a
/// clip change; stale ticks and phase resets are rejected here as well. Only
/// commit the returned mask after invulnerability and world occlusion checks.
#[allow(clippy::too_many_arguments)]
pub fn resolve_authored_actor_contact_swept_pending(
    attacker_capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    attacker_pose: Option<&ActorPoseSnapshot>,
    previous_attacker_pose: Option<&ActorPoseSnapshot>,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: Option<&ActorPoseSnapshot>,
    connected_mask: u16,
) -> (AuthoredActorContact, u16) {
    let Some(attacker_pose) = attacker_pose else {
        return (AuthoredActorContact::Miss, 0);
    };
    match ActiveStrike::sample(
        attacker_capsules,
        action,
        attacker_pose,
        previous_attacker_pose,
    )
    .contact(
        attacker_capsules,
        defender_capsules,
        defender_pose,
        connected_mask,
    ) {
        Some(contact) => (
            AuthoredActorContact::Hit {
                damage: contact.damage,
                poise_damage: contact.poise_damage,
            },
            contact.window,
        ),
        None => (AuthoredActorContact::Miss, 0),
    }
}

/// The first of the coming ticks `from..=until` on which an authored swing
/// connects with a defender, predicted with the contact code itself
/// ([`resolve_authored_actor_contact_swept_pending`]): the attacker's retained
/// pose replayed at the phase its swing reaches on each of those ticks
/// (`phase_at(k)`, `k` ticks from now on the swing's own clock), swept from
/// the tick before, against the defender's retained pose, both fighters where
/// they stand now. None when the blade does not reach the defender within
/// those ticks, or when either side has no authored capsules.
///
/// This is the moment a parry has to meet: the first authored hitbox frame
/// can come several ticks before the blade reaches the body.
#[allow(clippy::too_many_arguments)]
// Read a few ticks per swing, once per think: small over fast.
#[cfg_attr(target_arch = "mips", optimize(size))]
pub fn predicted_contact_tick(
    attacker_capsules: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    attacker_pose: &ActorPoseSnapshot,
    phase_at: impl Fn(u16) -> u32,
    defender_capsules: &[CombatCapsuleRecord],
    defender_pose: &ActorPoseSnapshot,
    from: u16,
    until: u16,
) -> Option<u16> {
    let at = |k: u16| {
        attacker_pose
            .with_tick(attacker_pose.tick().wrapping_add(u32::from(k)))
            .with_phase_q12(phase_at(k))
    };
    (from..=until).find(|&k| {
        let previous = k.checked_sub(1).map(at);
        matches!(
            resolve_authored_actor_contact_swept_pending(
                attacker_capsules,
                action,
                Some(&at(k)),
                previous.as_ref(),
                defender_capsules,
                Some(defender_pose),
                0,
            )
            .0,
            AuthoredActorContact::Hit { .. }
        )
    })
}

/// Whether this pose interval begins an authored melee swing. Overlapping
/// blades with the same start produce one cue; separated starts produce new
/// cues even if the attack animation stays the same.
pub fn melee_window_started(
    records: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    previous_phase: Option<u32>,
    phase: u32,
) -> bool {
    let previous = previous_phase.filter(|old| *old <= phase);
    records.iter().any(|record| {
        let start = u32::from(record.active_start_frame) << 12;
        record.flags & combat_capsule_flags::HITBOX != 0
            && record.action == action.to_index() as u8
            && phase >= start
            && previous.map_or(phase >> 12 <= u32::from(record.active_end_frame), |old| {
                old < start
            })
    })
}

/// The authored contact frames of `action` among a fighter's capsules: the
/// first frame any of its hitboxes is active and the last. None when the
/// action has no authored hitbox (its contact is timed by its record).
pub fn authored_contact_frames(
    records: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
) -> Option<(u16, u16)> {
    records
        .iter()
        .filter(|record| {
            record.flags & combat_capsule_flags::HITBOX != 0
                && record.action == action.to_index() as u8
        })
        .fold(None, |span, record| {
            let (first, last) = span.unwrap_or((u16::MAX, 0));
            Some((
                first.min(record.active_start_frame),
                last.max(record.active_end_frame),
            ))
        })
}

/// The authored frames an attack's contact covers: its hit volumes' active
/// frames, or for an attack with none (a shot), its projectile emitters'.
pub fn authored_event_frames(
    records: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
) -> Option<(u16, u16)> {
    event_frames(
        records.iter().map(|record| {
            (
                record.flags,
                record.action,
                record.active_start_frame,
                record.active_end_frame,
            )
        }),
        action,
    )
}

/// [`authored_event_frames`] over any capsule table's `(flags, action, first
/// frame, last frame)`: the cooked tables a host tool reads use the same rule.
pub fn event_frames(
    capsules: impl Iterator<Item = (u8, u8, u16, u16)> + Clone,
    action: CharacterAnimationAction,
) -> Option<(u16, u16)> {
    let span = |kind: u8| {
        capsules
            .clone()
            .filter(|(flags, owner, _, _)| flags & kind != 0 && *owner == action.to_index() as u8)
            .fold(None, |span, (_, _, first, last)| {
                let (low, high) = span.unwrap_or((u16::MAX, 0));
                Some((low.min(first), high.max(last)))
            })
    };
    span(combat_capsule_flags::HITBOX).or_else(|| span(combat_capsule_flags::PROJECTILE_EMITTER))
}

/// Ticks until a swing first makes contact, as anyone watching it reads
/// it: its contact runs over frames `first..=last` of its own timeline
/// (animation frames, or ticks for a swing timed by its record), it stands
/// at `phase_q12` (Q12 frames) and advances `rate_q12` per tick. 0 while
/// contact is live; None once the last contact frame has passed or when
/// the swing does not advance. The one reading every controller uses,
/// whichever fighter swings.
pub fn ticks_to_contact(first: u16, last: u16, phase_q12: u32, rate_q12: u32) -> Option<u16> {
    let start = u32::from(first) << 12;
    let end = (u32::from(last) + 1) << 12;
    if phase_q12 >= end {
        return None;
    }
    if phase_q12 >= start {
        return Some(0);
    }
    if rate_q12 == 0 {
        return None;
    }
    let ticks = (start - phase_q12).div_ceil(rate_q12);
    Some(ticks.min(u32::from(u16::MAX - 1)) as u16)
}

/// Follow the target during the first melee tell, then commit before contact.
/// Using the first window also prevents turning back toward a dodging player
/// between strikes of an already committed combo.
pub fn melee_tell_tracks(
    records: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    phase: u32,
) -> bool {
    records
        .iter()
        .filter(|record| {
            record.flags & combat_capsule_flags::HITBOX != 0
                && record.action == action.to_index() as u8
        })
        .map(|record| record.active_start_frame)
        .min()
        .is_some_and(|strike| phase < (u32::from(strike.saturating_sub(2)) << 12))
}

/// Aim through the charge, then leave two sampled frames of committed aim
/// before the first release. Released projectiles never consult this helper.
pub fn ranged_charge_tracks(
    records: &[CombatCapsuleRecord],
    action: CharacterAnimationAction,
    phase: u32,
) -> bool {
    records
        .iter()
        .filter(|record| {
            record.flags & combat_capsule_flags::PROJECTILE_EMITTER != 0
                && record.action == action.to_index() as u8
        })
        .map(|record| record.active_start_frame)
        .min()
        .is_some_and(|release| phase < (u32::from(release.saturating_sub(2)) << 12))
}

/// Four blade-edge samples, clamped to one authored damage window. The trail
/// disappears in gaps and cannot bridge from one swing into the next.
pub fn melee_trail_phases(
    record: &CombatCapsuleRecord,
    action: CharacterAnimationAction,
    phase: u32,
) -> Option<[u32; 4]> {
    let start = u32::from(record.active_start_frame) << 12;
    let end = (u32::from(record.active_end_frame) + 1) << 12;
    if record.flags & combat_capsule_flags::HITBOX == 0
        || record.action != action.to_index() as u8
        || phase < start
        || phase >= end
    {
        return None;
    }
    Some(core::array::from_fn(|i| {
        phase.saturating_sub((2 << 12) * i as u32 / 3).max(start)
    }))
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_swing_reads_ticks_to_its_first_contact_frame() {
        // Contact over frames 10..=12, one frame every two ticks.
        assert_eq!(ticks_to_contact(10, 12, 0, 2048), Some(20));
        assert_eq!(ticks_to_contact(10, 12, 9 << 12, 2048), Some(2));
        assert_eq!(ticks_to_contact(10, 12, (9 << 12) + 1, 2048), Some(2));
        assert_eq!(ticks_to_contact(10, 12, 10 << 12, 2048), Some(0));
        assert_eq!(ticks_to_contact(10, 12, (12 << 12) + 4095, 2048), Some(0));
        assert_eq!(ticks_to_contact(10, 12, 13 << 12, 2048), None);
        assert_eq!(ticks_to_contact(10, 12, 0, 0), None);
        // A record-timed swing: ticks are its frames, one per tick.
        assert_eq!(ticks_to_contact(16, 21, 5 << 12, 4096), Some(11));
    }

    use super::*;

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

        #[test]
        fn bolts_test_every_authored_hurtbox_else_the_body() {
            let hurt = |joint| {
                capsule_record(
                    joint,
                    combat_capsule_flags::HURTBOX,
                    CharacterAnimationAction::Idle,
                )
            };
            let hit = capsule_record(
                0,
                combat_capsule_flags::HITBOX,
                CharacterAnimationAction::LightAttack,
            );
            let body = body_hurt_capsule([10, 0, 20], 12, 64);
            assert_eq!(
                body,
                Some(WorldCombatCapsule {
                    start: [10, 12, 20],
                    end: [10, 52, 20],
                    radius: 12
                })
            );
            assert_eq!(body_hurt_capsule([0; 3], 0, 64), None);
            let pose = pose_at([0, 0, 0], 0);
            let mut got = (0usize, None);
            projectile_hurtboxes(&[hit, hurt(0), hurt(0)], Some(&pose), body, |c| {
                got = (got.0 + 1, Some(c))
            });
            assert_eq!(
                got.0, 2,
                "both authored hurtboxes, not the hitbox or the body"
            );
            got = (0, None);
            projectile_hurtboxes(&[hurt(0); 6], Some(&pose), body, |c| {
                got = (got.0 + 1, Some(c))
            });
            assert_eq!(got.0, PROJECTILE_HURTBOXES_PER_FIGHTER);
            got = (0, None);
            projectile_hurtboxes(&[hit, hurt(0)], None, body, |c| got = (got.0 + 1, Some(c)));
            assert_eq!(got, (1, body), "no pose: the body capsule");
            got = (0, None);
            projectile_hurtboxes(&[hit], Some(&pose), body, |c| got = (got.0 + 1, Some(c)));
            assert_eq!(got, (1, body), "no authored hurtbox: the body capsule");
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
                Some(&pose_at([1000, 2000, 3000], ACTIVE_PHASE)),
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
                    Some(&pose_at([1000, 2000, 3000], 4 << 12)),
                )
                .is_some(),
                "a skipped exact frame releases on the first retained pose after it"
            );
            let charge = authored_projectile_charge(
                &EMITTERS,
                ATTACK,
                Some(&pose_at([1000, 2000, 3000], 2 << 12)),
            )
            .expect("charge window is visible before release");
            assert_eq!(charge.position, [1020, 2030, 3040]);
            assert_eq!(charge.progress_q8, 128);
            assert_eq!(
                authored_projectile_release(
                    &EMITTERS,
                    ATTACK,
                    Some(&pose_at([1000, 2000, 3000], INACTIVE_PHASE)),
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
            let query = |frame: u32, mask| {
                authored_projectile_release_pending(
                    &emitters,
                    ATTACK,
                    Some(&pose_at([0; 3], frame << 12)),
                    mask,
                )
            };
            assert!(query(2, 0).is_none());
            assert_eq!(query(4, 0).unwrap().0, 0);
            assert_eq!(
                query(4, 0).unwrap().0,
                0,
                "pool-full retry keeps its marker"
            );
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
        fn swing_cues_cross_each_window_once_at_both_cadences() {
            let mut swings = [capsule_record(0, combat_capsule_flags::HITBOX, ATTACK); 3];
            for (record, start) in swings.iter_mut().zip([13, 28, 40]) {
                record.active_start_frame = start;
                record.active_end_frame = start + 5;
            }
            for step in [2048, 4096, 6144] {
                let mut previous = None;
                let mut cues = 0;
                for phase in (0..50 * 4096).step_by(step) {
                    cues += usize::from(melee_window_started(&swings, ATTACK, previous, phase));
                    assert!(!melee_window_started(&swings, ATTACK, Some(phase), phase));
                    previous = Some(phase);
                }
                assert_eq!(cues, 3);
            }
            assert!(!melee_window_started(
                &swings,
                CharacterAnimationAction::HeavyAttack,
                None,
                14 << 12
            ));
            assert!(melee_window_started(
                &swings,
                ATTACK,
                Some(45 << 12),
                14 << 12
            ));
        }

        #[test]
        fn melee_tell_commits_before_first_swing_and_stays_committed_between_swings() {
            let mut swings = [capsule_record(0, combat_capsule_flags::HITBOX, ATTACK); 2];
            swings[0].active_start_frame = 14;
            swings[1].active_start_frame = 24;
            assert!(melee_tell_tracks(&swings, ATTACK, 11 << 12));
            for frame in [12, 14, 20, 24, 30] {
                assert!(!melee_tell_tracks(&swings, ATTACK, frame << 12));
            }
            assert!(!melee_tell_tracks(&[], ATTACK, 0));
            assert!(!melee_tell_tracks(
                &swings,
                CharacterAnimationAction::HeavyAttack,
                0
            ));
        }

        #[test]
        fn weapon_clock_releases_from_live_walking_pose_once() {
            let mut emitter = capsule_record(0, combat_capsule_flags::PROJECTILE_EMITTER, ATTACK);
            emitter.active_start_frame = 2;
            emitter.active_end_frame = 3;
            // Gait phase is unrelated to the shot; the muzzle follows the moving actor.
            let actor_pose = pose_at([90, 20, -40], 18 << 12);
            let pose = Some(actor_pose);
            assert!(
                authored_projectile_release_pending(&[emitter], ATTACK, pose.as_ref(), 0).is_none()
            );
            assert!(authored_projectile_release_pending_at_frame(
                &[emitter],
                ATTACK,
                pose.as_ref(),
                0,
                1
            )
            .is_none());
            let (_, release) = authored_projectile_release_pending_at_frame(
                &[emitter],
                ATTACK,
                pose.as_ref(),
                0,
                2,
            )
            .unwrap();
            assert_eq!(
                release.position,
                transform_actor_combat_capsule(&emitter, &actor_pose)
                    .unwrap()
                    .start
            );
            assert!(authored_projectile_release_pending_at_frame(
                &[emitter],
                ATTACK,
                pose.as_ref(),
                1,
                2
            )
            .is_none());
            assert!(authored_projectile_release_pending_at_frame(
                &[emitter],
                ATTACK,
                pose.as_ref(),
                0,
                4
            )
            .is_none());
        }

        #[test]
        fn ranged_charge_tracks_until_two_frames_before_single_release() {
            let mut emitter = capsule_record(0, combat_capsule_flags::PROJECTILE_EMITTER, ATTACK);
            emitter.active_start_frame = 27;
            emitter.active_end_frame = 27;
            assert!(ranged_charge_tracks(&[emitter], ATTACK, 24 << 12));
            assert!(!ranged_charge_tracks(&[emitter], ATTACK, 25 << 12));
            assert!(!ranged_charge_tracks(&[emitter], ATTACK, 28 << 12));
            let pose = Some(pose_at([0; 3], 28 << 12));
            assert!(
                authored_projectile_release_pending(&[emitter], ATTACK, pose.as_ref(), 0).is_some()
            );
            assert!(
                authored_projectile_release_pending(&[emitter], ATTACK, pose.as_ref(), 1).is_none()
            );
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
                let query = |mask| {
                    resolve_authored_actor_contact_pending(
                        &swings,
                        ATTACK,
                        Some(&pose_at([0; 3], frame << 12)),
                        &HURTBOXES,
                        defender.as_ref(),
                        mask,
                    )
                };
                let (hit, window) = query(used);
                assert!(matches!(hit, AuthoredActorContact::Hit { .. }));
                assert_eq!(window, 1 << i);
                used |= window;
                assert_eq!(query(used), (AuthoredActorContact::Miss, 0));
                let phases = melee_trail_phases(&swings[i], ATTACK, frame << 12).unwrap();
                assert!(phases
                    .iter()
                    .all(|p| *p >= u32::from(swings[i].active_start_frame) << 12));
                let gap = frame + 2;
                assert!(melee_trail_phases(&swings[i], ATTACK, gap << 12).is_none());
                assert_eq!(
                    resolve_authored_actor_contact_pending(
                        &swings,
                        ATTACK,
                        Some(&pose_at([0; 3], gap << 12)),
                        &HURTBOXES,
                        defender.as_ref(),
                        used
                    ),
                    (AuthoredActorContact::Miss, 0)
                );
            }
            // Multiple volumes within one swing must not multiply its damage.
            let copies = [swings[0]; 2];
            let (_, window) = resolve_authored_actor_contact_pending(
                &copies,
                ATTACK,
                Some(&pose_at([0; 3], 4 << 12)),
                &HURTBOXES,
                defender.as_ref(),
                0,
            );
            assert_eq!(window, 3);
        }

        fn retained_pose(tick: u32, x: i32, phase: u32) -> ActorPoseSnapshot {
            ActorPoseSnapshot::new(
                SimTick::from_u32(tick),
                static_one_joint_animation(),
                phase,
                None,
                WorldVertex::new(x, 0, 0),
                Mat3I16::IDENTITY,
                LocalToWorldScale::IDENTITY,
                ModelPoseTranslation { x: 0, y: 0, z: 0 },
            )
        }

        #[test]
        fn enemy_sweep_catches_crossing_and_a_fully_skipped_strike() {
            let defender = Some(pose_at([0; 3], 0));
            let previous = retained_pose(1, -400, 1 << 12);
            let current = retained_pose(2, 400, 6 << 12);
            // Both retained samples are outside the active window and clear
            // of the defender. The active portion crosses the defender.
            assert_eq!(
                resolve_authored_actor_contact_pending(
                    &HITBOXES,
                    ATTACK,
                    Some(&current),
                    &HURTBOXES,
                    defender.as_ref(),
                    0,
                ),
                (AuthoredActorContact::Miss, 0)
            );
            let query = |mask| {
                resolve_authored_actor_contact_swept_pending(
                    &HITBOXES,
                    ATTACK,
                    Some(&current),
                    Some(&previous),
                    &HURTBOXES,
                    defender.as_ref(),
                    mask,
                )
            };
            assert_eq!(
                query(0),
                (
                    AuthoredActorContact::Hit {
                        damage: 25,
                        poise_damage: 35
                    },
                    1
                )
            );
            assert_eq!(query(1), (AuthoredActorContact::Miss, 0));
            assert_eq!(
                resolve_authored_actor_contact_swept_pending(
                    &HITBOXES,
                    ATTACK,
                    Some(&current),
                    Some(&previous),
                    &HURTBOXES,
                    Some(&pose_at([0, 0, 500], 0)),
                    0,
                ),
                (AuthoredActorContact::Miss, 0)
            );
        }

        #[test]
        fn enemy_sweep_does_not_cross_inactive_gaps_or_consume_another_window() {
            let mut swings = [HITBOXES[0]; 3];
            // Two volumes for the first strike, one for the later strike.
            swings[2].active_start_frame = 8;
            swings[2].active_end_frame = 10;
            let defender = Some(pose_at([0; 3], 0));
            let query = |old: &ActorPoseSnapshot, current: &ActorPoseSnapshot, mask| {
                resolve_authored_actor_contact_swept_pending(
                    &swings,
                    ATTACK,
                    Some(current),
                    Some(old),
                    &HURTBOXES,
                    defender.as_ref(),
                    mask,
                )
            };
            let first = query(
                &retained_pose(1, -400, 1 << 12),
                &retained_pose(2, 400, 6 << 12),
                0,
            );
            assert_eq!(first.1, 3);
            assert_eq!(
                query(
                    &retained_pose(2, -400, 6 << 12),
                    &retained_pose(3, 400, 7 << 12),
                    0
                ),
                (AuthoredActorContact::Miss, 0)
            );
            let second = query(
                &retained_pose(3, -400, 7 << 12),
                &retained_pose(4, 400, 11 << 12),
                first.1,
            );
            assert!(matches!(second.0, AuthoredActorContact::Hit { .. }));
            assert_eq!(second.1, 4);
        }

        #[test]
        fn clipped_sweep_excludes_root_travel_outside_the_damage_window() {
            let previous = retained_pose(1, -400, 0);
            let current = retained_pose(2, 400, 10 << 12);
            let mut hitbox = HITBOXES[0];
            for (start, end, valid_target) in [(0, 1, -320), (8, 9, 320)] {
                hitbox.active_start_frame = start;
                hitbox.active_end_frame = end;
                let query = |x| {
                    resolve_authored_actor_contact_swept_pending(
                        &[hitbox],
                        ATTACK,
                        Some(&current),
                        Some(&previous),
                        &HURTBOXES,
                        Some(&pose_at([x, 0, 0], 0)),
                        0,
                    )
                };
                // The full origin chord crosses zero, but only while the
                // weapon is inactive. Contact inside the clipped part remains.
                assert_eq!(query(0), (AuthoredActorContact::Miss, 0));
                assert!(matches!(
                    query(valid_target).0,
                    AuthoredActorContact::Hit { .. }
                ));
            }
        }

        /// A swing whose first authored hitbox is active from frame 2 but
        /// out of reach, and whose blade reaches the defender only on frames
        /// 6 and 7: the prediction names frame 6, the first real contact.
        #[test]
        fn the_prediction_finds_the_first_real_blade_contact() {
            let mut early = HITBOXES[0];
            early.start = [-300, 0, 0];
            early.end = [-300, 100, 0];
            early.active_start_frame = 2;
            early.active_end_frame = 3;
            let mut late = HITBOXES[0];
            late.active_start_frame = 6;
            late.active_end_frame = 7;
            let swing = [early, late];
            assert_eq!(authored_contact_frames(&swing, ATTACK), Some((2, 7)));
            let predict = |defender: [i32; 3], until: u16| {
                predicted_contact_tick(
                    &swing,
                    ATTACK,
                    &retained_pose(1, 0, 0),
                    |k| u32::from(k) << 12,
                    &HURTBOXES,
                    &pose_at(defender, 0),
                    2,
                    until,
                )
            };
            assert_eq!(predict([50, 0, 0], 10), Some(6));
            assert_eq!(predict([50, 0, 0], 5), None, "not within the window");
            assert_eq!(predict([1000, 0, 0], 10), None, "out of reach");
            assert_eq!(
                predicted_contact_tick(
                    &swing,
                    ATTACK,
                    &retained_pose(1, 0, 0),
                    |k| u32::from(k) << 12,
                    &[],
                    &pose_at([50, 0, 0], 0),
                    2,
                    10,
                ),
                None,
                "no hurtbox"
            );
        }

        #[test]
        fn enemy_sweep_rejects_stale_interrupted_and_restarted_poses() {
            let defender = Some(pose_at([0; 3], 0));
            let query = |old: Option<&ActorPoseSnapshot>, current: &ActorPoseSnapshot| {
                resolve_authored_actor_contact_swept_pending(
                    &HITBOXES,
                    ATTACK,
                    Some(current),
                    old,
                    &HURTBOXES,
                    defender.as_ref(),
                    0,
                )
            };
            let current = retained_pose(5, 400, 3 << 12);
            // Missing/changed clips are filtered by the caller. A stale pose
            // or a reset phase must not draw a damaging chord into a new action.
            for old in [
                None,
                Some(retained_pose(1, -400, 2 << 12)),
                Some(retained_pose(4, -400, 9 << 12)),
            ] {
                assert_eq!(
                    query(old.as_ref(), &current),
                    (AuthoredActorContact::Miss, 0)
                );
            }
            // A new action may still deal a legitimate current-pose hit.
            assert!(matches!(
                query(
                    Some(&retained_pose(4, -400, 9 << 12)),
                    &retained_pose(5, 0, 3 << 12)
                )
                .0,
                AuthoredActorContact::Hit { .. }
            ));
        }

        #[test]
        fn an_unauthored_side_never_connects() {
            let near = [1000, 2000, 3000];
            let attacker = Some(pose_at(near, ACTIVE_PHASE));
            let defender = Some(pose_at(near, 0));

            // Attacker with no HITBOX at all.
            assert_eq!(
                resolve_authored_actor_contact(
                    &HURTBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            // Attacker authored only for a DIFFERENT action slot.
            const HEAVY_ONLY: [CombatCapsuleRecord; 1] = [capsule_record(
                0,
                combat_capsule_flags::HITBOX,
                CharacterAnimationAction::HeavyAttack,
            )];
            assert_eq!(
                resolve_authored_actor_contact(
                    &HEAVY_ONLY,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            // Defender with no HURTBOX at all.
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &[],
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &HITBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );

            // Both sides authored and connecting: the authored numbers land.
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    defender.as_ref()
                ),
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
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    None,
                    &HURTBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    None
                ),
                AuthoredActorContact::Miss
            );
            // A frame outside the authored active window is a miss.
            let early = Some(pose_at(near, INACTIVE_PHASE));
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    early.as_ref(),
                    &HURTBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            // A hitbox naming a joint the rig does not have is a miss.
            const BAD_JOINT: [CombatCapsuleRecord; 1] =
                [capsule_record(9, combat_capsule_flags::HITBOX, ATTACK)];
            assert_eq!(
                resolve_authored_actor_contact(
                    &BAD_JOINT,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    defender.as_ref()
                ),
                AuthoredActorContact::Miss
            );
            // Geometric separation is a miss.
            let far = Some(pose_at([1000, 2000, 30_000], 0));
            assert_eq!(
                resolve_authored_actor_contact(
                    &HITBOXES,
                    ATTACK,
                    attacker.as_ref(),
                    &HURTBOXES,
                    far.as_ref()
                ),
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
    /// form of the cross-layout stage the old souls slice gate ran:
    /// before 2026-08-11 that gate scored melee 4 on one guest build and melee
    /// 3 on another, and the shape of the bug it was blamed on would show up
    /// here as an active window that shrinks when rendering gets slower.
    mod tick_authority {
        extern crate std;

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
        fn replay(render_cost_vblanks: u32, window: (u16, u16)) -> Replay {
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
                            let frame = phase >> 12;
                            if frame >= u32::from(window.0) && frame <= u32::from(window.1) {
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
            let baseline = replay(1, ACTIVE_WINDOW);
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
                let slow = replay(render_cost_vblanks, ACTIVE_WINDOW);
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
            // The window opens and closes on fixed offsets from the swing's
            // start tick, so both edges are frame-rate invariant too.
            let first = ATTACK_START + 2 * u32::from(ACTIVE_WINDOW.0);
            let last = ATTACK_START + 2 * u32::from(ACTIVE_WINDOW.1) + 1;
            for render_cost_vblanks in [1u32, 4, 12] {
                let run = replay(render_cost_vblanks, ACTIVE_WINDOW);
                assert_eq!(run.active_ticks.first().copied(), Some(first));
                assert_eq!(run.active_ticks.last().copied(), Some(last));
            }
        }
    }
}
