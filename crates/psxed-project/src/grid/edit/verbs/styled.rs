//! Styled runs: Corridor, Room and the Cathedral nave shaped by a
//! [`Style`](super::style::Style) on fine leaves.
//!
//! The run is first laid plain (flat floor, flat ceiling, whole sectors) by
//! the ordinary verb. The styler then reads each sector's own floor and
//! ceiling, adds the style's height offsets at every click of a 4 by 4 grid
//! of fine cells, and writes the result back with the **fewest leaves that
//! represent it exactly**: a block of 4, 2 or 1 click leaves only where the
//! heights are not already one plane (a flat bay stays one sector leaf). A
//! click inside a bay's inset becomes void, so the derived walls move in.
//! Walls follow by themselves: a derived wall takes its two edge heights
//! from the floor and ceiling it stands between, so a sloped profile gives a
//! sloped wall edge and a step between bays gives a riser.
//!
//! **Corners and height changes.** Every waypoint is a plain landing (a
//! square of the corridor's width, flat, no rhythm); the rhythm starts again
//! at the first bay of each leg. A leg that rises (ramp or stairs) keeps its
//! base slope and the profile is added on top, so a ribbed ramp is a ramp
//! with ribs.
//!
//! Refused before anything is written: a bay with less than body headroom
//! anywhere, a floor profile steeper than the motor stands on, and a step
//! between neighbouring fine cells taller than the motor climbs.

use std::collections::{BTreeMap, BTreeSet};

use psx_engine::WalkLimits;

use crate::grid::auto_split;
use crate::grid::cook::RuntimeUnits;
use crate::grid::leaf::LeafKey;
use crate::grid::mesh::{ColumnSpec, GridMesh, MeshAsset, MeshKind};
use crate::grid::room::GridRoom;
use crate::grid::sector::GridSector;
use crate::grid::surface::GridSurface;
use crate::grid::tree::normalize;
use crate::grid::units::{snap_to, FINE_UNITS};
use crate::grid::world::GridWorld;
use crate::ResourceId;

use super::super::meshes::{add_asset, place_mesh};
use super::super::rects::CellRect;
use super::common::body_headroom;
use super::corridor::{lay_corridor, CorridorParams, CorridorRefusal, LegShape, Waypoint};
use super::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomRefusal, RoomTarget};
use super::stairs::max_riser;
use super::style::{BayKind, Pier, Role, Roles, Style, Vault};

/// The steepest rise between two neighbouring clicks of a floor profile, grid
/// units: both components of a cell's gradient stay under the slope the motor
/// stands on (`sqrt(2) * 128 < 256 * 1.02`).
pub const MAX_CLICK_RISE: i32 = 128;

/// Why a styled run was not made. The document is untouched.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleRefusal {
    /// The style's numbers do not make a style.
    BadStyle(String),
    /// The plain corridor was refused.
    Corridor(CorridorRefusal),
    /// The plain room was refused.
    Room(RoomRefusal),
    /// Less than body headroom somewhere in this sector.
    TooLow {
        /// Least clear height, grid units.
        least: i32,
        /// The sector.
        cell: [i32; 2],
    },
    /// The floor profile rises more than [`MAX_CLICK_RISE`] in a click.
    FloorTooSteep {
        /// The rise found.
        rise: i32,
    },
    /// Neighbouring cells differ in floor height by more than the step-up.
    StepTooTall {
        /// The step found.
        riser: i32,
    },
    /// Finer cells already stand in this sector.
    FineCells {
        /// The sector.
        cell: [i32; 2],
    },
}

impl StyleRefusal {
    /// One line for the status bar.
    pub fn describe(&self) -> String {
        match self {
            Self::BadStyle(why) => why.clone(),
            Self::Corridor(c) => c.describe(),
            Self::Room(r) => r.describe(),
            Self::TooLow { least, cell } => format!(
                "Less than {least} units of headroom in sector ({}, {})",
                cell[0], cell[1]
            ),
            Self::FloorTooSteep { rise } => {
                format!("The floor profile rises {rise} units in a click: at most {MAX_CLICK_RISE}")
            }
            Self::StepTooTall { riser } => {
                format!("A step of {riser} units between bays is taller than the body climbs")
            }
            Self::FineCells { cell } => {
                format!(
                    "Finer cells already stand in sector ({}, {})",
                    cell[0], cell[1]
                )
            }
        }
    }
}

/// How a run maps world clicks to `(along, across)` clicks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    /// 0 for X, 1 for Z.
    pub along_axis: u8,
    /// +1 or -1: which way along increases.
    pub along_sign: i32,
    /// World click where along is 0.
    pub along_edge: i32,
    /// 0 for X, 1 for Z.
    pub across_axis: u8,
    /// +1 or -1.
    pub across_sign: i32,
    /// World click where across is 0.
    pub across_edge: i32,
}

impl Frame {
    fn pick(axis: u8, wx: i32, wz: i32) -> i32 {
        if axis == 0 {
            wx
        } else {
            wz
        }
    }

    /// Clicks along the run of the world point.
    pub fn along(&self, wx: i32, wz: i32) -> i32 {
        self.along_sign * (Self::pick(self.along_axis, wx, wz) - self.along_edge)
    }

    /// Clicks across the run of the world point.
    pub fn across(&self, wx: i32, wz: i32) -> i32 {
        self.across_sign * (Self::pick(self.across_axis, wx, wz) - self.across_edge)
    }

    /// The world click of `(along, across)`.
    pub fn world(&self, along: i32, across: i32) -> [i32; 2] {
        let a = self.along_edge + self.along_sign * along;
        let c = self.across_edge + self.across_sign * across;
        if self.along_axis == 0 {
            [a, c]
        } else {
            [c, a]
        }
    }
}

/// What one fine cell holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellData {
    /// Floor corners `[NW, NE, SE, SW]`.
    pub floor: [i32; 4],
    /// Ceiling corners.
    pub ceil: [i32; 4],
    /// Floor material.
    pub floor_mat: Option<ResourceId>,
    /// Ceiling material.
    pub ceil_mat: Option<ResourceId>,
    /// Wall material.
    pub wall_mat: Option<ResourceId>,
}

fn scale(v: i32, pct: i32) -> i32 {
    snap_to((i64::from(v) * i64::from(pct) / 100) as i32, FINE_UNITS)
}

/// A style over a run: what each fine cell holds.
pub struct Run<'a> {
    /// Where along and across are.
    pub frame: Frame,
    /// Width in clicks.
    pub width: i32,
    /// The style.
    pub style: &'a Style,
    /// Materials by role.
    pub roles: Roles,
}

impl Run<'_> {
    fn role(
        &self,
        kind: &BayKind,
        pick: impl Fn(&BayKind) -> Option<Role>,
        default: Role,
    ) -> Option<ResourceId> {
        self.roles.get(pick(kind).unwrap_or(default))
    }

    /// The fine cell at world click `(wx, wz)` over a sector whose floor and
    /// ceiling corners (`[NW, NE, SE, SW]`) are `base`, or `None` where the
    /// bay's inset leaves it void.
    pub fn cell(
        &self,
        sector: [i32; 2],
        base: ([i32; 4], [i32; 4]),
        wx: i32,
        wz: i32,
    ) -> Option<CellData> {
        let corners = [(wx, wz), (wx + 1, wz), (wx + 1, wz + 1), (wx, wz + 1)];
        let along = corners.map(|(x, z)| self.frame.along(x, z));
        let across = corners.map(|(x, z)| self.frame.across(x, z));
        let a0 = along.iter().copied().min().unwrap_or(0);
        let (kind, start, len) = self.style.bay_at(a0);
        let (lo, hi) = (
            across.iter().copied().min().unwrap_or(0),
            across.iter().copied().max().unwrap_or(0),
        );
        if lo < kind.inset || hi > self.width - kind.inset {
            return None;
        }
        let (bf, bc) = base;
        let mut floor = [0; 4];
        let mut ceil = [0; 4];
        for (i, &(x, z)) in corners.iter().enumerate() {
            let (px, pz) = (x - sector[0] * 4, z - sector[1] * 4);
            let (f, c) = (bilinear(bf, px, pz), bilinear(bc, px, pz));
            let fo = scale(
                self.style.floor.sample(self.width, across[i]),
                kind.floor_scale_pct,
            ) + kind.floor_offset;
            let co = scale(
                self.style.ceiling.sample(self.width, across[i]),
                kind.ceiling_scale_pct,
            ) + kind.ceiling_offset
                + self
                    .style
                    .vault
                    .offset(self.width, len, across[i], along[i] - start);
            floor[i] = snap_to(f + fo, FINE_UNITS);
            ceil[i] = snap_to(c + co, FINE_UNITS);
        }
        Some(CellData {
            floor,
            ceil,
            floor_mat: self.role(kind, |k| k.floor_role, self.style.floor_role),
            ceil_mat: self.role(kind, |k| k.ceiling_role, self.style.ceiling_role),
            wall_mat: self.role(kind, |k| k.wall_role, self.style.wall_role),
        })
    }
}

/// Bilinear height of a sector's corners `[NW, NE, SE, SW]` at click
/// `(px, pz)` of `0..=4`.
fn bilinear(h: [i32; 4], px: i32, pz: i32) -> i32 {
    let (nw, ne, se, sw) = (
        i64::from(h[0]),
        i64::from(h[1]),
        i64::from(h[2]),
        i64::from(h[3]),
    );
    let (px, pz) = (i64::from(px), i64::from(pz));
    let v = nw * (4 - px) * (4 - pz) + ne * px * (4 - pz) + se * px * pz + sw * (4 - px) * pz;
    snap_to((v / 16) as i32, FINE_UNITS)
}

/// A leaf of the result: its level, its corner within the sector in clicks,
/// and what it holds (`None` is void).
type Block = (u8, i32, i32, Option<CellData>);

fn planar(corners: &dyn Fn(usize) -> i64, e: i64) -> bool {
    // Heights of the block corners NW, NE, SE, SW (scaled by 1).
    corners(0) + corners(2) == corners(1) + corners(3) && e > 0
}

/// Cut a 4 by 4 grid of fine cells into the fewest leaves that hold it
/// exactly.
fn fit(grid: &[[Option<CellData>; 4]; 4], x: usize, z: usize, e: usize, out: &mut Vec<Block>) {
    let cells: Vec<Option<CellData>> = (z..z + e)
        .flat_map(|j| (x..x + e).map(move |i| (i, j)))
        .map(|(i, j)| grid[j][i])
        .collect();
    let level = match e {
        4 => 2,
        2 => 3,
        _ => 4,
    };
    if cells.iter().all(Option::is_none) {
        out.push((level, x as i32, z as i32, None));
        return;
    }
    if cells.iter().all(Option::is_some) {
        let c: Vec<CellData> = cells.iter().flatten().copied().collect();
        let first = c[0];
        let same = c.iter().all(|d| {
            (d.floor_mat, d.ceil_mat, d.wall_mat)
                == (first.floor_mat, first.ceil_mat, first.wall_mat)
        });
        if same {
            let g = |i: usize, j: usize| grid[j][i].expect("a cell");
            let (nw, ne) = (g(x, z), g(x + e - 1, z));
            let (se, sw) = (g(x + e - 1, z + e - 1), g(x, z + e - 1));
            let e64 = e as i64;
            let fl = [nw.floor[0], ne.floor[1], se.floor[2], sw.floor[3]];
            let ce = [nw.ceil[0], ne.ceil[1], se.ceil[2], sw.ceil[3]];
            let plane_ok = |h: &[i32; 4], pick: &dyn Fn(&CellData) -> [i32; 4]| {
                if !planar(&|i| i64::from(h[i]), e64) {
                    return false;
                }
                for j in 0..e {
                    for i in 0..e {
                        let cell = pick(&grid[z + j][x + i].expect("a cell"));
                        // Corners NW, NE, SE, SW at offsets within the block.
                        let spots = [(i, j), (i + 1, j), (i + 1, j + 1), (i, j + 1)];
                        for (k, (ci, cj)) in spots.into_iter().enumerate() {
                            let want = i64::from(h[0]) * e64
                                + i64::from(h[1] - h[0]) * ci as i64
                                + i64::from(h[3] - h[0]) * cj as i64;
                            if i64::from(cell[k]) * e64 != want {
                                return false;
                            }
                        }
                    }
                }
                true
            };
            if plane_ok(&fl, &|d| d.floor) && plane_ok(&ce, &|d| d.ceil) {
                out.push((
                    level,
                    x as i32,
                    z as i32,
                    Some(CellData {
                        floor: fl,
                        ceil: ce,
                        ..first
                    }),
                ));
                return;
            }
        }
    }
    if e > 1 {
        let h = e / 2;
        for (dx, dz) in [(0, 0), (h, 0), (0, h), (h, h)] {
            fit(grid, x + dx, z + dz, h, out);
        }
    } else if let Some(Some(d)) = cells.first() {
        out.push((4, x as i32, z as i32, Some(*d)));
    } else {
        out.push((4, x as i32, z as i32, None));
    }
}

fn surface(h: [i32; 4], material: Option<ResourceId>) -> GridSurface {
    let mut s = GridSurface::flat_auto(h[0], material);
    s.heights = h;
    auto_split::resplit(&mut s);
    s
}

/// Re-shape the sector `cell` of `room` through `run`. The sector must be a
/// whole-sector leaf with a floor and a ceiling.
pub fn restyle_sector(
    room: &mut GridRoom,
    cell: [i32; 2],
    run: &Run<'_>,
) -> Result<(), StyleRefusal> {
    let key = LeafKey::sector(cell);
    let Some(tpl) = room.leaf(key).cloned() else {
        return Err(StyleRefusal::FineCells { cell });
    };
    let (Some(f), Some(c)) = (tpl.ground_floor(), tpl.ground_ceiling()) else {
        return Ok(());
    };
    let base = (f.heights, c.heights);
    let mut grid = [[None; 4]; 4];
    for (j, row) in grid.iter_mut().enumerate() {
        for (i, slot) in row.iter_mut().enumerate() {
            *slot = run.cell(cell, base, cell[0] * 4 + i as i32, cell[1] * 4 + j as i32);
        }
    }
    let mut blocks = Vec::new();
    fit(&grid, 0, 0, 4, &mut blocks);
    room.remove_leaf(key);
    for (level, bx, bz, data) in blocks {
        let Some(d) = data else { continue };
        let k = LeafKey {
            level,
            x: cell[0] * 4 + bx,
            z: cell[1] * 4 + bz,
        };
        let mut s = GridSector::with_surfaces(
            Some(surface(d.floor, d.floor_mat)),
            Some(surface(d.ceil, d.ceil_mat)),
        );
        s.set_ground_wall_material(d.wall_mat);
        room.insert_leaf(k, s)
            .map_err(|_| StyleRefusal::FineCells { cell })?;
    }
    Ok(())
}

/// The fine cells a restyle of `cells` would write, for the preflight.
fn preview(
    room: &GridRoom,
    cells: &BTreeSet<[i32; 2]>,
    run: &Run<'_>,
) -> BTreeMap<[i32; 2], (Option<CellData>, [i32; 2])> {
    let mut out = BTreeMap::new();
    for &cell in cells {
        let Some(tpl) = room.leaf(LeafKey::sector(cell)) else {
            continue;
        };
        let (Some(f), Some(c)) = (tpl.ground_floor(), tpl.ground_ceiling()) else {
            continue;
        };
        for j in 0..4 {
            for i in 0..4 {
                let (wx, wz) = (cell[0] * 4 + i, cell[1] * 4 + j);
                out.insert(
                    [wx, wz],
                    (run.cell(cell, (f.heights, c.heights), wx, wz), cell),
                );
            }
        }
    }
    out
}

fn preflight(
    world: &GridWorld,
    room: &GridRoom,
    cells: &BTreeSet<[i32; 2]>,
    run: &Run<'_>,
) -> Result<(), StyleRefusal> {
    let least = body_headroom(world).unwrap_or(768);
    let riser_max = RuntimeUnits::new(world.scale)
        .ok()
        .map_or(512, |u| max_riser(&u, &WalkLimits::DEFAULT));
    let map = preview(room, cells, run);
    for (&[wx, wz], (data, sector)) in &map {
        let Some(d) = data else { continue };
        for i in 0..4 {
            if d.ceil[i] - d.floor[i] < least {
                return Err(StyleRefusal::TooLow {
                    least,
                    cell: *sector,
                });
            }
        }
        // Rise across a click, the four edges of the cell.
        for (a, b) in [(0, 1), (3, 2), (0, 3), (1, 2)] {
            let rise = (d.floor[a] - d.floor[b]).abs();
            if rise > MAX_CLICK_RISE {
                return Err(StyleRefusal::FloorTooSteep { rise });
            }
        }
        // Steps to the east and south neighbours.
        for (dx, dz, mine, theirs) in [(1, 0, [1, 2], [0, 3]), (0, 1, [3, 2], [0, 1])] {
            if let Some((Some(n), _)) = map.get(&[wx + dx, wz + dz]) {
                for k in 0..2 {
                    let riser = (d.floor[mine[k]] - n.floor[theirs[k]]).abs();
                    if riser > riser_max {
                        return Err(StyleRefusal::StepTooTall { riser });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Stand a column pair on `positions` (world clicks with their clear height).
fn place_piers(world: &mut GridWorld, room: usize, pier: Pier, spots: &[([i32; 2], i32, i32)]) {
    for &(click, floor, clear) in spots {
        let column = ColumnSpec {
            radius: pier.radius.clamp(64, 1024),
            height: clear.clamp(640, 16384),
            sides: 8,
            plinth: 64,
            capital: 64,
        };
        let Ok(asset) = add_asset(world, MeshAsset::new("pier", MeshKind::Column(column))) else {
            return;
        };
        let at = [click[0] * 256, floor, click[1] * 256];
        if place_mesh(world, room, GridMesh::new(0, asset, at)).is_err() {
            return;
        }
    }
}

/// A styled corridor.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledCorridor {
    /// Where it goes.
    pub target: RoomTarget,
    /// The path (axis-aligned legs).
    pub waypoints: Vec<Waypoint>,
    /// Width in whole sectors.
    pub width: i32,
    /// Floor height of the first waypoint when it names none.
    pub base: i32,
    /// Shape of a leg between two heights.
    pub leg: LegShape,
    /// The style.
    pub style: Style,
    /// Materials by role.
    pub roles: Roles,
}

/// What a styled corridor made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledOutcome {
    /// The room.
    pub room: usize,
    /// Whether it is new.
    pub created: bool,
    /// Sectors styled.
    pub styled: usize,
    /// Leaves in the room afterwards.
    pub leaves: usize,
    /// Piers stood.
    pub piers: usize,
}

fn offsets(width: i32) -> std::ops::RangeInclusive<i32> {
    -((width - 1) / 2)..=(width / 2)
}

/// Lay a styled corridor. One logical edit: the caller snapshots once.
pub fn lay_styled_corridor(
    world: &mut GridWorld,
    p: &StyledCorridor,
) -> Result<StyledOutcome, StyleRefusal> {
    p.style.check().map_err(StyleRefusal::BadStyle)?;
    let mut work = world.clone();
    let mut cp = CorridorParams::new(p.target.clone(), p.waypoints.clone(), p.style.headroom);
    cp.width = p.width;
    cp.base = p.base;
    cp.leg = p.leg;
    cp.floor_material = p.roles.get(p.style.floor_role);
    cp.ceiling_material = p.roles.get(p.style.ceiling_role);
    cp.wall_material = p.roles.get(p.style.wall_role);
    let plain = lay_corridor(&mut work, &cp).map_err(StyleRefusal::Corridor)?;
    let room_index = plain.room;
    // The landings stay plain: every waypoint's square.
    let mut landing: BTreeSet<[i32; 2]> = BTreeSet::new();
    for w in &p.waypoints {
        for ox in offsets(p.width) {
            for oz in offsets(p.width) {
                landing.insert([w.cell[0] + ox, w.cell[1] + oz]);
            }
        }
    }
    let width = p.width * 4;
    let mut styled = 0;
    let mut spots: Vec<([i32; 2], i32, i32)> = Vec::new();
    let omin = offsets(p.width).next().unwrap_or(0);
    // A cell two legs both reach (a corner of a wide corridor) stays plain.
    let mut reached: BTreeMap<[i32; 2], usize> = BTreeMap::new();
    for i in 0..p.waypoints.len() - 1 {
        let (a, b) = (p.waypoints[i].cell, p.waypoints[i + 1].cell);
        let d = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
        let len = (b[0] - a[0]).abs() + (b[1] - a[1]).abs();
        let lat = [-d[1], d[0]];
        for k in 1..len {
            for o in offsets(p.width) {
                let cell = [a[0] + d[0] * k + lat[0] * o, a[1] + d[1] * k + lat[1] * o];
                if !landing.contains(&cell) {
                    *reached.entry(cell).or_default() += 1;
                }
            }
        }
    }
    for i in 0..p.waypoints.len() - 1 {
        let (a, b) = (p.waypoints[i].cell, p.waypoints[i + 1].cell);
        let d = [(b[0] - a[0]).signum(), (b[1] - a[1]).signum()];
        let len = (b[0] - a[0]).abs() + (b[1] - a[1]).abs();
        let lat = [-d[1], d[0]];
        let along_axis = if d[0] != 0 { 0 } else { 1 };
        let across_axis = 1 - along_axis;
        let along_sign = d[usize::from(along_axis)];
        let across_sign = lat[usize::from(across_axis)];
        // The rhythm starts at the first sector of the leg that is not part
        // of a landing square (a wide corridor's landing reaches along it).
        let ax = usize::from(along_axis);
        let free: Vec<i32> = (1..len)
            .filter(|k| {
                let cell = [
                    a[0] + d[0] * k + lat[0] * omin,
                    a[1] + d[1] * k + lat[1] * omin,
                ];
                !landing.contains(&cell)
            })
            .collect();
        let (Some(&k0), Some(&k_last)) = (free.first(), free.last()) else {
            continue;
        };
        let along_edge = if along_sign > 0 {
            (a[ax] + k0) * 4
        } else {
            (a[ax] - k0 + 1) * 4
        };
        let a_lat = a[usize::from(across_axis)];
        let across_edge = if across_sign > 0 {
            (a_lat + omin) * 4
        } else {
            (a_lat - omin + 1) * 4
        };
        let frame = Frame {
            along_axis,
            along_sign,
            along_edge,
            across_axis,
            across_sign,
            across_edge,
        };
        let run = Run {
            frame,
            width,
            style: &p.style,
            roles: p.roles,
        };
        let mut cells: BTreeSet<[i32; 2]> = BTreeSet::new();
        for k in k0..=k_last {
            for o in offsets(p.width) {
                let cell = [a[0] + d[0] * k + lat[0] * o, a[1] + d[1] * k + lat[1] * o];
                if !landing.contains(&cell) && reached.get(&cell) == Some(&1) {
                    cells.insert(cell);
                }
            }
        }
        preflight(&work, &work.rooms[room_index], &cells, &run)?;
        for &cell in &cells {
            restyle_sector(&mut work.rooms[room_index], cell, &run)?;
            styled += 1;
        }
        // Piers at the start of each bay that has them.
        let mut at = 0;
        let leg_clicks = (k_last - k0 + 1) * 4;
        while at < leg_clicks {
            let (kind, start, blen) = p.style.bay_at(at);
            if let (Some(pier), true) = (kind.pier, start == at) {
                for c in [kind.inset + 1, width - kind.inset - 1] {
                    let w = frame.world(at, c);
                    let clear = p.style.headroom + kind.ceiling_offset;
                    let floor = work.rooms[room_index]
                        .leaf_at([w[0], w[1]])
                        .and_then(|(_, s)| s.ground_floor().map(|f| f.heights[0]))
                        .unwrap_or(p.base);
                    spots.push((w, floor, clear));
                    let _ = pier;
                }
            }
            at += blen;
        }
    }
    let mut piers = 0;
    // Group by pier recipe (the style's first pier kind).
    if let Some(pier) = p.style.rhythm.iter().find_map(|b| b.pier) {
        piers = spots.len();
        place_piers(&mut work, room_index, pier, &spots);
    }
    normalize(&mut work.rooms[room_index]);
    let leaves = work.rooms[room_index].leaf_count();
    *world = work;
    Ok(StyledOutcome {
        room: room_index,
        created: plain.created,
        styled,
        leaves,
        piers,
    })
}

/// A styled room: a rectangle with the style's vault over each bay.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledRoom {
    /// New room or an existing derived one.
    pub target: RoomTarget,
    /// The rectangle, world sectors.
    pub rect: CellRect,
    /// Floor height.
    pub base: i32,
    /// The style (its rhythm gives the bay length along X).
    pub style: Style,
    /// Materials by role.
    pub roles: Roles,
}

/// Lay a styled room.
pub fn lay_styled_room(
    world: &mut GridWorld,
    p: &StyledRoom,
) -> Result<StyledOutcome, StyleRefusal> {
    p.style.check().map_err(StyleRefusal::BadStyle)?;
    let mut work = world.clone();
    let rp = RoomParams {
        footprint: Footprint::Rect(p.rect),
        target: p.target.clone(),
        base: p.base,
        height: p.style.headroom,
        floor_material: p.roles.get(p.style.floor_role),
        ceiling_material: p.roles.get(p.style.ceiling_role),
        wall_material: p.roles.get(p.style.wall_role),
        leaves: LeafPolicy::Sectors,
        aim: None,
    };
    let made = lay_room(&mut work, &rp).map_err(StyleRefusal::Room)?;
    let frame = Frame {
        along_axis: 0,
        along_sign: 1,
        along_edge: p.rect.min[0] * 4,
        across_axis: 1,
        across_sign: 1,
        across_edge: p.rect.min[1] * 4,
    };
    let width = p.rect.depth() * 4;
    let run = Run {
        frame,
        width,
        style: &p.style,
        roles: p.roles,
    };
    let cells: BTreeSet<[i32; 2]> = p.rect.cells().collect();
    preflight(&work, &work.rooms[made.room], &cells, &run)?;
    for &cell in &cells {
        restyle_sector(&mut work.rooms[made.room], cell, &run)?;
    }
    let mut piers = 0;
    if let Some(pier) = p.style.corner_piers {
        // Four piers a click inside the corners of every bay.
        let (_, _, blen) = p.style.bay_at(0);
        let bays = (p.rect.width() * 4 + blen - 1) / blen;
        let mut spots = Vec::new();
        for b in 0..bays {
            let x0 = p.rect.min[0] * 4 + b * blen;
            for (dx, dz) in [(1, 1), (blen - 1, 1), (1, width - 1), (blen - 1, width - 1)] {
                let click = [x0 + dx, p.rect.min[1] * 4 + dz];
                spots.push((click, p.base, p.style.headroom));
            }
        }
        piers = spots.len();
        place_piers(&mut work, made.room, pier, &spots);
    }
    normalize(&mut work.rooms[made.room]);
    let leaves = work.rooms[made.room].leaf_count();
    *world = work;
    Ok(StyledOutcome {
        room: made.room,
        created: made.created,
        styled: cells.len(),
        leaves,
        piers,
    })
}

/// Shape the ceiling of cells already in `room` (for the Cathedral nave): the
/// style's offsets are added to what the sectors hold.
pub fn restyle_run(
    world: &mut GridWorld,
    room: usize,
    cells: &BTreeSet<[i32; 2]>,
    frame: Frame,
    width_clicks: i32,
    style: &Style,
    roles: Roles,
) -> Result<usize, StyleRefusal> {
    style.check().map_err(StyleRefusal::BadStyle)?;
    let run = Run {
        frame,
        width: width_clicks,
        style,
        roles,
    };
    preflight(world, &world.rooms[room], cells, &run)?;
    for &cell in cells {
        restyle_sector(&mut world.rooms[room], cell, &run)?;
    }
    normalize(&mut world.rooms[room]);
    Ok(cells.len())
}

/// Whether a style's vault shapes the ceiling.
pub fn is_vaulted(style: &Style) -> bool {
    !matches!(style.vault, Vault::None)
}
