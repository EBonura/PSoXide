//! The souls save block (phase 4 of `docs/game-runtime-plan.md`).
//!
//! One memory-card frame (128 bytes) holds the entire save: resting
//! at a checkpoint is the only save moment, enemies respawn on rest
//! and death by genre design, so the persisted state collapses to
//! the checkpoint id, player HP, and a persistent world-flag bitset
//! (bosses dead, one-way doors, fired-once triggers). The cook
//! interns persist-marked logic records into flag indices; this
//! module only stores and validates the block.
//!
//! Layout (little-endian, exactly [`psx_mc::FRAME_SIZE`] bytes):
//!
//! | offset | size | field                                    |
//! |--------|------|------------------------------------------|
//! | 0      | 4    | magic `b"PXSV"`                          |
//! | 4      | 2    | format version                           |
//! | 6      | 2    | flag table size the cook produced        |
//! | 8      | 2    | checkpoint id                            |
//! | 10     | 2    | player hp                                |
//! | 12     | 2    | player hp max                            |
//! | 14     | 2    | reserved (zero)                          |
//! | 16     | 104  | persistent flag bitset (832 flags)       |
//! | 120    | 8    | FNV-1a-64 over bytes 0..120              |
//!
//! Every reject path returns `None` ("new game"): wrong magic,
//! unknown version, a flag-table size that does not match the
//! running cook's table (stale save against re-authored content),
//! or a checksum mismatch. Loud, never partial.

use psx_hw::hash::fnv1a_64;

/// Identifies a PSoXide save frame.
pub const SAVE_MAGIC: [u8; 4] = *b"PXSV";
/// Bumped when the layout changes; older saves reject to new-game.
pub const SAVE_VERSION: u16 = 1;
/// Bytes of persistent-flag bitset in the block.
pub const SAVE_FLAG_BYTES: usize = 104;
/// Maximum persist-marked logic records a project may cook.
pub const SAVE_FLAG_CAPACITY: usize = SAVE_FLAG_BYTES * 8;
/// Size of the encoded block: one memory-card frame.
pub const SAVE_BLOCK_BYTES: usize = 128;

const CHECKSUM_OFFSET: usize = SAVE_BLOCK_BYTES - 8;

/// Decoded save state. Plain data; the game owns one instance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SaveBlock {
    /// Logic-record index of the checkpoint the player rested at.
    pub checkpoint_id: u16,
    /// Player hit points at rest.
    pub hp: u16,
    /// Player maximum hit points at rest.
    pub hp_max: u16,
    /// Number of flag-table entries the cook produced when this
    /// block was written; must match the running content exactly.
    pub flag_count: u16,
    /// Persistent world flags, one bit per cook-interned index.
    pub flags: [u8; SAVE_FLAG_BYTES],
}

impl SaveBlock {
    /// Fresh block for a new game: checkpoint 0, no flags set.
    pub const fn new(hp_max: u16, flag_count: u16) -> Self {
        Self {
            checkpoint_id: 0,
            hp: hp_max,
            hp_max,
            flag_count,
            flags: [0; SAVE_FLAG_BYTES],
        }
    }

    /// Read persistent flag `index`; out-of-range reads as unset.
    pub fn flag(&self, index: usize) -> bool {
        if index >= SAVE_FLAG_CAPACITY {
            return false;
        }
        self.flags[index / 8] & (1 << (index % 8)) != 0
    }

    /// Set persistent flag `index`; out-of-range is ignored (the
    /// cook rejects projects that exceed the capacity, so this is
    /// defense in depth, not a code path).
    pub fn set_flag(&mut self, index: usize) {
        if index >= SAVE_FLAG_CAPACITY {
            return;
        }
        self.flags[index / 8] |= 1 << (index % 8);
    }

    /// Encode into one card frame, checksummed.
    pub fn encode(&self, out: &mut [u8; SAVE_BLOCK_BYTES]) {
        out.fill(0);
        out[0..4].copy_from_slice(&SAVE_MAGIC);
        out[4..6].copy_from_slice(&SAVE_VERSION.to_le_bytes());
        out[6..8].copy_from_slice(&self.flag_count.to_le_bytes());
        out[8..10].copy_from_slice(&self.checkpoint_id.to_le_bytes());
        out[10..12].copy_from_slice(&self.hp.to_le_bytes());
        out[12..14].copy_from_slice(&self.hp_max.to_le_bytes());
        // 14..16 reserved, already zero.
        out[16..16 + SAVE_FLAG_BYTES].copy_from_slice(&self.flags);
        let checksum = fnv1a_64(&out[..CHECKSUM_OFFSET]);
        out[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
    }

    /// Decode a card frame. `expected_flag_count` is the running
    /// cook's flag-table size; any mismatch rejects. Returns `None`
    /// for every invalid shape -- the caller starts a new game.
    pub fn decode(buf: &[u8; SAVE_BLOCK_BYTES], expected_flag_count: u16) -> Option<Self> {
        if buf[0..4] != SAVE_MAGIC {
            return None;
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        if version != SAVE_VERSION {
            return None;
        }
        let checksum = u64::from_le_bytes([
            buf[120], buf[121], buf[122], buf[123], buf[124], buf[125], buf[126], buf[127],
        ]);
        if checksum != fnv1a_64(&buf[..CHECKSUM_OFFSET]) {
            return None;
        }
        let flag_count = u16::from_le_bytes([buf[6], buf[7]]);
        if flag_count != expected_flag_count {
            return None;
        }
        let mut flags = [0u8; SAVE_FLAG_BYTES];
        flags.copy_from_slice(&buf[16..16 + SAVE_FLAG_BYTES]);
        Some(Self {
            checkpoint_id: u16::from_le_bytes([buf[8], buf[9]]),
            hp: u16::from_le_bytes([buf[10], buf[11]]),
            hp_max: u16::from_le_bytes([buf[12], buf[13]]),
            flag_count,
            flags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SaveBlock {
        let mut block = SaveBlock::new(100, 7);
        block.checkpoint_id = 3;
        block.hp = 36;
        block.set_flag(0);
        block.set_flag(6);
        block
    }

    #[test]
    fn roundtrip_preserves_every_field() {
        let block = sample();
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        block.encode(&mut frame);
        assert_eq!(SaveBlock::decode(&frame, 7), Some(block));
    }

    #[test]
    fn wrong_magic_rejects() {
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        sample().encode(&mut frame);
        frame[0] = b'X';
        assert_eq!(SaveBlock::decode(&frame, 7), None);
    }

    #[test]
    fn unknown_version_rejects() {
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        sample().encode(&mut frame);
        frame[4] = SAVE_VERSION as u8 + 1;
        // Re-checksum so ONLY the version differs.
        let checksum = fnv1a_64(&frame[..CHECKSUM_OFFSET]);
        frame[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(SaveBlock::decode(&frame, 7), None);
    }

    #[test]
    fn flag_table_mismatch_rejects() {
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        sample().encode(&mut frame);
        assert_eq!(SaveBlock::decode(&frame, 8), None);
    }

    #[test]
    fn corrupt_payload_rejects() {
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        sample().encode(&mut frame);
        frame[10] ^= 0xFF; // hp byte flipped, checksum now stale
        assert_eq!(SaveBlock::decode(&frame, 7), None);
    }

    #[test]
    fn bitset_edges_hold() {
        let mut block = SaveBlock::new(1, SAVE_FLAG_CAPACITY as u16);
        assert!(!block.flag(0));
        block.set_flag(0);
        block.set_flag(SAVE_FLAG_CAPACITY - 1);
        block.set_flag(SAVE_FLAG_CAPACITY); // out of range: ignored
        assert!(block.flag(0));
        assert!(block.flag(SAVE_FLAG_CAPACITY - 1));
        assert!(!block.flag(SAVE_FLAG_CAPACITY));
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        block.encode(&mut frame);
        let back = SaveBlock::decode(&frame, SAVE_FLAG_CAPACITY as u16).unwrap();
        assert!(back.flag(SAVE_FLAG_CAPACITY - 1));
    }

    #[test]
    fn new_game_block_is_full_health_no_flags() {
        let block = SaveBlock::new(100, 3);
        assert_eq!(block.hp, 100);
        assert_eq!(block.checkpoint_id, 0);
        assert!((0..SAVE_FLAG_CAPACITY).all(|i| !block.flag(i)));
    }
}
