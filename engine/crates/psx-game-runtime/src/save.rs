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
//! | 8      | 4    | checkpoint stable id (0 means none)      |
//! | 12     | 2    | player hp                                |
//! | 14     | 2    | player hp max                            |
//! | 16     | 2    | saved facing yaw (Q12)                   |
//! | 18     | 2    | 1 when a saved pose follows, else 0      |
//! | 20     | 4    | saved x                                  |
//! | 24     | 4    | saved y                                  |
//! | 28     | 4    | saved z                                  |
//! | 32     | 104  | persistent flag bitset (832 flags)       |
//! | 136    | 112  | reserved for future savestate fields     |
//! | 248    | 8    | FNV-1a-64 over bytes 0..248              |
//!
//! Nothing in a save is an array index. The checkpoint is a stable id
//! ([`psx_level::StableId`], the hash of the checkpoint's authored id) and
//! the saved pose is a world position: on restore the room (or streamed
//! unit) is found from the position, so a recook that reorders or cuts rooms
//! leaves saves valid. Version 2 saved a logic-record index and a room index
//! and still loads (its checkpoint and room cannot be mapped, so both are
//! dropped); version 1 loads the flags only.
//!
//! Every reject path returns `None` ("new game"): wrong magic,
//! unknown version, a flag-table size that does not match the
//! running cook's table (stale save against re-authored content),
//! or a checksum mismatch. Loud, never partial.

use psx_hw::hash::fnv1a_64;

/// Identifies a PSoXide save frame.
pub const SAVE_MAGIC: [u8; 4] = *b"PXSV";
/// Bumped when the layout changes; older saves reject to new-game.
pub const SAVE_VERSION: u16 = 3;
/// Bytes of persistent-flag bitset in the block.
pub const SAVE_FLAG_BYTES: usize = 104;
/// Maximum persist-marked logic records a project may cook.
pub const SAVE_FLAG_CAPACITY: usize = SAVE_FLAG_BYTES * 8;
/// Size of the encoded payload. The memory-card filesystem still allocates
/// only one 8 KiB file block for this payload and its container header.
pub const SAVE_BLOCK_BYTES: usize = 256;

const CHECKSUM_OFFSET: usize = SAVE_BLOCK_BYTES - 8;
const FLAGS_OFFSET: usize = 32;
/// Layout of version 2, which still loads (see the module docs).
const V2_ROOM_NONE: u16 = u16::MAX;
const LEGACY_V1_BYTES: usize = 128;
const LEGACY_V1_CHECKSUM_OFFSET: usize = LEGACY_V1_BYTES - 8;
const LEGACY_V1_FLAGS_OFFSET: usize = 16;

/// Player pose captured at an intentional gameplay exit boundary.
///
/// This is persistence scaffolding, not an instruction to resume: New Game
/// flows remain free to ignore it and use the authored player spawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SavedPlayerPosition {
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
    /// Stable id of the checkpoint the player rested at, or zero.
    pub checkpoint_id: u32,
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
        out[8..12].copy_from_slice(&self.checkpoint_id.to_le_bytes());
        out[12..14].copy_from_slice(&self.hp.to_le_bytes());
        out[14..16].copy_from_slice(&self.hp_max.to_le_bytes());
        if let Some(position) = self.resume_position {
            out[16..18].copy_from_slice(&position.yaw.to_le_bytes());
            out[18..20].copy_from_slice(&1u16.to_le_bytes());
            out[20..24].copy_from_slice(&position.x.to_le_bytes());
            out[24..28].copy_from_slice(&position.y.to_le_bytes());
            out[28..32].copy_from_slice(&position.z.to_le_bytes());
        }
        // The future-state reserve remains zero.
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
        if version != SAVE_VERSION && version != 2 {
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
        let word = |at: usize| u16::from_le_bytes([buf[at], buf[at + 1]]);
        let long = |at: usize| i32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
        let mut flags = [0u8; SAVE_FLAG_BYTES];
        flags.copy_from_slice(&buf[FLAGS_OFFSET..FLAGS_OFFSET + SAVE_FLAG_BYTES]);
        if version == 2 {
            // Version 2: logic-record index and room index, neither of which
            // names anything now; keep the pose, which is a world position.
            let resume_position = (word(14) != V2_ROOM_NONE).then(|| SavedPlayerPosition {
                yaw: word(16),
                x: long(20),
                y: long(24),
                z: long(28),
            });
            return Some(Self {
                checkpoint_id: 0,
                hp: word(10),
                hp_max: word(12),
                flag_count,
                resume_position,
                flags,
            });
        }
        let resume_position = (word(18) == 1).then(|| SavedPlayerPosition {
            yaw: word(16),
            x: long(20),
            y: long(24),
            z: long(28),
        });
        Some(Self {
            checkpoint_id: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            hp: word(12),
            hp_max: word(14),
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
        // psx-numeric-allow-next-line: FNV-1a-64 checksum decode; xor/mul via compiler builtins at save/load only
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
            // A version 1 checkpoint was a logic-record index; it names
            // nothing now, so the migrated block has none.
            checkpoint_id: 0,
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
        block.checkpoint_id = psx_level::StableId::of_name("altar of ash").raw();
        block.hp = 36;
        block.resume_position = Some(SavedPlayerPosition {
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
        frame[12] ^= 0xFF; // hp byte flipped, checksum now stale
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
        assert_eq!(
            migrated.checkpoint_id, 0,
            "a record index is not a stable id"
        );
        assert_eq!(migrated.hp, 36);
        assert_eq!(migrated.resume_position, None);
        assert!(migrated.flag(0));
        assert!(migrated.flag(6));
    }

    #[test]
    fn a_checkpoint_stable_id_survives_a_recook_that_reorders_everything() {
        // Two cooks list the same checkpoints in another order; the save
        // names the checkpoint by its stable id, which finds it in both.
        let altar = psx_level::StableId::of_name("altar of ash");
        let gate = psx_level::StableId::of_name("gatehouse");
        let before = [gate, altar];
        let after = [altar, psx_level::StableId::of_name("new relay"), gate];
        let mut block = SaveBlock::new(100, 2);
        block.checkpoint_id = altar.raw();
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        block.encode(&mut frame);
        let loaded = SaveBlock::decode(&frame, 2).unwrap();
        let id = psx_level::StableId(loaded.checkpoint_id);
        assert_eq!(psx_level::position_of(&before, id), Some(1));
        assert_eq!(psx_level::position_of(&after, id), Some(0));
        // A checkpoint the recook removed finds nothing: start at the spawn.
        assert_eq!(psx_level::position_of(&[gate], id), None);
    }

    #[test]
    fn a_version_2_save_loads_with_its_pose_and_without_its_indices() {
        let mut frame = [0u8; SAVE_BLOCK_BYTES];
        frame[0..4].copy_from_slice(&SAVE_MAGIC);
        frame[4..6].copy_from_slice(&2u16.to_le_bytes());
        frame[6..8].copy_from_slice(&7u16.to_le_bytes());
        frame[8..10].copy_from_slice(&3u16.to_le_bytes()); // logic index
        frame[10..12].copy_from_slice(&36u16.to_le_bytes());
        frame[12..14].copy_from_slice(&100u16.to_le_bytes());
        frame[14..16].copy_from_slice(&5u16.to_le_bytes()); // room index
        frame[16..18].copy_from_slice(&3_072u16.to_le_bytes());
        frame[20..24].copy_from_slice(&(-1_024i32).to_le_bytes());
        frame[24..28].copy_from_slice(&512i32.to_le_bytes());
        frame[28..32].copy_from_slice(&8_192i32.to_le_bytes());
        frame[FLAGS_OFFSET] = 0b0100_0001;
        let checksum = fnv1a_64(&frame[..CHECKSUM_OFFSET]);
        frame[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        let loaded = SaveBlock::decode(&frame, 7).expect("a v2 save still loads");
        assert_eq!(loaded.checkpoint_id, 0);
        assert_eq!((loaded.hp, loaded.hp_max), (36, 100));
        assert_eq!(
            loaded.resume_position,
            Some(SavedPlayerPosition {
                x: -1_024,
                y: 512,
                z: 8_192,
                yaw: 3_072
            })
        );
        assert!(loaded.flag(0) && loaded.flag(6));
    }
}
