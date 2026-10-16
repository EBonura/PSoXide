//! The selection set and the range rules that extend it.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use super::super::sector::WallEdge;
use super::super::units::SECTOR_UNITS;
use super::super::world::GridWorld;
use super::rects::CellRect;
use super::refs::{faces_of_cell, CornerRef, FaceRef, FaceTarget, GridElement, SurfaceKind};

/// What is selected, in the order it was picked. The last element is the
/// primary one: range selection extends from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GridSelection {
    items: Vec<GridElement>,
}

impl GridSelection {
    /// An empty selection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The selected elements, oldest first.
    pub fn items(&self) -> &[GridElement] {
        &self.items
    }

    /// Whether nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of selected elements.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// The most recently picked element.
    pub fn primary(&self) -> Option<&GridElement> {
        self.items.last()
    }

    /// Whether `element` is selected.
    pub fn contains(&self, element: &GridElement) -> bool {
        self.items.contains(element)
    }

    /// Deselect everything.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Select only `element`.
    pub fn replace(&mut self, element: GridElement) {
        self.items.clear();
        self.items.push(element);
    }

    /// Select exactly `elements`.
    pub fn set(&mut self, elements: impl IntoIterator<Item = GridElement>) {
        self.items.clear();
        self.extend(elements);
    }

    /// Add `element` if it is not selected yet (and make it primary).
    pub fn add(&mut self, element: GridElement) {
        if let Some(at) = self.items.iter().position(|e| *e == element) {
            self.items.remove(at);
        }
        self.items.push(element);
    }

    /// Add several elements, keeping the first of any duplicates.
    pub fn extend(&mut self, elements: impl IntoIterator<Item = GridElement>) {
        for element in elements {
            if !self.items.contains(&element) {
                self.items.push(element);
            }
        }
    }

    /// Ctrl-click: remove `element` when selected, else add it.
    pub fn toggle(&mut self, element: GridElement) {
        if let Some(at) = self.items.iter().position(|e| *e == element) {
            self.items.remove(at);
        } else {
            self.items.push(element);
        }
    }

    /// Drop elements that no longer exist in `world`. Returns how many were
    /// dropped.
    pub fn retain_valid(&mut self, world: &GridWorld) -> usize {
        let before = self.items.len();
        self.items.retain(|element| match element {
            GridElement::Cell { room, cell } => !faces_of_cell(world, *room, *cell).is_empty(),
            GridElement::Face(face) => face.exists(world),
            GridElement::Edge(edge) => edge.face.exists(world),
            GridElement::Vertex(corner) => corner.height(world).is_some(),
        });
        before - self.items.len()
    }

    /// Every face the selection touches: a face stands for itself, a cell
    /// for all its faces, an edge or vertex for the face that owns it.
    pub fn faces(&self, world: &GridWorld) -> Vec<FaceRef> {
        let mut set: BTreeSet<FaceRef> = BTreeSet::new();
        for element in &self.items {
            match element {
                GridElement::Cell { room, cell } => {
                    set.extend(faces_of_cell(world, *room, *cell));
                }
                GridElement::Face(face) => {
                    set.insert(*face);
                }
                GridElement::Edge(edge) => {
                    set.insert(edge.face);
                }
                GridElement::Vertex(corner) => {
                    set.insert(corner.face);
                }
            }
        }
        set.into_iter().filter(|f| f.exists(world)).collect()
    }

    /// Corners that a height edit of this selection moves: a vertex is
    /// itself, an edge its two ends, a face or cell all its corners.
    pub fn corners(&self, world: &GridWorld) -> Vec<CornerRef> {
        let mut set: BTreeSet<CornerRef> = BTreeSet::new();
        for element in &self.items {
            match element {
                GridElement::Vertex(corner) => {
                    set.insert(*corner);
                }
                GridElement::Edge(edge) => set.extend(edge.corners()),
                GridElement::Face(face) => set.extend(face.corners(world)),
                GridElement::Cell { room, cell } => {
                    for face in faces_of_cell(world, *room, *cell) {
                        set.extend(face.corners(world));
                    }
                }
            }
        }
        set.into_iter()
            .filter(|c| c.height(world).is_some())
            .collect()
    }

    /// Distinct `(room, world sector)` pairs the selection touches.
    pub fn cells(&self) -> BTreeSet<(usize, [i32; 2])> {
        self.items.iter().map(|e| (e.room(), e.cell())).collect()
    }
}

/// Floors or ceilings of `kind` in the bounding rectangle of `anchor` and
/// `to` (cells in the same room), the Shift-click rule for surfaces.
pub fn range_surfaces(
    world: &GridWorld,
    room: usize,
    kind: SurfaceKind,
    anchor: &[[i32; 2]],
    to: [i32; 2],
) -> Vec<FaceRef> {
    let mut rect = CellRect::from_corners(to, to);
    for cell in anchor {
        rect.min = [rect.min[0].min(cell[0]), rect.min[1].min(cell[1])];
        rect.max = [rect.max[0].max(cell[0]), rect.max[1].max(cell[1])];
    }
    rect.cells()
        .map(|cell| FaceRef {
            room,
            cell,
            target: FaceTarget::Surface(kind),
        })
        .filter(|f| f.exists(world))
        .collect()
}

type Segment = ([i32; 2], [i32; 2]);

fn wall_segment(face: &FaceRef) -> Option<Segment> {
    let FaceTarget::Wall { edge, .. } = face.target else {
        return None;
    };
    let (a, b) = edge.endpoints();
    let at = |e: [i32; 2]| {
        [
            (face.cell[0] + e[0]).saturating_mul(SECTOR_UNITS),
            (face.cell[1] + e[1]).saturating_mul(SECTOR_UNITS),
        ]
    };
    Some((at(a), at(b)))
}

/// Walls on a connected path from `from` to `to`, the Shift-click rule for
/// walls. Two wall columns connect when they share an end point. The path
/// found is the shortest one; every wall of each column between the two
/// stack indices (inclusive) is returned. Returns just the two walls when
/// no path joins them.
pub fn range_walls(world: &GridWorld, from: FaceRef, to: FaceRef) -> Vec<FaceRef> {
    let (Some(FaceTarget::Wall { index: i0, .. }), Some(FaceTarget::Wall { index: i1, .. })) =
        (Some(from.target), Some(to.target))
    else {
        return vec![from, to];
    };
    if from.room != to.room {
        return vec![from, to];
    }
    let Some(room) = world.rooms.get(from.room) else {
        return vec![from, to];
    };
    // One node per (cell, edge) column of walls.
    type Column = ([i32; 2], WallEdge);
    let mut columns: Vec<(Column, Segment, usize)> = Vec::new();
    for (x, z, sector) in room.iter() {
        let cell = room.world_cell(x, z);
        for edge in WallEdge::ALL {
            let count = sector.walls(edge).len();
            if count == 0 {
                continue;
            }
            let probe = FaceRef::wall(from.room, cell, edge, 0);
            if let Some(segment) = wall_segment(&probe) {
                columns.push(((cell, edge), segment, count));
            }
        }
    }
    let column_of = |face: &FaceRef| match face.target {
        FaceTarget::Wall { edge, .. } => columns
            .iter()
            .position(|(key, _, _)| *key == (face.cell, edge)),
        FaceTarget::Surface(_) => None,
    };
    let (Some(start), Some(goal)) = (column_of(&from), column_of(&to)) else {
        return vec![from, to];
    };
    let mut by_point: HashMap<[i32; 2], Vec<usize>> = HashMap::new();
    for (i, (_, (a, b), _)) in columns.iter().enumerate() {
        by_point.entry(*a).or_default().push(i);
        by_point.entry(*b).or_default().push(i);
    }
    let mut parent: HashMap<usize, usize> = HashMap::new();
    let mut seen: HashSet<usize> = HashSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(current) = queue.pop_front() {
        if current == goal {
            break;
        }
        let (_, (a, b), _) = columns[current];
        for point in [a, b] {
            for &next in by_point.get(&point).into_iter().flatten() {
                if seen.insert(next) {
                    parent.insert(next, current);
                    queue.push_back(next);
                }
            }
        }
    }
    if !seen.contains(&goal) {
        return vec![from, to];
    }
    let mut path = vec![goal];
    while let Some(&p) = parent.get(path.last().unwrap()) {
        path.push(p);
    }
    let (lo, hi) = (i0.min(i1), i0.max(i1));
    let mut out = Vec::new();
    for column in path.into_iter().rev() {
        let ((cell, edge), _, count) = columns[column];
        for index in lo..=hi.min(count.saturating_sub(1)) {
            out.push(FaceRef::wall(from.room, cell, edge, index));
        }
    }
    out
}

/// Corners lying on the straight line between two corners (same room), the
/// Shift-click rule for vertices. The line must be axis-aligned or at 45
/// degrees in plan; any other pair returns just the two corners.
pub fn range_corners(world: &GridWorld, a: CornerRef, b: CornerRef) -> Vec<CornerRef> {
    let (Some(pa), Some(pb)) = (a.grid_pos(world), b.grid_pos(world)) else {
        return vec![a, b];
    };
    let (dx, dz) = (pb[0] - pa[0], pb[2] - pa[2]);
    let straight = dx == 0 || dz == 0 || dx.abs() == dz.abs();
    if !straight || a.face.room != b.face.room {
        return vec![a, b];
    }
    let Some(room) = world.rooms.get(a.face.room) else {
        return vec![a, b];
    };
    let mut out: BTreeSet<CornerRef> = BTreeSet::from([a, b]);
    let on_segment = |p: [i32; 3]| {
        let (ex, ez) = (p[0] - pa[0], p[2] - pa[2]);
        let cross = i64::from(ex) * i64::from(dz) - i64::from(ez) * i64::from(dx);
        let within = |v: i32, lo: i32, hi: i32| v >= lo.min(hi) && v <= lo.max(hi);
        cross == 0 && within(p[0], pa[0], pb[0]) && within(p[2], pa[2], pb[2])
    };
    for (x, z, _) in room.iter() {
        let cell = room.world_cell(x, z);
        for face in faces_of_cell(world, a.face.room, cell) {
            // Same kind of face as the anchor, so a floor range does not
            // also pull in the walls standing on it.
            let same_kind =
                std::mem::discriminant(&face.target) == std::mem::discriminant(&a.face.target);
            if !same_kind {
                continue;
            }
            for corner in face.corners(world) {
                if corner.grid_pos(world).is_some_and(on_segment) {
                    out.insert(corner);
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Every cell of `room` inside `rect` that holds geometry, as selectable
/// elements (the 2D box select).
pub fn cells_in_rect(world: &GridWorld, room: usize, rect: CellRect) -> Vec<GridElement> {
    let Some(r) = world.rooms.get(room) else {
        return Vec::new();
    };
    rect.cells()
        .filter(|cell| {
            r.local(*cell)
                .and_then(|[x, z]| r.sector(x, z))
                .is_some_and(|s| !s.is_empty())
        })
        .map(|cell| GridElement::Cell { room, cell })
        .collect()
}

/// Every face of `room` (Ctrl+A), diagonal walls included.
pub fn all_faces_of_room(world: &GridWorld, room: usize) -> Vec<FaceRef> {
    let Some(r) = world.rooms.get(room) else {
        return Vec::new();
    };
    r.iter()
        .flat_map(|(x, z, _)| faces_of_cell(world, room, r.world_cell(x, z)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::edit::refs::EdgeRef;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::GridScale;

    fn world(width: usize, depth: usize) -> GridWorld {
        let mut room = GridRoom::new("r", width, depth);
        for z in 0..depth {
            for x in 0..width {
                room.sector_or_insert(x, z).unwrap().floor = Some(GridSurface::flat(0, None));
            }
        }
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
        }
    }

    fn floor(cell: [i32; 2]) -> GridElement {
        GridElement::Face(FaceRef::floor(0, cell))
    }

    #[test]
    fn replace_toggle_and_primary() {
        let mut sel = GridSelection::new();
        sel.add(floor([0, 0]));
        sel.add(floor([1, 0]));
        assert_eq!(sel.primary(), Some(&floor([1, 0])));
        sel.toggle(floor([0, 0]));
        assert_eq!(sel.items(), &[floor([1, 0])]);
        sel.toggle(floor([2, 0]));
        assert_eq!(sel.len(), 2);
        sel.replace(floor([2, 2]));
        assert_eq!(sel.items(), &[floor([2, 2])]);
        // Re-adding moves it to the primary slot instead of duplicating.
        sel.add(floor([0, 0]));
        sel.add(floor([2, 2]));
        assert_eq!(sel.len(), 2);
        assert_eq!(sel.primary(), Some(&floor([2, 2])));
    }

    #[test]
    fn cells_expand_to_all_their_faces_and_corners() {
        let mut w = world(1, 1);
        w.rooms[0]
            .sector_or_insert(0, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 512, None));
        let mut sel = GridSelection::new();
        sel.add(GridElement::Cell {
            room: 0,
            cell: [0, 0],
        });
        assert_eq!(sel.faces(&w).len(), 2);
        assert_eq!(sel.corners(&w).len(), 8);
        // A vertex stands for its face; an edge for two corners.
        let vertex = CornerRef {
            face: FaceRef::floor(0, [0, 0]),
            layer: 0,
            corner: 1,
        };
        sel.set([GridElement::Vertex(vertex)]);
        assert_eq!(sel.faces(&w), vec![FaceRef::floor(0, [0, 0])]);
        assert_eq!(sel.corners(&w), vec![vertex]);
        sel.set([GridElement::Edge(EdgeRef {
            face: FaceRef::floor(0, [0, 0]),
            layer: 0,
            edge: 3,
        })]);
        assert_eq!(sel.corners(&w).len(), 2);
    }

    #[test]
    fn stale_elements_are_pruned() {
        let mut w = world(2, 1);
        let mut sel = GridSelection::new();
        sel.extend([floor([0, 0]), floor([1, 0]), floor([5, 5])]);
        assert_eq!(sel.retain_valid(&w), 1);
        w.rooms[0].sector_mut(0, 0).unwrap().floor = None;
        assert_eq!(sel.retain_valid(&w), 1);
        assert_eq!(sel.items(), &[floor([1, 0])]);
    }

    #[test]
    fn surface_range_is_the_bounding_rectangle() {
        let mut w = world(4, 4);
        w.rooms[0].sector_mut(2, 2).unwrap().floor = None;
        let faces = range_surfaces(&w, 0, SurfaceKind::Floor, &[[0, 0]], [2, 2]);
        // 3x3 rectangle minus the missing floor.
        assert_eq!(faces.len(), 8);
        let two = range_surfaces(&w, 0, SurfaceKind::Floor, &[[1, 1], [0, 3]], [3, 0]);
        assert_eq!(two.len(), 15, "{two:?}");
    }

    fn walls_around(w: &mut GridWorld) {
        // A row of cells with a north wall each, plus a stack of two on the
        // third and a detached wall far away.
        for x in 0..4 {
            w.rooms[0]
                .sector_mut(x, 0)
                .unwrap()
                .north
                .push(GridWall::span(0, 512, None));
        }
        w.rooms[0]
            .sector_mut(2, 0)
            .unwrap()
            .north
            .push(GridWall::span(512, 1024, None));
    }

    #[test]
    fn wall_range_follows_connected_walls_and_spans_stack_layers() {
        let mut w = world(6, 1);
        walls_around(&mut w);
        w.rooms[0]
            .sector_mut(5, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 512, None));
        let n = |x, i| FaceRef::wall(0, [x, 0], WallEdge::North, i);
        let layer0 = range_walls(&w, n(0, 0), n(3, 0));
        assert_eq!(layer0, vec![n(0, 0), n(1, 0), n(2, 0), n(3, 0)]);
        let both = range_walls(&w, n(0, 0), n(3, 1));
        // Layers 0..=1 where they exist: only cell 2 has a second layer.
        assert_eq!(both.len(), 5);
        assert!(both.contains(&n(2, 1)));
        // Cell 5 is not connected to cell 3 (cell 4 has no wall).
        assert_eq!(range_walls(&w, n(0, 0), n(5, 0)), vec![n(0, 0), n(5, 0)]);
    }

    #[test]
    fn wall_range_turns_corners() {
        let mut w = world(2, 2);
        w.rooms[0]
            .sector_mut(0, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 512, None));
        w.rooms[0]
            .sector_mut(0, 0)
            .unwrap()
            .west
            .push(GridWall::span(0, 512, None));
        w.rooms[0]
            .sector_mut(0, 1)
            .unwrap()
            .west
            .push(GridWall::span(0, 512, None));
        let a = FaceRef::wall(0, [0, 0], WallEdge::North, 0);
        let b = FaceRef::wall(0, [0, 1], WallEdge::West, 0);
        let path = range_walls(&w, a, b);
        assert_eq!(path.len(), 3);
        assert!(path.contains(&FaceRef::wall(0, [0, 0], WallEdge::West, 0)));
    }

    #[test]
    fn corner_range_runs_along_a_straight_line_of_one_face_kind() {
        let mut w = world(3, 1);
        w.rooms[0]
            .sector_mut(1, 0)
            .unwrap()
            .north
            .push(GridWall::span(0, 512, None));
        let nw = |x| CornerRef {
            face: FaceRef::floor(0, [x, 0]),
            layer: 0,
            corner: 0,
        };
        // NW of cell 0 and NW of cell 2 are on the north line.
        let line = range_corners(&w, nw(0), nw(2));
        // North line vertices: x = 0, 1, 2, 3 are not all included, only
        // the 0..=2 span; each interior vertex appears once per floor face
        // that owns it (NW of cell 1, NE of cell 0, ...).
        let xs: BTreeSet<i32> = line
            .iter()
            .map(|c| c.grid_pos(&w).unwrap()[0] / 1024)
            .collect();
        assert_eq!(xs, BTreeSet::from([0, 1, 2]));
        assert!(line
            .iter()
            .all(|c| matches!(c.face.target, FaceTarget::Surface(SurfaceKind::Floor))));
        // An off-axis pair returns just the two.
        let off = CornerRef {
            face: FaceRef::floor(0, [1, 0]),
            layer: 0,
            corner: 2,
        };
        let w2 = world(3, 3);
        let off_line = range_corners(&w2, nw(0), off);
        assert_eq!(off_line.len(), 2);
    }

    #[test]
    fn box_and_select_all_helpers() {
        let mut w = world(3, 3);
        w.rooms[0].sector_mut(1, 1).unwrap().floor = None;
        let cells = cells_in_rect(&w, 0, CellRect::from_corners([0, 0], [1, 1]));
        assert_eq!(cells.len(), 3, "the emptied cell has nothing to select");
        w.rooms[0]
            .sector_mut(0, 0)
            .unwrap()
            .nwse
            .push(GridWall::span(0, 512, None));
        let all = all_faces_of_room(&w, 0);
        assert_eq!(all.len(), 8 + 1, "diagonal walls are included");
    }
}
