//! `psx_engine::CollisionTraceProvider` over the grid world.

use super::body::{Query, Raw, NO_Y};
use super::scan::nc;
use super::world::{GridCollision, COORD_LIMIT};
use core::cell::Cell;
use psx_engine::{
    CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, CollisionTraceShape, RoomPoint,
};

/// Traces the memo of a [`TraceMemo`] holds.
const MEMO_ENTRIES: usize = 12;

/// One remembered trace: the exact request and the answer it got.
#[derive(Copy, Clone)]
struct MemoEntry {
    /// `start`, `end`, then radius and height (both -1 for a point trace) and
    /// whether the camera's world answered.
    key: [i32; 9],
    trace: CollisionTrace,
}

/// The last traces a [`GridTraceProvider`] answered, so asking the same
/// question again costs a lookup instead of a walk through the world.
///
/// The answer to a trace is a pure function of the request, of the rooms
/// resident when it is asked ([`crate::ResidentRooms::epoch`]) and of whether
/// the camera's world asked: the world is immutable between two changes of
/// that set. The memo remembers the epoch its entries were made at and drops
/// them all when the set changes, so a hit is the very answer a fresh trace
/// would give. Camera and motor ask the same question several times in one
/// tick (and again the next while nothing moves), which is why this pays.
pub struct TraceMemo {
    /// Resident set epoch of the entries.
    epoch: u32,
    /// Entries in use, the first `used` of `entries`.
    used: u8,
    /// Slot the next new answer takes once all are in use.
    next: u8,
    entries: [MemoEntry; MEMO_ENTRIES],
    /// The column tests the traces made, see [`BlockedMemo`].
    blocked: BlockedMemo,
}

/// Column tests the memo of [`BlockedMemo`] keeps.
const BLOCKED_ENTRIES: usize = 10;

/// Answers of the "is this body inside solid" tests the traces make. One
/// trace often starts where another ended (the camera's segment check and
/// sight line, the motor's step from where its last step stood), so the
/// same test comes again with nothing changed in between. The cells let the
/// trace, which only reads the world, fill it in. The same rules as
/// [`TraceMemo`] decide when an answer may be reused.
pub(crate) struct BlockedMemo {
    epoch: Cell<u32>,
    used: Cell<u8>,
    next: Cell<u8>,
    /// Point, radius, height and the body and camera flags of each entry.
    keys: [[Cell<i32>; 6]; BLOCKED_ENTRIES],
    blocked: [Cell<bool>; BLOCKED_ENTRIES],
}

impl BlockedMemo {
    const fn new() -> Self {
        Self {
            epoch: Cell::new(0),
            used: Cell::new(0),
            next: Cell::new(0),
            keys: [const { [const { Cell::new(0) }; 6] }; BLOCKED_ENTRIES],
            blocked: [const { Cell::new(false) }; BLOCKED_ENTRIES],
        }
    }

    /// Forget every answer unless they were made at `epoch`.
    #[inline(always)]
    fn sync(&self, epoch: u32) {
        if self.epoch.get() != epoch {
            self.epoch.set(epoch);
            self.used.set(0);
            self.next.set(0);
        }
    }

    /// The answer remembered for `key`, if any.
    #[inline(always)]
    pub(crate) fn find(&self, key: &[i32; 6]) -> Option<bool> {
        for index in 0..usize::from(self.used.get()) {
            let entry = &self.keys[index];
            if entry[0].get() == key[0]
                && entry[2].get() == key[2]
                && entry[1].get() == key[1]
                && entry[3].get() == key[3]
                && entry[4].get() == key[4]
                && entry[5].get() == key[5]
            {
                return Some(self.blocked[index].get());
            }
        }
        None
    }

    /// Remember `blocked` as the answer to `key`, replacing the oldest entry
    /// once the memo is full.
    #[inline(always)]
    pub(crate) fn remember(&self, key: [i32; 6], blocked: bool) {
        let used = usize::from(self.used.get());
        let slot = if used < BLOCKED_ENTRIES {
            self.used.set(used as u8 + 1);
            used
        } else {
            let slot = usize::from(self.next.get());
            self.next.set(if slot + 1 == BLOCKED_ENTRIES {
                0
            } else {
                slot as u8 + 1
            });
            slot
        };
        for (cell, word) in self.keys[slot].iter().zip(key) {
            cell.set(word);
        }
        self.blocked[slot].set(blocked);
    }
}

impl TraceMemo {
    /// Answers the memo keeps.
    pub const CAPACITY: usize = MEMO_ENTRIES;

    /// An empty memo.
    pub const fn new() -> Self {
        const NONE: MemoEntry = MemoEntry {
            key: [0; 9],
            trace: CollisionTrace {
                all_solid: false,
                start_solid: false,
                fraction_q12: 0,
                end: RoomPoint::ZERO,
                normal_q12: [0; 3],
                plane_distance: 0,
            },
        };
        Self {
            epoch: 0,
            used: 0,
            next: 0,
            entries: [NONE; MEMO_ENTRIES],
            blocked: BlockedMemo::new(),
        }
    }

    /// Answers held now.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        usize::from(self.used)
    }

    /// Forget every entry unless they were made at `epoch`.
    #[inline(always)]
    fn sync(&mut self, epoch: u32) {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.used = 0;
            self.next = 0;
        }
        self.blocked.sync(epoch);
    }

    /// The answer remembered for `key`, if any.
    #[inline(always)]
    fn find(&self, key: &[i32; 9]) -> Option<CollisionTrace> {
        self.entries[..usize::from(self.used)]
            .iter()
            .find(|entry| same_key(&entry.key, key))
            .map(|entry| entry.trace)
    }

    /// Remember `trace` as the answer to `key`, replacing the oldest entry
    /// once the memo is full.
    #[inline(always)]
    fn remember(&mut self, key: [i32; 9], trace: CollisionTrace) {
        let slot = if usize::from(self.used) < MEMO_ENTRIES {
            self.used += 1;
            usize::from(self.used) - 1
        } else {
            let slot = usize::from(self.next);
            self.next = if slot + 1 == MEMO_ENTRIES {
                0
            } else {
                self.next + 1
            };
            slot
        };
        self.entries[slot] = MemoEntry { key, trace };
    }
}

/// Whether two keys are equal, word by word with an early exit: the array
/// `==` lowers to a call to `memcmp` on MIPS, which costs more than the whole
/// compare of a key that differs in its first words (nearly all of them do).
#[inline(always)]
fn same_key(a: &[i32; 9], b: &[i32; 9]) -> bool {
    a[0] == b[0]
        && a[3] == b[3]
        && a[2] == b[2]
        && a[5] == b[5]
        && a[1] == b[1]
        && a[4] == b[4]
        && a[6] == b[6]
        && a[7] == b[7]
        && a[8] == b[8]
}

impl Default for TraceMemo {
    fn default() -> Self {
        Self::new()
    }
}

/// Trace provider over a [`GridCollision`].
///
/// Bodies need `radius <= S / 2`; larger radii are clamped to `S` so a trace
/// stays bounded rather than failing. Queries with a coordinate beyond
/// [`COORD_LIMIT`] return `false` and leave the output untouched.
pub struct GridTraceProvider<'a, 'b> {
    world: &'b GridCollision<'a>,
    /// The world with its camera flag set, copied at construction so a trace
    /// carries no copy on the (scratchpad) stack.
    camera_world: Option<GridCollision<'a>>,
    /// Answers remembered across traces, see [`TraceMemo`].
    memo: Option<&'b mut TraceMemo>,
}

impl<'a, 'b> GridTraceProvider<'a, 'b> {
    pub fn new(world: &'b GridCollision<'a>) -> Self {
        Self {
            world,
            camera_world: None,
            memo: None,
        }
    }

    /// Answer repeated requests from `memo` instead of tracing them again.
    /// The memo may be shared by providers of the same resident set, the
    /// camera's among them: the camera flag is part of what it remembers.
    pub fn with_memo(mut self, memo: &'b mut TraceMemo) -> Self {
        self.memo = Some(memo);
        self
    }

    /// The provider of the camera arm: free planes flagged
    /// `COLPLANE_CAMERA_PASS` (translucent banners, grates) are not there for
    /// it. The motor and every other trace use [`Self::new`] and still
    /// collide with them.
    pub fn for_camera(world: &'b GridCollision<'a>) -> Self {
        Self {
            world,
            camera_world: Some(GridCollision {
                camera_pass: true,
                ..*world
            }),
            memo: None,
        }
    }
}

impl<'a> GridCollision<'a> {
    /// Expand a raw result into the engine's trace.
    #[inline(never)]
    fn finish(&self, q: &Query<'_>, raw: &Raw, output: &mut CollisionTrace) {
        output.all_solid = raw.all_solid;
        output.start_solid = raw.start_solid;
        output.fraction_q12 = raw.fraction;
        output.end = RoomPoint::from_array(if raw.end_y != NO_Y {
            [q.a[0], raw.end_y, q.a[2]]
        } else if raw.fraction == 4096 {
            // The whole segment: no interpolation.
            [q.a[0] + q.d[0], q.a[1] + q.d[1], q.a[2] + q.d[2]]
        } else {
            q.at(raw.fraction)
        });
        output.normal_q12 = match raw.normal {
            nc::NONE => [0; 3],
            nc::UP => [0, 4096, 0],
            nc::FLOOR => self.floor_normal(&raw.src),
            nc::PLATFORM_TOP => self.platform_normal(&raw.src),
            nc::WALL_PLANE => self.wall_plane_normal(&raw.src),
            code => axis_normal(code),
        };
        output.plane_distance = 0;
    }
}

/// Q12 vector of a non-floor normal code.
#[inline(never)]
fn axis_normal(code: u8) -> [i16; 3] {
    const D: i16 = 2896;
    match code {
        nc::DOWN => [0, -4096, 0],
        nc::PX => [4096, 0, 0],
        nc::NX => [-4096, 0, 0],
        nc::PZ => [0, 0, 4096],
        nc::NZ => [0, 0, -4096],
        nc::PP => [D, 0, D],
        nc::PN => [D, 0, -D],
        nc::NP => [-D, 0, D],
        nc::NN => [-D, 0, -D],
        _ => [0; 3],
    }
}

impl CollisionTraceProvider for GridTraceProvider<'_, '_> {
    #[inline(never)]
    fn trace_into(&mut self, query: CollisionTraceQuery, output: &mut CollisionTrace) -> bool {
        let a = query.start.to_array();
        let b = query.end.to_array();
        if a.iter()
            .chain(b.iter())
            .any(|&c| (c.wrapping_add(COORD_LIMIT) as u32) > 2 * COORD_LIMIT as u32)
        {
            return false;
        }
        let world = match &self.camera_world {
            Some(camera) => camera,
            None => self.world,
        };
        let memo_key = match self.memo.as_deref_mut() {
            Some(memo) => {
                let (radius, height) = match query.shape {
                    CollisionTraceShape::Point => (-1, -1),
                    CollisionTraceShape::Body { radius, height } => (radius, height),
                };
                let key = [
                    a[0],
                    a[1],
                    a[2],
                    b[0],
                    b[1],
                    b[2],
                    radius,
                    height,
                    i32::from(world.camera_pass),
                ];
                memo.sync(world.set.epoch());
                if let Some(trace) = memo.find(&key) {
                    *output = trace;
                    return true;
                }
                Some(key)
            }
            None => None,
        };
        let (r, h, body) = match query.shape {
            CollisionTraceShape::Point => (0, 0, false),
            CollisionTraceShape::Body { radius, height } => {
                (radius.max(0).min(world.s), height.max(0), true)
            }
        };
        let q = Query {
            a,
            d: [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            r: r as i16,
            h: h.min(30_000) as i16,
            body,
            memo: self.memo.as_deref().map(|memo| &memo.blocked),
        };
        let mut raw = Raw::new();
        world.trace(&q, &mut raw);
        if raw.contact_hi != 0 {
            world.contact(&q, raw.fraction, i32::from(raw.contact_hi), &mut raw);
        }
        world.finish(&q, &raw, output);
        if let (Some(memo), Some(key)) = (self.memo.as_deref_mut(), memo_key) {
            memo.remember(key, *output);
        }
        true
    }
}
