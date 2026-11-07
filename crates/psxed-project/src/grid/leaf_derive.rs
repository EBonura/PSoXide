//! Derived walls across leaves of different sizes.
//!
//! The same three rules as [`super::derive`] (riser, drop, closure), applied
//! to **segments of the finest common subdivision** of an edge: where a
//! coarse leaf meets two finer ones there are two segments, each with its
//! own pair of profiles, and a leaf larger than a sector is cut at sector
//! borders so every face lies in one sector (the cell it is listed under).
//! A coarse leaf's profile at a segment end is read off its edge
//! (linear, the triangle that holds the edge), so the walls pass through the
//! hanging vertex the floors have there.
//!
//! Faces are made from one leaf's point of view at a time and only the
//! faces that leaf hosts are kept; the other side's faces come from the
//! other leaf's turn. A face is listed under the sector its host leaf lies
//! in, exactly where an authored front-facing wall would sit.

use super::derive::{between_len, open_top, DerivedKind, DerivedWall, Face};
use super::faces_derived::DerivedGeom;
use super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::overrides::{Axis, EdgeKey};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSurface, VOID_A, VOID_B};
use super::tree::neighbours::{along, facing, outside_click, runs_x, SIDES};
use super::tree::normalize::{side_corners, side_height, side_triangle};
use super::tree::surface_split::round_div;
use super::units::SECTOR_UNITS;
use super::world::GridWorld;

/// Grid units in one click.
const CLICK: i32 = SECTOR_UNITS / CLICKS_PER_SECTOR;

/// A derived wall of a leaf below or above a sector (or any leaf in a mixed
/// room), with where it lies.
pub(crate) struct LeafWall {
    /// The leaf that hosts (sees) the face.
    pub host: LeafKey,
    pub wall: DerivedWall,
    pub geom: DerivedGeom,
}

/// A leaf found across a side, with its heights lifted into the frame of the
/// room that looks at it.
#[derive(Clone, Copy)]
struct Beyond<'a> {
    key: LeafKey,
    sector: &'a GridSector,
    /// Added to the leaf's room-relative heights: its room's `y_offset`
    /// less the looking room's.
    lift: i32,
}

/// Where a room looks for the leaf across one of its sides.
///
/// A room is a world of its own, so a side with no leaf against it is open
/// space. A chunk of a ring world ([`GridWorld::is_chunked`]) is not: the
/// level is one surface cut into rooms, and across a chunk border lies the
/// next chunk's leaf, so the risers and drops there are derived against it
/// exactly as inside the room.
#[derive(Clone, Copy)]
pub(crate) struct Around<'a> {
    room: &'a GridRoom,
    /// The world and the index of `room` in it, for a chunked world.
    chunks: Option<(&'a GridWorld, usize)>,
}

impl<'a> Around<'a> {
    /// `room` alone: a side with no leaf of its own is open space.
    pub(crate) fn room(room: &'a GridRoom) -> Self {
        Self { room, chunks: None }
    }

    /// `room`, which is room `index` of `world`, looking into its sibling
    /// chunks when the world is chunked.
    pub(crate) fn in_world(room: &'a GridRoom, world: &'a GridWorld, index: usize) -> Self {
        Self {
            room,
            chunks: world.is_chunked().then_some((world, index)),
        }
    }

    /// [`Self::in_world`] whether or not the world says it is chunked, for
    /// the census of what chunking leaves open.
    fn in_chunks(room: &'a GridRoom, world: &'a GridWorld, index: usize) -> Self {
        Self {
            room,
            chunks: Some((world, index)),
        }
    }

    /// The leaf holding `click`: the room's own, else a sibling chunk's.
    fn leaf_at(&self, click: [i32; 2]) -> Option<Beyond<'a>> {
        if let Some((key, sector)) = self.room.leaves.leaf_at(click) {
            return Some(Beyond {
                key,
                sector,
                lift: 0,
            });
        }
        let (world, own) = self.chunks?;
        let cell = [
            click[0].div_euclid(CLICKS_PER_SECTOR),
            click[1].div_euclid(CLICKS_PER_SECTOR),
        ];
        world
            .rooms
            .iter()
            .enumerate()
            .filter(|(i, r)| *i != own && r.local(cell).is_some())
            .find_map(|(_, r)| {
                r.leaves.leaf_at(click).map(|(key, sector)| Beyond {
                    key,
                    sector,
                    lift: r.y_offset - self.room.y_offset,
                })
            })
    }

    /// The leaves that touch `key` across `side`, nearest the start of the
    /// side first, each once.
    fn across(&self, key: LeafKey, side: WallEdge) -> Vec<LeafKey> {
        let n = key.edge_clicks();
        let (start, _) = along(key, side);
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            match self.leaf_at(outside_click(key, side, i)) {
                Some(found) => {
                    out.push(found.key);
                    let (lo, len) = along(found.key, side);
                    i = lo + len - start;
                }
                None => i += 1,
            }
        }
        out
    }
}

/// What one side of a segment offers.
#[derive(Clone, Copy, Default)]
struct Side {
    space: bool,
    floor: Option<[i32; 2]>,
    ceil: Option<[i32; 2]>,
}

/// A surface's heights at the two ends of the segment `u` (clicks) of
/// `edge` of the leaf `key`, in increasing click order. `None` when the
/// triangle holding the edge is void.
fn profile(s: &GridSurface, key: LeafKey, edge: WallEdge, u: [i32; 2]) -> Option<[i32; 2]> {
    if s.is_void(side_triangle(s, edge)) {
        return None;
    }
    let [c0, c1] = side_corners(edge);
    let (a, b) = (
        i64::from(side_height(s, edge, c0)),
        i64::from(side_height(s, edge, c1)),
    );
    let (start, len) = along(key, edge);
    let at = |u: i32| {
        round_div(
            a * i64::from(start + len - u) + b * i64::from(u - start),
            i64::from(len),
        ) as i32
    };
    Some([at(u[0]), at(u[1])])
}

fn side_of(
    sector: Option<&GridSector>,
    key: LeafKey,
    edge: WallEdge,
    u: [i32; 2],
    lift: i32,
) -> Side {
    let Some(s) = sector else {
        return Side::default();
    };
    let lifted = |p: [i32; 2]| p.map(|y| y + lift);
    let floor = s
        .ground_floor()
        .and_then(|f| profile(f, key, edge, u))
        .map(lifted);
    let ceil = s
        .ground_ceiling()
        .and_then(|c| profile(c, key, edge, u))
        .map(lifted);
    Side {
        space: floor.is_some() || ceil.is_some(),
        floor,
        ceil,
    }
}

/// Material of the owner's surface at the segment: the last link of the
/// chain.
fn surface_material(
    sector: Option<&GridSector>,
    edge: WallEdge,
    ceiling: bool,
) -> Option<crate::ResourceId> {
    let s = sector?;
    let surface = if ceiling {
        s.ground_ceiling()
    } else {
        s.ground_floor()
    }?;
    surface.tri(side_triangle(surface, edge)).material
}

/// Whether a wall on `edge` walks in the increasing direction.
const fn forward(edge: WallEdge) -> bool {
    matches!(edge, WallEdge::North | WallEdge::East)
}

/// The derived walls hosted by the leaves in `cell`.
pub(super) fn cell_walls(
    around: &Around<'_>,
    cell: [i32; 2],
    leaves: &[(LeafKey, &GridSector)],
) -> Vec<LeafWall> {
    let mut out = Vec::new();
    for &(key, sector) in leaves {
        for side in SIDES {
            side_walls(around, cell, key, sector, side, &mut out);
        }
    }
    out
}

fn side_walls(
    around: &Around<'_>,
    cell: [i32; 2],
    key: LeafKey,
    sector: &GridSector,
    side: WallEdge,
    out: &mut Vec<LeafWall>,
) {
    let n = key.edge_clicks();
    let (start, _) = along(key, side);
    // The part of the side that lies in this sector, if the leaf's row or
    // column next to the side does.
    let (cell_along, cell_perp) = if runs_x(side) {
        (cell[0] * CLICKS_PER_SECTOR, cell[1] * CLICKS_PER_SECTOR)
    } else {
        (cell[1] * CLICKS_PER_SECTOR, cell[0] * CLICKS_PER_SECTOR)
    };
    let inside = match side {
        WallEdge::North => key.z,
        WallEdge::West => key.x,
        WallEdge::South => key.z + n - 1,
        _ => key.x + n - 1,
    };
    if inside < cell_perp || inside >= cell_perp + CLICKS_PER_SECTOR {
        return;
    }
    let lo = start.max(cell_along);
    let hi = (start + n).min(cell_along + CLICKS_PER_SECTOR);
    if lo >= hi {
        return;
    }
    side_segments(around, cell, key, sector, side, (lo, hi), out);
}

/// The derived walls hosted by `key` on every side, not cut at sector
/// borders: the segments of the finest common subdivision of each side with
/// its neighbours, which for a balanced tree are the whole side or one half.
/// For collision, which holds one record per leaf edge segment.
pub(crate) fn leaf_walls(around: &Around<'_>, key: LeafKey, sector: &GridSector) -> Vec<LeafWall> {
    let mut out = Vec::new();
    for side in SIDES {
        let (start, len) = along(key, side);
        side_segments(
            around,
            key.cell(),
            key,
            sector,
            side,
            (start, start + len),
            &mut out,
        );
    }
    out
}

/// The walls on one side of `key` over the clicks `span` along it.
fn side_segments(
    around: &Around<'_>,
    cell: [i32; 2],
    key: LeafKey,
    sector: &GridSector,
    side: WallEdge,
    (lo, hi): (i32, i32),
    out: &mut Vec<LeafWall>,
) {
    let n = key.edge_clicks();
    let (start, _) = along(key, side);
    // Segment ends: the clipped ends and the neighbours' borders.
    let mut breaks = vec![lo, hi];
    for nb in around.across(key, side) {
        let (s, len) = along(nb, side);
        breaks.extend([s, s + len].into_iter().filter(|b| *b > lo && *b < hi));
    }
    breaks.sort_unstable();
    breaks.dedup();
    let line = match side {
        WallEdge::North => key.z,
        WallEdge::South => key.z + n,
        WallEdge::West => key.x,
        _ => key.x + n,
    };
    for pair in breaks.windows(2) {
        let u = [pair[0], pair[1]];
        let neighbour = around.leaf_at(outside_click(key, side, u[0] - start));
        segment_walls(around, cell, (key, sector), neighbour, side, line, u, out);
    }
}

#[allow(clippy::too_many_arguments)]
fn segment_walls(
    around: &Around<'_>,
    cell: [i32; 2],
    me: (LeafKey, &GridSector),
    other: Option<Beyond<'_>>,
    side: WallEdge,
    line: i32,
    u: [i32; 2],
    out: &mut Vec<LeafWall>,
) {
    let room = around.room;
    let (key, sector) = me;
    let mine = side_of(Some(sector), key, side, u, 0);
    let theirs = match other {
        Some(o) => side_of(Some(o.sector), o.key, facing(side), u, o.lift),
        None => Side::default(),
    };
    // Canonical sides: the north or west one is "lo".
    let me_is_lo = matches!(side, WallEdge::South | WallEdge::East);
    let (lo, hi) = if me_is_lo {
        (mine, theirs)
    } else {
        (theirs, mine)
    };
    let len = (u[1] - u[0]) * CLICK;
    let seg_key = if runs_x(side) {
        EdgeKey {
            x: u[0],
            z: line,
            axis: Axis::X,
            len: (u[1] - u[0]) as u8,
        }
    } else {
        EdgeKey {
            x: line,
            z: u[0],
            axis: Axis::Z,
            len: (u[1] - u[0]) as u8,
        }
    };
    // (kind, host is lo, owner is lo, ring)
    let mut found: Vec<(DerivedKind, bool, bool, Vec<[i32; 2]>)> = Vec::new();
    if let (Some(lf), Some(hf)) = (lo.floor, hi.floor) {
        if let Some(ring) = between_len(hf, lf, lf, len) {
            found.push((DerivedKind::Riser, false, true, ring));
        }
        if let Some(ring) = between_len(lf, hf, lf, len) {
            found.push((DerivedKind::Riser, true, false, ring));
        }
    }
    if let (Some(lc), Some(hc)) = (lo.ceil, hi.ceil) {
        if let Some(ring) = between_len(lc, hc, lc, len) {
            found.push((DerivedKind::Drop, false, true, ring));
        }
        if let Some(ring) = between_len(hc, lc, lc, len) {
            found.push((DerivedKind::Drop, true, false, ring));
        }
    }
    for (is_lo, near, far) in [(true, lo, hi), (false, hi, lo)] {
        if !near.space || far.space {
            continue;
        }
        let Some(floor) = near.floor else { continue };
        let top = match near.ceil {
            Some(ceil) => ceil,
            None if room.defaults.open_wall > 0 => open_top(room, seg_key, floor),
            None => continue,
        };
        if let Some(ring) = between_len(floor, top, floor, len) {
            found.push((DerivedKind::Closure, is_lo, is_lo, ring));
        }
    }
    let [(ax, az), (bx, bz)] = if runs_x(side) {
        [(u[0], line), (u[1], line)]
    } else {
        [(line, u[0]), (line, u[1])]
    }
    .map(|(x, z)| (x * CLICK, z * CLICK));
    // The wall walks the host edge's own direction.
    let (a, b) = if forward(side) {
        ([ax, az], [bx, bz])
    } else {
        ([bx, bz], [ax, az])
    };
    let signed = |p: [i32; 2]| -> f64 {
        f64::from(match side {
            WallEdge::North => p[0],
            WallEdge::East => p[1],
            WallEdge::South => -p[0],
            _ => -p[1],
        })
    };
    for (kind, host_is_lo, owner_is_lo, ring) in found {
        if host_is_lo != me_is_lo {
            continue;
        }
        let (owner_sector, owner_edge, owner_cell) = if owner_is_lo == me_is_lo {
            (Some(sector), side, cell)
        } else {
            match other {
                Some(o) => (Some(o.sector), facing(side), o.key.cell()),
                None => (None, facing(side), cell),
            }
        };
        let face = Face {
            room,
            kind,
            host: cell,
            edge: side,
            back: false,
            owner: owner_cell,
            span: 0,
            key: seg_key,
            surface: surface_material(owner_sector, owner_edge, kind == DerivedKind::Drop),
        };
        let wall = face.build_with(&ring, !forward(side), len, owner_sector, Some(sector));
        let ys = wall.ring.iter().map(|p| p[1]);
        let run_y = [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)];
        let (s0, s1) = (signed(a), signed(b));
        out.push(LeafWall {
            host: key,
            geom: DerivedGeom {
                a,
                b,
                len,
                run_u: [s0.min(s1), s0.max(s1)],
                run_y,
                neighbours: [Vec::new(), Vec::new()],
            },
            wall,
        });
    }
}

/// A derived wall that exists only because a leaf of a sibling chunk lies
/// across the border: with each room alone nothing would stand there.
#[derive(Clone, Debug, PartialEq)]
pub struct SeamWall {
    /// The room that hosts the face.
    pub room: usize,
    /// The leaf that hosts it.
    pub host: LeafKey,
    /// The wall itself.
    pub wall: DerivedWall,
    /// Length along the border, grid units.
    pub len: i32,
    /// Lowest and highest point, world grid units.
    pub y: [i32; 2],
}

/// Every derived wall of the world that stands against a leaf of another
/// room and that the rooms taken one by one would leave out, whether or not
/// the world is chunked. In a world that is not chunked these are the walls
/// the borders between its rooms lack; in a chunked world they are the
/// walls the cook and the preview draw.
pub fn seam_walls(world: &GridWorld) -> Vec<SeamWall> {
    let mut out = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        let alone = Around::room(room);
        let beside = Around::in_chunks(room, world, index);
        for (key, sector) in room.leaves() {
            let known: Vec<DerivedWall> = leaf_walls(&alone, key, sector)
                .into_iter()
                .map(|w| w.wall)
                .collect();
            for w in leaf_walls(&beside, key, sector) {
                if known.contains(&w.wall) {
                    continue;
                }
                let ys = w.wall.ring.iter().map(|p| p[1] + room.y_offset);
                out.push(SeamWall {
                    room: index,
                    host: key,
                    len: w.geom.len,
                    y: [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)],
                    wall: w.wall,
                });
            }
        }
    }
    out
}

/// What lies across an open floor edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpenKind {
    /// No leaf of any room.
    Absent,
    /// A leaf whose triangle on this side is void.
    VoidNeighbour,
    /// The other, void, triangle of the same leaf (along the fold).
    VoidTriangle,
}

/// A stretch of floor edge that ends against nothing and that nothing
/// derived closes: the floor simply stops, and a viewer outside sees through
/// to whatever lies behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenEdge {
    /// The room that holds the floor.
    pub room: usize,
    /// The leaf.
    pub host: LeafKey,
    /// Its side.
    pub edge: WallEdge,
    /// What is across.
    pub kind: OpenKind,
    /// The stretch along the side, clicks (the whole leaf for a void
    /// triangle).
    pub u: [i32; 2],
    /// Floor height at its two ends, world grid units.
    pub floor: [i32; 2],
}

/// Every open floor edge of the world (see [`OpenEdge`]), looking across
/// the borders of chunks as the cook does. Rooms with a ceiling or an open
/// wall height close their own edges and report none.
pub fn open_edges(world: &GridWorld) -> Vec<OpenEdge> {
    let mut out = Vec::new();
    for (index, room) in world.rooms.iter().enumerate() {
        if room.defaults.open_wall > 0 {
            continue;
        }
        let around = Around::in_chunks(room, world, index);
        for (key, sector) in room.leaves() {
            if sector.ground_ceiling().is_some() {
                continue;
            }
            if let Some(floor) = sector.ground_floor() {
                if matches!(floor.void & 3, VOID_A | VOID_B) {
                    let solid = usize::from(floor.void & 3 == VOID_A);
                    let h = floor.heights_for(solid);
                    out.push(OpenEdge {
                        room: index,
                        host: key,
                        edge: WallEdge::Nwse,
                        kind: OpenKind::VoidTriangle,
                        u: [0, key.edge_clicks()],
                        floor: [
                            h.iter().copied().min().unwrap_or(0) + room.y_offset,
                            h.iter().copied().max().unwrap_or(0) + room.y_offset,
                        ],
                    });
                }
            }
            for side in SIDES {
                let (start, len) = along(key, side);
                let mut breaks = vec![start, start + len];
                for nb in around.across(key, side) {
                    let (s, l) = along(nb, side);
                    breaks.extend(
                        [s, s + l]
                            .into_iter()
                            .filter(|b| *b > start && *b < start + len),
                    );
                }
                breaks.sort_unstable();
                breaks.dedup();
                for pair in breaks.windows(2) {
                    let u = [pair[0], pair[1]];
                    let mine = side_of(Some(sector), key, side, u, room.y_offset);
                    let Some(floor) = mine.floor else { continue };
                    let kind = match around.leaf_at(outside_click(key, side, u[0] - start)) {
                        None => OpenKind::Absent,
                        Some(o) => {
                            let theirs = side_of(Some(o.sector), o.key, facing(side), u, o.lift);
                            if theirs.space {
                                continue;
                            }
                            OpenKind::VoidNeighbour
                        }
                    };
                    out.push(OpenEdge {
                        room: index,
                        host: key,
                        edge: side,
                        kind,
                        u,
                        floor,
                    });
                }
            }
        }
    }
    out
}

/// One derived wall of a room with leaves of several sizes, for drawing in
/// the plan: where it lies and what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WallSegment {
    /// Start and end in grid units on the plan, in the host's walking
    /// direction.
    pub a: [i32; 2],
    pub b: [i32; 2],
    /// Which rule made it.
    pub kind: DerivedKind,
    /// The user hid it.
    pub hidden: bool,
    /// It has a collision record.
    pub blocks: bool,
}

/// The derived walls of a room listed under world sector `cell`, as plan
/// segments. Meant for rooms with leaves of several sizes.
pub fn cell_wall_segments(room: &GridRoom, cell: [i32; 2]) -> Vec<WallSegment> {
    let leaves = room.leaves.in_sector(cell);
    cell_walls(&Around::room(room), cell, &leaves)
        .into_iter()
        .map(|w| WallSegment {
            a: w.geom.a,
            b: w.geom.b,
            kind: w.wall.kind,
            hidden: w.wall.hidden,
            blocks: w.wall.blocks(),
        })
        .collect()
}

/// Lowest and highest room-relative height of the visible derived walls
/// listed under `cell`, for culling bounds. Meant for rooms with leaves of
/// several sizes. Each room is taken alone: see [`derived_range_in`] for the
/// walls that stand against a sibling chunk.
pub fn derived_range(room: &GridRoom, cell: [i32; 2]) -> Option<(i32, i32)> {
    range_of(&Around::room(room), cell)
}

/// [`derived_range`] for room `room_index` of `world`, counting the walls
/// that stand against the sibling chunks of a chunked world, which reach as
/// far as the neighbour's floor and so can be far taller than the cell.
pub fn derived_range_in(
    world: &GridWorld,
    room_index: usize,
    cell: [i32; 2],
) -> Option<(i32, i32)> {
    let room = world.rooms.get(room_index)?;
    range_of(&Around::in_world(room, world, room_index), cell)
}

fn range_of(around: &Around<'_>, cell: [i32; 2]) -> Option<(i32, i32)> {
    let leaves = around.room.leaves.in_sector(cell);
    let mut range: Option<(i32, i32)> = None;
    for w in cell_walls(around, cell, &leaves)
        .iter()
        .filter(|w| !w.wall.hidden)
    {
        for p in &w.wall.ring {
            range = Some(match range {
                Some((lo, hi)) => (lo.min(p[1]), hi.max(p[1])),
                None => (p[1], p[1]),
            });
        }
    }
    range
}

/// Every derived wall hosted by leaf `key` (its four sides, then its
/// diagonals), in a fixed order: for one side and rule the faces come in the
/// order they lie along the side.
pub fn derived_walls_of(room: &GridRoom, key: LeafKey) -> Vec<DerivedWall> {
    let Some(sector) = room.leaf(key) else {
        return Vec::new();
    };
    leaf_walls(&Around::room(room), key, sector)
        .into_iter()
        .chain(super::leaf_diag::leaf_diag_walls(room, key, sector))
        .map(|w| w.wall)
        .collect()
}

/// Which of the faces of `kind` on `edge` of leaf `key` (see
/// [`derived_walls_of`]) lies at the grid point `at`: its index among them,
/// 0 when there is only one or none matches.
pub fn derived_segment(
    room: &GridRoom,
    key: LeafKey,
    edge: WallEdge,
    kind: DerivedKind,
    at: [i32; 2],
) -> u8 {
    let Some(sector) = room.leaf(key) else {
        return 0;
    };
    let walls: Vec<LeafWall> = leaf_walls(&Around::room(room), key, sector)
        .into_iter()
        .chain(super::leaf_diag::leaf_diag_walls(room, key, sector))
        .filter(|w| w.wall.edge == edge && w.wall.kind == kind)
        .collect();
    walls
        .iter()
        .position(|w| {
            let (lo, hi) = (
                [w.geom.a[0].min(w.geom.b[0]), w.geom.a[1].min(w.geom.b[1])],
                [w.geom.a[0].max(w.geom.b[0]), w.geom.a[1].max(w.geom.b[1])],
            );
            at[0] >= lo[0] && at[0] <= hi[0] && at[1] >= lo[1] && at[1] <= hi[1]
        })
        .unwrap_or(0) as u8
}
