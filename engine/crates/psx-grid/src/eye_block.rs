//! The disc layout of one bake eye's far panels (block format 3).
//!
//! A block is one run of sectors holding every panel of an eye: an *index*, then the stored
//! tiles of all its panels as one stream of LZ blocks ([`crate::tile_lz`]), the tiles the
//! camera sees with the horizon level first, the others after them. The index sits at the
//! start of the first sector and tells everything the guest needs before it reads a tile:
//!
//! | bytes | content |
//! |---|---|
//! | `HEADINGS * COLS` | per heading, per tile column, the mask of stored rows |
//! | `PANEL_BYTES` each | per present heading in heading order: the depth mesh, then the room of each cell |
//! | 2 each | per stored tile in [`stream_order`]: its length in bytes |
//!
//! The stream starts at the next multiple of four. Tiles follow one another, so one is found by
//! adding the lengths before it, with one rule: the block is cut into *groups* of
//! [`GROUP_SECTORS`] sectors, the unit the guest reads, and a tile never crosses from one group
//! into the next (the stream skips to the next group instead). A block is padded to whole
//! sectors.

use alloc::vec::Vec;

use crate::render::{CELLS, CENTRAL_ROWS, COLS, HEADINGS, MESH, ROWS, TILES};

/// Bytes of a sector.
pub const SECTOR: usize = 2048;
/// Sectors of a group, the unit of a read.
pub const GROUP_SECTORS: usize = 6;
/// Bytes of a group.
pub const GROUP_BYTES: usize = GROUP_SECTORS * SECTOR;
/// Bytes of the tile masks at the start of the index.
pub const MASK_BYTES: usize = HEADINGS * COLS;
/// Bytes of a present panel's depth mesh and cell rooms in the index.
pub const PANEL_BYTES: usize = MESH + CELLS;
/// Most tiles a block stores.
pub const MAX_TILES: usize = HEADINGS * TILES;
/// Marks a heading with no panel.
pub const NO_PANEL: u8 = 0xff;

/// Whether a heading stores any tile.
pub fn is_present(cols: &[u8; COLS]) -> bool {
    cols.iter().any(|&c| c != 0)
}

/// The stored tiles in the order the stream holds them, as `(heading, tile)` with
/// `tile = col * ROWS + row`: the tiles of the central rows of every present panel in heading
/// order and column by column, then those of the other rows. Returns how many.
pub fn stream_order(cols: &[[u8; COLS]; HEADINGS], out: &mut [(u8, u8); MAX_TILES]) -> usize {
    let mut count = 0;
    for outer in [false, true] {
        for (heading, panel) in cols.iter().enumerate() {
            for (col, &mask) in panel.iter().enumerate() {
                for row in 0..ROWS {
                    if mask >> row & 1 == 0 {
                        continue;
                    }
                    let central = CENTRAL_ROWS >> row & 1 != 0;
                    if central != outer {
                        out[count] = (heading as u8, (col * ROWS + row) as u8);
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

/// Bytes the index takes, for `panels` present headings and `tiles` stored tiles.
pub const fn index_bytes(panels: usize, tiles: usize) -> usize {
    MASK_BYTES + panels * PANEL_BYTES + 2 * tiles
}

/// Offset of the tile stream: the end of the index, to a multiple of four.
pub const fn data_start(panels: usize, tiles: usize) -> usize {
    index_bytes(panels, tiles).next_multiple_of(4)
}

/// Where a tile of `len` bytes goes when the stream is at `at`: `at` itself, or the start of
/// the next group when it would cross into it.
pub const fn place(at: usize, len: usize) -> usize {
    if at % GROUP_BYTES + len > GROUP_BYTES {
        (at / GROUP_BYTES + 1) * GROUP_BYTES
    } else {
        at
    }
}

/// What the index of a block says.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BlockIndex {
    /// Per heading and tile column, the mask of stored rows.
    pub cols: [[u8; COLS]; HEADINGS],
    /// Per heading, its place among the present panels, or [`NO_PANEL`].
    pub panel: [u8; HEADINGS],
    /// Stored tiles in all.
    pub tiles: u8,
    /// Byte offset of each stored tile from the start of the block; 0 for one not stored.
    pub offset: [[u32; TILES]; HEADINGS],
    /// Length of each stored tile in bytes; 0 for one not stored.
    pub len: [[u16; TILES]; HEADINGS],
    /// Byte after the last tile: the block takes this many bytes to whole sectors.
    pub end: u32,
}

impl BlockIndex {
    /// An index that stores nothing.
    pub const EMPTY: Self = Self {
        cols: [[0; COLS]; HEADINGS],
        panel: [NO_PANEL; HEADINGS],
        tiles: 0,
        offset: [[0; TILES]; HEADINGS],
        len: [[0; TILES]; HEADINGS],
        end: 0,
    };

    /// Sectors the block takes.
    pub fn sectors(&self) -> u32 {
        self.end.div_ceil(SECTOR as u32)
    }

    /// Read the index through `bytes`, which gives the block's byte at an offset below
    /// [`SECTOR`].
    pub fn parse(bytes: impl Fn(usize) -> u8) -> Self {
        let mut cols = [[0u8; COLS]; HEADINGS];
        for (heading, panel) in cols.iter_mut().enumerate() {
            for (col, mask) in panel.iter_mut().enumerate() {
                *mask = bytes(heading * COLS + col);
            }
        }
        let mut panel = [NO_PANEL; HEADINGS];
        let mut panels = 0usize;
        for heading in 0..HEADINGS {
            if is_present(&cols[heading]) {
                panel[heading] = panels as u8;
                panels += 1;
            }
        }
        let mut order = [(0u8, 0u8); MAX_TILES];
        let tiles = stream_order(&cols, &mut order);
        let lengths = MASK_BYTES + panels * PANEL_BYTES;
        let mut at = data_start(panels, tiles) as u32;
        let mut offset = [[0u32; TILES]; HEADINGS];
        let mut len = [[0u16; TILES]; HEADINGS];
        for (k, &(heading, tile)) in order[..tiles].iter().enumerate() {
            let length =
                u16::from(bytes(lengths + 2 * k)) | u16::from(bytes(lengths + 2 * k + 1)) << 8;
            at = place(at as usize, usize::from(length)) as u32;
            offset[usize::from(heading)][usize::from(tile)] = at;
            len[usize::from(heading)][usize::from(tile)] = length;
            at += u32::from(length);
        }
        Self {
            cols,
            panel,
            tiles: tiles as u8,
            offset,
            len,
            end: at,
        }
    }

    /// Whether `tile` of `heading` is stored.
    pub fn stored(&self, heading: usize, tile: usize) -> bool {
        self.len[heading][tile] != 0
    }

    /// Offset in the block of the depth mesh of `heading`'s panel (its room cells follow), if
    /// the heading has one.
    pub fn panel_at(&self, heading: usize) -> Option<usize> {
        match self.panel[heading] {
            NO_PANEL => None,
            ordinal => Some(MASK_BYTES + usize::from(ordinal) * PANEL_BYTES),
        }
    }
}

/// One panel as the builder takes it.
pub struct PanelPart<'a> {
    /// Stored rows per tile column.
    pub cols: [u8; COLS],
    /// The depth mesh.
    pub depth: &'a [u8; MESH],
    /// The room of each cell.
    pub rooms: &'a [u8; CELLS],
    /// The LZ block of each tile, `None` where it is not stored.
    pub tiles: [Option<&'a [u8]>; TILES],
}

/// Lay out the block of an eye: `panels` per heading, `None` for a heading with no panel.
/// Returns the bytes, padded to whole sectors.
///
/// # Panics
/// If a panel's `cols` and its stored tiles disagree, or a tile block is empty or longer than
/// a sector.
pub fn build_block(panels: &[Option<PanelPart<'_>>; HEADINGS]) -> Vec<u8> {
    let mut cols = [[0u8; COLS]; HEADINGS];
    for (heading, part) in panels.iter().enumerate() {
        if let Some(part) = part {
            cols[heading] = part.cols;
        }
    }
    let mut order = [(0u8, 0u8); MAX_TILES];
    let tiles = stream_order(&cols, &mut order);
    let present = panels.iter().flatten().count();
    let start = data_start(present, tiles);
    let mut out = alloc::vec![0u8; start];
    for heading in 0..HEADINGS {
        for col in 0..COLS {
            out[heading * COLS + col] = cols[heading][col];
        }
    }
    let mut at = MASK_BYTES;
    for part in panels.iter().flatten() {
        out[at..at + MESH].copy_from_slice(part.depth);
        out[at + MESH..at + PANEL_BYTES].copy_from_slice(part.rooms);
        at += PANEL_BYTES;
    }
    let lengths = at;
    for (k, &(heading, tile)) in order[..tiles].iter().enumerate() {
        let part = panels[usize::from(heading)]
            .as_ref()
            .expect("a stored tile has a panel");
        let block = part.tiles[usize::from(tile)].expect("a tile the masks store");
        assert!(
            !block.is_empty() && block.len() <= SECTOR,
            "tile block of {} bytes",
            block.len()
        );
        out[lengths + 2 * k] = block.len() as u8;
        out[lengths + 2 * k + 1] = (block.len() >> 8) as u8;
        out.resize(place(out.len(), block.len()), 0);
        out.extend_from_slice(block);
    }
    out.resize(out.len().next_multiple_of(SECTOR), 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

    #[test]
    fn a_block_reads_back_what_was_laid_out() {
        let depth = [7u8; MESH];
        let rooms = [3u8; CELLS];
        let (a, b, c, d) = ([1u8; 40], [2u8; 700], [3u8; 2048], [4u8; 5]);
        let mut tiles_a = [None; TILES];
        let mut tiles_b = [None; TILES];
        tiles_a[0] = Some(&a[..]);
        tiles_a[1] = Some(&b[..]);
        tiles_b[ROWS + 2] = Some(&c[..]);
        tiles_b[ROWS] = Some(&d[..]);
        let mut cols_a = [0u8; COLS];
        cols_a[0] = 0b011;
        let mut cols_b = [0u8; COLS];
        cols_b[1] = 0b101;
        let mut panels: [Option<PanelPart<'_>>; HEADINGS] = Default::default();
        panels[0] = Some(PanelPart {
            cols: cols_a,
            depth: &depth,
            rooms: &rooms,
            tiles: tiles_a,
        });
        panels[5] = Some(PanelPart {
            cols: cols_b,
            depth: &depth,
            rooms: &rooms,
            tiles: tiles_b,
        });
        let block = build_block(&panels);
        assert_eq!(block.len() % SECTOR, 0);
        let index = BlockIndex::parse(|k| block[k]);
        assert_eq!(index.tiles, 4);
        assert_eq!(index.panel[0], 0);
        assert_eq!(index.panel[5], 1);
        assert_eq!(index.panel[3], NO_PANEL);
        for (heading, tile, expect) in [
            (0, 0, &a[..]),
            (0, 1, &b[..]),
            (5, ROWS + 2, &c[..]),
            (5, ROWS, &d[..]),
        ] {
            assert!(index.stored(heading, tile));
            let at = index.offset[heading][tile] as usize;
            assert_eq!(
                &block[at..at + usize::from(index.len[heading][tile])],
                expect
            );
        }
        let at = index.panel_at(5).expect("a panel");
        assert_eq!(block[at], 7);
        assert_eq!(block[at + MESH], 3);
        // The stream runs panel by panel, column by column.
        assert!(index.offset[0][0] < index.offset[0][1]);
        assert!(index.offset[0][1] < index.offset[5][ROWS]);
    }

    #[test]
    fn no_tile_crosses_into_the_next_group() {
        let depth = [0u8; MESH];
        let rooms = [0u8; CELLS];
        let blocks: std::vec::Vec<std::vec::Vec<u8>> = (0..TILES)
            .map(|k| std::vec![k as u8; 1500 + 37 * k])
            .collect();
        let mut panels: [Option<PanelPart<'_>>; HEADINGS] = Default::default();
        for heading in [0, 2, 4, 6, 7] {
            panels[heading] = Some(PanelPart {
                cols: [(1 << ROWS) - 1; COLS],
                depth: &depth,
                rooms: &rooms,
                tiles: core::array::from_fn(|k| Some(blocks[k].as_slice())),
            });
        }
        let block = build_block(&panels);
        let index = BlockIndex::parse(|k| block[k]);
        let mut groups = 0;
        for heading in [0, 2, 4, 6, 7] {
            for (tile, expected) in blocks.iter().enumerate() {
                let at = index.offset[heading][tile] as usize;
                let len = usize::from(index.len[heading][tile]);
                assert_eq!(
                    at / GROUP_BYTES,
                    (at + len - 1) / GROUP_BYTES,
                    "{heading} {tile}"
                );
                assert_eq!(&block[at..at + len], &expected[..]);
                groups = groups.max(at / GROUP_BYTES + 1);
            }
        }
        assert!(groups >= 5, "the block spans several groups: {groups}");
        assert_eq!(index.sectors() as usize * SECTOR, block.len());
    }
}
