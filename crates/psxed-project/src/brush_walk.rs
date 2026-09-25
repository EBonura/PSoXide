//! Player-hull traversal proof against the cooked world.
//!
//! Blockout practice says to validate reachability while changes are still
//! cheap, and nothing here could answer "can the player actually get through
//! that doorway" short of building a disc and playing it for three minutes.
//!
//! This walks the engine's own collision, not an approximation of it:
//! `commit_body_step_with_trace_provider` against a resident PXBSP, which is
//! the same call `psx-game-runtime` makes to move a character. A gap it
//! refuses is a gap the game refuses.
//!
//! Coordinates in and out are AUTHORED units. The cook divides every length
//! by [`crate::units::WORLD_UNIT_DIVISOR`], so the trace runs in engine units
//! and the conversion happens here rather than in every caller.

use psx_bsp::collision::TraceScratch;
use psx_bsp::collision_provider::{select_body_hull, PxbspCollisionProvider};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::SliceReader;
use psx_engine::{
    commit_body_step_with_trace_provider, CharacterBlockerTraceProvider, CollisionTraceShape,
    RoomPoint,
};

use crate::playtest::{PlaytestPackage, PlaytestWorldGeometry};
use crate::units::WORLD_UNIT_DIVISOR;

/// One leg of a walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkStep {
    /// Where the hull ended up, in authored units.
    pub position: [i32; 3],
    /// Whether the hull covered the whole leg it was asked to.
    pub reached: bool,
}

/// Outcome of walking the player hull along a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkResult {
    /// Where the hull came to rest, in authored units.
    pub final_position: [i32; 3],
    /// Legs completed before it stopped making progress.
    pub legs_completed: usize,
    /// Every leg attempted.
    pub steps: Vec<WalkStep>,
    /// Whether the whole path was traversed.
    pub reached_end: bool,
}

/// Authored length to engine length, the way the cook does it.
fn to_engine(value: i32) -> i32 {
    value.div_euclid(WORLD_UNIT_DIVISOR)
}

/// Engine length back to authored.
fn to_authored(value: i32) -> i32 {
    value.saturating_mul(WORLD_UNIT_DIVISOR)
}

/// Walk the player hull through `waypoints`, in authored units.
///
/// Each leg is committed as one motor step, exactly as the runtime moves a
/// character, so step-ups, wall slides and floor snapping all apply. Legs are
/// subdivided because a single enormous step would tunnel: the motor is built
/// for per-tick movement, not teleportation.
pub fn walk_player_hull(
    package: &PlaytestPackage,
    waypoints: &[[i32; 3]],
    radius_authored: i32,
    height_authored: i32,
    leg_authored: i32,
) -> Result<WalkResult, String> {
    let PlaytestWorldGeometry::Pxbsp(world) = &package.world_geometry else {
        return Err("this project did not cook to a PXBSP world, so there is nothing to walk on"
            .to_string());
    };
    if waypoints.len() < 2 {
        return Err("give at least a start and an end waypoint".to_string());
    }
    let mut map = PxbspResidentMap::with_capacity(world.bytes.len());
    map.load(0, &mut SliceReader::new(&world.bytes))
        .map_err(|error| format!("load the cooked world for tracing: {error}"))?;

    let radius = to_engine(radius_authored).max(1);
    let height = to_engine(height_authored).max(1);
    let hull = select_body_hull(&world.body_hulls, radius, height).ok_or_else(|| {
        format!(
            "no cooked body hull fits a {radius_authored}x{height_authored} authored body; \
             the cook ships hulls sized for the authored characters"
        )
    })?;
    let leg = to_engine(leg_authored).max(1);

    let start = waypoints[0];
    let mut position = RoomPoint::new(
        to_engine(start[0]),
        to_engine(start[1]),
        to_engine(start[2]),
    );
    let mut steps = Vec::new();
    let mut legs_completed = 0usize;
    let mut reached_end = true;

    for target in &waypoints[1..] {
        let goal = RoomPoint::new(
            to_engine(target[0]),
            to_engine(target[1]),
            to_engine(target[2]),
        );
        let mut leg_reached = false;
        // Bounded: a path that stops making progress must end the walk rather
        // than spin. The cap is generous enough for a long corridor.
        for _ in 0..4096 {
            let dx = goal.x.saturating_sub(position.x);
            let dz = goal.z.saturating_sub(position.z);
            if dx == 0 && dz == 0 {
                leg_reached = true;
                break;
            }
            let (step_x, step_z) = clamp_step(dx, dz, leg);
            let before = position;
            let mut scratch = TraceScratch::new();
            let shape = CollisionTraceShape::Body { radius, height };
            let mut provider =
                PxbspCollisionProvider::new(&map, hull, &[], shape, &mut scratch)
                    .ok_or("the cooked world would not open a collision provider")?;
            let mut composed =
                CharacterBlockerTraceProvider::new_with_aabbs(&mut provider, &[], &[]);
            let outcome = commit_body_step_with_trace_provider(
                &mut composed, position, step_x, step_z, radius, height,
            )
            .map_err(|error| format!("the collision trace failed: {error:?}"))?;
            position = outcome.position;
            if position.x == before.x && position.z == before.z {
                // Blocked: the motor refused to move us at all.
                break;
            }
        }
        steps.push(WalkStep {
            position: [
                to_authored(position.x),
                to_authored(position.y),
                to_authored(position.z),
            ],
            reached: leg_reached,
        });
        if leg_reached {
            legs_completed += 1;
        } else {
            reached_end = false;
            break;
        }
    }

    Ok(WalkResult {
        final_position: [
            to_authored(position.x),
            to_authored(position.y),
            to_authored(position.z),
        ],
        legs_completed,
        steps,
        reached_end,
    })
}

/// Shorten a delta to at most `leg` units, keeping its direction.
fn clamp_step(dx: i32, dz: i32, leg: i32) -> (i32, i32) {
    let distance = ((i64::from(dx) * i64::from(dx) + i64::from(dz) * i64::from(dz)) as f64).sqrt();
    if distance <= f64::from(leg) || distance == 0.0 {
        return (dx, dz);
    }
    let scale = f64::from(leg) / distance;
    (
        (f64::from(dx) * scale).round() as i32,
        (f64::from(dz) * scale).round() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step clamping keeps direction and never overshoots. This is the only
    /// part of the walk that is pure arithmetic; the trace itself needs a
    /// cooked world with real textures, so it is proven live against a
    /// project instead (a wall must block, open floor must not).
    #[test]
    fn steps_are_clamped_without_changing_direction() {
        assert_eq!(clamp_step(10, 0, 100), (10, 0), "short legs pass through");
        assert_eq!(clamp_step(1000, 0, 100), (100, 0));
        // 3-4-5 triangle: the clamped vector keeps the ratio.
        assert_eq!(clamp_step(300, 400, 50), (30, 40));
        assert_eq!(clamp_step(0, 0, 50), (0, 0), "a zero delta stays zero");
        // Authored/engine conversion is the cook's divisor, both ways.
        assert_eq!(to_engine(1024), 64);
        assert_eq!(to_authored(64), 1024);
        // Negative authored coordinates must floor consistently, or a hull
        // starting west of the origin lands a unit off.
        assert_eq!(to_engine(-1024), -64);
    }
}
