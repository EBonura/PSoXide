//! Budgeted portal flood: which rooms an observer can reach before a radius
//! or a page budget runs out.
//!
//! A level cut into many small rooms cannot be streamed by counting portal
//! hops (two hops mean a few sectors over tiny rooms and a whole level over
//! big halls), and a ring of the neighbours cannot reach as far as a pool of
//! pages could hold. The flood walks the portal graph from the observer with
//! a cheap integer distance, nearest portal first, and stops when the next
//! room is beyond the radius or would take the resident set past the page
//! budget. The set it returns is round, deterministic (the same start gives
//! the same list byte for byte) and costs what it reaches, not what the level
//! holds.
//!
//! Distance is [`octagonal_distance`]: within 10 percent of the straight
//! distance, shifts and adds only, so the guest pays no square root per
//! portal. Each portal may carry a Q4 weight ([`LevelView::flood_weight`]) that
//! makes it dearer to flood through.
//!
//! A search can be sliced: [`Flood::ensure`] does at most a given number of
//! queue pops per call and keeps the last finished result in force until the
//! next one is complete, so the guest never spends a whole search in one
//! frame and never sees a half-built set.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::format::FLOOD_WEIGHT_ONE;
use crate::view::LevelView;
use crate::walk::centre;

const UNREACHED: i32 = i32::MAX;
/// Largest per-axis delta [`octagonal_distance`] looks at, so its sums and
/// the Q4 weighting stay inside `i32`.
const AXIS_LIMIT: u32 = 1 << 22;

/// Distance between two world points in runtime units: the largest axis
/// delta plus 3/8 of the middle one plus 1/4 of the smallest. Within 7
/// percent of the straight distance; integer only.
pub fn octagonal_distance(a: [i32; 3], b: [i32; 3]) -> i32 {
    let delta = |axis: usize| a[axis].wrapping_sub(b[axis]).unsigned_abs().min(AXIS_LIMIT) as i32;
    let (x, y, z) = (delta(0), delta(1), delta(2));
    let largest = x.max(y).max(z);
    let smallest = x.min(y).min(z);
    let middle = x + y + z - largest - smallest;
    largest + ((middle * 3) >> 3) + (smallest >> 2)
}

/// One room a flood reached.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FloodRoom {
    pub room: u16,
    /// Flood cost from the start, runtime units (0 for the start room).
    pub cost: i32,
}

/// Why a finished flood stopped.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FloodEnd {
    /// Nothing is left within the radius: every reachable room is in.
    Exhausted,
    /// The next room would have taken the set past the page budget.
    Budget(u16),
}

/// What one finished flood did, for counters and for the cook's budget.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FloodStats {
    /// Portals relaxed.
    pub relaxations: u32,
    /// Queue pushes.
    pub pushes: u32,
    /// Queue pops (stale ones included).
    pub pops: u32,
    /// Pages of the rooms in the set.
    pub pages: u32,
    pub end: FloodEnd,
}

impl FloodStats {
    const NONE: Self = Self {
        relaxations: 0,
        pushes: 0,
        pops: 0,
        pages: 0,
        end: FloodEnd::Exhausted,
    };
}

/// What a flood is asked for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FloodQuery {
    pub room: usize,
    pub at: [i32; 3],
    /// Runtime units; portals dearer than this are not entered.
    pub radius: i32,
    /// Pages the set may hold in all; [`NO_PAGE_BUDGET`] for no bound.
    pub page_budget: u32,
}

/// `page_budget` standing for "no bound".
pub const NO_PAGE_BUDGET: u32 = u32::MAX;

#[derive(Copy, Clone, PartialEq, Eq)]
struct Node {
    cost: i32,
    portal: u16,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` pops the greatest: reverse so the cheapest pops first,
        // the lower portal index first on a tie.
        other
            .cost
            .cmp(&self.cost)
            .then(other.portal.cmp(&self.portal))
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A finished (or building) set: the rooms in flood order and a cost per
/// room, cleaned by touch.
struct RoomSet {
    cost: Vec<i32>,
    list: Vec<FloodRoom>,
}

impl RoomSet {
    fn new(rooms: usize) -> Self {
        Self {
            cost: alloc::vec![UNREACHED; rooms],
            list: Vec::new(),
        }
    }

    fn clear(&mut self) {
        for entry in &self.list {
            self.cost[usize::from(entry.room)] = UNREACHED;
        }
        self.list.clear();
    }

    fn insert(&mut self, room: usize, cost: i32) {
        self.cost[room] = cost;
        self.list.push(FloodRoom {
            room: room as u16,
            cost,
        });
    }
}

/// A flood over one level, with the scratch it needs and the last finished
/// result.
pub struct Flood {
    portal_cost: Vec<i32>,
    touched: Vec<u16>,
    heap: BinaryHeap<Node>,
    building: RoomSet,
    done: RoomSet,
    /// The search in progress.
    running: Option<FloodQuery>,
    /// The query the result in force answers.
    answered: Option<FloodQuery>,
    stats: FloodStats,
    /// Counters of the search in progress.
    progress: FloodStats,
    epoch: u32,
}

impl Flood {
    /// Scratch for `level`.
    pub fn new(level: &LevelView<'_>) -> Self {
        Self {
            portal_cost: alloc::vec![UNREACHED; level.portals.len()],
            touched: Vec::new(),
            heap: BinaryHeap::new(),
            building: RoomSet::new(level.rooms.len()),
            done: RoomSet::new(level.rooms.len()),
            running: None,
            answered: None,
            stats: FloodStats::NONE,
            progress: FloodStats::NONE,
            epoch: 0,
        }
    }

    /// Rooms of the last finished flood, in flood order (nearest first, the
    /// start room first).
    pub fn rooms(&self) -> &[FloodRoom] {
        &self.done.list
    }

    /// Flood cost of `room` in the last finished flood, or `None` when it
    /// was not reached.
    #[inline]
    pub fn cost(&self, room: usize) -> Option<i32> {
        self.done
            .cost
            .get(room)
            .copied()
            .filter(|&cost| cost != UNREACHED)
    }

    /// Whether the last finished flood reached `room`.
    #[inline]
    pub fn contains(&self, room: usize) -> bool {
        self.cost(room).is_some()
    }

    /// What the last finished flood did.
    pub fn stats(&self) -> FloodStats {
        self.stats
    }

    /// Changes whenever a flood finished.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Whether a search is in progress.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// The query the result in force answers, if there is one.
    pub fn answered(&self) -> Option<FloodQuery> {
        self.answered
    }

    /// Forget the result in force and any search in progress; the next
    /// [`Self::ensure`] starts over.
    pub fn invalidate(&mut self) {
        self.reset_scratch();
        self.running = None;
        self.answered = None;
        self.done.clear();
    }

    /// Run `query` to the end now and make it the result in force. For the
    /// host (the cook, the editor, tests); the guest uses [`Self::ensure`].
    pub fn solve(&mut self, level: &LevelView<'_>, query: FloodQuery) {
        self.begin(level, query);
        self.advance(level, u32::MAX);
    }

    /// Keep the result in force answering `query`, doing at most `pops`
    /// queue pops of work. A query that moved less than `tolerance` runtime
    /// units along every axis from the last one asked (same room, radius and
    /// budget) changes nothing. Returns whether a flood finished in this
    /// call, so the caller knows to read [`Self::rooms`] again.
    pub fn ensure(
        &mut self,
        level: &LevelView<'_>,
        query: FloodQuery,
        tolerance: i32,
        pops: u32,
    ) -> bool {
        let last = self.running.or(self.answered);
        let same = last.is_some_and(|last| {
            last.room == query.room
                && last.radius == query.radius
                && last.page_budget == query.page_budget
                && (0..3).all(|axis| (query.at[axis] - last.at[axis]).abs() < tolerance)
        });
        if !same {
            self.begin(level, query);
        }
        if self.running.is_some() {
            return self.advance(level, pops);
        }
        false
    }

    fn reset_scratch(&mut self) {
        for &portal in &self.touched {
            self.portal_cost[usize::from(portal)] = UNREACHED;
        }
        self.touched.clear();
        self.heap.clear();
        self.building.clear();
    }

    fn begin(&mut self, level: &LevelView<'_>, query: FloodQuery) {
        self.reset_scratch();
        self.progress = FloodStats::NONE;
        if query.room >= level.rooms.len() {
            // Outside the level: an empty set, answered at once.
            self.running = None;
            self.finish(query, FloodEnd::Exhausted);
            return;
        }
        self.building.insert(query.room, 0);
        self.progress.pages = pages_of_room(level, query.room);
        self.running = Some(query);
        self.relax_from(level, query.room, query.at, 0, query);
    }

    /// Queue every portal of `room`, reached from `from` at `cost`.
    fn relax_from(
        &mut self,
        level: &LevelView<'_>,
        room: usize,
        from: [i32; 3],
        cost: i32,
        query: FloodQuery,
    ) {
        let record = &level.rooms[room];
        let first = usize::from(record.portal_first);
        for index in first..first + usize::from(record.portal_count) {
            self.progress.relaxations += 1;
            let weight = level
                .flood_weight
                .get(index)
                .map_or(i32::from(FLOOD_WEIGHT_ONE), |&w| i32::from(w));
            let to = centre(record, &level.portals[index]);
            let next = cost.saturating_add((octagonal_distance(from, to) * weight) >> 4);
            if next >= self.portal_cost[index] || next > query.radius {
                continue;
            }
            if self.portal_cost[index] == UNREACHED {
                self.touched.push(index as u16);
            }
            self.portal_cost[index] = next;
            self.progress.pushes += 1;
            self.heap.push(Node {
                cost: next,
                portal: index as u16,
            });
        }
    }

    /// Do up to `pops` queue pops of the search in progress. Returns whether
    /// it finished.
    fn advance(&mut self, level: &LevelView<'_>, pops: u32) -> bool {
        let Some(query) = self.running else {
            return false;
        };
        let mut left = pops;
        while left > 0 {
            let Some(Node { cost, portal }) = self.heap.pop() else {
                self.finish(query, FloodEnd::Exhausted);
                return true;
            };
            left -= 1;
            self.progress.pops += 1;
            if cost > self.portal_cost[usize::from(portal)] {
                continue;
            }
            let dest = usize::from(level.portals[usize::from(portal)].dest);
            if dest >= level.rooms.len() || self.building.cost[dest] != UNREACHED {
                continue;
            }
            let dest_pages = pages_of_room(level, dest);
            if self.progress.pages.saturating_add(dest_pages) > query.page_budget {
                self.finish(query, FloodEnd::Budget(dest as u16));
                return true;
            }
            self.progress.pages += dest_pages;
            self.building.insert(dest, cost);
            // Leaving through the portal puts the walker at its centre, which
            // is also where the matching portal of `dest` opens.
            let source = source_room(level, usize::from(portal));
            let at = centre(&level.rooms[source], &level.portals[usize::from(portal)]);
            self.relax_from(level, dest, at, cost, query);
        }
        false
    }

    fn finish(&mut self, query: FloodQuery, end: FloodEnd) {
        self.running = None;
        self.done.clear();
        core::mem::swap(&mut self.done, &mut self.building);
        self.stats = FloodStats {
            end,
            ..self.progress
        };
        self.answered = Some(query);
        self.epoch = self.epoch.wrapping_add(1);
        self.reset_scratch();
    }
}

/// Pages (2 KiB) the chunk of `room` takes.
#[inline]
pub fn pages_of_room(level: &LevelView<'_>, room: usize) -> u32 {
    u32::from(level.rooms[room].pack_sectors)
}

/// The room a portal belongs to (rooms own contiguous portal runs).
fn source_room(level: &LevelView<'_>, portal: usize) -> usize {
    level
        .rooms
        .partition_point(|r| usize::from(r.portal_first) <= portal)
        .saturating_sub(1)
        .min(level.rooms.len() - 1)
}
