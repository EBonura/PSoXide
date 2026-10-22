//! Walk distance over the portal graph: how far a body would have to walk,
//! through portal openings, to reach each room.
//!
//! Hops stop meaning distance once rooms are cut to a target size (two hops
//! over a corridor of tiny rooms is a few sectors, over big halls it is a
//! whole level), so draw range, entity activation and the set a streamer
//! must keep resident are all radii in sectors over this distance. The
//! distance to a room is the shortest path from the start point to the
//! centre of any portal that opens into it, going portal centre to portal
//! centre through each room on the way.
//!
//! One [`WalkSet`] holds its scratch (a cost per room and per portal) and
//! cleans only what a search touched, so a search costs what it reaches, not
//! the size of the level.

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Ordering;

use psx_math::int32::isqrt_i32;

use crate::format::{Portal, RoomRecord};
use crate::view::LevelView;

const UNREACHED: i32 = i32::MAX;
/// Per-axis delta clamp: three squares of it stay inside `i32`.
const AXIS_LIMIT: i32 = 16_383;

/// Straight distance between two world points, saturating far out.
pub(crate) fn distance(a: [i32; 3], b: [i32; 3]) -> i32 {
    let d = |axis: usize| (a[axis].wrapping_sub(b[axis])).clamp(-AXIS_LIMIT, AXIS_LIMIT);
    let (x, y, z) = (d(0), d(1), d(2));
    isqrt_i32(x * x + y * y + z * z)
}

/// World position of a portal's opening centre.
pub fn centre(room: &RoomRecord, portal: &Portal) -> [i32; 3] {
    let mut sum = [0i32; 3];
    for corner in &portal.v {
        for axis in 0..3 {
            sum[axis] += i32::from(corner[axis]);
        }
    }
    [
        room.origin[0] + sum[0] / 4,
        room.origin[1] + sum[1] / 4,
        room.origin[2] + sum[2] / 4,
    ]
}

#[derive(Copy, Clone, PartialEq, Eq)]
struct Node {
    cost: i32,
    portal: u16,
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap pops the greatest: reverse so the cheapest pops first.
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

/// Where a cached walk was started from.
#[derive(Copy, Clone)]
struct Anchor {
    room: usize,
    at: [i32; 3],
    radius: i32,
}

/// Rooms within a walk radius of a point, with their distances.
pub struct WalkSet {
    room_cost: Vec<i32>,
    portal_cost: Vec<i32>,
    rooms: Vec<u16>,
    portals: Vec<u16>,
    heap: BinaryHeap<Node>,
    anchor: Option<Anchor>,
    epoch: u32,
}

impl WalkSet {
    /// Scratch for `level`.
    pub fn new(level: &LevelView<'_>) -> Self {
        Self {
            room_cost: alloc::vec![UNREACHED; level.rooms.len()],
            portal_cost: alloc::vec![UNREACHED; level.portals.len()],
            rooms: Vec::new(),
            portals: Vec::new(),
            heap: BinaryHeap::new(),
            anchor: None,
            epoch: 0,
        }
    }

    fn clear(&mut self) {
        for &room in &self.rooms {
            self.room_cost[usize::from(room)] = UNREACHED;
        }
        for &portal in &self.portals {
            self.portal_cost[usize::from(portal)] = UNREACHED;
        }
        self.rooms.clear();
        self.portals.clear();
        self.heap.clear();
    }

    fn reach(&mut self, room: usize, cost: i32) {
        let slot = &mut self.room_cost[room];
        if *slot == UNREACHED {
            self.rooms.push(room as u16);
        }
        *slot = (*slot).min(cost);
    }

    fn expand(&mut self, level: &LevelView<'_>, room: usize, at: [i32; 3], cost: i32, radius: i32) {
        let record = &level.rooms[room];
        let first = usize::from(record.portal_first);
        for index in first..first + usize::from(record.portal_count) {
            let portal = &level.portals[index];
            let next = cost.saturating_add(distance(at, centre(record, portal)));
            if next > radius || next >= self.portal_cost[index] {
                continue;
            }
            if self.portal_cost[index] == UNREACHED {
                self.portals.push(index as u16);
            }
            self.portal_cost[index] = next;
            self.heap.push(Node {
                cost: next,
                portal: index as u16,
            });
        }
    }

    /// Search from `from` inside `room`, out to `radius` runtime units.
    pub fn compute(&mut self, level: &LevelView<'_>, room: usize, from: [i32; 3], radius: i32) {
        self.clear();
        self.epoch = self.epoch.wrapping_add(1);
        self.anchor = Some(Anchor {
            room,
            at: from,
            radius,
        });
        if room >= level.rooms.len() {
            return;
        }
        self.reach(room, 0);
        self.expand(level, room, from, 0, radius);
        while let Some(Node { cost, portal }) = self.heap.pop() {
            if cost > self.portal_cost[usize::from(portal)] {
                continue;
            }
            let dest = usize::from(level.portals[usize::from(portal)].dest);
            if dest >= level.rooms.len() {
                continue;
            }
            self.reach(dest, cost);
            // Leaving through the portal puts the walker at its centre, which
            // is also where the matching portal of `dest` opens.
            let source = self.source_of(level, usize::from(portal));
            let at = centre(&level.rooms[source], &level.portals[usize::from(portal)]);
            self.expand(level, dest, at, cost, radius);
        }
    }

    /// The room a portal belongs to (rooms own contiguous portal runs).
    fn source_of(&self, level: &LevelView<'_>, portal: usize) -> usize {
        level
            .rooms
            .partition_point(|r| usize::from(r.portal_first) <= portal)
            .saturating_sub(1)
            .min(level.rooms.len() - 1)
    }

    /// Like [`Self::compute`], but only when the start moved out of the
    /// cached search: another room, another radius, or `tolerance` runtime
    /// units away along an axis. Returns whether it searched again.
    pub fn ensure(
        &mut self,
        level: &LevelView<'_>,
        room: usize,
        from: [i32; 3],
        radius: i32,
        tolerance: i32,
    ) -> bool {
        if let Some(anchor) = self.anchor {
            let moved = (0..3).any(|axis| (from[axis] - anchor.at[axis]).abs() >= tolerance);
            if anchor.room == room && anchor.radius == radius && !moved {
                return false;
            }
        }
        self.compute(level, room, from, radius);
        true
    }

    /// Forget the cached search (the next [`Self::ensure`] searches again).
    pub fn invalidate(&mut self) {
        self.anchor = None;
    }

    /// Changes whenever a new search ran.
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Walk distance to `room`, runtime units, or `None` beyond the radius.
    #[inline]
    pub fn cost(&self, room: usize) -> Option<i32> {
        self.room_cost
            .get(room)
            .copied()
            .filter(|&c| c != UNREACHED)
    }

    /// Whether `room` is within the radius searched.
    #[inline]
    pub fn contains(&self, room: usize) -> bool {
        self.cost(room).is_some()
    }

    /// Whether `room` is within `radius` runtime units (a radius of at most
    /// the one searched).
    #[inline]
    pub fn within(&self, room: usize, radius: i32) -> bool {
        self.cost(room).is_some_and(|c| c <= radius)
    }

    /// Rooms reached, in the order the search found them.
    pub fn rooms(&self) -> impl Iterator<Item = usize> + '_ {
        self.rooms.iter().map(|&r| usize::from(r))
    }

    /// Rooms reached.
    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    /// Whether nothing was reached (no search ran, or the start was outside
    /// the level).
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }
}
