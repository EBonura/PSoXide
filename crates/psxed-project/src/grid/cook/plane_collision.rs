//! Collision records of free planes.
//!
//! Each collidable plane is classified once, from its authored normal, into
//! one of the three shapes `psx_grid::ColPlane` can hold (the meaning of every
//! field is on that type):
//!
//! * normal at least 0.7 up or down: a **platform**, a slab under a sloped
//!   surface over the plane's footprint. Bridges, decks, ramps, stairs made
//!   of tilted planes. The limit is the character motor's walk slope, so a
//!   platform is always something the motor will stand on.
//! * normal at most 0.35 up or down: a vertical **wall** segment of any
//!   direction and zero thickness, between the plane's lowest and highest
//!   corner.
//! * anything between: a **blocker** box around the plane, which over-blocks;
//!   the cook says so.
//!
//! Every record is copied into every sector its footprint touches (the
//! sector's extension names the run), carrying the plane's index as `id`.
//! All geometry is computed from the same rounded runtime corners the render
//! quads use, so what is drawn is what is stood on.

use psx_grid::{plane_kind, ColPlane, COLPLANE_CAMERA_PASS};

use super::units::RuntimeUnits;
use crate::grid::plane::{GridPlane, PlaneCollision, PlaneShape};
use crate::grid::room::GridRoom;

/// Thickness below a platform's surface, runtime units: one floor slab.
pub const PLATFORM_THICKNESS: i16 = 16;

/// The collision of one room's planes.
#[derive(Default)]
pub struct PlaneTables {
    /// One record per collidable plane, `id` set.
    pub records: Vec<ColPlane>,
    /// Per sector (`z * width + x`), indices into `records`.
    pub per_cell: Vec<Vec<usize>>,
    /// Lowest and highest local height over all records.
    pub range: Option<(i32, i32)>,
    /// Findings for the cook report.
    pub warnings: Vec<String>,
}

type P3 = [i32; 3];

fn midpoint(a: P3, b: P3) -> [f64; 3] {
    core::array::from_fn(|i| (f64::from(a[i]) + f64::from(b[i])) / 2.0)
}

fn to_i16(value: i32, what: &str) -> Result<i16, String> {
    i16::try_from(value).map_err(|_| format!("{what} {value} leaves the i16 range of a room"))
}

fn q12(slope: f64) -> Result<i16, String> {
    let value = (slope * 4096.0).round();
    if value.abs() > f64::from(i16::MAX) {
        return Err(format!("plane slope {slope:.2} is out of range"));
    }
    Ok(value as i16)
}

/// Runtime-unit, room-local corners `[TL, TR, BR, BL]`.
fn local_corners(
    units: &RuntimeUnits,
    plane: &GridPlane,
    room: &GridRoom,
    origin: [i32; 3],
) -> [P3; 4] {
    plane.corners().map(|g| {
        [
            units.rt(g[0]) - origin[0],
            units.rt(room.y_offset + g[1]) - origin[1],
            units.rt(g[2]) - origin[2],
        ]
    })
}

pub(super) fn platform(c: &[P3; 4], id: u8, thickness: i16) -> Result<ColPlane, String> {
    let e1 = [c[1][0] - c[0][0], c[1][1] - c[0][1], c[1][2] - c[0][2]].map(f64::from);
    let e3 = [c[3][0] - c[0][0], c[3][1] - c[0][1], c[3][2] - c[0][2]].map(f64::from);
    let det = e1[0] * e3[2] - e1[2] * e3[0];
    if det.abs() < 1.0 {
        return Err("a platform plane with no footprint".to_string());
    }
    let sx = (e1[1] * e3[2] - e3[1] * e1[2]) / det;
    let sz = (e1[0] * e3[1] - e3[0] * e1[1]) / det;
    let (sxq, szq) = (q12(sx)?, q12(sz)?);
    // The offset that centres the fitted surface on the four corners.
    let mut fit = 0.0;
    for corner in c {
        let dx = f64::from(corner[0] - c[0][0]);
        let dz = f64::from(corner[2] - c[0][2]);
        fit += f64::from(corner[1]) - (f64::from(sxq) * dx + f64::from(szq) * dz) / 4096.0;
    }
    let y0 = (fit / 4.0).round() as i32;
    // The runtime tests every footprint edge for `>= 0`, which holds inside
    // when the ring runs the way the cross product of its first two edges
    // turns positive. Reverse it (keeping the first corner, where `y0` is
    // measured) otherwise.
    let ring = {
        let xz = |i: usize| [i64::from(c[i][0]), i64::from(c[i][2])];
        let (a, b, d) = (xz(0), xz(1), xz(2));
        let cross = (b[0] - a[0]) * (d[1] - b[1]) - (b[1] - a[1]) * (d[0] - b[0]);
        if cross >= 0 {
            [0, 1, 2, 3]
        } else {
            [0, 3, 2, 1]
        }
    };
    Ok(ColPlane {
        kind: plane_kind::PLATFORM,
        flags: 0,
        id,
        reserved: 0,
        xz: ring.map(|i| {
            [
                c[i][0].clamp(-32768, 32767) as i16,
                c[i][2].clamp(-32768, 32767) as i16,
            ]
        }),
        y0: to_i16(y0, "platform height")?,
        sx_q12: sxq,
        sz_q12: szq,
        extent: thickness,
    })
}

pub(super) fn wall(c: &[P3; 4], id: u8) -> Result<Option<ColPlane>, String> {
    // The pair of opposite edges with the larger plan separation between
    // their midpoints gives the wall's line (a plane rolled on its side has
    // its width axis upright).
    let across = (midpoint(c[0], c[3]), midpoint(c[1], c[2]));
    let down = (midpoint(c[0], c[1]), midpoint(c[3], c[2]));
    let plan = |(a, b): ([f64; 3], [f64; 3])| (b[0] - a[0]).hypot(b[2] - a[2]);
    let (a, b) = if plan(across) >= plan(down) {
        across
    } else {
        down
    };
    let length = plan((a, b));
    if length < 1.0 {
        return Ok(None);
    }
    let low = c.iter().map(|p| p[1]).min().unwrap_or(0);
    let high = c.iter().map(|p| p[1]).max().unwrap_or(0);
    let r = |v: f64| v.round() as i32;
    Ok(Some(ColPlane {
        kind: plane_kind::WALL,
        flags: 0,
        id,
        reserved: 0,
        xz: [
            [to_i16(r(a[0]), "wall x")?, to_i16(r(a[2]), "wall z")?],
            [to_i16(r(b[0]), "wall x")?, to_i16(r(b[2]), "wall z")?],
            [0; 2],
            [0; 2],
        ],
        y0: to_i16(low, "wall bottom")?,
        sx_q12: q12((b[0] - a[0]) / length)?,
        sz_q12: q12((b[2] - a[2]) / length)?,
        extent: to_i16((high - low).max(1), "wall height")?,
    }))
}

pub(super) fn blocker(c: &[P3; 4], id: u8) -> Result<ColPlane, String> {
    let lo = |i: usize| c.iter().map(|p| p[i]).min().unwrap_or(0);
    let hi = |i: usize| c.iter().map(|p| p[i]).max().unwrap_or(0);
    Ok(ColPlane {
        kind: plane_kind::BLOCKER,
        flags: 0,
        id,
        reserved: 0,
        xz: [
            [to_i16(lo(0), "blocker x")?, to_i16(lo(2), "blocker z")?],
            [to_i16(hi(0), "blocker x")?, to_i16(hi(2), "blocker z")?],
            [0; 2],
            [0; 2],
        ],
        y0: to_i16(lo(1), "blocker bottom")?,
        sx_q12: 0,
        sz_q12: 0,
        extent: to_i16((hi(1) - lo(1)).max(1), "blocker height")?,
    })
}

/// Whether the convex polygon `poly` (plan points) overlaps the closed
/// rectangle `[x0, x1] x [z0, z1]` grown by `margin`.
fn polygon_hits_rect(poly: &[[f64; 2]], rect: [f64; 4], margin: f64) -> bool {
    let (x0, z0, x1, z1) = (
        rect[0] - margin,
        rect[1] - margin,
        rect[2] + margin,
        rect[3] + margin,
    );
    // Axis-aligned separation.
    let (pmin_x, pmax_x) = poly
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p[0]), b.max(p[0])));
    let (pmin_z, pmax_z) = poly
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p[1]), b.max(p[1])));
    if pmax_x < x0 || pmin_x > x1 || pmax_z < z0 || pmin_z > z1 {
        return false;
    }
    // Separation along each polygon edge normal.
    let corners = [[x0, z0], [x1, z0], [x1, z1], [x0, z1]];
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let n = [-(b[1] - a[1]), b[0] - a[0]];
        if n[0] == 0.0 && n[1] == 0.0 {
            continue;
        }
        let project = |p: [f64; 2]| p[0] * n[0] + p[1] * n[1];
        let (lo, hi) = poly
            .iter()
            .map(|p| project(*p))
            .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(v), h.max(v)));
        let (rlo, rhi) = corners
            .iter()
            .map(|p| project(*p))
            .fold((f64::MAX, f64::MIN), |(l, h), v| (l.min(v), h.max(v)));
        if rhi < lo || rlo > hi {
            return false;
        }
    }
    true
}

/// Plan polygon the record occupies, for assigning cells.
fn plan_polygon(record: &ColPlane) -> Vec<[f64; 2]> {
    let pt = |i: usize| [f64::from(record.xz[i][0]), f64::from(record.xz[i][1])];
    match record.kind {
        plane_kind::PLATFORM => (0..4).map(pt).collect(),
        plane_kind::WALL => vec![pt(0), pt(1)],
        _ => {
            let (lo, hi) = (pt(0), pt(1));
            vec![lo, [hi[0], lo[1]], hi, [lo[0], hi[1]]]
        }
    }
}

/// Classify every collidable plane of `room` and place it in its sectors.
///
/// `camera_pass` says whether the camera arm passes through a plane.
pub fn build(
    units: &RuntimeUnits,
    room: &GridRoom,
    room_index: usize,
    origin: [i32; 3],
    camera_pass: &mut dyn FnMut(&GridPlane) -> bool,
) -> Result<PlaneTables, String> {
    build_with_extra(units, room, room_index, origin, camera_pass, Vec::new())
}

/// A collision record made outside the room's plane list (a mesh proxy). Its
/// `id` is set by the build: the ids after the room's planes.
pub struct ExtraRecord {
    /// The record, `id` unset.
    pub record: ColPlane,
    /// Lowest and highest local height the record covers.
    pub range: (i32, i32),
    /// What to call it in errors.
    pub label: String,
}

/// [`build`], then the `extra` records after the planes (ids continue from the
/// room's plane count, so planes and proxies never share an id).
pub fn build_with_extra(
    units: &RuntimeUnits,
    room: &GridRoom,
    room_index: usize,
    origin: [i32; 3],
    camera_pass: &mut dyn FnMut(&GridPlane) -> bool,
    extra: Vec<ExtraRecord>,
) -> Result<PlaneTables, String> {
    let mut tables = PlaneTables {
        per_cell: vec![Vec::new(); room.width * room.depth],
        ..PlaneTables::default()
    };
    for (index, plane) in room.planes.iter().enumerate() {
        let shape = plane.shape();
        if shape == PlaneShape::None {
            continue;
        }
        let at = |what: &str| format!("room {room_index} plane {index}: {what}");
        let id = u8::try_from(index).map_err(|_| at("more than 255 planes"))?;
        let corners = local_corners(units, plane, room, origin);
        if plane.collision == PlaneCollision::Walkable && shape != PlaneShape::Platform {
            tables.warnings.push(format!(
                "grid world: {}",
                at("walkable but too steep to stand on, it blocks instead")
            ));
        }
        let mut record = if shape == PlaneShape::Platform {
            platform(&corners, id, PLATFORM_THICKNESS).map_err(|e| at(&e))?
        } else if shape == PlaneShape::Wall {
            match wall(&corners, id).map_err(|e| at(&e))? {
                Some(record) => record,
                None => {
                    tables
                        .warnings
                        .push(format!("grid world: {}", at("edge-on, no collision")));
                    continue;
                }
            }
        } else {
            tables.warnings.push(format!(
                "grid world: {}",
                at("leans too far for a wall and not flat enough for a platform; it blocks as a box")
            ));
            blocker(&corners, id).map_err(|e| at(&e))?
        };
        if camera_pass(plane) {
            record.flags |= COLPLANE_CAMERA_PASS;
        }
        let range = match record.kind {
            plane_kind::PLATFORM => {
                let ys = corners.iter().map(|c| c[1]);
                (
                    ys.clone().min().unwrap_or(0) - i32::from(record.extent),
                    ys.max().unwrap_or(0),
                )
            }
            _ => (
                i32::from(record.y0),
                i32::from(record.y0) + i32::from(record.extent),
            ),
        };
        place_record(&mut tables, units, room, record, range, &at)?;
    }
    let first_extra = room.planes.len();
    for (k, item) in extra.into_iter().enumerate() {
        let at = |what: &str| format!("room {room_index} {}: {what}", item.label);
        let id = u8::try_from(first_extra + k)
            .map_err(|_| at("more than 255 collision records in the room"))?;
        let mut record = item.record;
        record.id = id;
        place_record(&mut tables, units, room, record, item.range, &at)?;
    }
    for cell in &tables.per_cell {
        if cell.len() > usize::from(u8::MAX) {
            return Err(format!(
                "room {room_index}: {} planes share one sector, at most 255",
                cell.len()
            ));
        }
    }
    Ok(tables)
}

/// Add `record` to `tables`: into every sector its footprint touches, and
/// into the height range.
fn place_record(
    tables: &mut PlaneTables,
    units: &RuntimeUnits,
    room: &GridRoom,
    record: ColPlane,
    (lo, hi): (i32, i32),
    at: &dyn Fn(&str) -> String,
) -> Result<(), String> {
    let s = f64::from(units.sector());
    let polygon = plan_polygon(&record);
    let mut placed = 0usize;
    for cz in 0..room.depth {
        for cx in 0..room.width {
            let rect = [
                cx as f64 * s,
                cz as f64 * s,
                (cx + 1) as f64 * s,
                (cz + 1) as f64 * s,
            ];
            if polygon_hits_rect(&polygon, rect, 1.0) {
                tables.per_cell[cz * room.width + cx].push(tables.records.len());
                placed += 1;
            }
        }
    }
    if placed == 0 {
        return Err(at("lies outside the room"));
    }
    tables.range = Some(match tables.range {
        Some((a, b)) => (a.min(lo), b.max(hi)),
        None => (lo, hi),
    });
    tables.records.push(record);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::units::GridScale;

    fn units() -> RuntimeUnits {
        RuntimeUnits::new(GridScale { num: 5, den: 4 }).unwrap()
    }

    fn cooked(plane: GridPlane) -> PlaneTables {
        let mut room = GridRoom::new("r", 8, 8);
        room.planes.push(plane);
        build(&units(), &room, 0, [0; 3], &mut |p| {
            p.camera_pass == Some(true)
        })
        .unwrap()
    }

    fn walkable(center: [i32; 3], size: [i32; 2], rotation: [i32; 3]) -> GridPlane {
        let mut plane = GridPlane::new(center, size);
        plane.rotation = rotation;
        plane.collision = PlaneCollision::Walkable;
        plane
    }

    #[test]
    fn a_flat_walkable_plane_is_a_level_platform_over_its_cells() {
        // 2 by 1 sectors flat at 256 grid units (20 runtime units).
        let tables = cooked(walkable([2048, 256, 1536], [2048, 1024], [0, 90, 0]));
        let r = tables.records[0];
        assert_eq!(r.kind, plane_kind::PLATFORM);
        assert_eq!((r.y0, r.sx_q12, r.sz_q12), (20, 0, 0));
        let cells: Vec<usize> = tables
            .per_cell
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_empty())
            .map(|(i, _)| i)
            .collect();
        // x from 1024 to 3072 grid units is sectors 1 and 2, z 1024..2048 is
        // sector 1, plus the touching neighbours the one unit margin adds.
        assert!(cells.contains(&(8 + 1)) && cells.contains(&(8 + 2)));
        assert!(!cells.contains(&(8 * 5 + 1)));
    }

    #[test]
    fn a_tilted_plane_gets_its_slope() {
        // Pitch 90 - 14 degrees: rises along the plane's up axis.
        let tables = cooked(walkable([2048, 512, 2048], [2048, 2048], [0, 76, 0]));
        let r = tables.records[0];
        assert_eq!(r.kind, plane_kind::PLATFORM);
        // The up axis points toward -z, so the surface falls with +z: sz < 0.
        assert!(r.sz_q12 < 0 && r.sx_q12 == 0, "{r:?}");
        // tan(14 degrees) is 0.249.
        assert!((i32::from(-r.sz_q12) - 1021).abs() < 40, "{r:?}");
    }

    #[test]
    fn a_vertical_plane_is_a_wall_along_its_width() {
        let mut plane = GridPlane::new([2048, 1024, 2048], [2048, 2048]);
        plane.rotation = [30, 0, 0];
        plane.collision = PlaneCollision::Solid;
        let tables = cooked(plane);
        let r = tables.records[0];
        assert_eq!(r.kind, plane_kind::WALL);
        assert_eq!(r.extent, 160, "2048 grid units tall is 160 runtime units");
        // Direction is unit length in Q12.
        let len2 = i32::from(r.sx_q12).pow(2) + i32::from(r.sz_q12).pow(2);
        assert!((len2 - 4096 * 4096).abs() < 4096 * 8, "{r:?}");
    }

    #[test]
    fn a_plane_rolled_onto_its_side_still_gets_a_wall_line() {
        let mut plane = GridPlane::new([2048, 1024, 2048], [1024, 2048]);
        plane.rotation = [0, 0, 90];
        plane.collision = PlaneCollision::Solid;
        let r = cooked(plane).records[0];
        assert_eq!(r.kind, plane_kind::WALL);
        assert!(r.sx_q12.abs() > 4000, "{r:?}");
    }

    #[test]
    fn a_middling_lean_blocks_as_a_box_and_a_steep_walkable_warns() {
        let mut plane = GridPlane::new([2048, 1024, 2048], [1024, 1024]);
        plane.rotation = [0, 30, 0];
        plane.collision = PlaneCollision::Walkable;
        let tables = cooked(plane);
        assert_eq!(tables.records[0].kind, plane_kind::BLOCKER);
        assert_eq!(tables.warnings.len(), 2, "{:?}", tables.warnings);
    }

    #[test]
    fn decor_and_disabled_planes_cook_nothing() {
        let mut off = walkable([2048, 0, 2048], [1024, 1024], [0, 90, 0]);
        off.enabled = false;
        assert!(cooked(off).records.is_empty());
        let decor = GridPlane::new([2048, 0, 2048], [1024, 1024]);
        assert!(cooked(decor).records.is_empty());
    }

    #[test]
    fn the_record_matches_the_rendered_corners() {
        // Whatever the rotation, the platform surface evaluated at each
        // rounded corner is within one unit of the corner a quad draws.
        for rotation in [[0, 90, 0], [37, 80, 0], [200, 100, 0], [-60, 85, 20]] {
            let plane = walkable([4096, 512, 4096], [3072, 2048], rotation);
            let tables = cooked(plane.clone());
            let r = tables.records[0];
            assert_eq!(r.kind, plane_kind::PLATFORM, "{rotation:?}");
            for corner in local_corners(&units(), &plane, &GridRoom::new("r", 8, 8), [0; 3]) {
                let dx = corner[0] - i32::from(r.xz[0][0]);
                let dz = corner[2] - i32::from(r.xz[0][1]);
                let y =
                    i32::from(r.y0) + ((i32::from(r.sx_q12) * dx + i32::from(r.sz_q12) * dz) >> 12);
                assert!(
                    (y - corner[1]).abs() <= 2,
                    "{rotation:?}: {y} vs {corner:?}"
                );
            }
        }
    }
}
