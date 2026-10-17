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
}

impl DerivedKind {
    /// Lower-case label for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Riser => "riser",
            Self::Drop => "drop",
            Self::Closure => "closure",
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
    /// The host's edge it lies on (always a cardinal edge).
    pub edge: WallEdge,
    /// World sector the step belongs to.
    pub owner: [i32; 2],
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
}

impl DerivedWall {
    /// Whether the cook gives this face a collision wall record. Closure
    /// walls block unless the override makes them a ghost; risers and drops
    /// are left to the step rule unless the override asks for a wall.
    pub fn blocks(&self) -> bool {
        self.heights.is_some()
            && match self.kind {
                DerivedKind::Closure => self.solid != Some(false),
                _ => self.solid == Some(true),
            }
    }

    /// The face as the `GridWall` of the same look on `edge`'s stack of
    /// `cell`, for the wall tool's gap logic. `None` for triangles.
    pub fn as_wall_of(&self, cell: [i32; 2], edge: WallEdge) -> Option<GridWall> {
        let [ba, bb, tb, ta] = self.heights?;
        let same = cell == self.host && edge == self.edge;
        let heights = if same {
            [ba, bb, tb, ta]
        } else {
            [bb, ba, ta, tb]
        };
        Some(GridWall {
            heights,
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
        _ => ([cell[0] + 1, cell[1]], Axis::Z),
    }
}

/// Key of the segment on `edge` of world sector `cell`.
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
    let tri = (0..2)
        .find(|t| {
            let corners = surface.split.triangle(*t);
            corners.contains(&a) && corners.contains(&b)
        })
        .unwrap_or(0);
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
    let profile = |surface: &GridSurface| {
        let ([a, b], _) = edge_profile(surface, edge);
        Profile {
            h: if forward(edge) { [a, b] } else { [b, a] },
        }
    };
    Side {
        space: s.floor.is_some() || s.ceiling.is_some(),
        floor: s.floor.as_ref().map(profile),
        ceil: s.ceiling.as_ref().map(profile),
    }
}

fn sector_at(room: &GridRoom, cell: [i32; 2]) -> Option<&GridSector> {
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
fn between(lower: [i32; 2], upper: [i32; 2], anchor: [i32; 2]) -> Option<Vec<[i32; 2]>> {
    let len = SECTOR_UNITS;
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
        let host_edge = self.edge(host);
        let flip = !forward(host_edge);
        let len = SECTOR_UNITS;
        let ring: Vec<[i32; 2]> = ring
            .iter()
            .map(|p| [if flip { len - p[0] } else { p[0] }, p[1]])
            .collect();
        // A quad is `[a bottom, b bottom, b top, a top]` in the host's own
        // direction.
        let at_end = |u: i32| ring.iter().filter(|p| p[0] == u).count();
        let heights =
            (ring.len() == 4 && at_end(0) == 2 && at_end(len) == 2).then(|| quad_heights(&ring));
        let key = self.key;
        let ovr = self.room.edge_overrides.get(&key);
        let owner_sector = sector_at(self.room, self.cell(owner));
        let host_sector = sector_at(self.room, self.cell(host));
        let own_surface = |ceiling: bool| {
            let sector = owner_sector?;
            let surface = if ceiling {
                sector.ceiling.as_ref()
            } else {
                sector.floor.as_ref()
            }?;
            let ([_, _], tri) = edge_profile(surface, self.edge(owner));
            surface.tri(tri).material
        };
        let chained = owner_sector
            .and_then(|s| s.wall_material)
            .or_else(|| host_sector.and_then(|s| s.wall_material))
            .or(self.room.defaults.wall_material)
            .or_else(|| own_surface(kind == DerivedKind::Drop));
        DerivedWall {
            kind,
            host: self.cell(host),
            edge: host_edge,
            owner: self.cell(owner),
            key,
            ring,
            heights,
            material: ovr.and_then(|o| o.material).or(chained),
            hidden: ovr.is_some_and(|o| o.hidden),
            uv: ovr.and_then(|o| o.uv),
            solid: ovr.and_then(|o| o.solid),
        }
    }
}

/// `[bottom a, bottom b, top b, top a]` of a four-corner outline whose ends
/// are vertical, in the direction of increasing `u`.
fn quad_heights(ring: &[[i32; 2]]) -> [i32; 4] {
    let at = |u: i32, top: bool| {
        let ys = ring.iter().filter(|p| p[0] == u).map(|p| p[1]);
        if top {
            ys.max().unwrap_or(0)
        } else {
            ys.min().unwrap_or(0)
        }
    };
    [
        at(0, false),
        at(SECTOR_UNITS, false),
        at(SECTOR_UNITS, true),
        at(0, true),
    ]
}

/// Every derived wall on the physical edge of `cell`'s `edge`, whichever
/// sector hosts it. Empty for the diagonals.
pub fn physical_edge_walls(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> Vec<DerivedWall> {
    if edge.is_diagonal() {
        return Vec::new();
    }
    let (hi_cell, axis) = physical(cell, edge);
    let (lo_cell, lo_edge, hi_edge) = match axis {
        Axis::X => (
            [hi_cell[0], hi_cell[1] - 1],
            WallEdge::South,
            WallEdge::North,
        ),
        Axis::Z => ([hi_cell[0] - 1, hi_cell[1]], WallEdge::East, WallEdge::West),
    };
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
            None if room.defaults.open_wall > 0 => [
                floor.h[0] + room.defaults.open_wall,
                floor.h[1] + room.defaults.open_wall,
            ],
            None => continue,
        };
        if let Some(ring) = between(floor.h, top, floor.h) {
            out.push(e.wall(DerivedKind::Closure, pick, pick, &ring));
        }
    }
    out
}

/// Derived walls hosted by `edge` of `cell`, hidden ones included.
pub fn edge_walls(room: &GridRoom, cell: [i32; 2], edge: WallEdge) -> Vec<DerivedWall> {
    let mut walls = physical_edge_walls(room, cell, edge);
    walls.retain(|w| w.host == cell && w.edge == edge);
    walls
}

/// Derived walls hosted by `cell`, in edge order, hidden ones included.
pub fn cell_walls(room: &GridRoom, cell: [i32; 2]) -> Vec<DerivedWall> {
    CARDINAL
        .into_iter()
        .flat_map(|edge| edge_walls(room, cell, edge))
        .collect()
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

/// The run `wall` belongs to: the maximal chain of collinear faces on the
/// same side with the same kind, material and texture override, which a fit
/// mapping treats as one face.
pub fn run_extent(room: &GridRoom, wall: &DerivedWall) -> RunExtent {
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
            let seen_by_wall = match wall.facing {
                WallFacing::Front => d.host == cell && d.edge == edge,
                WallFacing::Back => d.host != cell || d.edge != edge,
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
