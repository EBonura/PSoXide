//! Which rooms a streamer wants resident, and how many pages that takes.
//!
//! The same walk-distance rule serves the guest and the cook. The guest asks
//! [`StreamWindow::wanted`] each frame for the rooms it must hold (the
//! **need** set: what the camera may draw and what the player's activation
//! radius reaches) and the rooms it should fetch ahead (the **lead** ring:
//! the need radius plus the level's `lead_radius`). The cook asks
//! [`plan_pool`] how many pages the busiest position of the level takes, so a
//! pool can be sized once, at cook time, and the level refused when it does
//! not fit.
//!
//! Distances are runtime units along the portal graph ([`crate::walk`]), not
//! portal hops.

use alloc::vec;
use alloc::vec::Vec;

use crate::format::SECTOR_BYTES;
use crate::view::LevelView;
use crate::walk::WalkSet;

/// Walk radius standing for "no bound" (see `activity`).
const UNBOUNDED: i32 = 1 << 28;

/// What a wanted room is wanted for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Want {
    /// Needed now: drawn from the camera, or inside the activation radius.
    Need,
    /// Needed soon: inside the lead ring only.
    Lead,
}

/// One room the streamer wants, with its walk distance from the observer in
/// runtime units (the nearer of the player and the camera for a need room).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WantedRoom {
    pub room: u16,
    pub want: Want,
    pub distance: i32,
}

/// Walk searches for one level: the camera's draw search, the player's
/// activation search and the player's lead search. Each caches its result
/// until the observer leaves its sector, as [`crate::RoomActivity`] does.
pub struct StreamWindow {
    draw: WalkSet,
    active: WalkSet,
    lead: WalkSet,
    draw_radius: i32,
    active_radius: i32,
    lead_radius: i32,
    tolerance: i32,
    /// Per room: 0 not seen this call, 1 need, 2 lead. Reset from `touched`.
    mark: Vec<u8>,
    touched: Vec<u16>,
}

impl StreamWindow {
    /// Searches over `level`, with the radii its core carries.
    pub fn new(level: &LevelView<'_>) -> Self {
        let s = level.sector_units() as i32;
        let sectors = |n: u8| i32::from(n) * s;
        let draw = if level.header.draw_far == 0 {
            UNBOUNDED
        } else {
            sectors(level.header.draw_far)
        };
        let active = if level.header.active_radius == 0 {
            UNBOUNDED
        } else {
            sectors(level.header.active_radius)
        };
        let lead = if draw >= UNBOUNDED {
            UNBOUNDED
        } else {
            draw.max(active)
                .saturating_add(sectors(level.header.lead_radius))
        };
        Self {
            draw: WalkSet::new(level),
            active: WalkSet::new(level),
            lead: WalkSet::new(level),
            draw_radius: draw,
            active_radius: active,
            lead_radius: lead,
            tolerance: s.max(1),
            mark: vec![0; level.rooms.len()],
            touched: Vec::new(),
        }
    }

    /// Radius, runtime units, of the need set around the camera.
    pub fn draw_radius(&self) -> i32 {
        self.draw_radius
    }

    /// Radius, runtime units, of the whole wanted set around the player.
    pub fn lead_radius(&self) -> i32 {
        self.lead_radius
    }

    /// Whether the level bounds nothing: every room is always wanted.
    pub fn unbounded(&self) -> bool {
        self.draw_radius >= UNBOUNDED
    }

    /// The rooms wanted with the player at `player` (room and position) and
    /// the camera at `camera`. A `None` observer is outside every room and
    /// adds nothing. `out` is cleared first and receives need rooms, then lead
    /// rooms, each group in the order the search found them.
    pub fn wanted(
        &mut self,
        level: &LevelView<'_>,
        player: Option<(usize, [i32; 3])>,
        camera: Option<(usize, [i32; 3])>,
        out: &mut Vec<WantedRoom>,
    ) {
        out.clear();
        let (t, ar, dr, lr) = (
            self.tolerance,
            self.active_radius,
            self.draw_radius,
            self.lead_radius,
        );
        if let Some((room, at)) = player {
            self.active.ensure(level, room, at, ar, t);
            self.lead.ensure(level, room, at, lr, t);
        } else {
            self.active.invalidate();
            self.lead.invalidate();
        }
        if let Some((room, at)) = camera {
            self.draw.ensure(level, room, at, dr, t);
        } else {
            self.draw.invalidate();
        }
        // Need: the camera's draw set and the player's activation set.
        for set in [&self.draw, &self.active] {
            for room in set.rooms() {
                let cost = set.cost(room).unwrap_or(0);
                Self::note(&mut self.mark, &mut self.touched, out, room, 1, cost);
            }
        }
        // Lead: the rest of the player's lead ring.
        for room in self.lead.rooms() {
            let cost = self.lead.cost(room).unwrap_or(0);
            Self::note(&mut self.mark, &mut self.touched, out, room, 2, cost);
        }
        for &room in &self.touched {
            self.mark[usize::from(room)] = 0;
        }
        self.touched.clear();
        // Need rooms first, nearest first inside each group.
        out.sort_by_key(|w| (w.want == Want::Lead, w.distance, w.room));
    }

    fn note(
        mark: &mut [u8],
        touched: &mut Vec<u16>,
        out: &mut Vec<WantedRoom>,
        room: usize,
        class: u8,
        cost: i32,
    ) {
        let slot = &mut mark[room];
        if *slot == 0 {
            *slot = class;
            touched.push(room as u16);
            out.push(WantedRoom {
                room: room as u16,
                want: if class == 1 { Want::Need } else { Want::Lead },
                distance: cost,
            });
        } else if class == 1 {
            // Seen earlier from the other observer: keep the nearer distance.
            if let Some(entry) = out.iter_mut().find(|w| usize::from(w.room) == room) {
                entry.distance = entry.distance.min(cost);
            }
        }
    }

    /// Forget every cached search.
    pub fn invalidate(&mut self) {
        self.draw.invalidate();
        self.active.invalidate();
        self.lead.invalidate();
    }
}

/// How big a room pool a level needs.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolPlan {
    /// Pages of every room chunk together: an all-resident run.
    pub total_pages: usize,
    /// Pages of the largest room chunk, the biggest run one room needs.
    pub max_room_pages: usize,
    /// Most pages the wanted set (need and lead) takes at any position tried.
    pub peak_pages: usize,
    /// Rooms in that set.
    pub peak_rooms: usize,
    /// Pages to reserve: the peak plus room for fragmentation and for the
    /// rooms still resident from the last position while the next fill lands,
    /// never more than the whole level.
    pub pool_pages: usize,
}

/// Pages one chunk of `bytes` bytes takes.
pub const fn pages_of(bytes: usize) -> usize {
    bytes.div_ceil(SECTOR_BYTES)
}

/// Size the room pool of `level`, given the pages each room chunk occupies.
///
/// The set a position wants is the cook's own [`StreamWindow`] with the
/// observer standing there. Positions tried: the centre and the four corners
/// of every room at mid height, and the centre of every portal (the busiest
/// spot is nearly always a doorway). A level whose draw distance is
/// unbounded, or whose wanted set is always everything, gets the whole level.
pub fn plan_pool(level: &LevelView<'_>, room_pages: &[usize]) -> PoolPlan {
    let total_pages: usize = room_pages.iter().sum();
    let max_room_pages = room_pages.iter().copied().max().unwrap_or(0);
    let mut plan = PoolPlan {
        total_pages,
        max_room_pages,
        peak_pages: total_pages,
        peak_rooms: room_pages.len(),
        pool_pages: total_pages,
    };
    let mut window = StreamWindow::new(level);
    if window.unbounded() || level.rooms.is_empty() {
        return plan;
    }
    let mut wanted = Vec::new();
    let mut peak = (0usize, 0usize);
    let mut try_at = |window: &mut StreamWindow, room: usize, at: [i32; 3]| {
        window.invalidate();
        window.wanted(level, Some((room, at)), Some((room, at)), &mut wanted);
        let pages: usize = wanted
            .iter()
            .map(|w| room_pages.get(usize::from(w.room)).copied().unwrap_or(0))
            .sum();
        if pages > peak.0 {
            peak = (pages, wanted.len());
        }
    };
    for room in 0..level.rooms.len() {
        let (min, max) = level.room_bounds(room);
        let mid = min[1] + (max[1] - min[1]) / 2;
        let cx = min[0] + (max[0] - min[0]) / 2;
        let cz = min[2] + (max[2] - min[2]) / 2;
        try_at(&mut window, room, [cx, mid, cz]);
        for (x, z) in [
            (min[0], min[2]),
            (max[0], min[2]),
            (min[0], max[2]),
            (max[0], max[2]),
        ] {
            // A corner is on the room's edge; pull it a hair inside so the
            // start is in the room it names.
            let inside = |v: i32, lo: i32, hi: i32| v.clamp(lo + 1, hi - 1);
            try_at(
                &mut window,
                room,
                [inside(x, min[0], max[0]), mid, inside(z, min[2], max[2])],
            );
        }
        let record = &level.rooms[room];
        for portal in level.room_portals(room) {
            let mut sum = [0i32; 3];
            for corner in &portal.v {
                for axis in 0..3 {
                    sum[axis] += i32::from(corner[axis]);
                }
            }
            try_at(
                &mut window,
                room,
                [
                    record.origin[0] + sum[0] / 4,
                    record.origin[1] + sum[1] / 4,
                    record.origin[2] + sum[2] / 4,
                ],
            );
        }
    }
    plan.peak_pages = peak.0;
    plan.peak_rooms = peak.1;
    plan.pool_pages = (peak.0 + peak.0 / 2 + max_room_pages).min(total_pages);
    plan
}
