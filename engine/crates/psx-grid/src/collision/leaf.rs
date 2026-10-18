//! Leaf lookup for tree rooms (`CHUNK_TREE`): from a sector, and a point in
//! it, to the leaf record that holds the point and the leaf's frame (origin
//! and edge), in two dependent loads and no loop.
//!
//! A tree room's `SECTORS` table holds one record per leaf. The per-sector
//! `SECTOR_INDEX` names, for each sector, the leaf that fills it (a leaf of
//! one, two or four sectors; the record carries its own size code), or a
//! 16 entry block of `SUBBLOCKS` when the sector is divided into leaves of
//! half or a quarter of its edge. A block entry is the leaf covering that
//! quarter sector. All of it is integer and branchy only on the index flag.
//!
//! Heights and wall tests then run in the **leaf frame**: offsets from the
//! leaf's own origin, edge `size`, and the reciprocal for that edge
//! ([`LeafDims`]). A dense room never reaches this file.

use super::world::GridCollision;
use crate::format::*;
use crate::height::weight_recip;
use crate::view::CollisionView;

/// Edge and interpolation reciprocal of every leaf size code, built once
/// from the level's sector edge.
#[derive(Copy, Clone, Debug)]
pub struct LeafDims {
    /// Edge in runtime units, indexed by [`leaf_code`].
    pub size: [i32; 5],
    /// `ceil(2^24 / size)`, the weight reciprocal of [`crate::height`].
    pub inv24: [u32; 5],
}

impl LeafDims {
    /// The dimensions for sector edge `s`. A level whose edge is not a
    /// multiple of four cannot hold sub-sector leaves (the cook refuses
    /// them); the half and quarter entries are then simply never used.
    pub fn for_sector(s: i32) -> Self {
        let size = [
            leaf_edge(s, leaf_code::SECTOR),
            leaf_edge(s, leaf_code::X2),
            leaf_edge(s, leaf_code::X4),
            leaf_edge(s, leaf_code::HALF),
            leaf_edge(s, leaf_code::QUARTER),
        ];
        Self {
            size,
            inv24: size.map(|e| weight_recip(e.max(1) as u32)),
        }
    }
}

/// What a sector holds when it has no leaf: no floor, no ceiling, no walls.
/// The same as an all zero record of a dense room.
pub(crate) static EMPTY: Sector = Sector {
    floor: [0; 4],
    ceil: [0; 4],
    flags: 0,
    ext: NO_EXT,
};

/// A leaf found by a lookup, with its frame in the room.
#[derive(Copy, Clone)]
pub(crate) struct LeafAt<'v> {
    pub rec: &'v Sector,
    /// Minimum corner, room-local runtime units.
    pub ox: i32,
    pub oz: i32,
    /// Edge, runtime units.
    pub size: i32,
    /// `1 + qz * 4 + qx` when the leaf was reached through a sub-block (the
    /// quarter sector the lookup landed in), else 0. With the sector it
    /// names the leaf again ([`GridCollision::leaf_named`]).
    pub quarter: u8,
    /// Index of the leaf's record in `SECTORS` ([`NO_LEAF`] for an empty
    /// sector or quarter; the cell index of the record in a dense room): the
    /// key of its extra spans ([`CollisionView::extra_spans`]).
    pub id: u16,
}

impl<'a> GridCollision<'a> {
    /// Edge of a leaf of size code `code`.
    #[inline(always)]
    pub(crate) fn leaf_size(&self, code: u16) -> i32 {
        self.leaf.size[usize::from(code).min(4)]
    }

    /// Weight reciprocal of a leaf of size code `code`.
    #[inline(always)]
    pub(crate) fn leaf_inv(&self, code: u16) -> u32 {
        self.leaf.inv24[usize::from(code).min(4)]
    }

    /// Edge and weight reciprocal for a record, from its size code. Code 0
    /// (every record of a dense room) is the sector's own pair.
    #[inline(always)]
    pub(crate) fn dims_of(&self, flags: u16) -> (u32, u32) {
        let code = flags >> SECTOR_LEAF_SHIFT;
        if code == 0 {
            (self.s as u32, self.inv24)
        } else {
            (self.leaf_size(code) as u32, self.leaf_inv(code))
        }
    }

    /// Quarter sector (0 to 3) of a sector-local offset: three compares, no
    /// division.
    #[inline(always)]
    fn quarter_of(v: i32, q: i32) -> i32 {
        i32::from(v >= q) + i32::from(v >= 2 * q) + i32::from(v >= 3 * q)
    }

    /// The leaf of `view` (a tree room) in sector `(cx, cz)` that holds the
    /// sector-local offset `(lx, lz)`. An empty sector, or an empty quarter,
    /// gives [`EMPTY`] with the frame of what it would have been.
    #[inline(never)]
    pub(crate) fn leaf_in<'v>(
        &self,
        view: &CollisionView<'v>,
        cx: usize,
        cz: usize,
        lx: i32,
        lz: i32,
    ) -> LeafAt<'v> {
        let s = self.s;
        let (sx, sz) = (cx as i32 * s, cz as i32 * s);
        let Some(&entry) = view.index.get(cz * view.width() + cx) else {
            return LeafAt {
                rec: &EMPTY,
                ox: sx,
                oz: sz,
                size: s,
                quarter: 0,
                id: NO_LEAF,
            };
        };
        let id = entry & INDEX_ID;
        if entry & INDEX_SUB != 0 {
            let q = s / 4;
            let (qx, qz) = (Self::quarter_of(lx, q), Self::quarter_of(lz, q));
            let slot = usize::from(id) * SUBBLOCK_ENTRIES + (qz * 4 + qx) as usize;
            let leaf = view.blocks.get(slot).copied().unwrap_or(NO_LEAF);
            let quarter = (1 + qz * 4 + qx) as u8;
            let rec = if leaf == NO_LEAF {
                None
            } else {
                view.sectors.get(usize::from(leaf))
            };
            return match rec {
                Some(rec) if rec.flags >> SECTOR_LEAF_SHIFT == leaf_code::HALF => {
                    let (hx, hz) = (qx & !1, qz & !1);
                    LeafAt {
                        rec,
                        ox: sx + hx * q,
                        oz: sz + hz * q,
                        size: 2 * q,
                        quarter,
                        id: leaf,
                    }
                }
                Some(rec) => LeafAt {
                    rec,
                    ox: sx + qx * q,
                    oz: sz + qz * q,
                    size: q,
                    quarter,
                    id: leaf,
                },
                None => LeafAt {
                    rec: &EMPTY,
                    ox: sx + qx * q,
                    oz: sz + qz * q,
                    size: q,
                    quarter,
                    id: NO_LEAF,
                },
            };
        }
        self.leaf_of_sector(view, cx, cz, id)
    }

    /// The leaf with id `id` that fills sector `(cx, cz)` (an index entry
    /// that is not a sub-block): a sector, or a leaf of two or four sectors
    /// whose origin follows from the room's `origin_mod`.
    #[inline(always)]
    fn leaf_of_sector<'v>(
        &self,
        view: &CollisionView<'v>,
        cx: usize,
        cz: usize,
        id: u16,
    ) -> LeafAt<'v> {
        let s = self.s;
        let rec = if id == NO_LEAF {
            None
        } else {
            view.sectors.get(usize::from(id))
        };
        let Some(rec) = rec else {
            return LeafAt {
                rec: &EMPTY,
                ox: cx as i32 * s,
                oz: cz as i32 * s,
                size: s,
                quarter: 0,
                id: NO_LEAF,
            };
        };
        let code = rec.flags >> SECTOR_LEAF_SHIFT;
        if code == leaf_code::X2 || code == leaf_code::X4 {
            let (mx, mz) = view.origin_mod();
            let x0 = leaf_origin_sector(cx as i32, code, mx);
            let z0 = leaf_origin_sector(cz as i32, code, mz);
            return LeafAt {
                rec,
                ox: x0 * s,
                oz: z0 * s,
                size: self.leaf_size(code),
                quarter: 0,
                id,
            };
        }
        LeafAt {
            rec,
            ox: cx as i32 * s,
            oz: cz as i32 * s,
            size: s,
            quarter: 0,
            id,
        }
    }

    /// The leaf named by a sector and a quarter code (`LeafAt::quarter`):
    /// what a hit's source remembers.
    pub(crate) fn leaf_named<'v>(
        &self,
        view: &CollisionView<'v>,
        cx: usize,
        cz: usize,
        quarter: u8,
    ) -> LeafAt<'v> {
        let q = self.s / 4;
        let k = i32::from(quarter.max(1)) - 1;
        // The middle of the named quarter sector lies inside its leaf.
        let (lx, lz) = if quarter == 0 {
            (0, 0)
        } else {
            ((k & 3) * q, (k >> 2) * q)
        };
        self.leaf_in(view, cx, cz, lx, lz)
    }

    /// How many index slots sector `(cx, cz)` lists leaves in: one for a
    /// dense room or a sector filled by one leaf, 16 for a divided sector
    /// (a leaf of half a sector appears four times; callers that must not
    /// repeat work skip the slots that are not its first).
    #[inline(always)]
    pub(crate) fn cell_slots(view: &CollisionView<'_>, cx: usize, cz: usize) -> usize {
        match view.index.get(cz * view.width() + cx) {
            Some(e) if e & INDEX_SUB != 0 => SUBBLOCK_ENTRIES,
            _ => 1,
        }
    }

    /// The leaf in slot `k` of sector `(cx, cz)` (`k < cell_slots`), `None`
    /// for an empty slot or one that repeats an earlier slot's leaf. A
    /// dense room has the one record of its sector.
    pub(crate) fn cell_leaf<'v>(
        &self,
        view: &CollisionView<'v>,
        cx: usize,
        cz: usize,
        k: usize,
    ) -> Option<LeafAt<'v>> {
        if !view.is_tree() {
            let rec = view.sectors.get(cz * view.width() + cx)?;
            let s = self.s;
            return Some(LeafAt {
                rec,
                ox: cx as i32 * s,
                oz: cz as i32 * s,
                size: s,
                quarter: 0,
                id: (cz * view.width() + cx) as u16,
            });
        }
        let entry = *view.index.get(cz * view.width() + cx)?;
        if entry & INDEX_SUB == 0 {
            let id = entry & INDEX_ID;
            return (k == 0 && id != NO_LEAF).then(|| self.leaf_of_sector(view, cx, cz, id));
        }
        let (qx, qz) = ((k & 3) as i32, (k >> 2) as i32);
        let q = self.s / 4;
        let leaf = view
            .blocks
            .get(usize::from(entry & INDEX_ID) * SUBBLOCK_ENTRIES + k)
            .copied()?;
        if leaf == NO_LEAF {
            return None;
        }
        let rec = view.sectors.get(usize::from(leaf))?;
        let (sx, sz) = (cx as i32 * self.s, cz as i32 * self.s);
        if rec.flags >> SECTOR_LEAF_SHIFT == leaf_code::HALF {
            // Only the first of its four slots.
            if qx & 1 != 0 || qz & 1 != 0 {
                return None;
            }
            return Some(LeafAt {
                rec,
                ox: sx + qx * q,
                oz: sz + qz * q,
                size: 2 * q,
                quarter: (1 + k) as u8,
                id: leaf,
            });
        }
        Some(LeafAt {
            rec,
            ox: sx + qx * q,
            oz: sz + qz * q,
            size: q,
            quarter: (1 + k) as u8,
            id: leaf,
        })
    }

    /// The leaf of room `view` holding the room-local point `(x, z)`, with
    /// the point's offsets in the leaf frame, for tree rooms. `None` outside
    /// the room.
    #[inline(always)]
    pub(crate) fn leaf_at_point<'v>(
        &self,
        view: &CollisionView<'v>,
        x: i32,
        z: i32,
    ) -> Option<LeafAt<'v>> {
        let (cx, cz, lx, lz) = self.cell_of(x, z, view.width(), view.depth())?;
        Some(self.leaf_in(view, cx, cz, lx as i32, lz as i32))
    }
}
