//! Deterministic, fixed-capacity combat projectiles.
//!
//! A projectile advances as a swept sphere every simulation tick. The sweep
//! is represented by the same [`WorldCombatCapsule`] primitive as authored
//! melee hitboxes and hurtboxes, so fast bolts cannot tunnel through an actor
//! and the forthcoming unified combat-volume system has one narrow phase.
//! World geometry is supplied through [`ProjectileWorldTracer`]: BSP, grid,
//! and game-specific collision stay outside this policy crate while failures
//! fail closed. Storage is all-zero-valid and heap-free for PS1 BSS use.

use psx_level::RoomIndex;
use psx_math::int32::{isqrt_i32, square_i32_saturating};

use crate::combat::{combat_capsule_sweep_contact_fraction_q12, WorldCombatCapsule};

/// Sentinel used when a projectile has no entity owner (for example a trap).
pub const NO_PROJECTILE_OWNER: u16 = u16::MAX;

/// Build a deterministic per-tick velocity which points from `start` to
/// `target`. Large world deltas are shifted together before normalization, so
/// the direction is preserved while all products remain native 32-bit on PS1.
pub fn velocity_toward(start: [i32; 3], target: [i32; 3], speed: u16) -> [i32; 3] {
    let mut delta = [
        target[0].saturating_sub(start[0]),
        target[1].saturating_sub(start[1]),
        target[2].saturating_sub(start[2]),
    ];
    let mut maximum = delta[0]
        .saturating_abs()
        .max(delta[1].saturating_abs())
        .max(delta[2].saturating_abs());
    while maximum > 16_000 {
        delta[0] >>= 1;
        delta[1] >>= 1;
        delta[2] >>= 1;
        maximum >>= 1;
    }
    let length = isqrt_i32(
        square_i32_saturating(delta[0])
            .saturating_add(square_i32_saturating(delta[1]))
            .saturating_add(square_i32_saturating(delta[2])),
    );
    if length == 0 || speed == 0 {
        return [0; 3];
    }
    let speed = i32::from(speed);
    let mut velocity = [
        delta[0].saturating_mul(speed) / length,
        delta[1].saturating_mul(speed) / length,
        delta[2].saturating_mul(speed) / length,
    ];
    if velocity == [0; 3] {
        let mut axis = 0usize;
        if delta[1].saturating_abs() > delta[axis].saturating_abs() {
            axis = 1;
        }
        if delta[2].saturating_abs() > delta[axis].saturating_abs() {
            axis = 2;
        }
        velocity[axis] = delta[axis].signum();
    }
    velocity
}

/// Logical combat side used for friendly-fire rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CombatTeam {
    /// Environmental attack: may hit every team except its exact owner.
    Neutral = 0,
    /// Player and player-owned attacks.
    Player = 1,
    /// Enemy and enemy-owned attacks.
    Enemy = 2,
}

/// Complete immutable description of one projectile at spawn time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectileSpawn {
    /// Room-local initial center.
    pub position: [i32; 3],
    /// Room-local displacement applied once per 60 Hz simulation tick.
    pub velocity: [i32; 3],
    /// Collision sphere radius in engine units.
    pub radius: u16,
    /// Health damage delivered by the first actor impact.
    pub damage: u16,
    /// Poise damage delivered by the first actor impact.
    pub poise_damage: u16,
    /// Maximum 60 Hz ticks before the projectile expires.
    pub lifetime_ticks: u16,
    /// Collision room containing the projectile.
    pub room: RoomIndex,
    /// Side used for friendly-fire filtering.
    pub team: CombatTeam,
    /// Entity index which fired it, or [`NO_PROJECTILE_OWNER`].
    pub owner: u16,
    /// Render tint retained by the runtime; collision does not interpret it.
    pub tint_rgb: [u8; 3],
}

/// Why a projectile could not enter the fixed pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileSpawnError {
    /// Zero radius or zero lifetime is not a live projectile contract.
    Invalid,
    /// Every fixed runtime slot is occupied.
    PoolFull,
}

/// Read-only snapshot used by render and debug consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectileSnapshot {
    /// Current center.
    pub position: [i32; 3],
    /// Per-tick displacement.
    pub velocity: [i32; 3],
    /// Collision sphere radius.
    pub radius: u16,
    /// Current room.
    pub room: RoomIndex,
    /// Render tint.
    pub tint_rgb: [u8; 3],
    /// Ticks left before expiry.
    pub lifetime_ticks: u16,
}

/// One actor hurtbox exposed to the projectile tick.
///
/// Multiple entries may share a `target`; the resolver selects the earliest
/// contact across all of that actor's capsules and emits only one impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectileTarget {
    /// Stable target identifier supplied back in the impact.
    pub target: u16,
    /// Target combat side.
    pub team: CombatTeam,
    /// Target collision room.
    pub room: RoomIndex,
    /// World-space hurtbox.
    pub hurtbox: WorldCombatCapsule,
}

/// Result of tracing one projectile sweep against static/moving world data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileWorldTrace {
    /// No world collision; `end` is normally the requested end.
    Clear {
        /// Authoritative unobstructed endpoint.
        end: [i32; 3],
    },
    /// World collision; `end` is the first safe/contact point.
    Hit {
        /// First world contact point.
        end: [i32; 3],
    },
    /// Collision data was missing, malformed, or overflowed. The projectile
    /// is removed at its old position so bad data cannot shoot through walls.
    Failed,
}

/// Game-specific world-collision adapter used by [`CombatProjectiles::tick`].
pub trait ProjectileWorldTracer {
    /// Clip a projectile center sweep against the current world.
    fn trace_projectile(
        &mut self,
        room: RoomIndex,
        start: [i32; 3],
        end: [i32; 3],
        radius: u16,
    ) -> ProjectileWorldTrace;
}

/// Kind of resolved projectile impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectileImpactKind {
    /// Static/moving world geometry, including fail-closed trace errors.
    World,
    /// Actor hurtbox contact.
    Target {
        /// Stable target identifier from [`ProjectileTarget`].
        target: u16,
    },
}

/// One projectile impact emitted after collision resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectileImpact {
    /// Pool slot of the projectile that stopped.
    pub projectile: u16,
    /// Collision classification.
    pub kind: ProjectileImpactKind,
    /// Room-local impact center.
    pub position: [i32; 3],
    /// Health damage copied from the projectile.
    pub damage: u16,
    /// Poise damage copied from the projectile.
    pub poise_damage: u16,
    /// Firing team.
    pub team: CombatTeam,
    /// Firing entity or [`NO_PROJECTILE_OWNER`].
    pub owner: u16,
}

impl ProjectileImpact {
    const EMPTY: Self = Self {
        projectile: 0,
        kind: ProjectileImpactKind::World,
        position: [0; 3],
        damage: 0,
        poise_damage: 0,
        team: CombatTeam::Neutral,
        owner: NO_PROJECTILE_OWNER,
    };
}

/// Fixed impact queue filled by one projectile update.
pub struct ProjectileImpacts<const N: usize> {
    entries: [ProjectileImpact; N],
    len: usize,
}

impl<const N: usize> ProjectileImpacts<N> {
    /// Empty, all-zero-compatible queue.
    pub const fn new() -> Self {
        Self {
            entries: [ProjectileImpact::EMPTY; N],
            len: 0,
        }
    }

    /// Remove all queued impacts without touching backing storage.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Populated impacts in deterministic resolution order.
    pub fn as_slice(&self) -> &[ProjectileImpact] {
        &self.entries[..self.len]
    }

    fn push(&mut self, impact: ProjectileImpact) -> bool {
        let Some(slot) = self.entries.get_mut(self.len) else {
            return false;
        };
        *slot = impact;
        self.len += 1;
        true
    }
}

impl<const N: usize> Default for ProjectileImpacts<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-tick projectile diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProjectileTickStats {
    /// Projectiles advanced this tick.
    pub advanced: u16,
    /// Actor impacts resolved.
    pub actor_hits: u16,
    /// World impacts resolved, including fail-closed trace failures.
    pub world_hits: u16,
    /// Projectiles removed by lifetime expiry.
    pub expired: u16,
    /// Impacts resolved after the caller's output queue filled.
    pub dropped_impacts: u16,
}

/// Heap-free projectile arena. Every field has a safe all-zero inactive state.
pub struct CombatProjectiles<const N: usize> {
    active: [u8; N],
    positions: [[i32; 3]; N],
    velocities: [[i32; 3]; N],
    radii: [u16; N],
    damage: [u16; N],
    poise_damage: [u16; N],
    lifetime_ticks: [u16; N],
    rooms: [RoomIndex; N],
    teams: [CombatTeam; N],
    owners: [u16; N],
    tints: [[u8; 3]; N],
}

impl<const N: usize> CombatProjectiles<N> {
    /// Empty arena suitable for static/BSS initialization.
    pub const fn new() -> Self {
        Self {
            active: [0; N],
            positions: [[0; 3]; N],
            velocities: [[0; 3]; N],
            radii: [0; N],
            damage: [0; N],
            poise_damage: [0; N],
            lifetime_ticks: [0; N],
            rooms: [RoomIndex::ZERO; N],
            teams: [CombatTeam::Neutral; N],
            owners: [NO_PROJECTILE_OWNER; N],
            tints: [[0; 3]; N],
        }
    }

    /// Clear all live slots in O(N), retaining deterministic storage.
    pub fn clear(&mut self) {
        let mut index = 0usize;
        while index < N {
            self.active[index] = 0;
            index += 1;
        }
    }

    /// Number of live projectiles.
    pub fn len(&self) -> usize {
        self.active.iter().filter(|active| **active != 0).count()
    }

    /// Whether no projectile is live.
    pub fn is_empty(&self) -> bool {
        !self.active.iter().any(|active| *active != 0)
    }

    /// Read one live slot for rendering/debugging.
    pub fn get(&self, index: usize) -> Option<ProjectileSnapshot> {
        if *self.active.get(index)? == 0 {
            return None;
        }
        Some(ProjectileSnapshot {
            position: self.positions[index],
            velocity: self.velocities[index],
            radius: self.radii[index],
            room: self.rooms[index],
            tint_rgb: self.tints[index],
            lifetime_ticks: self.lifetime_ticks[index],
        })
    }

    /// Insert a projectile into the first free slot.
    pub fn spawn(&mut self, spawn: ProjectileSpawn) -> Result<usize, ProjectileSpawnError> {
        if spawn.radius == 0 || spawn.lifetime_ticks == 0 {
            return Err(ProjectileSpawnError::Invalid);
        }
        let Some(index) = self.active.iter().position(|active| *active == 0) else {
            return Err(ProjectileSpawnError::PoolFull);
        };
        self.active[index] = 1;
        self.positions[index] = spawn.position;
        self.velocities[index] = spawn.velocity;
        self.radii[index] = spawn.radius;
        self.damage[index] = spawn.damage;
        self.poise_damage[index] = spawn.poise_damage;
        self.lifetime_ticks[index] = spawn.lifetime_ticks;
        self.rooms[index] = spawn.room;
        self.teams[index] = spawn.team;
        self.owners[index] = spawn.owner;
        self.tints[index] = spawn.tint_rgb;
        Ok(index)
    }

    /// Advance every live projectile once, clip against world collision, then
    /// resolve the earliest eligible actor hurtbox along the clipped sweep.
    /// One impact consumes the projectile. Actor ties are stable by target id,
    /// independent of hurtbox input order.
    pub fn tick<T: ProjectileWorldTracer, const I: usize>(
        &mut self,
        targets: &[ProjectileTarget],
        tracer: &mut T,
        impacts: &mut ProjectileImpacts<I>,
    ) -> ProjectileTickStats {
        impacts.clear();
        let mut stats = ProjectileTickStats::default();
        let mut index = 0usize;
        while index < N {
            if self.active[index] == 0 {
                index += 1;
                continue;
            }
            stats.advanced = stats.advanced.saturating_add(1);
            let start = self.positions[index];
            let requested_end = add3(start, self.velocities[index]);
            let trace =
                tracer.trace_projectile(self.rooms[index], start, requested_end, self.radii[index]);
            let (world_hit, trace_failed, end) = match trace {
                ProjectileWorldTrace::Clear { end } => (false, false, end),
                ProjectileWorldTrace::Hit { end } => (true, false, end),
                ProjectileWorldTrace::Failed => (true, true, start),
            };
            let sweep = WorldCombatCapsule {
                start,
                end,
                radius: self.radii[index],
            };
            let mut best: Option<(u16, u16)> = None;
            if !trace_failed {
                for target in targets {
                    if target.room != self.rooms[index]
                        || target.target == self.owners[index]
                        || !teams_can_hit(self.teams[index], target.team)
                    {
                        continue;
                    }
                    let Some(phase) =
                        combat_capsule_sweep_contact_fraction_q12(&sweep, &target.hurtbox)
                    else {
                        continue;
                    };
                    if best.is_none_or(|(best_phase, best_target)| {
                        phase < best_phase || (phase == best_phase && target.target < best_target)
                    }) {
                        best = Some((phase, target.target));
                    }
                }
            }

            let resolved = if let Some((phase, target)) = best {
                stats.actor_hits = stats.actor_hits.saturating_add(1);
                Some((
                    ProjectileImpactKind::Target { target },
                    interpolate3_q12(start, end, phase),
                ))
            } else if world_hit {
                stats.world_hits = stats.world_hits.saturating_add(1);
                Some((ProjectileImpactKind::World, end))
            } else {
                None
            };
            if let Some((kind, position)) = resolved {
                let impact = ProjectileImpact {
                    projectile: index.min(u16::MAX as usize) as u16,
                    kind,
                    position,
                    damage: self.damage[index],
                    poise_damage: self.poise_damage[index],
                    team: self.teams[index],
                    owner: self.owners[index],
                };
                if !impacts.push(impact) {
                    stats.dropped_impacts = stats.dropped_impacts.saturating_add(1);
                }
                self.active[index] = 0;
                index += 1;
                continue;
            }

            self.positions[index] = end;
            self.lifetime_ticks[index] = self.lifetime_ticks[index].saturating_sub(1);
            if self.lifetime_ticks[index] == 0 {
                self.active[index] = 0;
                stats.expired = stats.expired.saturating_add(1);
            }
            index += 1;
        }
        stats
    }
}

impl<const N: usize> Default for CombatProjectiles<N> {
    fn default() -> Self {
        Self::new()
    }
}

fn teams_can_hit(source: CombatTeam, target: CombatTeam) -> bool {
    source == CombatTeam::Neutral || source != target
}

fn add3(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [
        a[0].saturating_add(b[0]),
        a[1].saturating_add(b[1]),
        a[2].saturating_add(b[2]),
    ]
}

fn interpolate3_q12(start: [i32; 3], end: [i32; 3], phase_q12: u16) -> [i32; 3] {
    let phase = i32::from(phase_q12.min(4096));
    let interpolate = |a: i32, b: i32| {
        let delta = b.saturating_sub(a);
        let whole = delta / 4096;
        let fraction = delta % 4096;
        a.saturating_add(
            whole
                .saturating_mul(phase)
                .saturating_add(fraction.saturating_mul(phase) / 4096),
        )
    };
    [
        interpolate(start[0], end[0]),
        interpolate(start[1], end[1]),
        interpolate(start[2], end[2]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ClearWorld;

    impl ProjectileWorldTracer for ClearWorld {
        fn trace_projectile(
            &mut self,
            _room: RoomIndex,
            _start: [i32; 3],
            end: [i32; 3],
            _radius: u16,
        ) -> ProjectileWorldTrace {
            ProjectileWorldTrace::Clear { end }
        }
    }

    fn spawn(team: CombatTeam) -> ProjectileSpawn {
        ProjectileSpawn {
            position: [0, 0, 0],
            velocity: [1_000, 0, 0],
            radius: 10,
            damage: 25,
            poise_damage: 9,
            lifetime_ticks: 3,
            room: RoomIndex(2),
            team,
            owner: 7,
            tint_rgb: [20, 200, 255],
        }
    }

    fn target(id: u16, team: CombatTeam, x: i32) -> ProjectileTarget {
        ProjectileTarget {
            target: id,
            team,
            room: RoomIndex(2),
            hurtbox: WorldCombatCapsule {
                start: [x, 0, 0],
                end: [x, 0, 0],
                radius: 40,
            },
        }
    }

    #[test]
    fn swept_projectile_hits_without_tunnelling_and_stops() {
        let mut pool = CombatProjectiles::<2>::new();
        pool.spawn(spawn(CombatTeam::Enemy)).unwrap();
        let mut impacts = ProjectileImpacts::<2>::new();
        let stats = pool.tick(
            &[target(3, CombatTeam::Player, 500)],
            &mut ClearWorld,
            &mut impacts,
        );
        assert_eq!(stats.actor_hits, 1);
        assert!(pool.is_empty());
        assert_eq!(impacts.as_slice().len(), 1);
        assert_eq!(
            impacts.as_slice()[0].kind,
            ProjectileImpactKind::Target { target: 3 }
        );
        assert!((449..=451).contains(&impacts.as_slice()[0].position[0]));
    }

    #[test]
    fn closest_target_wins_and_friendly_owner_are_ignored() {
        let mut pool = CombatProjectiles::<1>::new();
        pool.spawn(spawn(CombatTeam::Enemy)).unwrap();
        let targets = [
            target(4, CombatTeam::Player, 800),
            target(9, CombatTeam::Enemy, 300),
            target(7, CombatTeam::Player, 200),
            target(5, CombatTeam::Player, 500),
        ];
        let mut impacts = ProjectileImpacts::<1>::new();
        pool.tick(&targets, &mut ClearWorld, &mut impacts);
        assert_eq!(
            impacts.as_slice()[0].kind,
            ProjectileImpactKind::Target { target: 5 }
        );
    }

    #[test]
    fn world_clip_blocks_targets_behind_it() {
        struct Wall;
        impl ProjectileWorldTracer for Wall {
            fn trace_projectile(
                &mut self,
                _room: RoomIndex,
                _start: [i32; 3],
                _end: [i32; 3],
                _radius: u16,
            ) -> ProjectileWorldTrace {
                ProjectileWorldTrace::Hit { end: [300, 0, 0] }
            }
        }

        let mut pool = CombatProjectiles::<1>::new();
        pool.spawn(spawn(CombatTeam::Enemy)).unwrap();
        let mut impacts = ProjectileImpacts::<1>::new();
        let stats = pool.tick(
            &[target(3, CombatTeam::Player, 500)],
            &mut Wall,
            &mut impacts,
        );
        assert_eq!(stats.world_hits, 1);
        assert_eq!(impacts.as_slice()[0].kind, ProjectileImpactKind::World);
        assert_eq!(impacts.as_slice()[0].position, [300, 0, 0]);
    }

    #[test]
    fn trace_failure_is_fail_closed() {
        struct Failed;
        impl ProjectileWorldTracer for Failed {
            fn trace_projectile(
                &mut self,
                _room: RoomIndex,
                _start: [i32; 3],
                _end: [i32; 3],
                _radius: u16,
            ) -> ProjectileWorldTrace {
                ProjectileWorldTrace::Failed
            }
        }

        let mut pool = CombatProjectiles::<1>::new();
        pool.spawn(spawn(CombatTeam::Enemy)).unwrap();
        let mut impacts = ProjectileImpacts::<1>::new();
        let stats = pool.tick(
            &[target(3, CombatTeam::Player, 0)],
            &mut Failed,
            &mut impacts,
        );
        assert_eq!(stats.world_hits, 1);
        assert_eq!(stats.actor_hits, 0);
        assert!(pool.is_empty());
        assert_eq!(impacts.as_slice()[0].kind, ProjectileImpactKind::World);
        assert_eq!(impacts.as_slice()[0].position, [0, 0, 0]);
    }

    #[test]
    fn pool_capacity_and_expiry_are_explicit() {
        let mut pool = CombatProjectiles::<1>::new();
        let mut short = spawn(CombatTeam::Enemy);
        short.lifetime_ticks = 1;
        pool.spawn(short).unwrap();
        assert_eq!(pool.spawn(short), Err(ProjectileSpawnError::PoolFull));
        let mut impacts = ProjectileImpacts::<1>::new();
        let stats = pool.tick(&[], &mut ClearWorld, &mut impacts);
        assert_eq!(stats.expired, 1);
        assert!(pool.is_empty());
    }

    #[test]
    fn velocity_toward_is_bounded_and_preserves_direction() {
        assert_eq!(velocity_toward([0; 3], [0; 3], 160), [0; 3]);
        assert_eq!(velocity_toward([0; 3], [1_000, 0, 0], 160), [160, 0, 0]);
        let diagonal = velocity_toward(
            [-1_000_000, 200, -1_000_000],
            [1_000_000, 200, 1_000_000],
            160,
        );
        assert_eq!(diagonal[0], diagonal[2]);
        assert!((112..=114).contains(&diagonal[0]));
        assert_eq!(diagonal[1], 0);
    }
}
