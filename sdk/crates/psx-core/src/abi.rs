// SPDX-License-Identifier: GPL-2.0-or-later
//! What a program and the core agree on, in the forms the core's code uses. Every address and
//! layout comes from `psx-scene-abi`, the one definition the disc tools share; this module only
//! adds the word views of the table row and the handoff block, and the core's own codes.

use psx_scene_abi as s;

pub use s::slot;

/// First byte of the core window.
pub const WINDOW: u32 = s::CORE_WINDOW.start;
/// One past the last byte of the core window; the handoff block starts here.
pub const WINDOW_END: u32 = s::CORE_WINDOW.end;
/// Bytes in the window: the core's image and its statics.
pub const WINDOW_BYTES: u32 = s::CORE_WINDOW.len();
/// `RAM_SIZE_OVERRIDE` every program on the core links with.
pub const RAM_SIZE: u32 = s::RAM_SIZE_OVERRIDE;
/// The stack top of every program on the core, and where a switch moves `$sp`.
pub const STACK_INIT: u32 = s::STACK_INIT;
/// Stack a switch uses at the top of the outgoing program's stack reserve.
pub const SWITCH_STACK: u32 = s::SWITCH_STACK;
/// A payload ends at or below this.
pub const PAYLOAD_END: u32 = s::PAYLOAD_END;
/// LBA bit: absolute on the disc, not offset by the member base (`psx_core_read`,
/// `psx_core_stream_open`).
pub const READ_ABSOLUTE: u32 = s::LBA_ABSOLUTE;
/// `psx_core_stream_open` flag: start with an implicit seek (SetLoc then ReadN, no SeekL).
pub const STREAM_IMPLICIT_SEEK: u32 = s::STREAM_IMPLICIT_SEEK;

/// The handoff block, by byte offset from [`block::ADDR`].
pub mod block {
    use psx_scene_abi::block as b;
    /// Its address, also passed in `$a3` on a core entry.
    pub const ADDR: u32 = psx_scene_abi::HANDOFF_BLOCK.start;
    /// Bytes.
    pub const BYTES: u32 = b::LENGTH as u32;
    /// "PSCB".
    pub const MAGIC: u32 = b::MAGIC;
    /// Layout version written by this core (version 1 until the core moves to the v2 table).
    pub const VERSION: u32 = b::VERSION as u32;
    /// `version:u16 | length:u16 << 16`.
    pub const OFF_VERSION: u32 = b::OFF_VERSION as u32;
    /// Offset of the magic.
    pub const OFF_MAGIC: u32 = b::OFF_MAGIC as u32;
    /// The running core's hash.
    pub const OFF_CORE_HASH: u32 = b::OFF_CORE_HASH as u32;
    /// The disc LBA offset.
    pub const OFF_LBA_OFFSET: u32 = b::OFF_LBA_OFFSET as u32;
    /// The CD-DA track base.
    pub const OFF_CDDA_BASE: u32 = b::OFF_CDDA_BASE as u32;
    /// `current:u16 | previous:u16 << 16`.
    pub const OFF_SCENES: u32 = b::OFF_CURRENT_SCENE as u32;
    /// The flags word.
    pub const OFF_FLAGS: u32 = b::OFF_FLAGS as u32;
    /// The switch counter.
    pub const OFF_SWITCHES: u32 = b::OFF_SWITCH_COUNT as u32;
    /// The four argument words.
    pub const OFF_ARGS: u32 = b::OFF_ARGS as u32;
    /// The loaded-image mask (version 2), core-written.
    pub const OFF_MASK: u32 = b::OFF_LOADED_MASK as u32;
    /// Flag: entered in-program by a switch (no `_start` ran).
    pub const FLAG_IN_PROGRAM: u32 = b::FLAG_IN_PROGRAM;
    /// `!MAGIC`, written last.
    pub const OFF_NOT_MAGIC: u32 = b::OFF_MAGIC_INV as u32;
    /// Flag: entered by a core switch.
    pub const FLAG_CORE_ENTRY: u32 = b::FLAG_CORE_ENTRY;
    /// Flag: the program's disc base is not 0 (a member of a bigger disc).
    pub const FLAG_NESTED: u32 = b::FLAG_NESTED;
    /// Previous-scene value after a BIOS boot.
    pub const NO_SCENE: u32 = psx_scene_abi::NO_SCENE as u32;
}

/// The scene table (psx-scene-abi's version 2) as the core reads it: by word of the program's RAM
/// copy, a 64-byte header, the scene rows, then the image rows at their image index, 32 bytes each.
pub mod table {
    use psx_scene_abi as s;
    /// Member-relative LBA of the table.
    pub const LBA: u32 = s::TABLE_LBA;
    /// Member-relative LBA of the boot exe.
    pub const BOOT_EXE_LBA: u32 = s::BOOT_EXE_LBA;
    /// The magic as two little-endian words.
    pub const MAGIC: [u32; 2] = [
        u32::from_le_bytes([s::MAGIC[0], s::MAGIC[1], s::MAGIC[2], s::MAGIC[3]]),
        u32::from_le_bytes([s::MAGIC[4], s::MAGIC[5], s::MAGIC[6], s::MAGIC[7]]),
    ];
    /// Table version.
    pub const VERSION: u32 = s::VERSION as u32;
    /// Image kind: a whole program (PS-X EXE with psx-rt and the core's entry).
    pub const KIND_PROGRAM: u32 = s::Kind::Program as u32;
    /// Image kind: an overlay linked against its owner program, entered in-program.
    pub const KIND_OVERLAY: u32 = s::Kind::Overlay as u32;
    /// Scene row word 0 flag: the scene starts its program (a program switch).
    pub const SCENE_START: u32 = (s::SCENE_FLAG_START as u32) << 24;
    /// Scene row word: the entry PC.
    pub const S_ENTRY: usize = 1;
    /// Scene row word: the images the scene needs in RAM on entry, one bit per image row.
    pub const S_NEEDS: usize = 3;
    /// Image row words: LBA (relative to the table's base; a program's is its header sector).
    pub const I_LBA: usize = 0;
    /// Bytes to load.
    pub const I_SIZE: usize = 1;
    /// Load address.
    pub const I_ADDR: usize = 2;
    /// Word FNV of the loaded bytes.
    pub const I_FNV: usize = 3;
    /// Core hash (a program).
    pub const I_CORE_HASH: usize = 4;
    /// An overlay: the images it overwrites, applied after its own bit is set (a VOLATILE
    /// overlay lists itself, so it is never kept loaded). A program: the overlays its own exe
    /// carries, loaded whenever it is entered.
    pub const I_EVICTS: usize = 5;
    /// A program's member base, relative to the table's base.
    pub const I_BASE: usize = 6;
    /// `kind | class << 8 | owner << 16 | cdda_base << 24`.
    pub const I_KIND: usize = 7;
    /// Rows the one sector holds, scenes and images together.
    pub const MAX_ROWS: u32 = s::MAX_ROWS as u32;
    /// Images at most (bits of the loaded mask).
    pub const MAX_IMAGES: u32 = s::MAX_IMAGES as u32;
    /// Words of the header and of a row.
    pub const HEADER_WORDS: u32 = (s::HEADER_BYTES / 4) as u32;
    /// Words of a row.
    pub const ROW_WORDS: u32 = (s::ROW_BYTES / 4) as u32;

    /// Where overlay `image`'s tail lands (a manifest `tail=n`: the sectors its row loads after
    /// its own bytes), as `(address, bytes)` from the program's copy of the table. `own_bytes` is
    /// the overlay file's size (`__overlay_N_bytes`); its sector-padded end is where the tail
    /// starts. `None` when the copy holds no such image or its row loads nothing past the overlay.
    #[must_use]
    pub fn tail(table: &[u32], image: u32, own_bytes: u32) -> Option<(u32, u32)> {
        let head = *table.get(2)?;
        if table.first() != Some(&MAGIC[0]) || image >= head >> 16 {
            return None;
        }
        let at = HEADER_WORDS + ROW_WORDS * ((*table.get(3)? & 0xffff) + image);
        let row = table.get(at as usize..(at + ROW_WORDS) as usize)?;
        let own = own_bytes.checked_next_multiple_of(SECTOR_BYTES)?;
        match row[I_SIZE].checked_sub(own) {
            Some(bytes) if bytes != 0 && row[I_KIND] & 0xff == KIND_OVERLAY => {
                Some((row[I_ADDR].wrapping_add(own), bytes))
            }
            _ => None,
        }
    }

    /// The table's boot scene id (header word 4), the scene its image 0 starts as.
    #[must_use]
    pub fn boot_scene(table: &[u32]) -> u32 {
        table.get(4).map_or(0, |w| w & 0xffff)
    }

    /// The images scene `id` needs (its row's word 3), found by its id field as the core finds it
    /// (a compact copy keeps only the scenes a program reaches); `None` when the copy holds no
    /// such scene or is not a table.
    #[must_use]
    pub fn scene_needs(table: &[u32], id: u32) -> Option<u32> {
        if table.get(..2)? != MAGIC {
            return None;
        }
        let scenes = *table.get(3)? & 0xffff;
        (0..scenes.min(MAX_ROWS)).find_map(|i| {
            let row = table.get((HEADER_WORDS + ROW_WORDS * i) as usize..)?;
            (row.first()? & 0xffff == id).then(|| row.get(S_NEEDS).copied())?
        })
    }

    /// Bytes of a sector.
    const SECTOR_BYTES: u32 = s::SECTOR_BYTES as u32;

    #[cfg(test)]
    mod tests {
        use super::*;

        fn table(size: u32, kind: u32) -> [u32; 40] {
            let mut t = [0u32; 40];
            t[0] = MAGIC[0];
            t[1] = MAGIC[1];
            t[2] = VERSION | 2 << 16; // two images
            t[3] = 1; // one scene
            let row = (HEADER_WORDS + ROW_WORDS * 2) as usize; // image 1
            t[row + I_SIZE] = size;
            t[row + I_ADDR] = 0x8010_0000;
            t[row + I_KIND] = kind;
            t
        }

        #[test]
        fn a_scene_is_found_by_its_id_with_what_it_needs() {
            let mut t = table(2048, KIND_OVERLAY);
            t[4] = 7; // boot scene 7
            let row = HEADER_WORDS as usize; // the copy's one scene row
            t[row] = 7 | 1 << 16;
            t[row + S_NEEDS] = 0b110;
            assert_eq!(boot_scene(&t), 7);
            assert_eq!(scene_needs(&t, 7), Some(0b110));
            assert_eq!(scene_needs(&t, 0), None, "no such scene");
            assert_eq!(scene_needs(&[0; 4], 7), None, "table not read");
        }

        #[test]
        fn a_tail_starts_at_the_overlays_padded_end() {
            let t = table(4 * 2048, KIND_OVERLAY);
            assert_eq!(tail(&t, 1, 5000), Some((0x8010_1800, 2048)));
            assert_eq!(tail(&t, 1, 6144), Some((0x8010_1800, 2048)));
            assert_eq!(tail(&t, 1, 6145), None, "no sector past the overlay");
            assert_eq!(tail(&t, 2, 5000), None, "no such image");
            assert_eq!(tail(&table(4 * 2048, KIND_PROGRAM), 1, 5000), None);
            assert_eq!(tail(&[0; 4], 1, 0), None, "table not read");
            assert_eq!(tail(&t[..20], 1, 5000), None, "copy too short");
        }
    }
}

/// `$a0` of a program entered with disc bases (psx-io's `disc_base::HANDOFF_MAGIC`).
pub const PSXD: u32 = 0x5053_5844;

/// Op codes, outcomes and limits of the register-level CD slot: psx-scene-abi's, the one
/// definition psx-io's `core-cd` uses too.
pub use s::cd_op;

/// Stage codes: what a refusal returns and what the fatal screen shows.
pub mod code {
    /// Success.
    pub const OK: u32 = 0;
    /// `__core_boot`: the window's hash is not this program's core hash.
    pub const WINDOW_HASH: u32 = 0x01;
    /// A core entry found no live core or no valid handoff block.
    pub const BAD_BLOCK: u32 = 0x02;
    /// The drive did not answer.
    pub const NO_ANSWER: u32 = 0x11;
    /// A Pause could not be proven.
    pub const PAUSE: u32 = 0x12;
    /// A read failed after its attempts.
    pub const READ: u32 = 0x13;
    /// The payload checksum was wrong after its attempts.
    pub const PAYLOAD_FNV: u32 = 0x14;
    /// No table, or its magic or version is not this core's.
    pub const TABLE_MAGIC: u32 = psx_scene_abi::status::TABLE;
    /// No such scene.
    pub const NO_ROW: u32 = 0x22;
    /// The row's core hash is not the running core's.
    pub const CORE_HASH: u32 = 0x23;
    /// The row's load bounds are outside the program area, or its kind is not built yet.
    pub const BOUNDS: u32 = 0x24;
    /// An in-program scene or an overlay whose program is not the one running.
    pub const NOT_LOADED: u32 = psx_scene_abi::status::NOT_LOADED;
    /// A stream client is attached.
    pub const CLIENT: u32 = 0x31;
}
