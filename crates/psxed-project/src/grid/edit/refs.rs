//! Stable references to the parts of a grid world.
//!
//! The editor selects, drags and edits faces, edges, vertices and whole
//! cells. All of them are addressed by the room index and the leaf's
//! [`LeafKey`] (world clicks), never by local storage coordinates, so a
//! reference survives the room growing or trimming around it. A wall is addressed by its index in
//! the edge's stack, so deleting walls must go from the highest index down
//! (see [`super::delete`]).

use super::super::derive::{self, DerivedKind, DerivedWall};
use super::super::leaf::{LeafKey, CLICKS_PER_SECTOR};
use super::super::overrides::WallMode;
use super::super::room::GridRoom;
use super::super::sector::{GridSector, GridSpan, WallEdge};
use super::super::surface::{GridSurface, GridWall};
use super::super::units::SECTOR_UNITS;
use super::super::world::GridWorld;

/// Floor or ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceKind {
    /// The surface you stand on.
    Floor,
    /// The surface overhead.
    Ceiling,
}

impl SurfaceKind {
    /// Lower-case label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Floor => "floor",
            Self::Ceiling => "ceiling",
        }
    }
}

/// What a face is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaceTarget {
    /// A floor or ceiling of one span of the leaf's column.
    Surface {
        /// Floor or ceiling.
        kind: SurfaceKind,
        /// Index of the span in the column, `0` the ground span. A selection
        /// is transient, so an index that shifts when a slab is inserted
        /// does no harm (`docs/grid/multi-storey.md`, 5.9).
        span: u8,
    },
    /// One wall in an edge's stack.
    Wall {
        /// Owning edge.
        edge: WallEdge,
        /// Index in the stack.
        index: usize,
    },
    /// A wall derived from a height difference, addressed by the sector
    /// that sees it, its edge and the rule that made it. Not stored, so it
    /// has no corners of its own; edits to it land in the room's
    /// [`EdgeOverride`](super::super::overrides::EdgeOverride)s.
    Derived {
        /// The host's edge it lies on.
        edge: WallEdge,
        /// Which rule made it.
        kind: DerivedKind,
        /// Which of the faces of that rule on that edge, in the order they
        /// lie along it: a cell with smaller neighbours across a side has
        /// two. Zero for a sector in a room of whole sectors.
        seg: u8,
        /// Index of the span of the host column that sees the face (`0` for a
        /// column of one span, and what [`FaceRef::derived`] makes): a column of
        /// several spans can stack faces of one rule on one edge.
        span: u8,
    },
}

/// One face of one sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FaceRef {
    /// Room index.
    pub room: usize,
    /// The leaf that owns the face: its level and world click. A whole
    /// sector is the level-2 leaf [`LeafKey::sector`].
    pub leaf: LeafKey,
    /// Which face of that leaf.
    pub target: FaceTarget,
}

impl FaceTarget {
    /// The floor of span `span`.
    pub const fn floor(span: u8) -> Self {
        Self::Surface {
            kind: SurfaceKind::Floor,
            span,
        }
    }

    /// The ceiling of span `span`.
    pub const fn ceiling(span: u8) -> Self {
        Self::Surface {
            kind: SurfaceKind::Ceiling,
            span,
        }
    }

    /// The floor or ceiling of the ground span.
    pub const fn ground(kind: SurfaceKind) -> Self {
        Self::Surface { kind, span: 0 }
    }

    /// The span a floor or ceiling belongs to; `None` for a wall.
    pub const fn span(self) -> Option<u8> {
        match self {
            Self::Surface { span, .. } => Some(span),
            _ => None,
        }
    }
}

/// Sector-local corner offsets `[x, z]` of corner `0..4` of a face. A
/// surface runs `[NW, NE, SE, SW]`; a wall runs `[bottom a, bottom b, top
/// b, top a]` along its edge's walking direction.
pub fn corner_offsets(target: FaceTarget) -> [[i32; 2]; 4] {
    match target {
        FaceTarget::Surface { .. } => [[0, 0], [1, 0], [1, 1], [0, 1]],
        FaceTarget::Wall { edge, .. } | FaceTarget::Derived { edge, .. } => {
            let (a, b) = edge.endpoints();
            [a, b, b, a]
        }
    }
}

impl FaceRef {
    /// A face of any leaf of `room`.
    pub const fn new(room: usize, leaf: LeafKey, target: FaceTarget) -> Self {
        Self { room, leaf, target }
    }

    /// The world sector `[x, z]` of a whole-sector leaf, or the sector that
    /// holds the corner of any other leaf (see [`LeafKey::cell`]).
    pub const fn cell(&self) -> [i32; 2] {
        self.leaf.cell()
    }

    /// The ground floor of `cell` in `room`.
    pub fn floor(room: usize, cell: [i32; 2]) -> Self {
        Self::floor_in(room, cell, 0)
    }

    /// The ground ceiling of `cell` in `room`.
    pub fn ceiling(room: usize, cell: [i32; 2]) -> Self {
        Self::ceiling_in(room, cell, 0)
    }

    /// The floor of span `span` of `cell` in `room`.
    pub fn floor_in(room: usize, cell: [i32; 2], span: u8) -> Self {
        Self::new(room, LeafKey::sector(cell), FaceTarget::floor(span))
    }

    /// The ceiling of span `span` of `cell` in `room`.
    pub fn ceiling_in(room: usize, cell: [i32; 2], span: u8) -> Self {
        Self::new(room, LeafKey::sector(cell), FaceTarget::ceiling(span))
    }

    /// The derived wall of `kind` on `edge` of `cell` in `room`.
    pub fn derived(room: usize, cell: [i32; 2], edge: WallEdge, kind: DerivedKind) -> Self {
        Self {
            room,
            leaf: LeafKey::sector(cell),
            target: FaceTarget::Derived {
                edge,
                kind,
                seg: 0,
                span: 0,
            },
        }
    }

    /// The face of the derived wall `wall` of `room`, span included.
    pub fn of_derived(room: usize, wall: &DerivedWall) -> Self {
        Self {
            room,
            leaf: LeafKey::sector(wall.host),
            target: FaceTarget::Derived {
                edge: wall.edge,
                kind: wall.kind,
                seg: 0,
                span: wall.span,
            },
        }
    }

    /// The derived wall this face names, if it is one and still exists.
    pub fn derived_wall(&self, world: &GridWorld) -> Option<DerivedWall> {
        let FaceTarget::Derived {
            edge,
            kind,
            seg,
            span,
        } = self.target
        else {
            return None;
        };
        let room = self.room_of(world)?;
        if room.wall_mode != WallMode::Derived {
            return None;
        }
        if !room.leaves.is_sectors_only() {
            return super::super::leaf_derive::derived_walls_of(room, self.leaf)
                .into_iter()
                .filter(|w| w.edge == edge && w.kind == kind)
                .nth(usize::from(seg));
        }
        let mut of_kind: Vec<DerivedWall> = derive::edge_walls(room, self.cell(), edge)
            .into_iter()
            .filter(|w| w.kind == kind)
            .collect();
        // One face of the rule needs no span; several are told apart by the
        // span that sees them, then by their order along the edge.
        if of_kind.len() > 1 {
            of_kind.retain(|w| w.span == span);
        }
        of_kind
            .into_iter()
            .nth(if seg == 0 { 0 } else { usize::from(seg) })
    }

    /// Wall `index` on `edge` of `cell` in `room`.
    pub fn wall(room: usize, cell: [i32; 2], edge: WallEdge, index: usize) -> Self {
        Self {
            room,
            leaf: LeafKey::sector(cell),
            target: FaceTarget::Wall { edge, index },
        }
    }

    /// The room, if it exists.
    pub fn room_of<'a>(&self, world: &'a GridWorld) -> Option<&'a GridRoom> {
        world.rooms.get(self.room)
    }

    /// The sector that owns the face.
    pub fn sector<'a>(&self, world: &'a GridWorld) -> Option<&'a GridSector> {
        self.room_of(world)?.leaf(self.leaf)
    }

    /// Mutable owning sector.
    pub fn sector_mut<'a>(&self, world: &'a mut GridWorld) -> Option<&'a mut GridSector> {
        world.rooms.get_mut(self.room)?.leaf_mut(self.leaf)
    }

    /// The surface, for floor and ceiling faces.
    pub fn surface<'a>(&self, world: &'a GridWorld) -> Option<&'a GridSurface> {
        let FaceTarget::Surface { kind, span } = self.target else {
            return None;
        };
        let span = self.sector(world)?.spans().get(usize::from(span))?;
        match kind {
            SurfaceKind::Floor => span.floor.as_ref(),
            SurfaceKind::Ceiling => span.ceiling.as_ref(),
        }
    }

    /// Mutable surface.
    pub fn surface_mut<'a>(&self, world: &'a mut GridWorld) -> Option<&'a mut GridSurface> {
        let FaceTarget::Surface { kind, span } = self.target else {
            return None;
        };
        let span = self
            .sector_mut(world)?
            .spans_mut()
            .get_mut(usize::from(span))?;
        match kind {
            SurfaceKind::Floor => span.floor.as_mut(),
            SurfaceKind::Ceiling => span.ceiling.as_mut(),
        }
    }

    /// The span of the leaf's column a floor or ceiling face belongs to.
    pub fn span_of<'a>(&self, world: &'a GridWorld) -> Option<&'a GridSpan> {
        let span = self.target.span()?;
        self.sector(world)?.spans().get(usize::from(span))
    }

    /// The wall, for wall faces.
    pub fn wall_of<'a>(&self, world: &'a GridWorld) -> Option<&'a GridWall> {
        let FaceTarget::Wall { edge, index } = self.target else {
            return None;
        };
        self.sector(world)?.walls(edge).get(index)
    }

    /// Mutable wall.
    pub fn wall_mut<'a>(&self, world: &'a mut GridWorld) -> Option<&'a mut GridWall> {
        let FaceTarget::Wall { edge, index } = self.target else {
            return None;
        };
        self.sector_mut(world)?.walls_mut(edge).get_mut(index)
    }

    /// Whether the face still exists.
    pub fn exists(&self, world: &GridWorld) -> bool {
        match self.target {
            FaceTarget::Surface { .. } => self.surface(world).is_some(),
            FaceTarget::Wall { .. } => self.wall_of(world).is_some(),
            FaceTarget::Derived { .. } => self.derived_wall(world).is_some(),
        }
    }

    /// Corner heights of one layer (`0` is the main set, `1` the second
    /// triangle's set of a split-level surface), grid units relative to the
    /// room's offset.
    pub fn heights(&self, world: &GridWorld, layer: u8) -> Option<[i32; 4]> {
        match self.target {
            FaceTarget::Surface { .. } => {
                let surface = self.surface(world)?;
                match layer {
                    0 => Some(surface.heights),
                    _ => surface.heights_b,
                }
            }
            FaceTarget::Wall { .. } => (layer == 0)
                .then(|| self.wall_of(world).map(|w| w.heights))
                .flatten(),
            FaceTarget::Derived { .. } => None,
        }
    }

    /// How many height layers the face has: `2` for a split-level surface,
    /// else `1`.
    pub fn layers(&self, world: &GridWorld) -> u8 {
        match self.surface(world) {
            Some(surface) if surface.heights_b.is_some() => 2,
            _ => 1,
        }
    }

    /// The four corners of every layer of the face.
    pub fn corners(&self, world: &GridWorld) -> Vec<CornerRef> {
        let mut out = Vec::new();
        if matches!(self.target, FaceTarget::Derived { .. }) {
            return out;
        }
        for layer in 0..self.layers(world) {
            for corner in 0..4 {
                out.push(CornerRef {
                    face: *self,
                    layer,
                    corner,
                });
            }
        }
        out
    }

    /// Grid position `[x, y, z]` of the face's centre in world grid units.
    pub fn centre(&self, world: &GridWorld) -> Option<[i32; 3]> {
        let corners = self.corners(world);
        let mut sum = [0i64; 3];
        let mut count = 0i64;
        for corner in corners.iter().filter(|c| c.layer == 0) {
            let p = corner.grid_pos(world)?;
            for axis in 0..3 {
                sum[axis] += i64::from(p[axis]);
            }
            count += 1;
        }
        (count > 0).then(|| sum.map(|v| (v / count) as i32))
    }
}

/// One height of one face, addressed by corner number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CornerRef {
    /// The face.
    pub face: FaceRef,
    /// Height layer, see [`FaceRef::heights`].
    pub layer: u8,
    /// Corner `0..4`, in the face's own order.
    pub corner: u8,
}

impl CornerRef {
    /// Current room-relative height.
    pub fn height(&self, world: &GridWorld) -> Option<i32> {
        self.face
            .heights(world, self.layer)
            .map(|h| h[usize::from(self.corner & 3)])
    }

    /// Grid position `[x, y, z]` in world grid units, y including the
    /// room's vertical offset.
    pub fn grid_pos(&self, world: &GridWorld) -> Option<[i32; 3]> {
        let room = self.face.room_of(world)?;
        let height = self.height(world)?;
        let offset = corner_offsets(self.face.target)[usize::from(self.corner & 3)];
        // The leaf's corner in clicks plus the offset in leaf edges; for a
        // whole sector this is the sector corner in sector units.
        let edge = self.face.leaf.edge_clicks();
        let at = |axis: usize| {
            let click = self.face.leaf.min_click()[axis] + offset[axis] * edge;
            click.saturating_mul(SECTOR_UNITS / CLICKS_PER_SECTOR)
        };
        Some([at(0), room.y_offset.saturating_add(height), at(1)])
    }

    /// Set the height so the corner sits at world grid height `world_y`.
    /// Returns whether anything changed.
    pub fn set_world_height(&self, world: &mut GridWorld, world_y: i32) -> bool {
        let Some(room) = world.rooms.get(self.face.room) else {
            return false;
        };
        let value = world_y - room.y_offset;
        let slot = usize::from(self.corner & 3);
        match self.face.target {
            FaceTarget::Surface { .. } => {
                let Some(surface) = self.face.surface_mut(world) else {
                    return false;
                };
                let heights = if self.layer == 0 {
                    &mut surface.heights
                } else {
                    let Some(h) = surface.heights_b.as_mut() else {
                        return false;
                    };
                    h
                };
                let changed = heights[slot] != value;
                heights[slot] = value;
                changed
            }
            FaceTarget::Wall { .. } => {
                let Some(wall) = self.face.wall_mut(world) else {
                    return false;
                };
                let changed = wall.heights[slot] != value;
                wall.heights[slot] = value;
                changed
            }
            // Derived faces follow the floors; they have no heights to set.
            FaceTarget::Derived { .. } => false,
        }
    }
}

/// One side of a face: the segment between corner `edge` and the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeRef {
    /// The face.
    pub face: FaceRef,
    /// Height layer.
    pub layer: u8,
    /// Edge `0..4`: it joins corner `edge` and `(edge + 1) % 4`.
    pub edge: u8,
}

impl EdgeRef {
    /// The two end corners.
    pub fn corners(&self) -> [CornerRef; 2] {
        let a = self.edge & 3;
        [
            CornerRef {
                face: self.face,
                layer: self.layer,
                corner: a,
            },
            CornerRef {
                face: self.face,
                layer: self.layer,
                corner: (a + 1) & 3,
            },
        ]
    }
}

/// Something the user can select.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GridElement {
    /// A whole sector: its floor, ceiling and every wall.
    Cell {
        /// Room index.
        room: usize,
        /// World sector.
        cell: [i32; 2],
    },
    /// One face.
    Face(FaceRef),
    /// One edge of a face.
    Edge(EdgeRef),
    /// One corner of a face.
    Vertex(CornerRef),
}

impl GridElement {
    /// Room the element lives in.
    pub fn room(&self) -> usize {
        match self {
            Self::Cell { room, .. } => *room,
            Self::Face(f) => f.room,
            Self::Edge(e) => e.face.room,
            Self::Vertex(v) => v.face.room,
        }
    }

    /// World sector the element lives in.
    pub fn cell(&self) -> [i32; 2] {
        match self {
            Self::Cell { cell, .. } => *cell,
            Self::Face(f) => f.cell(),
            Self::Edge(e) => e.face.cell(),
            Self::Vertex(v) => v.face.cell(),
        }
    }
}

/// Every face of a leaf: floor, ceiling, then walls edge by edge.
pub fn faces_of_leaf(world: &GridWorld, room: usize, leaf: LeafKey) -> Vec<FaceRef> {
    let mut out = Vec::new();
    let Some(sector) = world.rooms.get(room).and_then(|r| r.leaf(leaf)) else {
        return out;
    };
    let face = |target| FaceRef::new(room, leaf, target);
    for (index, span) in sector.spans().iter().enumerate() {
        let span_index = u8::try_from(index).unwrap_or(u8::MAX);
        if span.floor.is_some() {
            out.push(face(FaceTarget::floor(span_index)));
        }
        if span.ceiling.is_some() {
            out.push(face(FaceTarget::ceiling(span_index)));
        }
    }
    for edge in WallEdge::ALL {
        for index in 0..sector.walls(edge).len() {
            out.push(face(FaceTarget::Wall { edge, index }));
        }
    }
    out
}

/// Every face listed under a sector: those of the leaves whose minimum
/// corner lies in it (the sector itself, or the leaves inside it, or a leaf
/// above a sector that starts there), floor, ceiling, then walls edge by
/// edge. A leaf above a sector is listed under its north-west sector only,
/// so walking every cell of a room names each face once.
pub fn faces_of_cell(world: &GridWorld, room: usize, cell: [i32; 2]) -> Vec<FaceRef> {
    let Some(r) = world.rooms.get(room) else {
        return Vec::new();
    };
    r.leaves
        .in_sector(cell)
        .into_iter()
        .filter(|(key, _)| key.cell() == cell)
        .flat_map(|(key, _)| faces_of_leaf(world, room, key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::units::GridScale;

    pub(crate) fn world_with(configure: impl FnOnce(&mut GridRoom)) -> GridWorld {
        let mut room = GridRoom::new("r", 2, 2);
        room.origin = [4, -2];
        configure(&mut room);
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    #[test]
    fn faces_resolve_through_world_cells() {
        let world = world_with(|r| {
            r.sector_or_insert(1, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(256, None)));
            r.sector_or_insert(1, 0)
                .unwrap()
                .east
                .push(GridWall::span(0, 512, None));
        });
        let floor = FaceRef::floor(0, [5, -2]);
        assert!(floor.exists(&world));
        assert!(!FaceRef::ceiling(0, [5, -2]).exists(&world));
        assert!(!FaceRef::floor(0, [4, -2]).exists(&world));
        let wall = FaceRef::wall(0, [5, -2], WallEdge::East, 0);
        assert_eq!(wall.heights(&world, 0), Some([0, 0, 512, 512]));
        assert!(!FaceRef::wall(0, [5, -2], WallEdge::East, 1).exists(&world));
        assert_eq!(faces_of_cell(&world, 0, [5, -2]), vec![floor, wall]);
    }

    #[test]
    fn corner_positions_follow_the_face_order() {
        let world = world_with(|r| {
            r.y_offset = 100;
            let sector = r.sector_or_insert(0, 0).unwrap();
            let mut floor = GridSurface::flat(0, None);
            floor.heights = [10, 20, 30, 40];
            sector.set_ground_floor(Some(floor));
            sector.north.push(GridWall::span(0, 512, None));
        });
        let corner = |face, corner| CornerRef {
            face,
            layer: 0,
            corner,
        };
        let floor = FaceRef::floor(0, [4, -2]);
        // NE corner of the sector at world cell (4, -2).
        assert_eq!(
            corner(floor, 1).grid_pos(&world),
            Some([5 * 1024, 120, -2 * 1024])
        );
        let wall = FaceRef::wall(0, [4, -2], WallEdge::North, 0);
        // North walks west to east: corner 2 is the top at the east end.
        assert_eq!(
            corner(wall, 2).grid_pos(&world),
            Some([5 * 1024, 100 + 512, -2 * 1024])
        );
        assert_eq!(
            corner(wall, 3).grid_pos(&world),
            Some([4 * 1024, 100 + 512, -2 * 1024])
        );
    }

    #[test]
    fn setting_a_corner_uses_the_room_offset() {
        let mut world = world_with(|r| {
            r.y_offset = 100;
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(GridSurface::flat(0, None)));
        });
        let corner = CornerRef {
            face: FaceRef::floor(0, [4, -2]),
            layer: 0,
            corner: 2,
        };
        assert!(corner.set_world_height(&mut world, 612));
        assert_eq!(corner.height(&world), Some(512));
        assert!(!corner.set_world_height(&mut world, 612));
        // Layer 1 does not exist on a plain surface.
        let b = CornerRef { layer: 1, ..corner };
        assert_eq!(b.height(&world), None);
        assert!(!b.set_world_height(&mut world, 0));
    }

    #[test]
    fn split_level_surfaces_expose_both_layers() {
        let world = world_with(|r| {
            let mut floor = GridSurface::flat(0, None);
            floor.heights_b = Some([256; 4]);
            r.sector_or_insert(0, 0)
                .unwrap()
                .set_ground_floor(Some(floor));
        });
        let face = FaceRef::floor(0, [4, -2]);
        assert_eq!(face.layers(&world), 2);
        assert_eq!(face.corners(&world).len(), 8);
        assert_eq!(face.heights(&world, 1), Some([256; 4]));
    }
}
