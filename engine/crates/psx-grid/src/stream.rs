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

use crate::flood::{Flood, FloodQuery, FloodRoom, NO_PAGE_BUDGET};
use crate::format::{FloodHeader, SECTOR_BYTES};
use crate::view::LevelView;
use crate::walk::WalkSet;

/// Walk radius standing for "no bound" (see `activity`).
const UNBOUNDED: i32 = 1 << 28;

/// Queue pops one call may spend on each flood once the first has finished.
/// A flood to the resident radius of a hex 4 level takes about a hundred and fifty, so
/// it lands within a couple of ticks and no tick pays for a whole search.
pub const FLOOD_POPS_PER_CALL: u32 = 16;

/// Sectors past the draw radius a drawn room stays drawn, so a room on the
/// rim does not flicker in and out as the camera crosses a sector.
const DRAW_HYSTERESIS_SECTORS: i32 = 1;

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

/// The two floods of a level that streams by flood
/// ([`crate::LEVEL_BUDGET_FLOOD`]): the camera's draw flood and the player's
/// resident flood.
struct FloodWindow {
    /// From the camera, out to the draw radius plus the hysteresis band.
    draw: Flood,
    /// From the player, out to the resident radius and the page budget.
    lead: Flood,
    /// Draw radius, resident radius and page budget, runtime units and pages.
    near: i32,
    res: i32,
    budget: u32,
    /// The rooms drawn: the draw flood's rooms within the draw radius, plus
    /// the ones in the hysteresis band that were drawn before.
    drawn: Vec<FloodRoom>,
    /// Per room: whether it is in `drawn`.
    in_drawn: Vec<bool>,
    /// The draw flood this `drawn` was made from.
    drawn_epoch: u32,
}

impl FloodWindow {
    fn new(level: &LevelView<'_>, header: &FloodHeader) -> Self {
        let s = level.sector_units() as i32;
        Self {
            draw: Flood::new(level),
            lead: Flood::new(level),
            near: i32::from(header.near_radius) * s,
            res: i32::from(header.res_radius) * s,
            budget: u32::from(header.pool_cap_pages),
            drawn: Vec::new(),
            in_drawn: alloc::vec![false; level.rooms.len()],
            drawn_epoch: 0,
        }
    }

    /// Pops to spend this call: the first flood of a kind runs to the end (a
    /// loading screen waits for it), later ones are sliced.
    fn pops(flood: &Flood) -> u32 {
        if flood.answered().is_none() {
            u32::MAX
        } else {
            FLOOD_POPS_PER_CALL
        }
    }

    fn track_draw(&mut self, level: &LevelView<'_>, room: usize, at: [i32; 3], tolerance: i32) {
        let query = FloodQuery {
            room,
            at,
            radius: self.near + DRAW_HYSTERESIS_SECTORS * level.sector_units() as i32,
            page_budget: NO_PAGE_BUDGET,
        };
        let pops = Self::pops(&self.draw);
        if self.draw.ensure(level, query, tolerance, pops) {
            self.publish_drawn();
        }
    }

    fn track_lead(&mut self, level: &LevelView<'_>, room: usize, at: [i32; 3], tolerance: i32) {
        let query = FloodQuery {
            room,
            at,
            radius: self.res,
            page_budget: self.budget,
        };
        let pops = Self::pops(&self.lead);
        self.lead.ensure(level, query, tolerance, pops);
    }

    /// Make the draw flood's result the drawn set, keeping a rim room drawn
    /// that already was.
    fn publish_drawn(&mut self) {
        let band_start = self.near;
        let mut next = Vec::with_capacity(self.drawn.len() + 4);
        for entry in self.draw.rooms() {
            if entry.cost <= band_start || self.in_drawn[usize::from(entry.room)] {
                next.push(*entry);
            }
        }
        for entry in &self.drawn {
            self.in_drawn[usize::from(entry.room)] = false;
        }
        for entry in &next {
            self.in_drawn[usize::from(entry.room)] = true;
        }
        self.drawn = next;
        self.drawn_epoch = self.draw.epoch();
    }
}

/// Walk searches for one level: the camera's draw search, the player's
/// activation search and the player's lead search. Each caches its result
/// until the observer leaves its sector, as [`crate::RoomActivity`] does. A
/// flood level replaces the draw and lead searches with floods.
pub struct StreamWindow {
    draw: WalkSet,
    active: WalkSet,
    lead: WalkSet,
    draw_radius: i32,
    active_radius: i32,
    lead_radius: i32,
    tolerance: i32,
    /// The level streams as a ring ([`LevelView::ring_stream`]).
    ring: bool,
    /// The level streams by flood and not every room is resident.
    flood: Option<FloodWindow>,
    /// Per room: 0 not seen this call, 1 need, 2 lead. Reset from `touched`.
    mark: Vec<u8>,
    touched: Vec<u16>,
}

impl StreamWindow {
    /// Searches over `level`, with the radii its core carries.
    pub fn new(level: &LevelView<'_>) -> Self {
        let s = level.sector_units() as i32;
        let sectors = |n: u8| i32::from(n) * s;
        // A flood level whose rooms all fit the pool is held whole: no draw
        // distance and no flood.
        let held_whole = level.budget_flood().is_some_and(|h| h.pool_cap_pages == 0);
        let flood = level
            .budget_flood()
            .filter(|h| h.pool_cap_pages != 0)
            .map(|h| FloodWindow::new(level, h));
        let draw = if level.header.draw_far == 0 || held_whole {
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
            ring: level.ring_stream(),
            flood,
            mark: vec![0; level.rooms.len()],
            touched: Vec::new(),
        }
    }

    /// Radius, runtime units, of the need set around the camera.
    pub fn draw_radius(&self) -> i32 {
        self.flood.as_ref().map_or(self.draw_radius, |f| f.near)
    }

    /// Radius, runtime units, of the whole wanted set around the player.
    pub fn lead_radius(&self) -> i32 {
        self.flood.as_ref().map_or(self.lead_radius, |f| f.res)
    }

    /// The rooms the camera draws, nearest first, when the level streams by
    /// flood; `None` for any other level (ask [`crate::RoomActivity`]).
    /// Valid as of the last [`Self::wanted`].
    pub fn drawn_rooms(&self) -> Option<&[FloodRoom]> {
        self.flood.as_ref().map(|f| f.drawn.as_slice())
    }

    /// Whether the level streams by flood.
    pub fn floods(&self) -> bool {
        self.flood.is_some()
    }

    /// What the last finished resident flood did, for counters.
    pub fn flood_stats(&self) -> Option<crate::flood::FloodStats> {
        self.flood.as_ref().map(|f| f.lead.stats())
    }

    /// Whether the level bounds nothing: every room is always wanted.
    pub fn unbounded(&self) -> bool {
        !self.ring && self.flood.is_none() && self.draw_radius >= UNBOUNDED
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
        if self.flood.is_some() {
            self.wanted_flood(level, player, camera, out);
            return;
        }
        if self.ring {
            self.wanted_ring(level, [player, camera], out);
            return;
        }
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

    /// [`Self::wanted`] for a flood level: need is the camera's drawn set and
    /// the player's activation walk, lead the rest of the player's resident
    /// flood.
    fn wanted_flood(
        &mut self,
        level: &LevelView<'_>,
        player: Option<(usize, [i32; 3])>,
        camera: Option<(usize, [i32; 3])>,
        out: &mut Vec<WantedRoom>,
    ) {
        let Some(flood) = self.flood.as_mut() else {
            return;
        };
        let (t, ar) = (self.tolerance, self.active_radius);
        if let Some((room, at)) = player {
            self.active.ensure(level, room, at, ar, t);
            flood.track_lead(level, room, at, t);
        } else {
            self.active.invalidate();
            flood.lead.invalidate();
        }
        if let Some((room, at)) = camera {
            flood.track_draw(level, room, at, t);
        } else {
            flood.draw.invalidate();
        }
        for entry in &flood.drawn {
            Self::note(
                &mut self.mark,
                &mut self.touched,
                out,
                usize::from(entry.room),
                1,
                entry.cost,
            );
        }
        for room in self.active.rooms() {
            let cost = self.active.cost(room).unwrap_or(0);
            Self::note(&mut self.mark, &mut self.touched, out, room, 1, cost);
        }
        for entry in flood.lead.rooms() {
            Self::note(
                &mut self.mark,
                &mut self.touched,
                out,
                usize::from(entry.room),
                2,
                entry.cost,
            );
        }
        for &room in &self.touched {
            self.mark[usize::from(room)] = 0;
        }
        self.touched.clear();
        out.sort_by_key(|w| (w.want == Want::Lead, w.distance, w.room));
    }

    /// [`Self::wanted`] for a ring level: each observer's room and the rooms
    /// one portal away are needed. The lead is the ring of the neighbour the
    /// player is nearest (the rooms it would need on stepping across), so
    /// they are on their way before the player gets there.
    fn wanted_ring(
        &mut self,
        level: &LevelView<'_>,
        observers: [Option<(usize, [i32; 3])>; 2],
        out: &mut Vec<WantedRoom>,
    ) {
        let rooms = level.rooms.len();
        // The player's nearest neighbour and its distance.
        let mut ahead: Option<(usize, i32)> = None;
        for (index, observer) in observers.into_iter().enumerate() {
            let Some((room, at)) = observer.filter(|(r, _)| *r < rooms) else {
                continue;
            };
            Self::note(&mut self.mark, &mut self.touched, out, room, 1, 0);
            let record = &level.rooms[room];
            for portal in level.room_portals(room) {
                let cost = crate::walk::distance(at, crate::walk::centre(record, portal));
                let dest = usize::from(portal.dest);
                if dest < rooms {
                    Self::note(&mut self.mark, &mut self.touched, out, dest, 1, cost);
                    if index == 0 && ahead.is_none_or(|(_, c)| cost < c) {
                        ahead = Some((dest, cost));
                    }
                }
            }
        }
        if let Some((next, cost)) = ahead {
            let record = &level.rooms[next];
            for portal in level.room_portals(next) {
                let dest = usize::from(portal.dest);
                if dest < rooms {
                    let far = cost.saturating_add(crate::walk::distance(
                        crate::walk::centre(record, portal),
                        record_centre(record, level.sector_units() as i32),
                    ));
                    Self::note(&mut self.mark, &mut self.touched, out, dest, 2, far);
                }
            }
        }
        for &room in &self.touched {
            self.mark[usize::from(room)] = 0;
        }
        self.touched.clear();
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
        if let Some(flood) = self.flood.as_mut() {
            flood.draw.invalidate();
            flood.lead.invalidate();
            for entry in &flood.drawn {
                flood.in_drawn[usize::from(entry.room)] = false;
            }
            flood.drawn.clear();
        }
    }
}

/// Plan centre of a room's rectangle at its origin height, world units.
fn record_centre(record: &crate::format::RoomRecord, s: i32) -> [i32; 3] {
    [
        record.origin[0] + i32::from(record.width) * s / 2,
        record.origin[1],
        record.origin[2] + i32::from(record.depth) * s / 2,
    ]
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
    // A ring or flood level's wanted set already holds the rooms it will need
    // next (the lead), so it needs room for one more chunk landing, not half
    // its peak again.
    plan.pool_pages = if window.ring || window.flood.is_some() {
        peak.0 + max_room_pages
    } else {
        peak.0 + peak.0 / 2 + max_room_pages
    }
    .min(total_pages);
    plan
}

/// The room that contains `point`, from the level core alone (no chunk needs
/// to be resident). Among rooms whose footprint holds the point, the one whose
/// vertical range holds it, else the vertically nearest; `hint` wins a tie,
/// then the lowest index. `None` outside every footprint.
pub fn room_at(level: &LevelView<'_>, hint: usize, point: [i32; 3]) -> Option<usize> {
    let mut best: Option<(i32, bool, usize)> = None;
    for &room in level.lattice_rooms_at(point[0], point[2]) {
        let room = usize::from(room);
        let (min, max) = level.room_bounds(room);
        if point[0] < min[0] || point[0] >= max[0] || point[2] < min[2] || point[2] >= max[2] {
            continue;
        }
        if !level.room_has_sector(room, point[0], point[2]) {
            continue;
        }
        let gap = (min[1] - point[1]).max(point[1] - max[1]).max(0);
        let key = (gap, room != hint, room);
        if best.is_none_or(|b| key < b) {
            best = Some(key);
        }
    }
    best.map(|(_, _, room)| room)
}

impl StreamWindow {
    /// Walk distance, runtime units, from the player to `room` as of the last
    /// [`Self::wanted`], or `u32::MAX` beyond the lead radius.
    pub fn distance_of(&self, room: u16) -> u32 {
        let cost = match &self.flood {
            Some(flood) => flood
                .lead
                .cost(usize::from(room))
                .or_else(|| flood.drawn.iter().find(|f| f.room == room).map(|f| f.cost)),
            None => self.lead.cost(usize::from(room)),
        };
        cost.map_or(u32::MAX, |cost| cost.max(0) as u32)
    }
}
