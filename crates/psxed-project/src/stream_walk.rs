//! Route walkability (M8): can the player body walk the generated route set
//! through the cooked, streamed world?
//!
//! The generator writes every route as a polyline of engine-unit waypoints
//! that goes round the vestibule baffles. A polyline that looks fine on the
//! module grid can still be blocked once the world is cooked (a clipped wall,
//! a cut that shaved a door, a floor patch dropped as unreachable). So every
//! cooked stress world is checked here: install every region into a resident
//! map and trace the player hull along each segment; a route is walkable when
//! every waypoint is in open space and no segment touches a wall.

use psx_bsp::collision::{Trace, TraceScratch, CONTENTS_EMPTY};
use psx_bsp::pxbsp_resident::stream::RegionLoader;
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};

use crate::brush_world::stream_cook::StreamedBrushWorld;
use crate::stream_world::RouteSet;

/// Player body hull index in a model's collision hulls.
const PLAYER_HULL: usize = 1;
const Q: i32 = 4096;

/// One place a route stops being walkable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub route: String,
    /// Waypoint the segment starts at.
    pub segment: usize,
    pub from: [i32; 3],
    pub to: [i32; 3],
    /// `None`: the waypoint itself is inside a wall. `Some(f)`: the trace
    /// stopped at `f` in Q12 (4096 is the whole segment); -1 when the trace
    /// itself failed.
    pub fraction: Option<i32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalkReport {
    pub routes: usize,
    pub waypoints: usize,
    pub segments: usize,
    /// Walker height the routes were traced at, units.
    pub height: i32,
    /// Blocked pieces on door passages: the legs between the points either
    /// side of a door, which the generator places clear of the baffles.
    pub blocked: Vec<Blocked>,
    /// Blocked legs that start or end at a module centre. Interior detail may
    /// stand there; the walker's motor slides round it, so they are reported
    /// and do not fail a walk.
    pub blocked_at_centres: usize,
    /// Waypoints inside a wall (not counting module centres).
    pub waypoints_in_walls: usize,
}

impl WalkReport {
    pub fn walkable(&self) -> bool {
        self.blocked.is_empty()
    }
}

fn point(p: [i32; 3]) -> Vec3I32 {
    Vec3I32 {
        x: p[0].saturating_mul(Q),
        y: p[1].saturating_mul(Q),
        z: p[2].saturating_mul(Q),
    }
}

/// Slots of the resident map a walk uses. A map cannot hold every region of a
/// stress world (the slot geometry would overflow a wire index), so the walk
/// keeps only the regions a segment passes through resident, as the guest does.
const WALK_SLOTS: u16 = 32;
/// Lateral reach of the player hull plus a margin, units: regions this close
/// to the segment are resident too, so the hull never meets an unloaded wall.
const REACH: i32 = 48;
/// Spacing of the samples that find the regions a segment passes, units.
const STEP: i32 = 16;
/// Length of one traced piece of a segment, units: short, so the walker follows
/// a slope the way the motor steps up it.
const PIECE: i32 = 16;
/// Where a ground probe starts: above the stress world's terrain, below its roofs.
const GROUND_TOP: i32 = 160;

fn load(world: &StreamedBrushWorld, slots: u16) -> Result<PxbspResidentMap, String> {
    let mut map = PxbspResidentMap::with_capacity(world.container.len() * 8 + (1 << 22));
    map.load_streamed(1, &mut SliceReader::new(&world.container), slots)
        .map_err(|e| format!("streamed container: {e:?}"))?;
    Ok(map)
}

/// The regions within [`REACH`] of the segment `a` to `b`, found on a map with
/// nothing installed (every region reads unresident there).
fn regions_near(locator: &PxbspResidentMap, a: [i32; 3], b: [i32; 3]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::new();
    let len = (0..3).map(|i| (b[i] - a[i]).abs()).max().unwrap_or(0);
    let steps = (len / STEP).max(1);
    for k in 0..=steps {
        let at = [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * k / steps);
        for (dx, dz) in [(0, 0), (REACH, 0), (-REACH, 0), (0, REACH), (0, -REACH)] {
            let p = point([at[0] + dx, at[1], at[2] + dz]);
            if let Some(r) = locator.unresident_region_at(p) {
                if !out.contains(&r) {
                    out.push(r);
                }
            }
        }
    }
    out
}

/// Height of the floor under `(x, z)` for the player hull: a trace down from
/// above the tallest terrain, 0 when nothing is hit. Needs the regions near
/// the point resident.
fn ground(map: &PxbspResidentMap, scratch: &mut TraceScratch, x: i32, z: i32) -> i32 {
    let Some(hull) = map.model_collision_hull(0, PLAYER_HULL) else {
        return 0;
    };
    let mut trace = Trace::default();
    let top = point([x, GROUND_TOP, z]);
    let bottom = point([x, -64, z]);
    if hull.trace_into(&top, &bottom, scratch, &mut trace)
        && trace.fraction < Q
        && !trace.start_solid.is_set()
    {
        trace.end.y >> 12
    } else {
        0
    }
}

/// Walk every route of `routes` through the cooked `world`.
///
/// Routes carry floor-level waypoints (y 0) and the spawn (y of
/// `player_start`); the walker's height is its spawn height throughout, since
/// the floors of the stress world are flat at 0.
pub fn check_routes(world: &StreamedBrushWorld, routes: &RouteSet) -> Result<WalkReport, String> {
    // Most slots the map's wire indices address (the loader's own check).
    let (caps, top) = (world.index.caps, world.index.top);
    let fits = |n: usize| {
        let reach = |t: u16, cap: u16| usize::from(t) + n * usize::from(cap);
        reach(top.planes, caps.planes) <= i16::MAX as usize
            && reach(top.nodes, caps.nodes) <= i16::MAX as usize
            && reach(top.clip_nodes, caps.clip_nodes) <= i16::MAX as usize
            && reach(top.leaves, caps.leaves) <= i16::MAX as usize
            && reach(top.faces, caps.faces) <= u16::MAX as usize
            && reach(top.vertices, caps.vertices) <= u16::MAX as usize
            && reach(top.marks, caps.marks) <= u16::MAX as usize
    };
    let slots = (1..=WALK_SLOTS as usize)
        .rev()
        .find(|&n| fits(n))
        .unwrap_or(1)
        .min(world.index.regions.len()) as u16;
    let locator = load(world, 1)?;
    let mut map = load(world, slots)?;
    let mut loader = RegionLoader::new(SliceReader::new(&world.region_pack), 0);
    let mut buf = vec![0u8; world.region_pack.len().max(4096)];
    let mut report = WalkReport::default();
    let mut scratch = TraceScratch::new();
    // Make exactly `need` resident (evicting what is not needed when full).
    let mut make_resident = |map: &mut PxbspResidentMap, need: &[u16]| -> Result<(), String> {
        for &r in need {
            if map.streaming().is_some_and(|s| s.is_resident(r)) {
                continue;
            }
            if map.streaming().and_then(|s| s.free_slot()).is_none() {
                let victim = (0..world.index.regions.len() as u16)
                    .find(|v| {
                        !need.contains(v) && map.streaming().is_some_and(|s| s.is_resident(*v))
                    })
                    .ok_or("every slot holds a needed region")?;
                map.uninstall_region(victim)
                    .map_err(|e| format!("uninstall {victim}: {e:?}"))?;
            }
            let n = loader
                .load(&world.index, r, &mut buf)
                .map_err(|e| format!("region {r}: {e:?}"))?;
            let slot = map
                .streaming()
                .and_then(|s| s.free_slot())
                .ok_or_else(|| format!("no free slot for region {r}"))?;
            map.install_region(r, slot, &buf[..n])
                .map_err(|e| format!("install {r}: {e:?}"))?;
        }
        Ok(())
    };
    let height = routes.player_start[1];
    report.height = height;
    let level = |p: [i32; 3]| [p[0], height, p[2]];
    let half = routes.module_units / 2;
    let centres: Vec<[i32; 2]> = routes
        .modules
        .iter()
        .map(|m| [m.x + half, m.z + half])
        .collect();
    let is_centre = |p: [i32; 3]| centres.contains(&[p[0], p[2]]);
    for route in &routes.routes {
        report.routes += 1;
        report.waypoints += route.points.len();
        let mut points: Vec<[i32; 3]> = Vec::with_capacity(route.points.len());
        for p in &route.points {
            let flat = level(*p);
            make_resident(&mut map, &regions_near(&locator, flat, flat))?;
            points.push([p[0], ground(&map, &mut scratch, p[0], p[2]) + height, p[2]]);
        }
        for (i, p) in points.iter().enumerate() {
            make_resident(&mut map, &regions_near(&locator, *p, *p))?;
            let hull = map
                .model_collision_hull(0, PLAYER_HULL)
                .ok_or("no player hull")?;
            if hull.point_contents(point(*p)) != Some(CONTENTS_EMPTY) && !is_centre(*p) {
                report.waypoints_in_walls += 1;
                report.blocked.push(Blocked {
                    route: route.name.clone(),
                    segment: i,
                    from: *p,
                    to: *p,
                    fraction: None,
                });
            }
        }
        for (i, pair) in points.windows(2).enumerate() {
            report.segments += 1;
            // Trace in pieces so only the regions near a piece need be resident.
            let span = (0..3)
                .map(|k| (pair[1][k] - pair[0][k]).abs())
                .max()
                .unwrap_or(0);
            let pieces = (span / PIECE).max(1);
            let at =
                |k: i32| [0, 1, 2].map(|c| pair[0][c] + (pair[1][c] - pair[0][c]) * k / pieces);
            for k in 0..pieces {
                let (mut from, mut to) = (at(k), at(k + 1));
                make_resident(&mut map, &regions_near(&locator, from, to))?;
                from[1] = ground(&map, &mut scratch, from[0], from[2]) + height;
                to[1] = ground(&map, &mut scratch, to[0], to[2]) + height;
                let hull = map
                    .model_collision_hull(0, PLAYER_HULL)
                    .ok_or("no player hull")?;
                let mut trace = Trace::default();
                let ok = hull.trace_into(&point(from), &point(to), &mut scratch, &mut trace);
                if !ok {
                    // Malformed hull or scratch overflow, not a wall.
                    trace.fraction = -1;
                }
                if trace.fraction < Q || trace.start_solid.is_set() || trace.all_solid.is_set() {
                    if is_centre(pair[0]) || is_centre(pair[1]) {
                        report.blocked_at_centres += 1;
                    } else {
                        report.blocked.push(Blocked {
                            route: route.name.clone(),
                            segment: i,
                            from,
                            to,
                            fraction: Some(trace.fraction),
                        });
                    }
                    break;
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_region::PartitionParams;
    use crate::brush_world::stream_cook::{cook_project_streamed, CookedWorld};
    use crate::brush_world::BrushWorldCookMode;
    use crate::stream_world::{generate, load_donor, StreamWorldConfig};
    use std::path::PathBuf;

    fn projects_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects")
    }

    #[test]
    fn small_stress_world_routes_are_walkable() {
        let donor = load_donor(&projects_dir()).expect("donor");
        let generated = generate(&StreamWorldConfig::small([4, 4]), &donor).expect("world");
        let root = projects_dir().join("graybox-reach");
        let mut params = PartitionParams::default();
        generated.overrides.apply(&mut params);
        let CookedWorld::Streamed(world) = cook_project_streamed(
            &generated.project,
            &root,
            BrushWorldCookMode::Draft,
            [0; 3],
            &params,
        )
        .expect("cook") else {
            panic!("expected more than one region");
        };
        let report = check_routes(&world, &generated.routes).expect("walk");
        assert!(report.segments > 10);
        assert!(
            report.walkable(),
            "{} of {} door-passage legs blocked, first {:?}",
            report.blocked.len(),
            report.segments,
            report.blocked.first()
        );
    }
}
