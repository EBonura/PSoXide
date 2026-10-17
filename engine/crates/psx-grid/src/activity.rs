//! Which rooms are drawn and which can hold active entities, by walk
//! distance over the portal graph.
//!
//! Two radii from the level core (in sectors): the **draw distance**, past
//! which a room is neither drawn nor needed in memory, and the **activation
//! radius** inside which entities think. Both are walked from a start point
//! (the camera, the player), not counted in portal hops: hops stop meaning
//! distance once rooms are cut to a target size. A room that is not
//! resident is never drawn and never active, whatever its distance.

use alloc::vec::Vec;

use crate::resident::ResidentRooms;
use crate::view::LevelView;
use crate::walk::WalkSet;

/// Walk radius, in runtime units, standing for "no bound": far past any
/// coordinate a level can hold, small enough to add distances without
/// overflow.
const UNBOUNDED: i32 = 1 << 28;

/// Draw and activation sets of one level.
pub struct RoomActivity {
    draw: WalkSet,
    /// Activation searches: the player's and the camera's, kept apart so two
    /// observers that stand in different places do not make each other
    /// search again at every call.
    active: [WalkSet; 2],
    draw_radius: i32,
    active_radius: i32,
    /// How far a start may drift before the search is run again: one sector.
    tolerance: i32,
}

impl RoomActivity {
    /// Sets over `level`, with the radii its core carries.
    pub fn new(level: &LevelView<'_>) -> Self {
        let s = level.sector_units() as i32;
        let radius = |sectors: u8| {
            if sectors == 0 {
                UNBOUNDED
            } else {
                i32::from(sectors) * s
            }
        };
        Self {
            draw: WalkSet::new(level),
            active: [WalkSet::new(level), WalkSet::new(level)],
            draw_radius: radius(level.header.draw_far),
            active_radius: radius(level.header.active_radius),
            tolerance: s.max(1),
        }
    }

    /// The draw distance, runtime units.
    pub fn draw_radius(&self) -> i32 {
        self.draw_radius
    }

    /// The activation radius, runtime units.
    pub fn active_radius(&self) -> i32 {
        self.active_radius
    }

    /// Search around the player, unless the cached search still holds.
    /// Returns whether it searched again.
    pub fn track_player(&mut self, level: &LevelView<'_>, room: usize, at: [i32; 3]) -> bool {
        self.active[0].ensure(level, room, at, self.active_radius, self.tolerance)
    }

    /// Search around the camera, for what is active where the camera is.
    pub fn track_camera(&mut self, level: &LevelView<'_>, room: usize, at: [i32; 3]) -> bool {
        self.active[1].ensure(level, room, at, self.active_radius, self.tolerance)
    }

    /// Whether entities in `room` may think: it is resident and within the
    /// activation radius of the last tracked player position.
    pub fn room_active(&self, resident: &ResidentRooms<'_>, room: usize) -> bool {
        resident.contains(room) && self.active[0].within(room, self.active_radius)
    }

    /// Rooms within the activation radius that are resident.
    pub fn active_rooms<'s>(
        &'s self,
        resident: &'s ResidentRooms<'_>,
    ) -> impl Iterator<Item = usize> + 's {
        self.active[0]
            .rooms()
            .filter(|&room| resident.contains(room))
    }

    /// Rooms within the activation radius of the camera that are resident.
    pub fn camera_active_rooms<'s>(
        &'s self,
        resident: &'s ResidentRooms<'_>,
    ) -> impl Iterator<Item = usize> + 's {
        self.active[1]
            .rooms()
            .filter(|&room| resident.contains(room))
    }

    /// The rooms a frame may draw from `eye` in `room`: resident rooms within
    /// the draw distance, nearest the eye's own room first. The eye's room
    /// is `None` when the camera is outside every room, and then every
    /// resident room within a straight `draw_radius` is a candidate.
    pub fn draw_rooms(
        &mut self,
        level: &LevelView<'_>,
        resident: &ResidentRooms<'_>,
        room: Option<usize>,
        eye: [i32; 3],
        out: &mut Vec<usize>,
    ) {
        out.clear();
        match room {
            Some(room) => {
                self.draw
                    .ensure(level, room, eye, self.draw_radius, self.tolerance);
                out.extend(self.draw.rooms().filter(|&r| resident.contains(r)));
            }
            None => {
                for slot in resident.iter() {
                    let room = usize::from(slot.room);
                    if self.near_enough(level, room, eye) {
                        out.push(room);
                    }
                }
            }
        }
    }

    /// Whether the box of `room` reaches within the draw distance of `eye`
    /// in a straight line.
    fn near_enough(&self, level: &LevelView<'_>, room: usize, eye: [i32; 3]) -> bool {
        if self.draw_radius >= UNBOUNDED {
            return true;
        }
        let (min, max) = level.room_bounds(room);
        let reach = self.draw_radius.min(16_383);
        let mut sum = 0i32;
        for axis in 0..3 {
            let gap = (min[axis] - eye[axis]).max(eye[axis] - max[axis]).max(0);
            let gap = gap.min(16_383);
            sum += gap * gap;
        }
        sum <= reach * reach
    }

    /// Forget both cached searches, so the next frame searches again.
    pub fn invalidate(&mut self) {
        self.draw.invalidate();
        self.active[0].invalidate();
        self.active[1].invalidate();
    }
}
