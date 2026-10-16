//! Creating and deleting floors and ceilings over a rectangle of sectors.
//!
//! The room grows to take in the rectangle (towards negative coordinates
//! too, moving its origin so nothing shifts in the world), and shrinks back
//! after a delete. Growth that would pass [`MAX_ROOM_SIDE`] is refused
//! before anything changes, so an oversize room fails loudly in the editor.

use crate::ResourceId;

use super::super::limits::MAX_ROOM_SIDE;
use super::super::room::GridRoom;
use super::super::surface::GridSurface;
use super::super::world::GridWorld;
use super::refs::SurfaceKind;

/// Inclusive rectangle of world sectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    /// Smallest corner, `[x, z]`.
    pub min: [i32; 2],
    /// Largest corner, `[x, z]`, inclusive.
    pub max: [i32; 2],
}

impl CellRect {
    /// The rectangle spanned by two cells in either order.
    pub fn from_corners(a: [i32; 2], b: [i32; 2]) -> Self {
        Self {
            min: [a[0].min(b[0]), a[1].min(b[1])],
            max: [a[0].max(b[0]), a[1].max(b[1])],
        }
    }

    /// Sectors along X.
    pub fn width(&self) -> i32 {
        self.max[0] - self.min[0] + 1
    }

    /// Sectors along Z.
    pub fn depth(&self) -> i32 {
        self.max[1] - self.min[1] + 1
    }

    /// Number of sectors.
    pub fn count(&self) -> usize {
        (self.width().max(0) as usize) * (self.depth().max(0) as usize)
    }

    /// Whether `cell` is inside.
    pub fn contains(&self, cell: [i32; 2]) -> bool {
        (self.min[0]..=self.max[0]).contains(&cell[0])
            && (self.min[1]..=self.max[1]).contains(&cell[1])
    }

    /// Every cell, `z` major.
    pub fn cells(&self) -> impl Iterator<Item = [i32; 2]> + '_ {
        (self.min[1]..=self.max[1]).flat_map(|z| (self.min[0]..=self.max[0]).map(move |x| [x, z]))
    }
}

/// Why a rectangle edit changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RectRefusal {
    /// The room index does not exist.
    NoSuchRoom,
    /// The room would grow past [`MAX_ROOM_SIDE`] along one side.
    RoomTooLarge {
        /// Sectors the room would need along the offending side.
        needed: usize,
    },
}

/// What a rectangle edit did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RectOutcome {
    /// Surfaces created or removed.
    pub changed: usize,
    /// Cells left alone: a surface was already there (create) or none was
    /// (delete).
    pub skipped: usize,
    /// Whether the room's extent changed.
    pub resized: bool,
    /// Set when nothing was done.
    pub refused: Option<RectRefusal>,
}

impl RectOutcome {
    fn refused(reason: RectRefusal) -> Self {
        Self {
            refused: Some(reason),
            ..Self::default()
        }
    }
}

/// Grow `room` to include the world sectors in `rect`, refusing when a side
/// would exceed the cap. Returns whether the room changed.
pub fn grow_room_to(room: &mut GridRoom, rect: CellRect) -> Result<bool, RectRefusal> {
    let low = [
        room.origin[0].min(rect.min[0]),
        room.origin[1].min(rect.min[1]),
    ];
    let high = [
        (room.origin[0] + room.width as i32 - 1).max(rect.max[0]),
        (room.origin[1] + room.depth as i32 - 1).max(rect.max[1]),
    ];
    let (need_x, need_z) = (
        (high[0] - low[0] + 1) as usize,
        (high[1] - low[1] + 1) as usize,
    );
    if need_x > MAX_ROOM_SIDE || need_z > MAX_ROOM_SIDE {
        return Err(RectRefusal::RoomTooLarge {
            needed: need_x.max(need_z),
        });
    }
    let before = (room.width, room.depth, room.origin);
    room.grow_to_include(
        i64::from(rect.min[0]) - i64::from(room.origin[0]),
        i64::from(rect.min[1]) - i64::from(room.origin[1]),
    );
    room.grow_to_include(
        i64::from(rect.max[0]) - i64::from(room.origin[0]),
        i64::from(rect.max[1]) - i64::from(room.origin[1]),
    );
    Ok(before != (room.width, room.depth, room.origin))
}

/// Create a flat `kind` surface at room-relative `height` on every sector of
/// `rect` that has none. Existing surfaces are kept untouched.
pub fn create_surfaces(
    world: &mut GridWorld,
    room: usize,
    kind: SurfaceKind,
    rect: CellRect,
    height: i32,
    material: Option<ResourceId>,
) -> RectOutcome {
    let Some(target) = world.rooms.get_mut(room) else {
        return RectOutcome::refused(RectRefusal::NoSuchRoom);
    };
    let mut outcome = RectOutcome::default();
    match grow_room_to(target, rect) {
        Ok(resized) => outcome.resized = resized,
        Err(refusal) => return RectOutcome::refused(refusal),
    }
    for cell in rect.cells() {
        let Some([x, z]) = target.local(cell) else {
            continue;
        };
        let Some(sector) = target.sector_or_insert(x, z) else {
            continue;
        };
        let slot = match kind {
            SurfaceKind::Floor => &mut sector.floor,
            SurfaceKind::Ceiling => &mut sector.ceiling,
        };
        if slot.is_some() {
            outcome.skipped += 1;
        } else {
            *slot = Some(GridSurface::flat(height, material));
            outcome.changed += 1;
        }
    }
    outcome
}

/// Remove the `kind` surface from every sector of `rect`, then drop empty
/// sectors and shrink the room around what is left.
pub fn delete_surfaces(
    world: &mut GridWorld,
    room: usize,
    kind: SurfaceKind,
    rect: CellRect,
) -> RectOutcome {
    let Some(target) = world.rooms.get_mut(room) else {
        return RectOutcome::refused(RectRefusal::NoSuchRoom);
    };
    let before = (target.width, target.depth, target.origin);
    let mut outcome = RectOutcome::default();
    for cell in rect.cells() {
        let Some([x, z]) = target.local(cell) else {
            outcome.skipped += 1;
            continue;
        };
        let Some(sector) = target.sector_mut(x, z) else {
            outcome.skipped += 1;
            continue;
        };
        let slot = match kind {
            SurfaceKind::Floor => &mut sector.floor,
            SurfaceKind::Ceiling => &mut sector.ceiling,
        };
        if slot.take().is_some() {
            outcome.changed += 1;
        } else {
            outcome.skipped += 1;
        }
    }
    if outcome.changed > 0 {
        target.trim_empty();
    }
    outcome.resized = before != (target.width, target.depth, target.origin);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::units::GridScale;

    fn world() -> GridWorld {
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![GridRoom::new("r", 1, 1)],
        }
    }

    fn has(world: &GridWorld, cell: [i32; 2], kind: SurfaceKind) -> bool {
        let room = &world.rooms[0];
        let Some([x, z]) = room.local(cell) else {
            return false;
        };
        room.sector(x, z).is_some_and(|s| match kind {
            SurfaceKind::Floor => s.floor.is_some(),
            SurfaceKind::Ceiling => s.ceiling.is_some(),
        })
    }

    #[test]
    fn a_rectangle_creates_floors_and_grows_the_room() {
        let mut world = world();
        let rect = CellRect::from_corners([2, 1], [-1, -2]);
        let out = create_surfaces(&mut world, 0, SurfaceKind::Floor, rect, 256, None);
        assert_eq!((out.changed, out.skipped), (16, 0));
        assert!(out.resized);
        let room = &world.rooms[0];
        assert_eq!(room.origin, [-1, -2]);
        assert_eq!((room.width, room.depth), (4, 4));
        for cell in rect.cells() {
            assert!(has(&world, cell, SurfaceKind::Floor));
        }
        // The cell the room started with (0,0) is inside the rectangle too.
        let [x, z] = room.local([0, 0]).unwrap();
        assert_eq!(
            room.sector(x, z).unwrap().floor.as_ref().unwrap().heights,
            [256; 4]
        );
    }

    #[test]
    fn existing_surfaces_are_kept_and_counted_as_skipped() {
        let mut world = world();
        create_surfaces(
            &mut world,
            0,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [1, 0]),
            0,
            None,
        );
        let out = create_surfaces(
            &mut world,
            0,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [2, 0]),
            512,
            None,
        );
        assert_eq!((out.changed, out.skipped), (1, 2));
        let room = &world.rooms[0];
        assert_eq!(
            room.sector(0, 0).unwrap().floor.as_ref().unwrap().heights,
            [0; 4]
        );
        assert_eq!(
            room.sector(2, 0).unwrap().floor.as_ref().unwrap().heights,
            [512; 4]
        );
    }

    #[test]
    fn ceilings_and_floors_are_independent() {
        let mut world = world();
        let rect = CellRect::from_corners([0, 0], [1, 1]);
        create_surfaces(&mut world, 0, SurfaceKind::Floor, rect, 0, None);
        let out = create_surfaces(&mut world, 0, SurfaceKind::Ceiling, rect, 3072, None);
        assert_eq!(out.changed, 4);
        assert!(!out.resized);
        let out = delete_surfaces(&mut world, 0, SurfaceKind::Ceiling, rect);
        assert_eq!(out.changed, 4);
        assert!(has(&world, [1, 1], SurfaceKind::Floor));
        assert!(!has(&world, [1, 1], SurfaceKind::Ceiling));
    }

    #[test]
    fn deleting_shrinks_the_room_without_moving_what_is_left() {
        let mut world = world();
        create_surfaces(
            &mut world,
            0,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [3, 3]),
            0,
            None,
        );
        let out = delete_surfaces(
            &mut world,
            0,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [3, 1]),
        );
        assert_eq!(out.changed, 8);
        assert!(out.resized);
        let room = &world.rooms[0];
        assert_eq!(room.origin, [0, 2]);
        assert_eq!((room.width, room.depth), (4, 2));
        assert!(has(&world, [3, 3], SurfaceKind::Floor));
        // Deleting where nothing is skips.
        let out = delete_surfaces(
            &mut world,
            0,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [1, 0]),
        );
        assert_eq!((out.changed, out.skipped), (0, 2));
    }

    #[test]
    fn rooms_never_grow_past_the_cap_and_nothing_changes_when_refused() {
        let mut world = world();
        let before = world.clone();
        let rect = CellRect::from_corners([0, 0], [MAX_ROOM_SIDE as i32, 0]);
        let out = create_surfaces(&mut world, 0, SurfaceKind::Floor, rect, 0, None);
        assert_eq!(
            out.refused,
            Some(RectRefusal::RoomTooLarge {
                needed: MAX_ROOM_SIDE + 1
            })
        );
        assert_eq!(world, before);
        let ok = CellRect::from_corners([0, 0], [MAX_ROOM_SIDE as i32 - 1, 0]);
        assert!(
            create_surfaces(&mut world, 0, SurfaceKind::Floor, ok, 0, None)
                .refused
                .is_none()
        );
    }

    #[test]
    fn a_missing_room_is_refused() {
        let mut world = world();
        let out = create_surfaces(
            &mut world,
            4,
            SurfaceKind::Floor,
            CellRect::from_corners([0, 0], [0, 0]),
            0,
            None,
        );
        assert_eq!(out.refused, Some(RectRefusal::NoSuchRoom));
    }
}
