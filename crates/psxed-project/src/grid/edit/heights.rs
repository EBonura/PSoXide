//! Raising and lowering corners, with coincident corners welded together.
//!
//! A corner is "coincident" with another when both sit at exactly the same
//! world grid position (rooms included), so neighbouring floors, the walls
//! standing on them and the matching corners of an overlapping room all move
//! as one unless linking is switched off. The set of moving corners is fixed
//! when the gesture starts; every update then sets absolute heights from the
//! start values, so a drag can be repeated, reversed or cancelled exactly.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::super::auto_split;
use super::super::surface::GridSplit;
use super::super::world::GridWorld;
use super::refs::{faces_of_cell, CornerRef, FaceRef, FaceTarget};

/// Every corner of every face in the world, bucketed by grid position.
pub struct WeldIndex {
    buckets: HashMap<[i32; 3], Vec<CornerRef>>,
}

impl WeldIndex {
    /// Index the whole world.
    pub fn build(world: &GridWorld) -> Self {
        let mut buckets: HashMap<[i32; 3], Vec<CornerRef>> = HashMap::new();
        for (room_index, room) in world.rooms.iter().enumerate() {
            for [x, z] in room.face_cells() {
                let cell = room.world_cell(x, z);
                for face in faces_of_cell(world, room_index, cell) {
                    for corner in face.corners(world) {
                        if let Some(pos) = corner.grid_pos(world) {
                            buckets.entry(pos).or_default().push(corner);
                        }
                    }
                }
            }
        }
        Self { buckets }
    }

    /// Corners at the same position as `corner`, itself included.
    pub fn coincident(&self, world: &GridWorld, corner: &CornerRef) -> Vec<CornerRef> {
        corner
            .grid_pos(world)
            .and_then(|pos| self.buckets.get(&pos))
            .cloned()
            .unwrap_or_else(|| vec![*corner])
    }
}

/// The child of a leaf that holds corner `corner` (`[NW, NE, SE, SW]`) of it.
const fn corner_child(corner: u8) -> usize {
    match corner & 3 {
        0 => 0,
        1 => 1,
        2 => 3,
        _ => 2,
    }
}

/// The corner of the leaf that exists now: a leaf that was subdivided since
/// the corner was named is descended to the child that holds it.
fn resolve(world: &GridWorld, corner: CornerRef) -> CornerRef {
    if !matches!(corner.face.target, FaceTarget::Surface { .. }) {
        return corner;
    }
    let Some(room) = world.rooms.get(corner.face.room) else {
        return corner;
    };
    let mut key = corner.face.leaf;
    while room.leaf(key).is_none() {
        match key.children() {
            Some(children) => key = children[corner_child(corner.corner)],
            None => return corner,
        }
    }
    CornerRef {
        face: FaceRef {
            leaf: key,
            ..corner.face
        },
        ..corner
    }
}

/// The leaves above a sector whose moving corners would bend them: moving
/// the same amount of a plane's corners keeps it a plane only when as many
/// of the corners `NW, SE` move as of `NE, SW`.
fn bent_by(corners: &BTreeSet<CornerRef>) -> BTreeSet<(usize, crate::grid::leaf::LeafKey)> {
    let mut moved: BTreeMap<(usize, crate::grid::leaf::LeafKey), [i32; 2]> = BTreeMap::new();
    for c in corners {
        if c.face.leaf.level >= crate::grid::leaf::SECTOR_LEVEL
            || !matches!(c.face.target, FaceTarget::Surface { .. })
        {
            continue;
        }
        let entry = moved.entry((c.face.room, c.face.leaf)).or_default();
        entry[usize::from(c.corner & 1)] += 1;
    }
    moved
        .into_iter()
        .filter(|(_, n)| n[0] != n[1])
        .map(|(k, _)| k)
        .collect()
}

/// Make `corners` (and, with `link`, the corners welded to them) corners that
/// can move without bending a leaf above a sector: such a leaf is one plane,
/// so one whose corners would not all move alike is subdivided first (down
/// to sectors where a single corner is grabbed). Subdividing is lossless; the
/// document looks the same afterwards. Returns the corners to move, and
/// `corners` themselves on the refined leaves (same order), for a selection
/// to follow.
pub fn refine_corners(
    world: &mut GridWorld,
    corners: &[CornerRef],
    link: bool,
) -> (Vec<CornerRef>, Vec<CornerRef>) {
    let mut inputs: Vec<CornerRef> = corners.to_vec();
    let mut set: BTreeSet<CornerRef> = inputs.iter().copied().collect();
    loop {
        if link {
            let index = WeldIndex::build(world);
            for corner in set.clone() {
                set.extend(index.coincident(world, &corner));
            }
        }
        let bent = bent_by(&set);
        if bent.is_empty() {
            return (set.into_iter().collect(), inputs);
        }
        for (room, key) in bent {
            if let Some(room) = world.rooms.get_mut(room) {
                if room.leaf(key).is_some() {
                    let _ = crate::grid::tree::subdivide(room, key);
                }
            }
        }
        set = set.into_iter().map(|c| resolve(world, c)).collect();
        for input in &mut inputs {
            *input = resolve(world, *input);
        }
    }
}

/// What an auto-split surface held when the drag began: diagonal and heights.
type SurfaceStart = (GridSplit, [i32; 4], Option<[i32; 4]>);

/// A started height gesture: the corners that move and where they began.
#[derive(Clone, Debug, Default)]
pub struct HeightDrag {
    targets: Vec<(CornerRef, i32)>,
    /// The diagonal and heights each moving auto-split surface started with.
    /// Every apply restores the diagonal before choosing again, so the
    /// result depends only on the heights and a drag back to zero puts every
    /// diagonal back too.
    splits: BTreeMap<FaceRef, SurfaceStart>,
    /// Lowest and highest start height of the moving corners, world grid
    /// units, for the UI's hint of how far it may go.
    pub range: (i32, i32),
}

impl HeightDrag {
    /// Start moving `corners`. With `link` every corner coincident with one
    /// of them moves too.
    pub fn begin(world: &GridWorld, corners: &[CornerRef], link: bool) -> Self {
        let mut set: BTreeSet<CornerRef> = corners.iter().copied().collect();
        if link {
            let index = WeldIndex::build(world);
            for corner in corners {
                set.extend(index.coincident(world, corner));
            }
        }
        let targets: Vec<(CornerRef, i32)> = set
            .into_iter()
            .filter_map(|c| Some((c, c.grid_pos(world)?[1])))
            .collect();
        let range = targets
            .iter()
            .fold((i32::MAX, i32::MIN), |(lo, hi), (_, y)| {
                (lo.min(*y), hi.max(*y))
            });
        let mut splits = BTreeMap::new();
        for (corner, _) in &targets {
            if !matches!(corner.face.target, FaceTarget::Surface { .. }) {
                continue;
            }
            if let Some(surface) = corner.face.surface(world) {
                if surface.auto_split {
                    splits.insert(
                        corner.face,
                        (surface.split, surface.heights, surface.heights_b),
                    );
                }
            }
        }
        Self {
            targets,
            splits,
            range,
        }
    }

    /// [`Self::begin`] after making the corners ones that can move alone (see
    /// [`refine_corners`]); the world may gain leaves, none of which changes
    /// what it looks like.
    pub fn begin_refining(world: &mut GridWorld, corners: &[CornerRef], link: bool) -> Self {
        Self::begin_following(world, corners, link).0
    }

    /// [`Self::begin_refining`], also returning `corners` on the refined
    /// leaves so a selection can follow them.
    pub fn begin_following(
        world: &mut GridWorld,
        corners: &[CornerRef],
        link: bool,
    ) -> (Self, Vec<CornerRef>) {
        let (moving, followed) = refine_corners(world, corners, link);
        // Linking is done: the set is closed under coincidence already.
        (Self::begin(world, &moving, false), followed)
    }

    /// Number of corners that will move.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether nothing would move.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Place every corner at its start height plus `delta` grid units.
    /// Returns whether any height changed.
    pub fn apply(&self, world: &mut GridWorld, delta: i32) -> bool {
        let mut changed = false;
        for (corner, start) in &self.targets {
            changed |= corner.set_world_height(world, start.saturating_add(delta));
        }
        // Where a corner moved, hanging corners of finer neighbours follow
        // the coarse edge they lie on.
        let mut rooms: Vec<usize> = self.targets.iter().map(|(c, _)| c.face.room).collect();
        rooms.sort_unstable();
        rooms.dedup();
        for room in rooms {
            if let Some(room) = world.rooms.get_mut(room) {
                if !room.leaves.is_sectors_only() {
                    crate::grid::tree::normalize(room);
                }
            }
        }
        // Surfaces that follow their heights choose their diagonal again,
        // from where it started.
        for (face, (split, heights, heights_b)) in &self.splits {
            if let Some(surface) = face.surface_mut(world) {
                surface.split = *split;
                // Only a surface that moved chooses again, so a drag back to
                // zero puts the diagonal back too.
                if surface.heights != *heights || surface.heights_b != *heights_b {
                    auto_split::resplit(surface);
                }
            }
        }
        changed
    }

    /// Faces whose corners move, for repainting and budget updates.
    pub fn faces(&self) -> BTreeSet<FaceRef> {
        self.targets.iter().map(|(c, _)| c.face).collect()
    }
}

/// Move `corners` by `delta` in one step (keyboard nudges and tests).
pub fn nudge(world: &mut GridWorld, corners: &[CornerRef], delta: i32, link: bool) -> bool {
    HeightDrag::begin(world, corners, link).apply(world, delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::room::GridRoom;
    use crate::grid::surface::{GridSurface, GridWall};
    use crate::grid::units::{GridScale, CLICK_UNITS};

    fn flat_world(width: usize, depth: usize) -> GridWorld {
        let mut room = GridRoom::new("r", width, depth);
        for z in 0..depth {
            for x in 0..width {
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(0, None)));
            }
        }
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn corner(cell: [i32; 2], corner: u8) -> CornerRef {
        CornerRef {
            face: FaceRef::floor(0, cell),
            layer: 0,
            corner,
        }
    }

    fn height(world: &GridWorld, cell: [i32; 2], c: u8) -> i32 {
        corner(cell, c).height(world).unwrap()
    }

    #[test]
    fn shared_floor_corners_move_together_when_linked() {
        let mut world = flat_world(2, 2);
        // The centre vertex of the 2x2 floor is NW of (1,1), NE of (0,1),
        // SE of (0,0) and SW of (1,0).
        assert!(nudge(&mut world, &[corner([0, 0], 2)], CLICK_UNITS, true));
        assert_eq!(height(&world, [0, 0], 2), 256);
        assert_eq!(height(&world, [1, 0], 3), 256);
        assert_eq!(height(&world, [0, 1], 1), 256);
        assert_eq!(height(&world, [1, 1], 0), 256);
        // Nothing else moved.
        assert_eq!(height(&world, [0, 0], 0), 0);
        assert_eq!(height(&world, [1, 1], 2), 0);
    }

    #[test]
    fn unlinked_drags_move_only_the_picked_corner() {
        let mut world = flat_world(2, 2);
        nudge(&mut world, &[corner([0, 0], 2)], 256, false);
        assert_eq!(height(&world, [0, 0], 2), 256);
        assert_eq!(height(&world, [1, 1], 0), 0);
        assert_eq!(height(&world, [1, 0], 3), 0);
    }

    #[test]
    fn walls_standing_on_a_corner_follow_it() {
        let mut world = flat_world(2, 1);
        world.rooms[0]
            .sector_or_insert(0, 0)
            .unwrap()
            .east
            .push(GridWall::span(0, 1024, None));
        // East wall of cell 0: a = NE bottom, b = SE bottom. Raise the
        // floor corner NE of cell 0 (shared with NW of cell 1).
        nudge(&mut world, &[corner([0, 0], 1)], 256, true);
        let wall = FaceRef::wall(0, [0, 0], crate::grid::WallEdge::East, 0);
        assert_eq!(wall.heights(&world, 0), Some([256, 0, 1024, 1024]));
        assert_eq!(height(&world, [1, 0], 0), 256);
    }

    #[test]
    fn corners_in_other_rooms_weld_by_world_position() {
        let mut world = flat_world(1, 1);
        let mut upper = GridRoom::new("upper", 1, 1);
        upper.y_offset = 512;
        upper
            .sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
        world.rooms.push(upper);
        // The lower floor corner is at world y 0, the upper one at 512, so
        // they do not coincide...
        nudge(&mut world, &[corner([0, 0], 0)], 256, true);
        assert_eq!(
            CornerRef {
                face: FaceRef::floor(1, [0, 0]),
                layer: 0,
                corner: 0
            }
            .height(&world),
            Some(0)
        );
        // ...until a floor is raised to meet it: the upper room's offset is
        // 512, so a corner at room-relative 512 in room 0 is welded to it.
        world.rooms[0]
            .sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(512, None)));
        world.rooms[1]
            .sector_or_insert(0, 0)
            .unwrap()
            .set_ground_floor(Some(GridSurface::flat(0, None)));
        nudge(&mut world, &[corner([0, 0], 0)], 256, true);
        let other = CornerRef {
            face: FaceRef::floor(1, [0, 0]),
            layer: 0,
            corner: 0,
        };
        assert_eq!(other.height(&world), Some(256));
    }

    #[test]
    fn a_drag_is_absolute_so_it_can_be_replayed_and_reversed() {
        let mut world = flat_world(1, 1);
        let drag = HeightDrag::begin(&world, &[corner([0, 0], 0)], true);
        assert_eq!(drag.len(), 1);
        drag.apply(&mut world, 512);
        drag.apply(&mut world, 256);
        assert_eq!(height(&world, [0, 0], 0), 256);
        drag.apply(&mut world, 0);
        assert_eq!(height(&world, [0, 0], 0), 0);
        assert_eq!(drag.range, (0, 0));
    }

    #[test]
    fn a_whole_face_drag_lifts_every_corner_of_it() {
        let mut world = flat_world(3, 1);
        let face = FaceRef::floor(0, [1, 0]);
        let corners = face.corners(&world);
        nudge(&mut world, &corners, 256, true);
        for c in 0..4 {
            assert_eq!(height(&world, [1, 0], c), 256);
        }
        // The neighbours share two corners each and so lean towards it.
        assert_eq!(height(&world, [0, 0], 1), 256);
        assert_eq!(height(&world, [0, 0], 2), 256);
        assert_eq!(height(&world, [0, 0], 0), 0);
        assert_eq!(height(&world, [2, 0], 0), 256);
        assert_eq!(height(&world, [2, 0], 1), 0);
    }

    #[test]
    fn an_auto_split_floor_picks_its_diagonal_from_the_dragged_heights_and_goes_back() {
        use crate::grid::surface::GridSplit;
        let mut world = flat_world(1, 1);
        world.rooms[0]
            .sector_or_insert(0, 0)
            .unwrap()
            .ground_floor_mut()
            .unwrap()
            .auto_split = true;
        let original = world.clone();
        let drag = HeightDrag::begin(&world, &[corner([0, 0], 2)], false);
        // Lifting the SE corner leaves NE-SW the level diagonal.
        drag.apply(&mut world, 256);
        let split = |w: &GridWorld| {
            w.rooms[0]
                .sector(0, 0)
                .unwrap()
                .ground_floor()
                .unwrap()
                .split
        };
        assert_eq!(split(&world), GridSplit::NeSw);
        drag.apply(&mut world, 512);
        assert_eq!(
            split(&world),
            GridSplit::NeSw,
            "same decision at any height"
        );
        // Back to zero: heights and diagonal are the document's again.
        drag.apply(&mut world, 0);
        assert_eq!(world, original);
        // A legacy surface keeps its diagonal through the same drag.
        let mut legacy = original.clone();
        legacy.rooms[0]
            .sector_mut(0, 0)
            .unwrap()
            .ground_floor_mut()
            .unwrap()
            .auto_split = false;
        HeightDrag::begin(&legacy, &[corner([0, 0], 2)], false).apply(&mut legacy, 256);
        assert_eq!(split(&legacy), GridSplit::NwSe);
    }
}
