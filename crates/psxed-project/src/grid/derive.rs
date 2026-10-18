//! Derived walls: the vertical faces a room gets from its height
//! differences instead of from `GridWall` records.
//!
//! A wall is a function of the two sectors on either side of an edge. Taking
//! each side's floor and ceiling profile along the edge (linear between the
//! two edge corners, the triangle that holds the edge for a split-level
//! surface):
//!
//! * **R1 riser**: both sides have floors and they differ. The sector with
//!   the higher floor owns the face, which fills the strip between the two
//!   profiles and is seen from the lower side.
//! * **R2 drop**: both sides have ceilings and they differ. The sector with
//!   the lower ceiling owns it; it is seen from the higher side.
//! * **R3 closure**: the neighbour holds no sector (or one with neither a
//!   floor nor a ceiling). The sector closes itself: its open interval
//!   between floor and ceiling, or floor and the room's `open_wall` height
//!   when it has no ceiling.
//! * Anything else (a floorless neighbour, equal profiles) makes no wall.
//!
//! The same three rules run along the **diagonal** of one sector (see
//! [`super::derive_diag`]): the two triangles of a split-level surface are the
//! two sides (R1d riser, R2d drop), and a void triangle ([`GridSurface::void`])
//! is a side with no space, so its neighbour triangle closes itself along the
//! diagonal (R3d). A void triangle is also empty space across a cardinal
//! edge, so the sector next to it closes with an ordinary R3.
//!
//! Profiles that cross inside the edge (a bow tie) are cut at the crossing
//! into two triangles with opposite owners. Equal profiles make nothing,
//! which is the point: a continuous floor never grows a wall.
//!
//! Every face has an *owner* (the sector the step belongs to, which decides
//! the material chain) and a *host* (the sector it is seen from, where it is
//! listed, culled and collided, exactly where an authored front-facing wall
//! of the same look would sit). Geometry is computed once per physical edge
//! in a fixed direction, so both sectors of an edge see identical numbers.

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::overrides::{Axis, EdgeKey, SEGMENT_CLICKS};
use super::room::GridRoom;
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSurface, GridWall};
use super::units::SECTOR_UNITS;

/// Which rule made a derived face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DerivedKind {
    /// R1: a step up between two floors.
    Riser,
    /// R2: a step down between two ceilings.
    Drop,
    /// R3: the edge of the room.
    Closure,
    /// R4: a slab seen from a taller neighbour, owned by the span above it
    /// (see [`super::derive_spans`]).
    Slab,
}

impl DerivedKind {
    /// Lower-case label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Riser => "riser",
            Self::Drop => "drop",
            Self::Closure => "closure",
            Self::Slab => "slab",
        }
    }
}

/// One derived wall face on one sector edge.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedWall {
    /// The rule that made it.
    pub kind: DerivedKind,
    /// World sector it is seen from and listed under.
    pub host: [i32; 2],
    /// The host's edge it lies on: a cardinal edge, or the sector's own
    /// diagonal (see [`Self::back`]).
    pub edge: WallEdge,
    /// Only for a diagonal: the face is seen from the BACK of the edge (the
    /// side its [`WallEdge::front_direction`] points away from), so its
    /// walking direction and `ring` run from the edge's `b` end to its `a`
    /// end. Always false for a cardinal edge.
    pub back: bool,
    /// World sector the step belongs to.
    pub owner: [i32; 2],
    /// Index of the span of the host column the face is seen from (`0` in
    /// a column of one span): a column of several can hold faces of one rule
    /// stacked on one edge.
    pub span: u8,
    /// Address of the physical segment, shared by both sectors.
    pub key: EdgeKey,
    /// Outline `[u, y]`, `u` in grid units along the host edge's walking
    /// direction (`0..=SECTOR_UNITS`), `y` room relative. Three or four
    /// corners.
    pub ring: Vec<[i32; 2]>,
    /// `[bottom a, bottom b, top b, top a]` along the host edge when the
    /// outline is that quad (what an authored wall of the same shape holds).
    pub heights: Option<[i32; 4]>,
    /// Material after the chain, or the override.
    pub material: Option<ResourceId>,
    /// The override hides the face.
    pub hidden: bool,
    /// Per-segment texture placement from the override.
    pub uv: Option<FaceUv>,
    /// Collision override.
    pub solid: Option<bool>,
    /// Band set assignment from the override.
    pub band_set: Option<u8>,
}

impl DerivedWall {
    /// Whether the cook gives this face a collision wall record. Closure
    /// walls block unless the override makes them a ghost; risers and drops
    /// are left to the step rule unless the override asks for a wall.
    pub fn blocks(&self) -> bool {
        self.heights.is_some()
            && match self.kind {
                DerivedKind::Closure | DerivedKind::Slab => self.solid != Some(false),
                _ => self.solid == Some(true),
            }
    }

    /// [`Self::heights`] in the walking direction of the edge itself, which
    /// is the order a collision wall record holds them in (a diagonal face
    /// seen from the back walks the other way).
    pub fn edge_heights(&self) -> Option<[i32; 4]> {
        let [ba, bb, tb, ta] = self.heights?;
        Some(if self.back {
            [bb, ba, ta, tb]
        } else {
            [ba, bb, tb, ta]
        })
    }

    /// The face as the `GridWall` of the same look on `edge`'s stack of
    /// `cell`, for the wall tool's gap logic. `None` for triangles.
    pub fn as_wall_of(&self, cell: [i32; 2], edge: WallEdge) -> Option<GridWall> {
        let [ba, bb, tb, ta] = self.heights?;
        // `heights` run along the host's walking direction; a wall of
        // `edge` runs a -> b of the edge itself.
        let same = cell == self.host && edge == self.edge && !self.back;
        let reversed = if edge.is_diagonal() { self.back } else { !same };
        let heights = if reversed {
            [bb, ba, ta, tb]
        } else {
            [ba, bb, tb, ta]
        };
        Some(GridWall {
            heights,
            facing: if self.back {
                super::surface::WallFacing::Back
            } else {
                super::surface::WallFacing::Front
            },
            ..GridWall::span(0, 0, self.material)
        })
    }
}

/// Edges of a sector a derived wall can sit on.
pub const CARDINAL: [WallEdge; 4] = [
    WallEdge::North,
    WallEdge::East,
    WallEdge::South,
    WallEdge::West,
];

/// The physical edge a cardinal edge of `cell` is: the sector on its
/// south/east side and the axis it runs along.
const fn physical(cell: [i32; 2], edge: WallEdge) -> ([i32; 2], Axis) {
    match edge {
        WallEdge::North => (cell, Axis::X),
        WallEdge::West => (cell, Axis::Z),
        WallEdge::South => ([cell[0], cell[1] + 1], Axis::X),
        WallEdge::East => ([cell[0] + 1, cell[1]], Axis::Z),
        // A diagonal belongs to its own sector alone.
        WallEdge::Nwse => (cell, Axis::NwSe),
        WallEdge::Nesw => (cell, Axis::NeSw),
    }
}

/// Key of the segment on `edge` of world sector `cell`. A diagonal is keyed by
/// its own sector.
pub fn edge_key(cell: [i32; 2], edge: WallEdge) -> EdgeKey {
    let (hi, axis) = physical(cell, edge);
    EdgeKey {
        x: hi[0] * i32::from(SEGMENT_CLICKS),
        z: hi[1] * i32::from(SEGMENT_CLICKS),
        axis,
        len: SEGMENT_CLICKS,
    }
}

/// A surface's height at both ends of `edge` (in the edge's own walking
/// direction) and the triangle that holds the edge.
fn edge_profile(surface: &GridSurface, edge: WallEdge) -> ([i32; 2], usize) {
    let [a, b] = super::edit::walls::edge_corner_indices(edge);
    let tri = super::edit::walls::edge_triangle(surface, edge);
    let heights = surface.heights_for(tri);
    ([heights[a], heights[b]], tri)
}

/// Whether `edge`'s walking direction is the canonical one (+X / +Z).
const fn forward(edge: WallEdge) -> bool {
    matches!(edge, WallEdge::North | WallEdge::East)
}

/// One profile in the canonical direction.
#[derive(Clone, Copy)]
struct Profile {
    h: [i32; 2],
}

/// What one side of an edge offers.
#[derive(Clone, Copy, Default)]
struct Side {
    space: bool,
    floor: Option<Profile>,
    ceil: Option<Profile>,
}

fn side_of(sector: Option<&GridSector>, edge: WallEdge) -> Side {
    let Some(s) = sector else {
        return Side::default();
    };
    // A void triangle is no space: the edge it holds offers nothing.
    let profile = |surface: &GridSurface| {
        let ([a, b], tri) = edge_profile(surface, edge);
        (!surface.is_void(tri)).then_some(Profile {
            h: if forward(edge) { [a, b] } else { [b, a] },
        })
    };
    let floor = s.ground_floor().and_then(profile);
    let ceil = s.ground_ceiling().and_then(profile);
    Side {
        space: floor.is_some() || ceil.is_some(),
        floor,
        ceil,
    }
}

pub(super) fn sector_at(room: &GridRoom, cell: [i32; 2]) -> Option<&GridSector> {
    let [x, z] = room.local(cell)?;
    room.sector(x, z)
}

fn round_div(num: i64, den: i64) -> i64 {
    let den = den.max(1);
    if num >= 0 {
        (num + den / 2) / den
    } else {
        -((-num + den / 2) / den)
    }
}

/// The polygon(s) where `upper` is above `lower` along an edge of length
/// [`SECTOR_UNITS`], in canonical `[u, y]`. `anchor` is the profile the
/// crossing height is read from, so both owners of a bow tie agree on it.
pub(super) fn between(lower: [i32; 2], upper: [i32; 2], anchor: [i32; 2]) -> Option<Vec<[i32; 2]>> {
    between_len(lower, upper, anchor, SECTOR_UNITS)
}

/// [`between`] for an edge segment of `len` grid units (a leaf below a
/// sector, or the part of an edge two different leaves share).
pub(super) fn between_len(
    lower: [i32; 2],
    upper: [i32; 2],
    anchor: [i32; 2],
    len: i32,
) -> Option<Vec<[i32; 2]>> {
    let (d0, d1) = (upper[0] - lower[0], upper[1] - lower[1]);
    if d0 >= 0 && d1 >= 0 {
        if d0 == 0 && d1 == 0 {
            return None;
        }
        return Some(vec![
            [0, lower[0]],
            [len, lower[1]],
            [len, upper[1]],
            [0, upper[0]],
        ]);
    }
    if d0 <= 0 && d1 <= 0 {
        return None;
    }
    // The profiles cross: keep the triangle on the side where `upper` wins.
    let (m0, m1) = (i64::from(d0.abs()), i64::from(d1.abs()));
    let uc = round_div(i64::from(len) * m0, m0 + m1).clamp(0, i64::from(len)) as i32;
    let yc = anchor[0]
        + round_div(
            i64::from(anchor[1] - anchor[0]) * i64::from(uc),
            i64::from(len),
        ) as i32;
    if d0 > 0 {
        (uc > 0).then(|| vec![[0, lower[0]], [0, upper[0]], [uc, yc]])
    } else {
        (uc < len).then(|| vec![[len, lower[1]], [len, upper[1]], [uc, yc]])
    }
}

/// Which of the two sectors of a physical edge.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// North / west sector.
    Lo,
    /// South / east sector.
    Hi,
}

struct Edge<'a> {
    room: &'a GridRoom,
    lo_cell: [i32; 2],
    hi_cell: [i32; 2],
    lo_edge: WallEdge,
    hi_edge: WallEdge,
    key: EdgeKey,
}

impl Edge<'_> {
    fn cell(&self, pick: Pick) -> [i32; 2] {
        match pick {
            Pick::Lo => self.lo_cell,
            Pick::Hi => self.hi_cell,
        }
    }

    fn edge(&self, pick: Pick) -> WallEdge {
        match pick {
            Pick::Lo => self.lo_edge,
            Pick::Hi => self.hi_edge,
        }
    }

    /// Build the face for `ring` (canonical) hosted by `host`, owned by
    /// `owner`.
    fn wall(&self, kind: DerivedKind, host: Pick, owner: Pick, ring: &[[i32; 2]]) -> DerivedWall {
        let owner_sector = sector_at(self.room, self.cell(owner));
        let surface = {
            let sector = owner_sector;
            let surface = sector.and_then(|s| {
                if kind == DerivedKind::Drop {
                    s.ground_ceiling()
                } else {
                    s.ground_floor()
                }
            });
            surface.and_then(|surface| {
                let ([_, _], tri) = edge_profile(surface, self.edge(owner));
                surface.tri(tri).material
            })
        };
        let host_edge = self.edge(host);
        Face {
            room: self.room,
            kind,
            host: self.cell(host),
            edge: host_edge,
            back: false,
            owner: self.cell(owner),
            span: 0,
            key: self.key,
            surface,
        }
        .build(ring, !forward(host_edge))
    }
}

/// What every derived face needs to know besides its outline.
pub(super) struct Face<'a> {
    pub room: &'a GridRoom,
    pub kind: DerivedKind,
    pub host: [i32; 2],
    pub edge: WallEdge,
    pub back: bool,
    pub owner: [i32; 2],
    /// Index of the host column's span the face is seen from.
    pub span: u8,
    pub key: EdgeKey,
    /// Material of the owner's surface triangle, the last link of the
    /// material chain.
    pub surface: Option<ResourceId>,
}

impl Face<'_> {
    /// The face for `ring` (canonical `[u, y]`, `u` along the key's
    /// direction); `flip` reverses `u` into the host's walking direction.
    pub(super) fn build(&self, ring: &[[i32; 2]], flip: bool) -> DerivedWall {
        let owner = sector_at(self.room, self.owner);
        let host = sector_at(self.room, self.host);
        self.build_with(ring, flip, SECTOR_UNITS, owner, host)
    }

    /// [`Self::build`] for a segment of `len` grid units whose owner and
    /// host leaves are given (the material chain reads their wall
    /// materials).
    pub(super) fn build_with(
        &self,
        ring: &[[i32; 2]],
        flip: bool,
        len: i32,
        owner_sector: Option<&GridSector>,
        host_sector: Option<&GridSector>,
    ) -> DerivedWall {
        let ring: Vec<[i32; 2]> = ring
            .iter()
            .map(|p| [if flip { len - p[0] } else { p[0] }, p[1]])
            .collect();
        // A quad is `[a bottom, b bottom, b top, a top]` in the host's own
        // direction.
        let at_end = |u: i32| ring.iter().filter(|p| p[0] == u).count();
        let heights = (ring.len() == 4 && at_end(0) == 2 && at_end(len) == 2)
            .then(|| quad_heights_len(&ring, len));
        let ovr = self.room.edge_overrides.get(&self.key);
        let chained = owner_sector
            .and_then(|s| s.ground_wall_material())
            .or_else(|| host_sector.and_then(|s| s.ground_wall_material()))
            .or(self.room.defaults.wall_material)
            .or(self.surface);
        DerivedWall {
            kind: self.kind,
            host: self.host,
            edge: self.edge,
            back: self.back,
            owner: self.owner,
            span: self.span,
            key: self.key,
            ring,
            heights,
            material: ovr.and_then(|o| o.material).or(chained),
            hidden: ovr.is_some_and(|o| o.hidden),
            uv: ovr.and_then(|o| o.uv),
            solid: ovr.and_then(|o| o.solid),
            band_set: ovr.and_then(|o| o.band_set),
        }
    }
}

/// `[bottom a, bottom b, top b, top a]` of a four-corner outline whose ends
/// are vertical, in the direction of increasing `u`, for a segment of `len`
/// grid units.
pub(super) fn quad_heights_len(ring: &[[i32; 2]], len: i32) -> [i32; 4] {
    let at = |u: i32, top: bool| {
        let ys = ring.iter().filter(|p| p[0] == u).map(|p| p[1]);
        if top {
            ys.max().unwrap_or(0)
        } else {
            ys.min().unwrap_or(0)
        }
    };
    [at(0, false), at(len, false), at(len, true), at(0, true)]
}

/// Every derived wall on the physical edge of `cell`'s `edge`, whichever
/// sector hosts it. Empty for the diagonals.
pub fn physical_edge_walls(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> Vec<DerivedWall> {
    if edge.is_diagonal() {
        let mut walls = super::derive_diag::diagonal_walls(room, cell);
        walls.retain(|w| w.edge == edge);
        return walls;
    }
    let (hi_cell, axis) = physical(cell, edge);
    let (lo_cell, lo_edge, hi_edge) = match axis {
        Axis::X => (
            [hi_cell[0], hi_cell[1] - 1],
            WallEdge::South,
            WallEdge::North,
        ),
        _ => ([hi_cell[0] - 1, hi_cell[1]], WallEdge::East, WallEdge::West),
    };
    // A column of several spans on either side: the open-set rule.
    let multi = |c: [i32; 2]| sector_at(room, c).is_some_and(|s| !s.is_single_span());
    if multi(lo_cell) || multi(hi_cell) {
        return super::derive_spans::edge_walls(room, lo_cell, lo_edge, hi_cell, hi_edge);
    }
    let e = Edge {
        room,
        lo_cell,
        hi_cell,
        lo_edge,
        hi_edge,
        key: edge_key(hi_cell, hi_edge),
    };
    let lo = side_of(sector_at(room, lo_cell), lo_edge);
    let hi = side_of(sector_at(room, hi_cell), hi_edge);
    let mut out = Vec::new();
    // R1: the higher floor owns the strip, the lower sector hosts it.
    if let (Some(lf), Some(hf)) = (lo.floor, hi.floor) {
        if let Some(ring) = between(hf.h, lf.h, lf.h) {
            out.push(e.wall(DerivedKind::Riser, Pick::Hi, Pick::Lo, &ring));
        }
        if let Some(ring) = between(lf.h, hf.h, lf.h) {
            out.push(e.wall(DerivedKind::Riser, Pick::Lo, Pick::Hi, &ring));
        }
    }
    // R2: the lower ceiling owns the strip, the higher sector hosts it.
    if let (Some(lc), Some(hc)) = (lo.ceil, hi.ceil) {
        if let Some(ring) = between(lc.h, hc.h, lc.h) {
            out.push(e.wall(DerivedKind::Drop, Pick::Hi, Pick::Lo, &ring));
        }
        if let Some(ring) = between(hc.h, lc.h, lc.h) {
            out.push(e.wall(DerivedKind::Drop, Pick::Lo, Pick::Hi, &ring));
        }
    }
    // R3: a sector next to nothing closes itself.
    for (pick, near, far) in [(Pick::Lo, lo, hi), (Pick::Hi, hi, lo)] {
        if !near.space || far.space {
            continue;
        }
        let Some(floor) = near.floor else { continue };
        let top = match near.ceil {
            Some(ceil) => ceil.h,
            None if room.defaults.open_wall > 0 => open_top(room, e.key, floor.h),
            None => continue,
        };
        if let Some(ring) = between(floor.h, top, floor.h) {
            out.push(e.wall(DerivedKind::Closure, pick, pick, &ring));
        }
    }
    out
}

/// The top of a closure wall with no ceiling: `open_wall` over the floor
/// profile (in the key's direction), plus the override's crumbled-top
/// offsets.
pub(super) fn open_top(room: &GridRoom, key: EdgeKey, floor: [i32; 2]) -> [i32; 2] {
    let delta = room
        .edge_overrides
        .get(&key)
        .map_or([0, 0], |o| o.top_delta.map(i32::from));
    let wall = room.defaults.open_wall;
    [floor[0] + wall + delta[0], floor[1] + wall + delta[1]]
}

/// Derived walls hosted by `edge` of `cell`, hidden ones included.
pub fn edge_walls(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> Vec<DerivedWall> {
    let mut walls = physical_edge_walls(room, cell, edge);
    walls.retain(|w| w.host == cell && w.edge == edge);
    walls
}

/// Derived walls hosted by `cell`, in edge order, hidden ones included.
pub fn cell_walls(room: &GridRoom, cell: [i32; 2]) -> Vec<DerivedWall> {
    let mut walls: Vec<DerivedWall> = CARDINAL
        .into_iter()
        .flat_map(|edge| edge_walls(room, cell, edge))
        .collect();
    walls.extend(super::derive_diag::diagonal_walls(room, cell));
    walls
}

/// Lowest and highest height of the visible derived walls hosted by `cell`
/// (room relative), for culling bounds.
pub fn derived_range(room: &GridRoom, cell: [i32; 2]) -> Option<(i32, i32)> {
    let mut range: Option<(i32, i32)> = None;
    for wall in cell_walls(room, cell).iter().filter(|w| !w.hidden) {
        for p in &wall.ring {
            range = Some(match range {
                Some((lo, hi)) => (lo.min(p[1]), hi.max(p[1])),
                None => (p[1], p[1]),
            });
        }
    }
    range
}

/// Where a run of derived walls extends: the frame its texture is measured
/// against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunExtent {
    /// Smallest and largest position along the host edge's walking
    /// direction, grid units (the same signed coordinate texels use: `x`
    /// for a north edge, minus `x` for a south edge, and so on).
    pub u: [f64; 2],
    /// Lowest and highest room-relative height.
    pub y: [i32; 2],
    /// Sectors in the run.
    pub cells: usize,
}

/// The faces of the run `wall` belongs to, `wall` included: the maximal
/// chain of collinear faces on the same side with the same kind, material
/// and texture override, which a fit mapping treats as one face.
pub fn run_members(room: &GridRoom, wall: &DerivedWall) -> Vec<DerivedWall> {
    // A diagonal face is one segment: nothing is collinear with it.
    if wall.edge.is_diagonal() {
        return vec![wall.clone()];
    }
    let along = match wall.edge {
        WallEdge::North | WallEdge::South => [1, 0],
        _ => [0, 1],
    };
    let same = |cell: [i32; 2]| {
        edge_walls(room, cell, wall.edge).into_iter().find(|w| {
            w.kind == wall.kind && !w.hidden && w.material == wall.material && w.uv == wall.uv
        })
    };
    let mut members = vec![wall.clone()];
    for sign in [-1, 1] {
        let mut cell = wall.host;
        loop {
            cell = [cell[0] + along[0] * sign, cell[1] + along[1] * sign];
            match same(cell) {
                Some(next) => members.push(next),
                None => break,
            }
        }
    }
    members
}

/// The run `wall` belongs to, as the frame its texture is measured against
/// (see [`run_members`]).
pub fn run_extent(room: &GridRoom, wall: &DerivedWall) -> RunExtent {
    if wall.edge.is_diagonal() {
        let ys = wall.ring.iter().map(|p| p[1]);
        return RunExtent {
            u: [0.0, f64::from(SECTOR_UNITS) * core::f64::consts::SQRT_2],
            y: [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)],
            cells: 1,
        };
    }
    let along = match wall.edge {
        WallEdge::North | WallEdge::South => [1, 0],
        _ => [0, 1],
    };
    let members = run_members(room, wall);
    let line = |w: &DerivedWall| i64::from(if along[0] == 1 { w.host[0] } else { w.host[1] });
    let (lo, hi) = (
        members.iter().map(line).min().unwrap_or(0),
        members.iter().map(line).max().unwrap_or(0),
    );
    let (start, end) = (
        (lo * i64::from(SECTOR_UNITS)) as f64,
        ((hi + 1) * i64::from(SECTOR_UNITS)) as f64,
    );
    let u = match wall.edge {
        WallEdge::North | WallEdge::East => [start, end],
        _ => [-end, -start],
    };
    let ys = members.iter().flat_map(|w| w.ring.iter().map(|p| p[1]));
    let y = [ys.clone().min().unwrap_or(0), ys.max().unwrap_or(0)];
    RunExtent {
        u,
        y,
        cells: members.len(),
    }
}

/// The derived faces on `cell`'s `edge` (from either side) as authored
/// walls of that stack, so the wall tool sees them as already placed.
pub fn virtual_walls(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> Vec<GridWall> {
    physical_edge_walls(room, cell, edge)
        .iter()
        .filter(|w| !w.hidden)
        .filter_map(|w| w.as_wall_of(cell, edge))
        .collect()
}

/// Whether authored `wall` (index in `edge`'s stack of `cell`) lies inside a
/// visible derived face seen from the same side, where the two would draw
/// on top of each other.
pub fn duplicates_derived(
    room: &GridRoom,
    cell: [i32; 2],
    edge: WallEdge,
    wall: &GridWall,
) -> bool {
    use super::surface::WallFacing;
    use super::units::FINE_UNITS;
    if wall.is_degenerate() {
        return false;
    }
    physical_edge_walls(room, cell, edge)
        .iter()
        .filter(|d| !d.hidden)
        .any(|d| {
            let from_front = d.host == cell && d.edge == edge && !d.back;
            let seen_by_wall = match wall.facing {
                WallFacing::Front => from_front,
                WallFacing::Back => !from_front,
                WallFacing::Both => true,
            };
            let Some(dh) = d.as_wall_of(cell, edge).map(|w| w.heights) else {
                return false;
            };
            let h = wall.heights;
            seen_by_wall
                && h[0] <= dh[0] + FINE_UNITS
                && h[1] <= dh[1] + FINE_UNITS
                && h[3] >= dh[3] - FINE_UNITS
                && h[2] >= dh[2] - FINE_UNITS
        })
}
