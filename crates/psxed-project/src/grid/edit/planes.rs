//! Editing free planes: add, move, resize, rotate, retexture, remove, copy.
//!
//! Every edit goes through [`update_plane`] (or [`add_plane`]), which clamps
//! the plane into the document's ranges and grows its room so the plane's
//! footprint stays inside it: the render box and the collision table are
//! per-room, so a plane outside its room would silently not collide.

use crate::face_uv::FaceUv;
use crate::ResourceId;

use super::super::limits::{MAX_PLANES_PER_ROOM, MAX_ROOM_SIDE};
use super::super::plane::GridPlane;
use super::super::world::GridWorld;
use super::surfaces::UvOp;

/// A plane, addressed by its room and its index in the room's list. Indices
/// shift when a plane before it is removed, so hold one only within a single
/// interaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlaneId {
    /// Room index.
    pub room: usize,
    /// Index in the room's plane list.
    pub index: usize,
}

impl PlaneId {
    /// The plane, if it exists.
    pub fn get<'a>(&self, world: &'a GridWorld) -> Option<&'a GridPlane> {
        world.rooms.get(self.room)?.planes.get(self.index)
    }

    /// Mutable plane.
    pub fn get_mut<'a>(&self, world: &'a mut GridWorld) -> Option<&'a mut GridPlane> {
        world.rooms.get_mut(self.room)?.planes.get_mut(self.index)
    }

    /// Whether the plane exists.
    pub fn exists(&self, world: &GridWorld) -> bool {
        self.get(world).is_some()
    }
}

/// Why a plane edit was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneRefusal {
    /// The room does not exist.
    NoRoom,
    /// The room already holds [`MAX_PLANES_PER_ROOM`] planes.
    TooMany,
    /// Growing the room to hold the plane would pass [`MAX_ROOM_SIDE`].
    RoomTooLarge,
}

impl PlaneRefusal {
    /// One-line message for the status bar.
    pub fn describe(self) -> String {
        match self {
            Self::NoRoom => "No room to put the plane in".to_string(),
            Self::TooMany => format!("A room holds at most {MAX_PLANES_PER_ROOM} planes"),
            Self::RoomTooLarge => format!("The room would pass {MAX_ROOM_SIDE} sectors"),
        }
    }
}

/// Grow room `room` so the footprint of its plane `index` is inside it.
/// Returns false, leaving the room alone, when that would pass the size cap.
pub fn fit_room(world: &mut GridWorld, room: usize, index: usize) -> bool {
    let Some(r) = world.rooms.get_mut(room) else {
        return false;
    };
    let Some(plane) = r.planes.get(index) else {
        return false;
    };
    let (lo, hi) = plane.cell_span();
    let low = [r.origin[0].min(lo[0]), r.origin[1].min(lo[1])];
    let high = [
        (r.origin[0] + r.width as i32 - 1).max(hi[0]),
        (r.origin[1] + r.depth as i32 - 1).max(hi[1]),
    ];
    let side = MAX_ROOM_SIDE as i32;
    if high[0] - low[0] + 1 > side || high[1] - low[1] + 1 > side {
        return false;
    }
    r.grow_to_include(
        i64::from(lo[0]) - i64::from(r.origin[0]),
        i64::from(lo[1]) - i64::from(r.origin[1]),
    );
    r.grow_to_include(
        i64::from(hi[0]) - i64::from(r.origin[0]),
        i64::from(hi[1]) - i64::from(r.origin[1]),
    );
    true
}

/// Add `plane` to `room`, normalised, growing the room to hold it.
pub fn add_plane(
    world: &mut GridWorld,
    room: usize,
    mut plane: GridPlane,
) -> Result<PlaneId, PlaneRefusal> {
    let r = world.rooms.get(room).ok_or(PlaneRefusal::NoRoom)?;
    if r.planes.len() >= MAX_PLANES_PER_ROOM {
        return Err(PlaneRefusal::TooMany);
    }
    plane.normalize();
    let index = r.planes.len();
    world.rooms[room].planes.push(plane);
    if !fit_room(world, room, index) {
        world.rooms[room].planes.pop();
        return Err(PlaneRefusal::RoomTooLarge);
    }
    Ok(PlaneId { room, index })
}

/// Change a plane with `edit`, normalise it and fit its room. When the edit
/// would push the room past the size cap, it is undone and `false` returned.
/// Returns whether the plane changed.
pub fn update_plane(world: &mut GridWorld, id: PlaneId, edit: impl FnOnce(&mut GridPlane)) -> bool {
    let Some(plane) = id.get_mut(world) else {
        return false;
    };
    let before = plane.clone();
    edit(plane);
    plane.normalize();
    if *plane == before {
        return false;
    }
    if !fit_room(world, id.room, id.index) {
        if let Some(plane) = id.get_mut(world) {
            *plane = before;
        }
        return false;
    }
    true
}

/// Set the material of a plane.
pub fn set_plane_material(
    world: &mut GridWorld,
    id: PlaneId,
    material: Option<ResourceId>,
) -> bool {
    update_plane(world, id, |p| p.material = material)
}

/// Edit the texture placement of a plane.
pub fn edit_plane_uv(world: &mut GridWorld, id: PlaneId, op: UvOp) -> bool {
    update_plane(world, id, |p| op.apply(&mut p.uv))
}

/// Set the texture placement of a plane outright.
pub fn set_plane_uv(world: &mut GridWorld, id: PlaneId, uv: FaceUv) -> bool {
    update_plane(world, id, |p| p.uv = uv)
}

/// Remove planes. Higher indices go first so every id stays valid while the
/// list shrinks. Returns how many existed.
pub fn remove_planes(world: &mut GridWorld, ids: &[PlaneId]) -> usize {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    let mut removed = 0;
    for id in sorted {
        if let Some(room) = world.rooms.get_mut(id.room) {
            if id.index < room.planes.len() {
                room.planes.remove(id.index);
                removed += 1;
            }
        }
    }
    removed
}

/// Planes copied out of a room, with the point they are placed relative to.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaneClipboard {
    /// The copies, positions world-space.
    pub planes: Vec<GridPlane>,
    /// Centre of the group's box in `x` and `z`, grid units: the point the
    /// paste ghost hangs from.
    pub anchor: [i32; 2],
}

impl PlaneClipboard {
    /// Copy planes. `None` for an empty list.
    pub fn capture(world: &GridWorld, ids: &[PlaneId]) -> Option<Self> {
        let planes: Vec<GridPlane> = ids.iter().filter_map(|id| id.get(world).cloned()).collect();
        if planes.is_empty() {
            return None;
        }
        let (mut lo, mut hi) = ([i32::MAX; 2], [i32::MIN; 2]);
        for plane in &planes {
            let (a, b) = plane.bounds();
            for (axis, k) in [(0, 0), (1, 2)] {
                lo[axis] = lo[axis].min(a[k]);
                hi[axis] = hi[axis].max(b[k]);
            }
        }
        Some(Self {
            planes,
            anchor: [(lo[0] + hi[0]) / 2, (lo[1] + hi[1]) / 2],
        })
    }

    /// Turn the group a quarter clockwise about its anchor, seen from above.
    pub fn turn(&mut self) {
        for plane in &mut self.planes {
            let (dx, dz) = (
                plane.center[0] - self.anchor[0],
                plane.center[2] - self.anchor[1],
            );
            plane.center[0] = self.anchor[0] - dz;
            plane.center[2] = self.anchor[1] + dx;
            plane.rotation[0] = super::super::plane::wrap_degrees(plane.rotation[0] - 90);
        }
    }

    /// Mirror the group left to right (along `x`) about its anchor.
    pub fn flip_x(&mut self) {
        for plane in &mut self.planes {
            plane.center[0] = 2 * self.anchor[0] - plane.center[0];
            // Mirroring turns the facing direction about the vertical axis.
            plane.rotation[0] = super::super::plane::wrap_degrees(-plane.rotation[0]);
            plane.rotation[2] = -plane.rotation[2];
        }
    }

    /// Paste into `room`, with the anchor at world `(x, z)`. Returns the new
    /// ids; the first refusal stops the paste (planes already added stay).
    pub fn paste(
        &self,
        world: &mut GridWorld,
        room: usize,
        at: [i32; 2],
    ) -> (Vec<PlaneId>, Option<PlaneRefusal>) {
        let mut ids = Vec::new();
        for plane in &self.planes {
            let mut copy = plane.clone();
            copy.center[0] += at[0] - self.anchor[0];
            copy.center[2] += at[1] - self.anchor[1];
            match add_plane(world, room, copy) {
                Ok(id) => ids.push(id),
                Err(refusal) => return (ids, Some(refusal)),
            }
        }
        (ids, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::plane::{PlaneCollision, MAX_PLANE_SIZE, MIN_PLANE_SIZE};
    use crate::grid::room::GridRoom;
    use crate::grid::units::{GridScale, SECTOR_UNITS};

    fn world() -> GridWorld {
        GridWorld {
            scale: GridScale { num: 1, den: 1 },
            rooms: vec![GridRoom::new("r", 2, 2)],
        }
    }

    #[test]
    fn adding_a_plane_grows_the_room_to_hold_its_footprint() {
        let mut w = world();
        // Centred two sectors west of the room, 2048 wide: reaches from
        // sector -3 to -1.
        let plane = GridPlane::new([-2 * SECTOR_UNITS, 0, SECTOR_UNITS], [2048, 1024]);
        let id = add_plane(&mut w, 0, plane).unwrap();
        let room = &w.rooms[0];
        assert!(room.origin[0] <= -3, "{:?}", room.origin);
        assert_eq!(id, PlaneId { room: 0, index: 0 });
        let (lo, hi) = room.planes[0].cell_span();
        assert!(room.local(lo).is_some() && room.local(hi).is_some());
    }

    #[test]
    fn refusals_leave_the_document_alone() {
        let mut w = world();
        for _ in 0..MAX_PLANES_PER_ROOM {
            add_plane(&mut w, 0, GridPlane::new([512, 0, 512], [512, 512])).unwrap();
        }
        assert_eq!(
            add_plane(&mut w, 0, GridPlane::new([0; 3], [512, 512])),
            Err(PlaneRefusal::TooMany)
        );
        let mut w = world();
        let far = GridPlane::new([200 * SECTOR_UNITS, 0, 0], [512, 512]);
        assert_eq!(add_plane(&mut w, 0, far), Err(PlaneRefusal::RoomTooLarge));
        assert!(w.rooms[0].planes.is_empty());
        assert_eq!((w.rooms[0].width, w.rooms[0].depth), (2, 2));
        assert_eq!(
            add_plane(&mut w, 5, GridPlane::new([0; 3], [512; 2])),
            Err(PlaneRefusal::NoRoom)
        );
    }

    #[test]
    fn updates_clamp_normalise_and_report_changes() {
        let mut w = world();
        let id = add_plane(&mut w, 0, GridPlane::new([512, 0, 512], [1024, 1024])).unwrap();
        assert!(update_plane(&mut w, id, |p| p.size = [10, 99_999]));
        assert_eq!(id.get(&w).unwrap().size, [MIN_PLANE_SIZE, MAX_PLANE_SIZE]);
        assert!(update_plane(&mut w, id, |p| p.rotation = [270, 0, 0]));
        assert_eq!(id.get(&w).unwrap().rotation, [-90, 0, 0]);
        assert!(!update_plane(&mut w, id, |p| p.rotation = [-90, 0, 0]));
        // Pushing past the room cap restores the plane.
        let before = id.get(&w).unwrap().clone();
        assert!(!update_plane(&mut w, id, |p| p.center[0] = 300 * SECTOR_UNITS));
        assert_eq!(id.get(&w).unwrap(), &before);
        assert!(!set_plane_material(&mut w, id, None));
        assert!(edit_plane_uv(&mut w, id, UvOp::FlipU));
        assert_eq!(id.get(&w).unwrap().uv.scale_q8, [-256, 256]);
    }

    #[test]
    fn removal_goes_from_the_top_and_ignores_stale_ids() {
        let mut w = world();
        for i in 0..4 {
            let mut p = GridPlane::new([512, 0, 512], [512, 512]);
            p.center[1] = i * 100;
            add_plane(&mut w, 0, p).unwrap();
        }
        let ids = [
            PlaneId { room: 0, index: 1 },
            PlaneId { room: 0, index: 3 },
            PlaneId { room: 0, index: 1 },
            PlaneId { room: 0, index: 9 },
        ];
        assert_eq!(remove_planes(&mut w, &ids), 2);
        let heights: Vec<i32> = w.rooms[0].planes.iter().map(|p| p.center[1]).collect();
        assert_eq!(heights, vec![0, 200]);
    }

    #[test]
    fn copy_paste_turn_and_flip_move_the_group() {
        let mut w = world();
        let mut a = GridPlane::new([512, 0, 512], [512, 512]);
        a.collision = PlaneCollision::Solid;
        let b = GridPlane::new([1536, 256, 512], [512, 512]);
        let ids = [
            add_plane(&mut w, 0, a).unwrap(),
            add_plane(&mut w, 0, b).unwrap(),
        ];
        let mut clip = PlaneClipboard::capture(&w, &ids).unwrap();
        assert_eq!(clip.anchor, [1024, 512]);
        let (pasted, refusal) = clip.paste(&mut w, 0, [1024, 1536]);
        assert!(refusal.is_none());
        assert_eq!(pasted.len(), 2);
        assert_eq!(pasted[0].get(&w).unwrap().center, [512, 0, 1536]);
        assert_eq!(pasted[1].get(&w).unwrap().center, [1536, 256, 1536]);
        // A quarter turn swaps the offsets from the anchor.
        clip.turn();
        let rotated: Vec<[i32; 3]> = clip.planes.iter().map(|p| p.center).collect();
        assert_eq!(rotated, vec![[1024, 0, 0], [1024, 256, 1024]]);
        assert_eq!(clip.planes[0].rotation[0], -90);
        clip.flip_x();
        assert_eq!(clip.planes[0].center[0], 1024);
        assert!(PlaneClipboard::capture(&w, &[]).is_none());
    }
}
