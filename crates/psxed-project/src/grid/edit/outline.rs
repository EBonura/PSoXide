//! Organic outlines: filling a polygon or a winding corridor with floors and
//! turning the staircase of its outline into 45 degree lines.
//!
//! A footprint of whole sectors can only have axis-aligned edges. A convex
//! staircase corner (a cell with exactly two adjacent neighbours missing) is
//! replaced by a diagonal: the outer triangle of the cell is voided (so the
//! other half closes itself with a diagonal wall) or, for a plateau edge,
//! cracked down to the level around it. For a run of unit steps the
//! diagonals of neighbouring cells meet corner to corner and line up into one
//! 45 degree wall. Steps wider than one sector stay steps: a 1:2 line needs
//! the finer quadtree cells.

use std::collections::BTreeSet;

use crate::ResourceId;

use super::super::surface::{GridSplit, GridSurface, VOID_A, VOID_B};
use super::super::world::GridWorld;
use super::natural::make_split_level;
use super::rects::{create_surfaces, CellRect, RectOutcome, RectRefusal};
use super::refs::SurfaceKind;

/// The corner of a cell that sticks out into the missing cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OuterCorner {
    /// North-east: the north and east neighbours are missing.
    Ne,
    /// South-east.
    Se,
    /// South-west.
    Sw,
    /// North-west.
    Nw,
}

impl OuterCorner {
    /// The diagonal that cuts this corner off.
    pub const fn split(self) -> GridSplit {
        match self {
            Self::Ne | Self::Sw => GridSplit::NwSe,
            Self::Se | Self::Nw => GridSplit::NeSw,
        }
    }

    /// The triangle (`0` or `1`) of that split that holds the corner.
    pub const fn triangle(self) -> usize {
        match self {
            Self::Ne | Self::Nw => 0,
            Self::Se | Self::Sw => 1,
        }
    }

    /// The cells diagonally beyond the corner, for the outline test.
    const fn beyond(self) -> [i32; 2] {
        match self {
            Self::Ne => [1, -1],
            Self::Se => [1, 1],
            Self::Sw => [-1, 1],
            Self::Nw => [-1, -1],
        }
    }
}

/// The outer corner of `cell` in a footprint, when it is a convex staircase
/// corner: exactly two adjacent cardinal neighbours are missing, the other two
/// are there, and so is nothing diagonally beyond the corner.
pub fn outer_corner(present: &dyn Fn([i32; 2]) -> bool, cell: [i32; 2]) -> Option<OuterCorner> {
    let at = |dx: i32, dz: i32| present([cell[0] + dx, cell[1] + dz]);
    let (n, e, s, w) = (at(0, -1), at(1, 0), at(0, 1), at(-1, 0));
    let corner = match (n, e, s, w) {
        (false, false, true, true) => OuterCorner::Ne,
        (true, false, false, true) => OuterCorner::Se,
        (true, true, false, false) => OuterCorner::Sw,
        (false, true, true, false) => OuterCorner::Nw,
        _ => return None,
    };
    let [dx, dz] = corner.beyond();
    (!at(dx, dz)).then_some(corner)
}

/// What to do with the outer triangle of a corner cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerCut {
    /// Void it (and the ceiling's): the cell keeps half, closed by a
    /// diagonal wall.
    Void,
    /// Crack the floor's outer triangle down by this many grid units: a
    /// plateau corner cut at 45 degrees with a diagonal riser.
    Crack {
        /// How far the outer triangle drops.
        drop: i32,
    },
}

/// What [`cut_corners`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutReport {
    /// Corner cells cut.
    pub cut: usize,
    /// Convex corners left alone: a split-level surface with another diagonal.
    pub skipped: usize,
}

fn has_space(world: &GridWorld, room: usize, cell: [i32; 2]) -> bool {
    let Some(r) = world.rooms.get(room) else {
        return false;
    };
    r.local(cell)
        .and_then(|[x, z]| r.sector(x, z))
        .is_some_and(|s| {
            s.ground_floor().is_some_and(|f| f.void != VOID_A | VOID_B)
                || s.ground_ceiling().is_some()
        })
}

/// Cut the convex staircase corners among `cells` of `room`. The outline is
/// the room's own floors, or `footprint` when given (a region's cells, for
/// the Raise tool's plateau corners).
pub fn cut_corners(
    world: &mut GridWorld,
    room: usize,
    cells: &[[i32; 2]],
    footprint: Option<&BTreeSet<[i32; 2]>>,
    cut: CornerCut,
) -> CutReport {
    let mut corners: Vec<([i32; 2], OuterCorner)> = Vec::new();
    {
        let present = |c: [i32; 2]| match footprint {
            Some(set) => set.contains(&c),
            None => has_space(world, room, c),
        };
        for &cell in cells {
            if let Some(corner) = outer_corner(&present, cell) {
                corners.push((cell, corner));
            }
        }
    }
    let mut report = CutReport::default();
    let Some(r) = world.rooms.get_mut(room) else {
        return report;
    };
    for (cell, corner) in corners {
        let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) else {
            continue;
        };
        let mut done = false;
        let mut surfaces: Vec<&mut GridSurface> = Vec::new();
        match cut {
            CornerCut::Void => {
                let (floor, ceiling) = sector.ground_floor_and_ceiling_mut();
                surfaces.extend(floor);
                surfaces.extend(ceiling);
            }
            CornerCut::Crack { .. } => surfaces.extend(sector.ground_floor_mut()),
        }
        for surface in surfaces {
            // A split-level surface keeps the diagonal it has; the corner is
            // only cut if that already is the right one.
            if surface.heights_b.is_some() && surface.split != corner.split() {
                report.skipped += 1;
                continue;
            }
            surface.split = corner.split();
            surface.auto_split = false;
            match cut {
                CornerCut::Void => {
                    surface.void |= if corner.triangle() == 0 {
                        VOID_A
                    } else {
                        VOID_B
                    }
                }
                CornerCut::Crack { drop } => {
                    make_split_level(surface);
                    let tri = corner.triangle();
                    let heights = if tri == 0 {
                        &mut surface.heights
                    } else {
                        surface.heights_b.get_or_insert(surface.heights)
                    };
                    for h in heights.iter_mut() {
                        *h = h.saturating_sub(drop);
                    }
                }
            }
            done = true;
        }
        report.cut += usize::from(done);
    }
    report
}

/// Bring back every void triangle among `cells`. Returns surfaces changed.
pub fn restore_voids(world: &mut GridWorld, room: usize, cells: &[[i32; 2]]) -> usize {
    let Some(r) = world.rooms.get_mut(room) else {
        return 0;
    };
    let mut changed = 0;
    for &cell in cells {
        let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) else {
            continue;
        };
        let (floor, ceiling) = sector.ground_floor_and_ceiling_mut();
        for surface in [floor, ceiling].into_iter().flatten() {
            changed += usize::from(std::mem::take(&mut surface.void) != 0);
        }
    }
    changed
}

/// The cells whose centres lie inside the polygon, in sector coordinates
/// (`[x, z]`, an integer is a lattice corner so cell `(i, j)` has its centre
/// at `(i + 0.5, j + 0.5)`). Even-odd rule; fewer than three points make an
/// empty set.
pub fn polygon_cells(points: &[[f64; 2]]) -> BTreeSet<[i32; 2]> {
    let mut out = BTreeSet::new();
    if points.len() < 3 {
        return out;
    }
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in points {
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    for z in lo[1].floor() as i32..=hi[1].ceil() as i32 {
        let pz = f64::from(z) + 0.5;
        // Crossings of the row `pz` with the polygon's edges.
        let mut xs: Vec<f64> = Vec::new();
        for i in 0..points.len() {
            let (a, b) = (points[i], points[(i + 1) % points.len()]);
            if (a[1] <= pz) != (b[1] <= pz) {
                xs.push(a[0] + (pz - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
            }
        }
        xs.sort_by(f64::total_cmp);
        for pair in xs.chunks_exact(2) {
            let first = (pair[0] - 0.5).ceil() as i32;
            let last = (pair[1] - 0.5).floor() as i32;
            for x in first..=last {
                // Centre `x + 0.5` strictly between the crossings.
                let cx = f64::from(x) + 0.5;
                if cx > pair[0] && cx < pair[1] {
                    out.insert([x, z]);
                }
            }
        }
    }
    out
}

/// The cells whose centres are within `width / 2` sectors of the polyline
/// through `points` (sector coordinates, as [`polygon_cells`]).
pub fn corridor_cells(points: &[[f64; 2]], width: f64) -> BTreeSet<[i32; 2]> {
    let mut out = BTreeSet::new();
    let half = (width / 2.0).max(0.0);
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for p in points {
        for a in 0..2 {
            lo[a] = lo[a].min(p[a] - half);
            hi[a] = hi[a].max(p[a] + half);
        }
    }
    if points.is_empty() {
        return out;
    }
    let dist2 = |p: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dz * dz;
        let t = if len2 == 0.0 {
            0.0
        } else {
            (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
        };
        let (qx, qz) = (a[0] + t * dx, a[1] + t * dz);
        (p[0] - qx).powi(2) + (p[1] - qz).powi(2)
    };
    for z in lo[1].floor() as i32..=hi[1].ceil() as i32 {
        for x in lo[0].floor() as i32..=hi[0].ceil() as i32 {
            let c = [f64::from(x) + 0.5, f64::from(z) + 0.5];
            let near = if points.len() == 1 {
                dist2(c, points[0], points[0])
            } else {
                points
                    .windows(2)
                    .map(|w| dist2(c, w[0], w[1]))
                    .fold(f64::MAX, f64::min)
            };
            if near <= half * half + 1e-9 {
                out.insert([x, z]);
            }
        }
    }
    out
}

/// What to lay down over a footprint.
#[derive(Clone, Copy, Debug, Default)]
pub struct FillSpec {
    /// Floor height, grid units relative to the room's offset.
    pub floor: i32,
    /// Ceiling height, when the footprint is roofed.
    pub ceiling: Option<i32>,
    /// Floor material.
    pub floor_material: Option<ResourceId>,
    /// Ceiling material.
    pub ceiling_material: Option<ResourceId>,
    /// Also set the height of surfaces that already exist (default: leave
    /// them alone).
    pub overwrite: bool,
}

/// What [`fill_cells`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillReport {
    /// Floors made.
    pub floors: usize,
    /// Ceilings made.
    pub ceilings: usize,
    /// Existing surfaces whose height was set (`overwrite`).
    pub reset: usize,
    /// Cells that already had a surface and were left alone.
    pub kept: usize,
}

/// Lay floors (and ceilings) over `cells` of `room`; the room grows to hold
/// them.
pub fn fill_cells(
    world: &mut GridWorld,
    room: usize,
    cells: &BTreeSet<[i32; 2]>,
    spec: FillSpec,
) -> Result<FillReport, RectRefusal> {
    let mut report = FillReport::default();
    let (Some(lo), Some(hi)) = (
        cells.iter().fold(None, |a: Option<[i32; 2]>, c| {
            Some(a.map_or(*c, |a| [a[0].min(c[0]), a[1].min(c[1])]))
        }),
        cells.iter().fold(None, |a: Option<[i32; 2]>, c| {
            Some(a.map_or(*c, |a| [a[0].max(c[0]), a[1].max(c[1])]))
        }),
    ) else {
        return Ok(report);
    };
    // Grow once, over the bounding box, so a refusal leaves nothing behind.
    {
        let Some(r) = world.rooms.get_mut(room) else {
            return Err(RectRefusal::NoSuchRoom);
        };
        super::rects::grow_room_to(r, CellRect::from_corners(lo, hi))?;
    }
    for &cell in cells {
        let one = CellRect::from_corners(cell, cell);
        let mut note = |outcome: RectOutcome, made: &mut usize| {
            *made += outcome.changed;
            report.kept += outcome.skipped;
        };
        let (mut floors, mut ceilings) = (0, 0);
        note(
            create_surfaces(
                world,
                room,
                SurfaceKind::Floor,
                one,
                spec.floor,
                spec.floor_material,
            ),
            &mut floors,
        );
        if let Some(h) = spec.ceiling {
            note(
                create_surfaces(
                    world,
                    room,
                    SurfaceKind::Ceiling,
                    one,
                    h,
                    spec.ceiling_material,
                ),
                &mut ceilings,
            );
        }
        report.floors += floors;
        report.ceilings += ceilings;
        if spec.overwrite {
            let Some(r) = world.rooms.get_mut(room) else {
                continue;
            };
            let Some(sector) = r.local(cell).and_then(|[x, z]| r.sector_mut(x, z)) else {
                continue;
            };
            let (floor, ceiling) = sector.ground_floor_and_ceiling_mut();
            let heights = [(floor, spec.floor), (ceiling, spec.ceiling.unwrap_or(0))];
            for (i, (surface, h)) in heights.into_iter().enumerate() {
                let Some(surface) = surface else { continue };
                if (i == 1 && spec.ceiling.is_none())
                    || (i == 0 && floors == 1)
                    || (i == 1 && ceilings == 1)
                {
                    continue;
                }
                if surface.heights != [h; 4] || surface.heights_b.is_some() {
                    surface.heights = [h; 4];
                    surface.heights_b = None;
                    report.reset += 1;
                }
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::derive;
    use crate::grid::faces::{all_faces, GridFaceKind};
    use crate::grid::room::GridRoom;
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![GridRoom::new_derived("cave", 1, 1)],
            ..GridWorld::default()
        }
    }

    /// Everything with x + z <= n inside a 6 by 6 box: a unit staircase.
    fn staircase(n: i32) -> BTreeSet<[i32; 2]> {
        let mut set = BTreeSet::new();
        for z in 0..6 {
            for x in 0..6 {
                if x + z <= n {
                    set.insert([x, z]);
                }
            }
        }
        set
    }

    #[test]
    fn polygons_and_corridors_rasterise_at_cell_centres() {
        let square = polygon_cells(&[[1.0, 1.0], [4.0, 1.0], [4.0, 3.0], [1.0, 3.0]]);
        assert_eq!(square.len(), 6);
        assert!(square.contains(&[1, 1]) && square.contains(&[3, 2]));
        assert!(!square.contains(&[4, 1]) && !square.contains(&[0, 1]));
        // A triangle: its hypotenuse crosses the cell centres exactly on the
        // diagonal, and the tie goes to neither side.
        let tri = polygon_cells(&[[0.0, 0.0], [4.0, 0.0], [0.0, 4.0]]);
        assert_eq!(tri.len(), 6, "{tri:?}");
        assert!(polygon_cells(&[[0.0, 0.0], [1.0, 1.0]]).is_empty());
        let corridor = corridor_cells(&[[0.5, 0.5], [5.5, 0.5], [5.5, 3.5]], 1.0);
        assert!(corridor.contains(&[0, 0]) && corridor.contains(&[5, 3]));
        assert!(!corridor.contains(&[0, 1]));
        assert_eq!(corridor.len(), 6 + 3);
    }

    #[test]
    fn a_unit_staircase_gets_one_45_degree_line_of_voids() {
        let mut w = world();
        let cells = staircase(6);
        let report = fill_cells(
            &mut w,
            0,
            &cells,
            FillSpec {
                floor: 0,
                ceiling: Some(1024),
                ..FillSpec::default()
            },
        )
        .unwrap();
        assert_eq!((report.floors, report.ceilings), (cells.len(), cells.len()));
        let list: Vec<[i32; 2]> = cells.iter().copied().collect();
        let cut = cut_corners(&mut w, 0, &list, None, CornerCut::Void);
        // The five cells on the staircase (x + z == 6) and the three corners
        // of the box (north-west, north-east, south-west).
        assert_eq!(cut.cut, 8);
        let r = &w.rooms[0];
        let on_line: Vec<[i32; 2]> = list.iter().copied().filter(|c| c[0] + c[1] == 6).collect();
        assert_eq!(on_line.len(), 5);
        // Every diagonal closure of the staircase lies on the one line
        // X + Z = 7 (sector units).
        let closures: Vec<_> = on_line
            .iter()
            .flat_map(|&c| derive::cell_walls(r, c))
            .filter(|d| d.edge.is_diagonal() && d.kind == derive::DerivedKind::Closure)
            .collect();
        assert_eq!(closures.len(), 5);
        let faces = all_faces(&w);
        let mut checked = 0;
        for f in faces.iter().filter(|f| {
            on_line.contains(&f.cell)
                && matches!(f.kind, GridFaceKind::Derived { edge, .. } if edge.is_diagonal())
        }) {
            for p in &f.points {
                assert_eq!(p[0] + p[2], 7 * 1024, "{p:?}");
            }
            checked += 1;
        }
        assert_eq!(checked, 5);
    }

    #[test]
    fn restoring_brings_the_squares_back_exactly() {
        let mut w = world();
        let cells = staircase(2);
        fill_cells(
            &mut w,
            0,
            &cells,
            FillSpec {
                floor: 0,
                ..FillSpec::default()
            },
        )
        .unwrap();
        let before = w.clone();
        let list: Vec<[i32; 2]> = cells.iter().copied().collect();
        let cut = cut_corners(&mut w, 0, &list, None, CornerCut::Void);
        assert_ne!(w, before);
        let restored = restore_voids(&mut w, 0, &list);
        assert_eq!(restored, cut.cut);
        // Splits stay as set (a diagonal, not a hole, is what remains).
        for c in &list {
            let s = w.rooms[0]
                .local(*c)
                .and_then(|[x, z]| w.rooms[0].sector(x, z))
                .unwrap();
            assert_eq!(s.ground_floor().unwrap().void, 0);
        }
    }

    #[test]
    fn corridors_and_peninsulas_keep_square_ends() {
        let mut w = world();
        let cells = corridor_cells(&[[0.5, 0.5], [4.5, 0.5]], 1.0);
        fill_cells(
            &mut w,
            0,
            &cells,
            FillSpec {
                floor: 0,
                ..FillSpec::default()
            },
        )
        .unwrap();
        let list: Vec<[i32; 2]> = cells.iter().copied().collect();
        let cut = cut_corners(&mut w, 0, &list, None, CornerCut::Void);
        assert_eq!(
            cut.cut, 0,
            "a one-wide corridor has no convex staircase corner"
        );
    }

    #[test]
    fn a_plateau_corner_cracks_down_to_the_level_around_it() {
        let mut w = world();
        // A 3 by 3 plateau at 512 in a flat 5 by 5 floor.
        let all: BTreeSet<[i32; 2]> = (0..5).flat_map(|z| (0..5).map(move |x| [x, z])).collect();
        fill_cells(
            &mut w,
            0,
            &all,
            FillSpec {
                floor: 0,
                ..FillSpec::default()
            },
        )
        .unwrap();
        let plateau: BTreeSet<[i32; 2]> =
            (1..4).flat_map(|z| (1..4).map(move |x| [x, z])).collect();
        for c in &plateau {
            let s = w.rooms[0]
                .local(*c)
                .and_then(|[x, z]| w.rooms[0].sector_mut(x, z))
                .unwrap();
            s.ground_floor_mut().unwrap().heights = [512; 4];
        }
        let list: Vec<[i32; 2]> = plateau.iter().copied().collect();
        let cut = cut_corners(
            &mut w,
            0,
            &list,
            Some(&plateau),
            CornerCut::Crack { drop: 512 },
        );
        assert_eq!(cut.cut, 4);
        // The north-west plateau cell has its outer (NW) triangle at 0 and the
        // inner at 512, with a derived diagonal riser between.
        let s = w.rooms[0].sector(1, 1).unwrap().ground_floor().unwrap();
        assert_eq!(s.split, GridSplit::NeSw);
        assert_eq!(s.heights, [-0; 4].map(|_: i32| 0));
        assert_eq!(s.heights_b, Some([512; 4]));
        let diag = derive::cell_walls(&w.rooms[0], [1, 1])
            .into_iter()
            .filter(|d| d.edge.is_diagonal())
            .count();
        assert_eq!(diag, 1);
    }
}
