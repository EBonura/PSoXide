//! A chunk's baked cube sky stored sparsely, against a background.
//!
//! The cube sky atlas is six 256x256 faces side by side, four bits a texel,
//! one 16-colour palette per face. Seen from a chunk, nearly all of it is
//! plain sky or void: a ray that hits nothing paints the same colour whatever
//! the chunk, so after quantising to the face's palette its index depends only
//! on where the texel sits in the face. That index map is the *background*
//! ([`Background`]), kept once for the level as runs of equal indices along
//! each row of each face.
//!
//! A chunk's sky is then the background plus the 8x8 tiles that differ from
//! it. Entry layout (little-endian), one entry per chunk:
//!
//! ```text
//! u16 version (1)        u16 tile_count
//! u8  mask[768]          bit i set: tile i is stored; tile i = face * 1024 + ty * 32 + tx,
//!                        bit 0 of byte 0 first
//! tile[tile_count] x 32 B  in tile order; 8 words, word r = texel row ty * 8 + r,
//!                        texel x of the 8 in nibble x (left texel lowest)
//! ```
//!
//! Nothing is lost against the dense atlas: a tile is left out only when
//! every one of its texels equals the background index there.

/// Faces of the atlas: +X, -X, +Y, -Y, +Z, -Z.
pub const FACES: usize = 6;
/// Texels along a face's edge.
pub const FACE_TEXELS: usize = 256;
/// Texels along a tile's edge.
pub const TILE_TEXELS: usize = 8;
/// Tiles along a face's edge.
pub const TILES_PER_SIDE: usize = FACE_TEXELS / TILE_TEXELS;
/// Tiles on one face.
pub const TILES_PER_FACE: usize = TILES_PER_SIDE * TILES_PER_SIDE;
/// Tiles in the atlas.
pub const TILE_COUNT: usize = FACES * TILES_PER_FACE;
/// Bytes in the tile mask.
pub const MASK_BYTES: usize = TILE_COUNT / 8;
/// Words in the tile mask.
pub const MASK_WORDS: usize = MASK_BYTES / 4;
/// Bytes of version and tile count ahead of the mask.
pub const HEADER_BYTES: usize = 4;
/// Where the first stored tile starts in an entry.
pub const TILES_OFFSET: usize = HEADER_BYTES + MASK_BYTES;
/// Bytes of one stored tile.
pub const TILE_BYTES: usize = 32;
/// Words of one stored tile (one per texel row).
pub const TILE_WORDS: usize = TILE_BYTES / 4;
/// The entry format this module reads and writes.
pub const VERSION: u16 = 1;
/// Bytes on a disc sector.
pub const SECTOR_BYTES: usize = 2048;

/// Face, tile row and tile column of tile `index`.
pub const fn tile_position(index: usize) -> (usize, usize, usize) {
    (
        index / TILES_PER_FACE,
        index % TILES_PER_FACE / TILES_PER_SIDE,
        index % TILES_PER_SIDE,
    )
}

/// Bytes of an entry that stores `tiles` tiles.
pub const fn entry_bytes(tiles: usize) -> usize {
    TILES_OFFSET + tiles * TILE_BYTES
}

/// Sectors an entry that stores `tiles` tiles occupies.
pub const fn entry_sectors(tiles: usize) -> usize {
    entry_bytes(tiles).div_ceil(SECTOR_BYTES)
}

/// The tile count of an entry, if `head` (at least [`HEADER_BYTES`] bytes)
/// starts a readable one.
pub fn header_tile_count(head: &[u8]) -> Option<usize> {
    let word = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes([*head.get(at)?, *head.get(at + 1)?]))
    };
    let (version, count) = (word(0)?, usize::from(word(2)?));
    (version == VERSION && count <= TILE_COUNT).then_some(count)
}

/// The texel indices, along rows of every face, of a sky with nothing in it:
/// what every texel that no ray hit reads. Borrowed from the level manifest.
///
/// `rows[face * 256 + row]` is the first run of that row, `rows[face * 256 +
/// row + 1]` where the next row's runs start (so `rows` is 1,537 long). A run
/// is two bytes: the last texel column it covers and its palette index; the
/// last run of a row ends at column 255.
#[derive(Copy, Clone, Debug)]
pub struct Background<'a> {
    rows: &'a [u16],
    runs: &'a [u8],
}

impl<'a> Background<'a> {
    /// A background over the given tables (`rows` empty: none).
    pub const fn new(rows: &'a [u16], runs: &'a [u8]) -> Self {
        Self { rows, runs }
    }

    /// Whether the level has no background (and so no chunk skies).
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The word of texel row `row` of face `face` for the eight texels of
    /// tile column `tile_x`: palette indices in nibbles, left texel lowest.
    pub fn row_word(&self, face: usize, row: usize, tile_x: usize) -> u32 {
        let k = face * FACE_TEXELS + row;
        let (first, next) = (usize::from(self.rows[k]), usize::from(self.rows[k + 1]));
        if next - first == 1 {
            // One run spans the row: every tile of it reads the same index.
            return u32::from(self.runs[first * 2 + 1]) * 0x1111_1111;
        }
        let left = tile_x * TILE_TEXELS;
        let mut at = first * 2;
        while usize::from(self.runs[at]) < left {
            at += 2;
        }
        if usize::from(self.runs[at]) > left + TILE_TEXELS - 2 {
            // One run covers the tile.
            return u32::from(self.runs[at + 1]) * 0x1111_1111;
        }
        let mut word = 0;
        for texel in 0..TILE_TEXELS {
            while usize::from(self.runs[at]) < left + texel {
                at += 2;
            }
            word |= u32::from(self.runs[at + 1] & 0xF) << (4 * texel);
        }
        word
    }

    /// The index at texel `(x, y)` of `face`.
    pub fn index_at(&self, face: usize, x: usize, y: usize) -> u8 {
        (self.row_word(face, y, x / TILE_TEXELS) >> (4 * (x % TILE_TEXELS))) as u8 & 0xF
    }

    /// The eight words of a background tile.
    pub fn tile_words(&self, face: usize, tile_y: usize, tile_x: usize) -> [u32; TILE_WORDS] {
        core::array::from_fn(|r| self.row_word(face, tile_y * TILE_TEXELS + r, tile_x))
    }
}

/// Write the palette indices an entry stands for into `out`, the atlas as one
/// row-major image `FACES * FACE_TEXELS` wide, a texel an index byte. The
/// reference decoder: tests hold the stream to it. `None` when the entry is
/// not readable.
pub fn decode_indices(entry: &[u8], background: &Background<'_>, out: &mut [u8]) -> Option<()> {
    let count = header_tile_count(entry)?;
    let mask = entry.get(HEADER_BYTES..TILES_OFFSET)?;
    let tiles = entry.get(TILES_OFFSET..TILES_OFFSET + count * TILE_BYTES)?;
    let stored = mask.iter().map(|b| b.count_ones() as usize).sum::<usize>();
    if stored != count || out.len() != FACES * FACE_TEXELS * FACE_TEXELS {
        return None;
    }
    let mut next = 0;
    for index in 0..TILE_COUNT {
        let (face, tile_y, tile_x) = tile_position(index);
        let is_stored = mask[index / 8] >> (index % 8) & 1 != 0;
        let words: [u32; TILE_WORDS] = if is_stored {
            let at = next * TILE_BYTES;
            next += 1;
            core::array::from_fn(|r| {
                let b = &tiles[at + r * 4..at + r * 4 + 4];
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            })
        } else {
            background.tile_words(face, tile_y, tile_x)
        };
        for (r, word) in words.iter().enumerate() {
            let y = tile_y * TILE_TEXELS + r;
            let x = face * FACE_TEXELS + tile_x * TILE_TEXELS;
            for t in 0..TILE_TEXELS {
                out[y * FACES * FACE_TEXELS + x + t] = (word >> (4 * t)) as u8 & 0xF;
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// Rows with `runs_of(face, row)` runs, built the way the cook builds them.
    fn tables(runs_of: impl Fn(usize, usize) -> Vec<(u8, u8)>) -> (Vec<u16>, Vec<u8>) {
        let (mut rows, mut runs) = (Vec::new(), Vec::new());
        for face in 0..FACES {
            for row in 0..FACE_TEXELS {
                rows.push((runs.len() / 2) as u16);
                for (end, index) in runs_of(face, row) {
                    runs.extend([end, index]);
                }
            }
        }
        rows.push((runs.len() / 2) as u16);
        (rows, runs)
    }

    /// Row 3 of face 1 changes index at column 100; the rest is one index per face.
    fn sample() -> (Vec<u16>, Vec<u8>) {
        tables(|face, row| {
            if (face, row) == (1, 3) {
                std::vec![(99, 4), (255, 5)]
            } else {
                std::vec![(255, face as u8 + 1)]
            }
        })
    }

    #[test]
    fn tile_positions_follow_face_row_column_order() {
        assert_eq!(tile_position(0), (0, 0, 0));
        assert_eq!(tile_position(31), (0, 0, 31));
        assert_eq!(tile_position(32), (0, 1, 0));
        assert_eq!(tile_position(1024), (1, 0, 0));
        assert_eq!(tile_position(TILE_COUNT - 1), (5, 31, 31));
    }

    #[test]
    fn entry_sizes_round_up_to_sectors() {
        assert_eq!(entry_bytes(0), 772);
        assert_eq!(entry_sectors(0), 1);
        assert_eq!(entry_sectors(39), 1); // 772 + 1248 = 2020
        assert_eq!(entry_sectors(40), 2);
    }

    #[test]
    fn a_header_needs_the_version_and_a_possible_count() {
        let head = |version: u16, count: u16| {
            let mut b = version.to_le_bytes().to_vec();
            b.extend(count.to_le_bytes());
            b
        };
        assert_eq!(header_tile_count(&head(1, 77)), Some(77));
        assert_eq!(header_tile_count(&head(2, 77)), None);
        assert_eq!(header_tile_count(&head(1, 6145)), None);
        assert_eq!(header_tile_count(&[1, 0]), None);
    }

    #[test]
    fn a_uniform_row_reads_one_index_in_every_nibble() {
        let (rows, runs) = sample();
        let bg = Background::new(&rows, &runs);
        assert_eq!(bg.row_word(0, 7, 12), 0x1111_1111);
        assert_eq!(bg.row_word(5, 255, 31), 0x6666_6666);
    }

    #[test]
    fn a_row_that_changes_index_splits_inside_the_tile_it_changes_in() {
        let (rows, runs) = sample();
        let bg = Background::new(&rows, &runs);
        // Columns 96..104: 96..=99 are index 4, 100..=103 index 5.
        assert_eq!(bg.row_word(1, 3, 12), 0x5555_4444);
        assert_eq!(bg.row_word(1, 3, 11), 0x4444_4444);
        assert_eq!(bg.row_word(1, 3, 13), 0x5555_5555);
        assert_eq!(bg.index_at(1, 99, 3), 4);
        assert_eq!(bg.index_at(1, 100, 3), 5);
        assert_eq!(bg.index_at(1, 255, 3), 5);
        assert_eq!(bg.index_at(1, 0, 4), 2);
    }

    #[test]
    fn decoding_lays_stored_tiles_over_the_background() {
        let (rows, runs) = sample();
        let bg = Background::new(&rows, &runs);
        // One stored tile: face 2, tile row 1, tile column 2, row r all index r + 8.
        let tile = 2 * TILES_PER_FACE + TILES_PER_SIDE + 2;
        let mut entry = std::vec![0u8; entry_bytes(1)];
        entry[..2].copy_from_slice(&VERSION.to_le_bytes());
        entry[2..4].copy_from_slice(&1u16.to_le_bytes());
        entry[HEADER_BYTES + tile / 8] |= 1 << (tile % 8);
        for r in 0..TILE_WORDS {
            let word = (r as u32 + 8) * 0x1111_1111;
            entry[TILES_OFFSET + r * 4..][..4].copy_from_slice(&word.to_le_bytes());
        }
        let mut out = std::vec![0u8; FACES * FACE_TEXELS * FACE_TEXELS];
        decode_indices(&entry, &bg, &mut out).unwrap();
        let at =
            |face: usize, x: usize, y: usize| out[y * FACES * FACE_TEXELS + face * FACE_TEXELS + x];
        // Inside the tile: face 2, x 16..24, y 8..16.
        assert_eq!(at(2, 16, 8), 8);
        assert_eq!(at(2, 23, 15), 15);
        // Around it: the background.
        assert_eq!(at(2, 15, 8), 3);
        assert_eq!(at(2, 24, 15), 3);
        assert_eq!(at(1, 99, 3), 4);
        assert_eq!(at(1, 100, 3), 5);
        assert_eq!(at(5, 0, 0), 6);
    }

    #[test]
    fn an_entry_that_disagrees_with_its_count_does_not_decode() {
        let (rows, runs) = sample();
        let bg = Background::new(&rows, &runs);
        let mut entry = std::vec![0u8; entry_bytes(1)];
        entry[..2].copy_from_slice(&VERSION.to_le_bytes());
        entry[2..4].copy_from_slice(&1u16.to_le_bytes()); // one tile promised, mask empty
        let mut out = std::vec![0u8; FACES * FACE_TEXELS * FACE_TEXELS];
        assert!(decode_indices(&entry, &bg, &mut out).is_none());
    }
}
