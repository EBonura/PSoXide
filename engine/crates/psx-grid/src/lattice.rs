//! The world lattice: a coarse plan grid over every room footprint, so a
//! query that knows where it is finds the few rooms near it without walking
//! the whole directory.
//!
//! The lattice is part of the level core (version 4) and is built from the
//! room records alone, so it covers resident and non-resident rooms alike: a
//! room that is not loaded still has to be found to stand in for the solid
//! box it leaves behind. Cells are squares of `1 << shift` runtime units
//! starting at the lowest room corner, so the cell of a coordinate is one
//! subtraction and one shift. Each cell lists, in ascending room order, every
//! room whose footprint touches it (compressed rows: `start[i]..start[i+1]`
//! indexes `rooms`).

use alloc::vec::Vec;

use crate::format::{LevelHeader, RoomRecord};
use crate::view::LevelView;

/// Worlds with at most this many rooms skip the lattice: a loop over a
/// handful of footprints costs less than the cell lookup.
pub const SMALL_WORLD_ROOMS: usize = 4;

/// Smallest and largest cell edge exponents the cook considers (128 to
/// 32768 runtime units).
const MIN_SHIFT: u8 = 7;
const MAX_SHIFT: u8 = 15;

/// The lattice tables as they are written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LatticeTables {
    pub shift: u8,
    pub origin: [i32; 2],
    pub cells: [u16; 2],
    /// `cells[0] * cells[1] + 1` row starts.
    pub start: Vec<u16>,
    /// Room ids, row by row.
    pub rooms: Vec<u16>,
}

/// How far (in sectors) a room's footprint is dilated before it is listed in
/// the cells it touches. A body is at most one sector in radius, so the list
/// of the one cell holding a body's centre already names every room the body
/// can touch, with room to spare: a body query reads one cell, never several.
pub const MARGIN_SECTORS: i32 = 2;

fn footprint(record: &RoomRecord, s: i32) -> ([i32; 2], [i32; 2]) {
    let lo = [record.origin[0], record.origin[2]];
    let hi = [
        record.origin[0] + i32::from(record.width) * s,
        record.origin[2] + i32::from(record.depth) * s,
    ];
    (lo, hi)
}

impl LatticeTables {
    /// Build the lattice of `rooms` with sector size `s`.
    ///
    /// Footprints are dilated by [`MARGIN_SECTORS`] sectors before they are
    /// listed. The cell edge is the smallest power of two that keeps the
    /// cell count within `max(16, 2 * rooms)` and the room list within
    /// `u16`.
    pub fn build(rooms: &[RoomRecord], s: u32) -> Result<Self, alloc::string::String> {
        Self::build_owned(rooms, s, &[], 2)
    }

    /// [`Self::build`] for rooms that own only some sectors of their
    /// rectangle (`occupancy`, one bit per sector row-major, as the core
    /// carries it; empty for whole rectangles): a room is listed in a cell
    /// only when a sector it owns, dilated by the margin, touches it. The
    /// cell budget is `max(16, per_room * rooms)`.
    pub fn build_owned(
        rooms: &[RoomRecord],
        s: u32,
        occupancy: &[Vec<u8>],
        per_room: usize,
    ) -> Result<Self, alloc::string::String> {
        if rooms.is_empty() {
            return Ok(Self {
                start: alloc::vec![0, 0],
                cells: [1, 1],
                shift: MAX_SHIFT,
                ..Self::default()
            });
        }
        let s = s as i32;
        let margin = MARGIN_SECTORS * s;
        let mut lo = [i32::MAX; 2];
        let mut hi = [i32::MIN; 2];
        for record in rooms {
            let (a, b) = footprint(record, s);
            let (a, b) = (
                [a[0] - margin, a[1] - margin],
                [b[0] + margin, b[1] + margin],
            );
            for axis in 0..2 {
                lo[axis] = lo[axis].min(a[axis]);
                hi[axis] = hi[axis].max(b[axis]);
            }
        }
        let budget = (per_room * rooms.len()).max(16);
        let mut shift = MAX_SHIFT;
        let mut cells = [1usize; 2];
        for candidate in MIN_SHIFT..=MAX_SHIFT {
            let edge = 1u32 << candidate;
            // `hi >= lo` on both axes, so the extent is a plain u32;
            // `div_ceil` rounds up without forming `extent + edge`.
            let dim = |axis: usize| (((hi[axis] - lo[axis]) as u32).div_ceil(edge)).max(1) as usize;
            let (cx, cz) = (dim(0), dim(1));
            if cx * cz <= budget && cx <= usize::from(u16::MAX) && cz <= usize::from(u16::MAX) {
                shift = candidate;
                cells = [cx, cz];
                break;
            }
            if candidate == MAX_SHIFT {
                cells = [cx.min(usize::from(u16::MAX)), cz.min(usize::from(u16::MAX))];
            }
        }
        let total = cells[0] * cells[1];
        let mut lists: Vec<Vec<u16>> = alloc::vec![Vec::new(); total];
        for (index, record) in rooms.iter().enumerate() {
            let (a, b) = footprint(record, s);
            let (a, b) = (
                [a[0] - margin, a[1] - margin],
                [b[0] + margin, b[1] + margin],
            );
            let cell = |v: i32, axis: usize, last: bool| -> usize {
                let v = if last { v - 1 } else { v };
                (((v - lo[axis]) >> shift).max(0) as usize).min(cells[axis] - 1)
            };
            let (cx0, cx1) = (cell(a[0], 0, false), cell(b[0], 0, true));
            let (cz0, cz1) = (cell(a[1], 1, false), cell(b[1], 1, true));
            let owned = occupancy.get(index);
            for cz in cz0..=cz1 {
                for cx in cx0..=cx1 {
                    let listed = match owned {
                        None => true,
                        Some(bits) => {
                            // The cell, shrunk back by the margin, in the
                            // room's sectors: any owned sector inside?
                            let edge = 1i32 << shift;
                            let (x0, z0) = (lo[0] + cx as i32 * edge, lo[1] + cz as i32 * edge);
                            let range = |a: i32, origin: i32, n: u8| {
                                let first = (a - margin - origin).div_euclid(s).max(0);
                                let last = (a + edge + margin - 1 - origin)
                                    .div_euclid(s)
                                    .min(i32::from(n) - 1);
                                (first, last)
                            };
                            let (sx0, sx1) = range(x0, record.origin[0], record.width);
                            let (sz0, sz1) = range(z0, record.origin[2], record.depth);
                            let w = usize::from(record.width);
                            (sz0..=sz1).any(|z| {
                                (sx0..=sx1).any(|x| {
                                    let bit = z as usize * w + x as usize;
                                    bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
                                })
                            })
                        }
                    };
                    if listed {
                        lists[cz * cells[0] + cx].push(index as u16);
                    }
                }
            }
        }
        let refs: usize = lists.iter().map(Vec::len).sum();
        if refs > usize::from(u16::MAX) {
            return Err(alloc::format!(
                "the world lattice lists {refs} room references, at most 65535 (too many rooms overlap the same cells)"
            ));
        }
        let mut start = Vec::with_capacity(total + 1);
        let mut flat = Vec::with_capacity(refs);
        for list in &lists {
            start.push(flat.len() as u16);
            flat.extend_from_slice(list);
        }
        start.push(flat.len() as u16);
        Ok(Self {
            shift,
            origin: lo,
            cells: [cells[0] as u16, cells[1] as u16],
            start,
            rooms: flat,
        })
    }
}

/// The lattice as the reader sees it.
#[derive(Copy, Clone, Debug)]
pub struct Lattice<'a> {
    /// How far footprints were dilated before listing, runtime units
    /// (`MARGIN_SECTORS` sectors).
    pub margin: i32,
    pub shift: u8,
    pub origin: [i32; 2],
    pub cells: [u16; 2],
    pub start: &'a [u16],
    pub rooms: &'a [u16],
}

impl<'a> Lattice<'a> {
    /// Rooms listed in the cell holding the point `(x, z)`: every room whose
    /// footprint comes within [`MARGIN_SECTORS`] sectors of the cell, so for
    /// any box of half-extent up to the margin centred on the point it holds
    /// every room the box can touch. A plain slice of the room list, nothing
    /// to iterate or carry.
    #[inline(always)]
    pub fn rooms_at(&self, x: i32, z: i32) -> &'a [u16] {
        let cx = x.wrapping_sub(self.origin[0]) >> self.shift;
        let cz = z.wrapping_sub(self.origin[1]) >> self.shift;
        if (cx as u32) >= u32::from(self.cells[0]) || (cz as u32) >= u32::from(self.cells[1]) {
            return &[];
        }
        let i = cz as usize * usize::from(self.cells[0]) + cx as usize;
        let (first, end) = (usize::from(self.start[i]), usize::from(self.start[i + 1]));
        self.rooms.get(first..end).unwrap_or(&[])
    }

    /// Whether the tables agree with the header and with each other.
    pub fn consistent(&self, room_count: usize) -> bool {
        let total = usize::from(self.cells[0]) * usize::from(self.cells[1]);
        self.start.len() == total + 1
            && usize::from(self.start.last().copied().unwrap_or(0)) == self.rooms.len()
            && self.start.windows(2).all(|w| w[0] <= w[1])
            && self.rooms.iter().all(|&r| usize::from(r) < room_count)
            && (MIN_SHIFT..=MAX_SHIFT).contains(&self.shift)
    }

    /// Rooms whose footprint may touch the inclusive plan box
    /// `[x0, x1] x [z0, z1]` (world runtime units), each at most once.
    #[inline]
    pub fn candidates(
        &self,
        level: &'a LevelView<'a>,
        (x0, z0): (i32, i32),
        (x1, z1): (i32, i32),
    ) -> Candidates<'a> {
        let cell = |v: i32, axis: usize| (v.wrapping_sub(self.origin[axis])) >> self.shift;
        let (nx, nz) = (i32::from(self.cells[0]), i32::from(self.cells[1]));
        let (cx0, cx1) = (cell(x0, 0), cell(x1, 0));
        let (cz0, cz1) = (cell(z0, 1), cell(z1, 1));
        let mut it = Candidates {
            level,
            x0: cx0.max(0) as u16,
            z0: cz0.max(0) as u16,
            x1: cx1.min(nx - 1).max(0) as u16,
            z1: cz1.min(nz - 1).max(0) as u16,
            cx: 0,
            cz: 0,
            k: 0,
            end: 0,
        };
        if cx1 < 0 || cz1 < 0 || cx0 >= nx || cz0 >= nz {
            // Entirely outside the lattice: nothing to visit.
            it.cz = 1;
            it.z1 = 0;
            return it;
        }
        it.cx = it.x0;
        it.cz = it.z0;
        it.load();
        it
    }
}

/// Iterator over the rooms near a plan box, ascending within a cell. Small
/// on purpose: it lives in the frame of a collision trace, which runs on a
/// 1 KiB scratchpad stack.
pub struct Candidates<'a> {
    level: &'a LevelView<'a>,
    /// Visited cell range, inclusive, clamped to the lattice.
    x0: u16,
    z0: u16,
    x1: u16,
    z1: u16,
    cx: u16,
    cz: u16,
    k: u16,
    end: u16,
}

impl Candidates<'_> {
    #[inline(always)]
    fn load(&mut self) {
        let i = usize::from(self.cz) * usize::from(self.level.header.lattice_cells[0])
            + usize::from(self.cx);
        self.k = self.level.lattice_start[i];
        self.end = self.level.lattice_start[i + 1];
    }
}

impl Iterator for Candidates<'_> {
    type Item = usize;

    #[inline]
    fn next(&mut self) -> Option<usize> {
        loop {
            if self.k < self.end {
                let room = usize::from(self.level.lattice_rooms[usize::from(self.k)]);
                self.k += 1;
                // A room is listed in every cell it touches; it is yielded in
                // the first visited one, the cell holding the corner of its
                // (dilated) overlap with the visited range.
                let h = self.level.header;
                let margin = MARGIN_SECTORS * i32::from(h.sector_units);
                let origin = self.level.rooms[room].origin;
                let first = |v: i32, axis: usize, low: u16| -> u16 {
                    (((v - margin).wrapping_sub(h.lattice_origin[axis]) >> h.lattice_shift)
                        .max(i32::from(low))
                        .min(i32::from(h.lattice_cells[axis]) - 1)) as u16
                };
                if self.cx == first(origin[0], 0, self.x0)
                    && self.cz == first(origin[2], 1, self.z0)
                {
                    return Some(room);
                }
                continue;
            }
            if self.cz > self.z1 {
                return None;
            }
            self.cx += 1;
            if self.cx > self.x1 {
                self.cx = self.x0;
                self.cz += 1;
                if self.cz > self.z1 {
                    return None;
                }
            }
            self.load();
        }
    }
}

/// Rooms near a plan box: every room of a small world, the lattice cells'
/// rooms otherwise.
pub enum RoomsNear<'a> {
    All(core::ops::Range<u32>),
    Lattice(Candidates<'a>),
}

impl Iterator for RoomsNear<'_> {
    type Item = usize;

    #[inline(always)]
    fn next(&mut self) -> Option<usize> {
        match self {
            Self::All(range) => range.next().map(|room| room as usize),
            Self::Lattice(candidates) => candidates.next(),
        }
    }
}

impl LevelHeader {
    /// The lattice descriptor the header carries, tables supplied by the
    /// caller.
    pub fn lattice<'a>(&self, start: &'a [u16], rooms: &'a [u16]) -> Lattice<'a> {
        Lattice {
            margin: MARGIN_SECTORS * i32::from(self.sector_units),
            shift: self.lattice_shift,
            origin: self.lattice_origin,
            cells: self.lattice_cells,
            start,
            rooms,
        }
    }
}
