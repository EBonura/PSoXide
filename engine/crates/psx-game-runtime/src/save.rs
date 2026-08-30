//! The souls save block (phase 4 of `docs/game-runtime-plan.md`).
//!
//! One compact memory-card payload holds the entire save: checkpoint
//! state, persistent world flags, and an optional exit pose reserved
//! for a future Continue/savestate flow. Games may record that pose
//! without restoring it; a New Game path still begins at its authored
//! player spawn. The cook interns persist-marked logic records into
//! flag indices; this module only stores and validates the payload.
//!
//! Layout (little-endian, 256 bytes; still comfortably within one
//! 8 KiB PlayStation memory-card file block):
//!
//! | offset | size | field                                    |
//! |--------|------|------------------------------------------|
//! | 0      | 4    | magic `b"PXSV"`                          |
//! | 4      | 2    | format version                           |
//! | 6      | 2    | flag table size the cook produced        |
//! | 8      | 2    | checkpoint id                            |
//! | 10     | 2    | player hp                                |
//! | 12     | 2    | player hp max                            |
//! | 14     | 2    | saved room (`u16::MAX` means absent)      |
//! | 16     | 2    | saved facing yaw (Q12)                    |
//! | 18     | 2    | reserved (zero)                          |
//! | 20     | 4    | saved x                                  |
//! | 24     | 4    | saved y                                  |
//! | 28     | 4    | saved z                                  |
//! | 32     | 104  | persistent flag bitset (832 flags)       |
//! | 136    | 112  | reserved for future savestate fields     |
//! | 248    | 8    | FNV-1a-64 over bytes 0..248              |
//!
//! Every reject path returns `None` ("new game"): wrong magic,
//! unknown version, a flag-table size that does not match the
//! running cook's table (stale save against re-authored content),
//! or a checksum mismatch. Loud, never partial.

use psx_hw::hash::fnv1a_64;

/// Identifies a PSoXide save frame.
pub const SAVE_MAGIC: [u8; 4] = *b"PXSV";
/// Bumped when the layout changes; older saves reject to new-game.
pub const SAVE_VERSION: u16 = 2;
/// Bytes of persistent-flag bitset in the block.
pub const SAVE_FLAG_BYTES: usize = 104;
/// Maximum persist-marked logic records a project may cook.
pub const SAVE_FLAG_CAPACITY: usize = SAVE_FLAG_BYTES * 8;
/// Size of the encoded payload. The memory-card filesystem still allocates
/// only one 8 KiB file block for this payload and its container header.
pub const SAVE_BLOCK_BYTES: usize = 256;

const CHECKSUM_OFFSET: usize = SAVE_BLOCK_BYTES - 8;
const FLAGS_OFFSET: usize = 32;
const SAVED_ROOM_NONE: u16 = u16::MAX;
const LEGACY_V1_BYTES: usize = 128;
const LEGACY_V1_CHECKSUM_OFFSET: usize = LEGACY_V1_BYTES - 8;
const LEGACY_V1_FLAGS_OFFSET: usize = 16;

/// Player pose captured at an intentional gameplay exit boundary.
///
/// This is persistence scaffolding, not an instruction to resume: New Game
/// flows remain free to ignore it and use the authored player spawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SavedPlayerPosition {
    /// Cooked room index containing the player.
    pub room: u16,
    /// World-space x coordinate in engine units.
    pub x: i32,
    /// World-space y coordinate in engine units.
    pub y: i32,
    /// World-space z coordinate in engine units.
    pub z: i32,
    /// Facing rotation encoded as one-turn Q12.
    pub yaw: u16,
}

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
    /// Last pose captured while leaving gameplay. A future Continue flow may
    /// restore this; New Game deliberately does not.
    pub resume_position: Option<SavedPlayerPosition>,
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
            resume_position: None,
            flags: [0; SAVE_FLAG_BYTES],
        }
    }

    /// Replace the stored exit pose, returning whether the save changed.
    pub fn set_resume_position(&mut self, position: SavedPlayerPosition) -> bool {
        let changed = self.resume_position != Some(position);
        self.resume_position = Some(position);
        changed
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
        if let Some(position) = self.resume_position {
            out[14..16].copy_from_slice(&position.room.to_le_bytes());
            out[16..18].copy_from_slice(&position.yaw.to_le_bytes());
            out[20..24].copy_from_slice(&position.x.to_le_bytes());
            out[24..28].copy_from_slice(&position.y.to_le_bytes());
            out[28..32].copy_from_slice(&position.z.to_le_bytes());
        } else {
            out[14..16].copy_from_slice(&SAVED_ROOM_NONE.to_le_bytes());
        }
        // 18..20 and the future-state reserve remain zero.
        out[FLAGS_OFFSET..FLAGS_OFFSET + SAVE_FLAG_BYTES].copy_from_slice(&self.flags);
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
        // psx-numeric-allow-next-line: FNV-1a-64 checksum decode; xor/mul via compiler builtins at save/load only
        let checksum = u64::from_le_bytes([
            buf[CHECKSUM_OFFSET],
            buf[CHECKSUM_OFFSET + 1],
            buf[CHECKSUM_OFFSET + 2],
            buf[CHECKSUM_OFFSET + 3],
            buf[CHECKSUM_OFFSET + 4],
            buf[CHECKSUM_OFFSET + 5],
            buf[CHECKSUM_OFFSET + 6],
            buf[CHECKSUM_OFFSET + 7],
        ]);
        if checksum != fnv1a_64(&buf[..CHECKSUM_OFFSET]) {
            return None;
        }
        let flag_count = u16::from_le_bytes([buf[6], buf[7]]);
        if flag_count != expected_flag_count {
            return None;
        }
        let saved_room = u16::from_le_bytes([buf[14], buf[15]]);
        let resume_position = if saved_room == SAVED_ROOM_NONE {
            None
        } else {
            Some(SavedPlayerPosition {
                room: saved_room,
                yaw: u16::from_le_bytes([buf[16], buf[17]]),
                x: i32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
                y: i32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]),
                z: i32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
            })
        };
        let mut flags = [0u8; SAVE_FLAG_BYTES];
        flags.copy_from_slice(&buf[FLAGS_OFFSET..FLAGS_OFFSET + SAVE_FLAG_BYTES]);
        Some(Self {
            checkpoint_id: u16::from_le_bytes([buf[8], buf[9]]),
            hp: u16::from_le_bytes([buf[10], buf[11]]),
            hp_max: u16::from_le_bytes([buf[12], buf[13]]),
            flag_count,
            resume_position,
            flags,
        })
    }

    /// Decode the original 128-byte v1 payload so persistent world progress
    /// survives the position-schema upgrade. The migrated block has no exit
    /// pose; the next gameplay handoff captures one and writes v2.
    pub fn decode_legacy_v1(buf: &[u8], expected_flag_count: u16) -> Option<Self> {
        if buf.len() != LEGACY_V1_BYTES || buf[0..4] != SAVE_MAGIC {
            return None;
        }
        if u16::from_le_bytes([buf[4], buf[5]]) != 1 {
            return None;
        }
        let checksum = u64::from_le_bytes([
            buf[LEGACY_V1_CHECKSUM_OFFSET],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 1],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 2],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 3],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 4],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 5],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 6],
            buf[LEGACY_V1_CHECKSUM_OFFSET + 7],
        ]);
        if checksum != fnv1a_64(&buf[..LEGACY_V1_CHECKSUM_OFFSET]) {
            return None;
        }
        let flag_count = u16::from_le_bytes([buf[6], buf[7]]);
        if flag_count != expected_flag_count {
            return None;
        }
        let mut flags = [0u8; SAVE_FLAG_BYTES];
        flags.copy_from_slice(
            &buf[LEGACY_V1_FLAGS_OFFSET..LEGACY_V1_FLAGS_OFFSET + SAVE_FLAG_BYTES],
        );
        Some(Self {
            checkpoint_id: u16::from_le_bytes([buf[8], buf[9]]),
            hp: u16::from_le_bytes([buf[10], buf[11]]),
            hp_max: u16::from_le_bytes([buf[12], buf[13]]),
            flag_count,
            resume_position: None,
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
        block.resume_position = Some(SavedPlayerPosition {
            room: 2,
            x: -1_024,
            y: 512,
            z: 8_192,
            yaw: 3_072,
        });
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
        assert_eq!(block.resume_position, None);
        assert!((0..SAVE_FLAG_CAPACITY).all(|i| !block.flag(i)));
    }

    #[test]
    fn setting_resume_position_reports_only_real_changes() {
        let mut block = SaveBlock::new(100, 0);
        let position = SavedPlayerPosition {
            room: 4,
            x: 10,
            y: 20,
            z: 30,
            yaw: 40,
        };
        assert!(block.set_resume_position(position));
        assert!(!block.set_resume_position(position));
    }

    #[test]
    fn legacy_v1_migrates_flags_without_inventing_a_resume_pose() {
        let mut frame = [0u8; LEGACY_V1_BYTES];
        frame[0..4].copy_from_slice(&SAVE_MAGIC);
        frame[4..6].copy_from_slice(&1u16.to_le_bytes());
        frame[6..8].copy_from_slice(&7u16.to_le_bytes());
        frame[8..10].copy_from_slice(&3u16.to_le_bytes());
        frame[10..12].copy_from_slice(&36u16.to_le_bytes());
        frame[12..14].copy_from_slice(&100u16.to_le_bytes());
        frame[LEGACY_V1_FLAGS_OFFSET] = 0b0100_0001;
        let checksum = fnv1a_64(&frame[..LEGACY_V1_CHECKSUM_OFFSET]);
        frame[LEGACY_V1_CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());

        let migrated = SaveBlock::decode_legacy_v1(&frame, 7).expect("valid v1 save");
        assert_eq!(migrated.checkpoint_id, 3);
        assert_eq!(migrated.hp, 36);
        assert_eq!(migrated.resume_position, None);
        assert!(migrated.flag(0));
        assert!(migrated.flag(6));
    }
}
