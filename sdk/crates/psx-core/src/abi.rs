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
    pub const VERSION: u32 = psx_scene_abi::v1::BLOCK_VERSION as u32;
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
    /// `!MAGIC`, written last.
    pub const OFF_NOT_MAGIC: u32 = b::OFF_MAGIC_INV as u32;
    /// Flag: entered by a core switch.
    pub const FLAG_CORE_ENTRY: u32 = b::FLAG_CORE_ENTRY;
    /// Flag: the program's disc base is not 0 (a member of a bigger disc).
    pub const FLAG_NESTED: u32 = b::FLAG_NESTED;
    /// Previous-scene value after a BIOS boot.
    pub const NO_SCENE: u32 = psx_scene_abi::NO_SCENE as u32;
}

/// The scene table, by word of its one sector.
pub mod table {
    // The version 1 table until the core moves to version 2 (psx_scene_abi::v1).
    use psx_scene_abi::v1 as s;
    /// Member-relative LBA of the table.
    pub const LBA: u32 = s::TABLE_LBA;
    /// Member-relative LBA of the boot exe.
    pub const BOOT_EXE_LBA: u32 = s::BOOT_EXE_LBA;
    /// "PSXSCN01" as two little-endian words.
    pub const MAGIC: [u32; 2] = [
        u32::from_le_bytes([s::MAGIC[0], s::MAGIC[1], s::MAGIC[2], s::MAGIC[3]]),
        u32::from_le_bytes([s::MAGIC[4], s::MAGIC[5], s::MAGIC[6], s::MAGIC[7]]),
    ];
    /// Table version.
    pub const VERSION: u32 = s::VERSION as u32;
    /// Row kind: a whole program.
    pub const KIND_PROGRAM: u32 = s::Kind::Program as u32;
    /// Word of the header holding `row_count << 16 | version`.
    pub const W_HEAD_VERSION: usize = 2;
    /// Rows the one sector holds.
    pub const MAX_ROWS: u32 = s::rows_capacity(1) as u32;
    /// Row words: `id:u16 | kind:u8 << 16 | flags:u8 << 24`.
    pub const W_ID_KIND: usize = 0;
    /// Exe header sector LBA, relative to the base the table was read at.
    pub const W_LBA: usize = 1;
    /// Sectors of the file.
    pub const W_SECTORS: usize = 2;
    /// Load address.
    pub const W_T_ADDR: usize = 3;
    /// Bytes to load from `lba + 1`: whole sectors, stopping before the stored core image.
    pub const W_T_SIZE: usize = 4;
    /// Entry PC.
    pub const W_PC: usize = 5;
    /// Word FNV of the loaded bytes.
    pub const W_FNV: usize = 6;
    /// Core hash.
    pub const W_CORE_HASH: usize = 7;
    /// Initial SP.
    pub const W_SP: usize = 8;
    /// `return:u16 | attempts:u8 << 16 | min_block_version:u8 << 24`.
    pub const W_POLICY: usize = 9;
    /// `partner:u16 | cdda_base:u16 << 16` (the CD-DA base is added to the current one).
    pub const W_PARTNER_CDDA: usize = 10;
    /// The program's member base, relative to the base the table was read at (0: same image).
    pub const W_LBA_OFFSET: usize = 11;

    /// Word index of row `id` within the table's one sector.
    #[must_use]
    pub const fn row_word(id: u32) -> u32 {
        (s::HEADER_BYTES as u32 + id * s::ROW_BYTES as u32) / 4
    }
}

/// `$a0` of a program entered with disc bases (psx-io's `disc_base::HANDOFF_MAGIC`).
pub const PSXD: u32 = 0x5053_5844;

/// Op codes of the register-level CD slot.
pub mod cd_op {
    /// Send a command and wait for its acknowledge.
    pub const COMMAND: u32 = 0;
    /// Send a command and return at once.
    pub const COMMAND_NO_WAIT: u32 = 1;
    /// Read the controller's IRQ flag.
    pub const IRQ_FLAG: u32 = 2;
    /// Acknowledge flag bits.
    pub const IRQ_ACK: u32 = 3;
    /// Set the CD audio mixer and apply it.
    pub const MIXER: u32 = 4;
    /// Set the controller's IRQ enables; returns the old ones.
    pub const IRQ_ENABLE: u32 = 5;
    /// Pop the response FIFO.
    pub const RESPONSE: u32 = 6;
}

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
    /// The scene table's magic or version is not this core's.
    pub const TABLE_MAGIC: u32 = 0x21;
    /// No such scene.
    pub const NO_ROW: u32 = 0x22;
    /// The row's core hash is not the running core's.
    pub const CORE_HASH: u32 = 0x23;
    /// The row's load bounds are outside the program area, or its kind is not built yet.
    pub const BOUNDS: u32 = 0x24;
    /// A stream client is attached.
    pub const CLIENT: u32 = 0x31;
}
