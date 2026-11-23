//! The scene core's fixed layout, in one place every side reads.
//!
//! This crate holds what the disc tools (mkisopsx, the demo disc's mkdisc),
//! the post-link checks (psoxide-hazard), the harness and the guest core must
//! agree on byte for byte:
//!
//! - the memory map: the core window, the handoff block, the stack top a
//!   hosting program links for ([`CORE_WINDOW`], [`HANDOFF_BLOCK`]);
//! - the core's jump-table slot addresses ([`slot`]);
//! - the handoff block's field offsets ([`block`]);
//! - the `SCENES.TBL` wire format ([`Header`], [`SceneRow`], [`ImageRow`],
//!   [`encode_table`]).
//!
//! The table is version 2 (`PSXSCN02`): scene rows say where a scene enters,
//! image rows say what is loaded where, so a Program can host scenes of its
//! own and Overlays without a program switch. A version 1 table is refused
//! ([`status::TABLE`]); [`v1`] keeps what the prebuilt core still reads.
//!
//! Version 2 layout, one sector at LBA 22, all little-endian:
//!
//! ```text
//! 0x00            header, 64 B (Header)
//! 0x40            S scene rows, 32 B each, index = scene id (SceneRow)
//! 0x40 + 32 * S   I image rows, 32 B each, index = loaded-mask bit (ImageRow)
//!                 I in 1..=32, S + I <= 62; image 0 is the boot exe at LBA 23
//! ```
//!
//! The core never reads the table from the disc: each program reads it once
//! and passes a RAM copy to every `psx_core_switch` (`table_ptr`). That copy
//! may be the whole sector or a compact one holding only the rows the program
//! can reach ([`encode_compact`]); the same decoders read both.
//!
//! Items marked
//! **provisional** can still move after the core's first builds and proofs;
//! the window bounds are one constant so a real core build can move them by
//! one edit.
//!
//! No dependencies, no allocation, `no_std`: the guest core can link it.
//! Numbers are little-endian. "Word FNV" is FNV-1a 32 over little-endian
//! `u32` words, `h = (h ^ word) * 0x0100_0193` from `0x811C_9DC5`, the same
//! function as `psx_pack::WordFnv32` (tested against it).

#![no_std]

/// A half-open address range, `start..end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// First address in the range.
    pub start: u32,
    /// First address past the range.
    pub end: u32,
}

impl Span {
    /// Bytes in the range.
    pub const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// True for an empty range.
    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// True when `lo..hi` (half-open, non-empty) lies wholly inside.
    pub const fn holds(&self, lo: u32, hi: u32) -> bool {
        self.start <= lo && lo < hi && hi <= self.end
    }
}

/// Cached main RAM (KSEG0).
pub const RAM_BASE: u32 = 0x8000_0000;

/// Where every scene's image loads (psoxide.ld's `LOAD_ADDR`, psyq.ld's).
pub const LOAD_ADDR: u32 = 0x8001_0000;

/// The core window: the prebuilt core's code, data and bss, at the same
/// addresses in every program that hosts it. The core keeps no stack here:
/// a switch runs at the top of the outgoing program's stack reserve, every
/// other call on the caller's stack. **Provisional** at 8,192 B (down from
/// the earlier 10,240 B cap); the final number comes after P1, and this
/// constant is the one place that moves.
pub const CORE_WINDOW: Span = Span {
    start: 0x801f_df00,
    end: 0x801f_ff00,
};

/// The handoff block, 256 B at the top of RAM, above the window. Its address
/// is fixed and is also passed in `$a3` on a core entry.
pub const HANDOFF_BLOCK: Span = Span {
    start: 0x801f_ff00,
    end: 0x8020_0000,
};

/// A hosting program's `STACK_INIT`: its stack starts where the window does.
pub const STACK_INIT: u32 = CORE_WINDOW.start;

/// The `RAM_SIZE_OVERRIDE` a hosting program links psoxide.ld with
/// (`-T` file before the script). psoxide.ld puts `STACK_INIT` 0x100 below
/// `RAM_BASE + RAM_SIZE`, so this lands it on [`STACK_INIT`].
pub const RAM_SIZE_OVERRIDE: u32 = STACK_INIT + 0x100 - RAM_BASE;

/// Stack a switch runs on, below [`STACK_INIT`]: no payload may load into it.
pub const SWITCH_STACK: u32 = 0x400;

/// The highest address a Program row's payload may end at: the core refuses
/// an image row whose `load_addr + size` is above it (stage 0x24).
pub const PAYLOAD_END: u32 = STACK_INIT - SWITCH_STACK;

/// The core's jump table: one `j` per 8-byte slot at the start of the window.
/// The addresses are the ABI, fixed across core builds.
pub mod slot {
    use super::CORE_WINDOW;

    /// Bytes per slot.
    pub const SLOT_BYTES: u32 = 8;
    /// `__psx_core_attach(a0, a1, a2, hash)`, called only by `__core_boot`.
    pub const ATTACH: u32 = CORE_WINDOW.start;
    /// `psx_core_switch(scene, args) -> u32`.
    pub const SWITCH: u32 = CORE_WINDOW.start + SLOT_BYTES;
    /// `psx_core_read(lba, sectors, dst, hook) -> u32`.
    pub const READ: u32 = CORE_WINDOW.start + 2 * SLOT_BYTES;
    /// `psx_core_pause() -> u32`.
    pub const PAUSE: u32 = CORE_WINDOW.start + 3 * SLOT_BYTES;
    /// `psx_core_client_attach() -> u32`.
    pub const CLIENT_ATTACH: u32 = CORE_WINDOW.start + 4 * SLOT_BYTES;
    /// `psx_core_client_detach()`.
    pub const CLIENT_DETACH: u32 = CORE_WINDOW.start + 5 * SLOT_BYTES;
    /// `psx_core_fatal(stage) -> !`.
    pub const FATAL: u32 = CORE_WINDOW.start + 6 * SLOT_BYTES;
    /// `psx_core_music_yield()`.
    pub const MUSIC_YIELD: u32 = CORE_WINDOW.start + 7 * SLOT_BYTES;
    /// `psx_core_stream_open(lba, flags) -> u32`, a stream client's start.
    pub const STREAM_OPEN: u32 = CORE_WINDOW.start + 8 * SLOT_BYTES;
    /// `psx_core_stream_sector(dst) -> u32`, the next sector of an open stream.
    pub const STREAM_SECTOR: u32 = CORE_WINDOW.start + 9 * SLOT_BYTES;
    /// `psx_core_cd(op, a, b, c) -> u32`, register-level CD for a game's own driver.
    pub const CD: u32 = CORE_WINDOW.start + 10 * SLOT_BYTES;
    /// `psx_core_set_idle_hook(hook)`, called from every spin of every core wait.
    pub const SET_IDLE_HOOK: u32 = CORE_WINDOW.start + 11 * SLOT_BYTES;
    /// `psx_core_load_image(image, table) -> u32`: load an overlay and return to the caller.
    pub const LOAD_IMAGE: u32 = CORE_WINDOW.start + 12 * SLOT_BYTES;
    /// Slots in the table.
    pub const COUNT: usize = 13;
    /// One past the table's last slot: the core's own code starts here.
    pub const TABLE_END: u32 = CORE_WINDOW.start + COUNT as u32 * SLOT_BYTES;
}

/// The deepest stack each slot uses below the caller's `$sp`, in bytes: every frame of its worst
/// call chain inside the core, measured from the core image's own code (frame sizes and calls of
/// `psx-core/image`, prebuilt `psx-core.bin`). A program's stack proof adds the slot's figure at
/// each call site. The idle hook (`psx_core_set_idle_hook`) and a read's per-sector hook are the
/// program's own code: they run on top of the slot's frames, so their depth adds to the slot's.
/// The switch runs its load on a stack of its own at `STACK_INIT` (see `SWITCH_STACK`), so
/// [`stack::SWITCH`] is only what it uses on the caller's stack before that.
pub mod stack {
    /// `__psx_core_attach` (called by `__core_boot` only).
    pub const ATTACH: u32 = 72;
    /// `psx_core_switch`, on the caller's stack, before it moves to the switch stack.
    pub const SWITCH: u32 = 368;
    /// The switch's load, on the switch stack at `STACK_INIT`; under `SWITCH_STACK`.
    pub const SWITCH_LOAD: u32 = 352;
    /// `psx_core_read`.
    pub const READ: u32 = 320;
    /// `psx_core_pause`.
    pub const PAUSE: u32 = 216;
    /// `psx_core_client_attach`.
    pub const CLIENT_ATTACH: u32 = 0;
    /// `psx_core_client_detach`.
    pub const CLIENT_DETACH: u32 = 0;
    /// `psx_core_fatal`.
    pub const FATAL: u32 = 64;
    /// `psx_core_music_yield`.
    pub const MUSIC_YIELD: u32 = 8;
    /// `psx_core_stream_open`.
    pub const STREAM_OPEN: u32 = 240;
    /// `psx_core_stream_sector`.
    pub const STREAM_SECTOR: u32 = 136;
    /// `psx_core_cd`.
    pub const CD: u32 = 224;
    /// `psx_core_set_idle_hook`.
    pub const SET_IDLE_HOOK: u32 = 0;
    /// `psx_core_load_image`: the rows, the load and its checks.
    pub const LOAD_IMAGE: u32 = 368;
    /// The deepest of the slots a program calls (the switch's load runs on its own stack).
    pub const MAX: u32 = 368;

    const _: () = assert!(SWITCH_LOAD <= super::SWITCH_STACK);

    /// What one jump-table slot costs the stack of the program that calls it.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Slot {
        /// The slot's address, a [`super::slot`] constant.
        pub addr: u32,
        /// The symbol psoxide-core.ld gives the slot.
        pub name: &'static str,
        /// Bytes it uses below the caller's `$sp`, its hooks apart.
        pub depth: u32,
        /// It can call the program's idle hook (it waits on the drive).
        pub idle_hook: bool,
        /// It calls the per-sector hook passed to it (`psx_core_read`'s fourth argument).
        pub sector_hook: bool,
    }

    const fn row(addr: u32, name: &'static str, depth: u32, idle_hook: bool) -> Slot {
        Slot {
            addr,
            name,
            depth,
            idle_hook,
            sector_hook: false,
        }
    }

    /// Every slot, in table order. A slot that runs a hook runs it on top of its own frames, so a
    /// stack proof adds the hook's depth to the slot's. [`SWITCH_LOAD`] is not here: it runs on the
    /// switch stack, never the caller's.
    pub const SLOTS: [Slot; super::slot::COUNT] = {
        use super::slot as s;
        [
            row(s::ATTACH, "__psx_core_attach", ATTACH, false),
            row(s::SWITCH, "psx_core_switch", SWITCH, true),
            Slot {
                sector_hook: true,
                ..row(s::READ, "psx_core_read", READ, true)
            },
            row(s::PAUSE, "psx_core_pause", PAUSE, true),
            row(
                s::CLIENT_ATTACH,
                "psx_core_client_attach",
                CLIENT_ATTACH,
                false,
            ),
            row(
                s::CLIENT_DETACH,
                "psx_core_client_detach",
                CLIENT_DETACH,
                false,
            ),
            row(s::FATAL, "psx_core_fatal", FATAL, false),
            row(s::MUSIC_YIELD, "psx_core_music_yield", MUSIC_YIELD, false),
            row(s::STREAM_OPEN, "psx_core_stream_open", STREAM_OPEN, true),
            row(
                s::STREAM_SECTOR,
                "psx_core_stream_sector",
                STREAM_SECTOR,
                true,
            ),
            row(s::CD, "psx_core_cd", CD, true),
            row(
                s::SET_IDLE_HOOK,
                "psx_core_set_idle_hook",
                SET_IDLE_HOOK,
                false,
            ),
            row(s::LOAD_IMAGE, "psx_core_load_image", LOAD_IMAGE, true),
        ]
    };

    /// The slot that starts at `addr`, `None` for any other address (inside the window or not).
    pub const fn slot_at(addr: u32) -> Option<Slot> {
        let mut i = 0;
        while i < SLOTS.len() {
            if SLOTS[i].addr == addr {
                return Some(SLOTS[i]);
            }
            i += 1;
        }
        None
    }
}

/// The op codes of the register-level CD slot, [`slot::CD`]:
/// `psx_core_cd(op, a, b, c) -> u32`, for a game's own drive driver (an XA
/// player). Every op leaves controller register index 0 selected.
pub mod cd_op {
    /// Send a command and wait for its acknowledge (INT3). `a` points at the
    /// parameter bytes, `b` = count | command << 8; the controller's IRQ
    /// enables are masked for the command and put back after it. The response
    /// ([`RESPONSE_BYTES`] at most) is copied to `c` when `c` is not null.
    /// Returns the outcome ([`ACKNOWLEDGED`], [`DRIVE_ERROR`], [`TIMEOUT`]) |
    /// response length << 8.
    pub const COMMAND: u32 = 0;
    /// Send a command and return at once: the parameter FIFO emptied, the
    /// parameters and the command byte written, the IRQ enables untouched.
    /// The caller collects the answers with [`IRQ_FLAG`], [`IRQ_ACK`] and
    /// [`RESPONSE`]. Returns [`ACKNOWLEDGED`], or [`TIMEOUT`] when the
    /// parameter FIFO never made room.
    pub const COMMAND_NO_WAIT: u32 = 1;
    /// The controller's IRQ flag (0 none, 1..5 INT1..INT5).
    pub const IRQ_FLAG: u32 = 2;
    /// Acknowledge flag bits `a` and the CD-ROM bit of `I_STAT`.
    pub const IRQ_ACK: u32 = 3;
    /// The CD audio mixer, `a` = L->L | L->R << 8 | R->R << 16 | R->L << 24,
    /// then applied.
    pub const MIXER: u32 = 4;
    /// Set the controller's IRQ enables to `a`; returns the old ones.
    pub const IRQ_ENABLE: u32 = 5;
    /// Pop the response FIFO ([`RESPONSE_BYTES`] at most) into `c`, or
    /// discard it when `c` is null; returns the bytes popped.
    pub const RESPONSE: u32 = 6;

    /// [`COMMAND`] outcome: the drive acknowledged.
    pub const ACKNOWLEDGED: u32 = 0;
    /// [`COMMAND`] outcome: the drive answered INT5; the core acknowledged it.
    pub const DRIVE_ERROR: u32 = 1;
    /// [`COMMAND`] outcome: no acknowledge within the core's acknowledge bound.
    pub const TIMEOUT: u32 = 2;

    /// Parameter bytes [`COMMAND`] sends at most.
    pub const MAX_PARAMS: u32 = 16;
    /// Response bytes [`COMMAND`] and [`RESPONSE`] copy at most.
    pub const RESPONSE_BYTES: usize = 16;
}

/// OR'd into the `lba` argument of `psx_core_read` and `psx_core_stream_open`:
/// the LBA is absolute on the disc, not offset by the member base, for a file
/// in the host disc's root. The same bit marks an image row's LBA. A
/// member-relative LBA never reaches bit 31 (a CD holds under 2^19 sectors).
pub const LBA_ABSOLUTE: u32 = 1 << 31;

/// `psx_core_stream_open` flag bit 0: skip the SeekL and go straight to ReadN.
pub const STREAM_IMPLICIT_SEEK: u32 = 1 << 0;

/// The handoff block's fields, as byte offsets from [`HANDOFF_BLOCK`]`.start`.
pub mod block {
    /// `"PSCB"` read as a little-endian word.
    pub const MAGIC: u32 = 0x4243_5350;
    /// `!MAGIC`, the block's last word, written last.
    pub const MAGIC_INV: u32 = !MAGIC;
    /// Block version this layout describes: 2 adds [`OFF_LOADED_MASK`] and
    /// [`FLAG_IN_PROGRAM`].
    pub const VERSION: u16 = 2;
    /// Valid length the core writes.
    pub const LENGTH: u16 = 0x100;

    /// u32 magic.
    pub const OFF_MAGIC: usize = 0x00;
    /// u16 block version.
    pub const OFF_VERSION: usize = 0x04;
    /// u16 valid length in bytes.
    pub const OFF_LENGTH: usize = 0x06;
    /// u32 core hash of the running core.
    pub const OFF_CORE_HASH: usize = 0x08;
    /// u32 disc LBA offset (member start on a nested disc, 0 standalone).
    pub const OFF_LBA_OFFSET: usize = 0x0c;
    /// u32 CD-DA track base.
    pub const OFF_CDDA_BASE: usize = 0x10;
    /// u16 current scene id.
    pub const OFF_CURRENT_SCENE: usize = 0x14;
    /// u16 previous scene id, 0xFFFF after a BIOS boot.
    pub const OFF_PREVIOUS_SCENE: usize = 0x16;
    /// u32 flags, see [`FLAG_CORE_ENTRY`], [`FLAG_NESTED`] and [`FLAG_IN_PROGRAM`].
    pub const OFF_FLAGS: usize = 0x18;
    /// u32 switch counter since the BIOS boot.
    pub const OFF_SWITCH_COUNT: usize = 0x1c;
    /// Four u32 argument words from the switching scene's `core_switch`.
    pub const OFF_ARGS: usize = 0x20;
    /// 64-byte settings record (**provisional** layout: psx-settings owns it).
    pub const OFF_SETTINGS: usize = 0x30;
    /// Settings record bytes.
    pub const SETTINGS_BYTES: usize = 64;
    /// u16 partner credit id shown at boot, 0 none (**provisional**).
    pub const OFF_PARTNER_CREDIT: usize = 0x70;
    /// u32 loaded mask, one bit per image row, written by the core (block version 2).
    pub const OFF_LOADED_MASK: usize = 0x74;
    /// Scene-private area, meaning agreed by the two scenes that use it.
    pub const OFF_PRIVATE: usize = 0x80;
    /// Scene-private bytes.
    pub const PRIVATE_BYTES: usize = 124;
    /// u32 `!magic`.
    pub const OFF_MAGIC_INV: usize = 0xfc;

    /// Flag bit 0: the program was entered by the core (a switch).
    pub const FLAG_CORE_ENTRY: u32 = 1 << 0;
    /// Flag bit 1: the boot program was chain-loaded with `PSXD` (nested disc).
    pub const FLAG_NESTED: u32 = 1 << 1;
    /// Flag bit 2: entered as an in-program scene (no `_start` ran).
    pub const FLAG_IN_PROGRAM: u32 = 1 << 2;
}

/// User-data bytes per CD sector.
pub const SECTOR_BYTES: usize = 2048;

/// Member-relative LBA of `SCENES.TBL`: the file right after `SYSTEM.CNF`
/// (LBA 21). A program reads it once at boot (at `TABLE_LBA + lba_offset` on
/// a nested disc) and keeps a RAM copy, whole or [compact](encode_compact);
/// the core takes that copy as `table_ptr` and never seeks to the table at a
/// switch.
pub const TABLE_LBA: u32 = 22;

/// Member-relative LBA of the boot exe, right after the one-sector table.
pub const BOOT_EXE_LBA: u32 = TABLE_LBA + 1;

/// File name the disc tools give the table in the ISO 9660 root.
pub const TABLE_FILE_NAME: &str = "SCENES.TBL";

/// Identifies a version 2 scene table.
pub const MAGIC: [u8; 8] = *b"PSXSCN02";

/// The version 1 magic. A version 2 core refuses such a table with [`status::TABLE`].
pub const MAGIC_V1: [u8; 8] = *b"PSXSCN01";

/// Table version this layout describes. A new field in a reserved slot keeps it.
pub const VERSION: u16 = 2;

/// Bytes in the header, at offset 0.
pub const HEADER_BYTES: usize = 64;

/// Bytes per row, scene and image rows alike. Rows start at [`HEADER_BYTES`].
pub const ROW_BYTES: usize = 32;

/// Sectors the table takes: always one.
pub const TABLE_SECTORS: u16 = 1;

/// Rows the one sector holds, scenes and images together: `S + I <= 62`.
pub const MAX_ROWS: usize = (SECTOR_BYTES - HEADER_BYTES) / ROW_BYTES;

/// Images a table may have: one bit each in a `u32` loaded mask.
pub const MAX_IMAGES: usize = 32;

/// The boot program's image index: the exe at [`BOOT_EXE_LBA`].
pub const BOOT_IMAGE: u8 = 0;

/// Bytes reserved for a scene's name.
pub const NAME_BYTES: usize = 16;

/// A scene id that names no scene (the handoff block's previous scene after a BIOS boot).
pub const NO_SCENE: u16 = 0xffff;

/// Status codes the core returns for a table it refuses, named here because the build and the
/// harness check them too.
pub mod status {
    /// The table's magic or version is not this core's; a version 1 table (`PSXSCN01`) included.
    pub const TABLE: u32 = 0x21;
    /// The scene needs an image that is not loaded: an in-program scene while another Program
    /// runs, or an Overlay whose owner's bit is clear in the loaded mask.
    pub const NOT_LOADED: u32 = 0x25;
}

/// Scene flag bit 0: the scene shows its partner credit.
pub const SCENE_FLAG_PARTNER_CREDIT: u8 = 1 << 0;

/// Scene flag bit 1, Program scenes only: the scene starts its Program. Its entry is the exe's
/// PC (`__core_boot`) and the core does a program switch. A Program scene without it is an
/// in-program entry, a function inside the running exe. Overlay scenes ignore it.
pub const SCENE_FLAG_START: u8 = 1 << 1;

/// Byte offset of scene row `id`.
pub const fn scene_row_offset(id: u16) -> usize {
    HEADER_BYTES + id as usize * ROW_BYTES
}

/// Byte offset of image row `index` in a table of `scene_count` scenes: images follow scenes.
pub const fn image_row_offset(scene_count: u16, index: u8) -> usize {
    HEADER_BYTES + (scene_count as usize + index as usize) * ROW_BYTES
}

/// What an image is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// A whole PS-X EXE: its row points at the header sector and loads from the next one.
    Program = 0,
    /// A blob a running Program loads at a fixed address inside its reserved RAM.
    Overlay = 1,
}

impl Kind {
    /// The kind for a wire byte, `None` for one this layout does not know.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Kind::Program),
            1 => Some(Kind::Overlay),
            _ => None,
        }
    }
}

/// How an Overlay is kept. Read by the build only; the core ignores it. A Program row carries 0.
///
/// A scene that runs inside its Program's exe (once called BOOT_RESIDENT) has no image row of its
/// own: its scene row names the Program's image, so it has no class byte either.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    /// Loaded with its siblings and kept: one blob, one read, never overlapping a co-resident image.
    ResidentSet = 1,
    /// Loaded over whatever its range intersects (its evicts mask).
    Swap = 2,
}

impl Class {
    /// The class for a wire byte, `None` for 0 or one this layout does not know.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Class::ResidentSet),
            2 => Some(Class::Swap),
            _ => None,
        }
    }
}

/// The table header, 64 bytes at offset 0.
///
/// ```text
/// 0x00  8  magic "PSXSCN02"
/// 0x08  2  table version, 2
/// 0x0A  2  image count I, 1..=32
/// 0x0C  2  scene count S, S + I <= 62
/// 0x0E  2  row bytes, 32
/// 0x10  2  boot scene id
/// 0x12  2  table sectors, 1
/// 0x14  4  word FNV over all rows, (S + I) * 8 words
/// 0x18  2  flags: bit 0 COMPACT (a RAM copy, see encode_compact); 0 on the disc
/// 0x1A 38  reserved, 0
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Image rows, after the scene rows.
    pub image_count: u16,
    /// Scene rows, from offset 0x40.
    pub scene_count: u16,
    /// The scene the boot exe runs as.
    pub boot_scene: u16,
    /// Word FNV over every row's bytes.
    pub checksum: u32,
    /// A compact RAM copy ([`HEADER_FLAG_COMPACT`]); the disc's table has it clear.
    pub compact: bool,
}

/// Header flag bit 0 (u16 at 0x18): the table is a compact RAM copy. Its scene rows are only
/// the scenes one program can reach, in ascending id, each found by its id field rather than by
/// position; its image rows are images `0..image_count` by position (index = mask bit), with the
/// rows of images it cannot reach all zero. `boot_scene` is the disc table's, kept as read.
pub const HEADER_FLAG_COMPACT: u16 = 1 << 0;

impl Header {
    /// Write the header's 64 bytes, reserved bytes zero.
    pub fn encode(&self, out: &mut [u8; HEADER_BYTES]) {
        *out = [0; HEADER_BYTES];
        out[0x00..0x08].copy_from_slice(&MAGIC);
        put16(out, 0x08, VERSION);
        put16(out, 0x0a, self.image_count);
        put16(out, 0x0c, self.scene_count);
        put16(out, 0x0e, ROW_BYTES as u16);
        put16(out, 0x10, self.boot_scene);
        put16(out, 0x12, TABLE_SECTORS);
        put32(out, 0x14, self.checksum);
        put16(
            out,
            0x18,
            if self.compact { HEADER_FLAG_COMPACT } else { 0 },
        );
    }

    /// Read a header and check what it alone can show: magic, version, row size, sector count,
    /// the counts' limits and the boot scene. [`TableError::status`] gives the core's code.
    pub fn decode(bytes: &[u8; HEADER_BYTES]) -> Result<Self, TableError> {
        if bytes[0x00..0x08] == MAGIC_V1 {
            return Err(TableError::Version1);
        }
        if bytes[0x00..0x08] != MAGIC {
            return Err(TableError::Magic);
        }
        let version = get16(bytes, 0x08);
        if version != VERSION {
            return Err(TableError::Version(version));
        }
        let flags = get16(bytes, 0x18);
        if get16(bytes, 0x0e) != ROW_BYTES as u16
            || get16(bytes, 0x12) != TABLE_SECTORS
            || flags & !HEADER_FLAG_COMPACT != 0
        {
            return Err(TableError::Shape);
        }
        let header = Header {
            image_count: get16(bytes, 0x0a),
            scene_count: get16(bytes, 0x0c),
            boot_scene: get16(bytes, 0x10),
            checksum: get32(bytes, 0x14),
            compact: flags & HEADER_FLAG_COMPACT != 0,
        };
        check_counts(header.scene_count as usize, header.image_count as usize)?;
        if !header.compact && header.boot_scene >= header.scene_count {
            return Err(TableError::NoBootScene(header.boot_scene));
        }
        Ok(header)
    }
}

/// One scene, 32 bytes, at `0x40 + 32 * id`.
///
/// ```text
/// 0x00  2  scene id, equal to the row index
/// 0x02  1  image index
/// 0x03  1  flags: bit 0 show partner credit, bit 1 START (Program scenes)
/// 0x04  4  entry PC: a START scene's exe PC (__core_boot), else the function to call
/// 0x08  2  partner credit id, 0 none
/// 0x0A  1  minimum handoff block version
/// 0x0B  1  reserved, 0
/// 0x0C  4  reserved, 0
/// 0x10 16  name, ASCII, NUL-padded
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneRow {
    /// Scene id; [`encode_table`] checks it equals the row's index.
    pub id: u16,
    /// The image the scene runs in.
    pub image: u8,
    /// [`SCENE_FLAG_PARTNER_CREDIT`], [`SCENE_FLAG_START`].
    pub flags: u8,
    /// Entry PC.
    pub entry: u32,
    /// Partner credit id, 0 none.
    pub partner_credit: u16,
    /// Minimum handoff block version the scene needs.
    pub min_block_version: u8,
    /// ASCII name, NUL-padded.
    pub name: [u8; NAME_BYTES],
}

impl SceneRow {
    /// True when the scene starts its Program ([`SCENE_FLAG_START`]).
    pub const fn starts_program(&self) -> bool {
        self.flags & SCENE_FLAG_START != 0
    }

    /// The name as text, up to its first NUL.
    pub fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_BYTES);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    /// `name` as a row name: truncated to [`NAME_BYTES`], NUL-padded.
    pub fn name_bytes(name: &str) -> [u8; NAME_BYTES] {
        let mut out = [0u8; NAME_BYTES];
        let n = name.len().min(NAME_BYTES);
        out[..n].copy_from_slice(&name.as_bytes()[..n]);
        out
    }

    /// Write the row's 32 bytes, reserved bytes zero.
    pub fn encode(&self, out: &mut [u8; ROW_BYTES]) {
        *out = [0; ROW_BYTES];
        put16(out, 0x00, self.id);
        out[0x02] = self.image;
        out[0x03] = self.flags;
        put32(out, 0x04, self.entry);
        put16(out, 0x08, self.partner_credit);
        out[0x0a] = self.min_block_version;
        out[0x10..0x10 + NAME_BYTES].copy_from_slice(&self.name);
    }

    /// Read a row. Reserved bytes are not checked: a later layout may use them.
    pub fn decode(bytes: &[u8; ROW_BYTES]) -> Self {
        let mut name = [0u8; NAME_BYTES];
        name.copy_from_slice(&bytes[0x10..0x10 + NAME_BYTES]);
        SceneRow {
            id: get16(bytes, 0x00),
            image: bytes[0x02],
            flags: bytes[0x03],
            entry: get32(bytes, 0x04),
            partner_credit: get16(bytes, 0x08),
            min_block_version: bytes[0x0a],
            name,
        }
    }
}

/// One image, 32 bytes, after the scene rows; its index is its bit in the loaded mask.
///
/// ```text
/// 0x00  4  LBA relative to the table base, bit 31 LBA_ABSOLUTE. A Program: its EXE header
///          sector (it loads from LBA + 1). An Overlay: its first data sector.
/// 0x04  4  load size in bytes, whole sectors; a Program's stops before its stored core copy
/// 0x08  4  load address
/// 0x0C  4  word FNV of the loaded bytes
/// 0x10  4  core hash (Program; 0 for an Overlay)
/// 0x14  4  evicts mask (Overlay: the images its range intersects); 0 for a Program
/// 0x18  4  member base relative to the table base (Program; 0 for an Overlay)
/// 0x1C  1  kind: 0 Program, 1 Overlay
/// 0x1D  1  class (Overlay: 1 RESIDENT_SET, 2 SWAP; 0 for a Program); build only
/// 0x1E  1  owner (Overlay: its Program's image index; reserved for a Program, written 0 and
///          not read by the core)
/// 0x1F  1  CD-DA base delta (Program; 0 for an Overlay)
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageRow {
    /// LBA relative to the table base, [`LBA_ABSOLUTE`] for one in the host disc's root.
    pub lba: u32,
    /// Bytes loaded, whole sectors.
    pub size: u32,
    /// Load address.
    pub load_addr: u32,
    /// Word FNV over the `size` loaded bytes.
    pub checksum: u32,
    /// Core hash a Program carries; 0 for an Overlay.
    pub core_hash: u32,
    /// Overlay: the images whose load ranges intersect its own. 0 for a Program.
    pub evicts: u32,
    /// Program: sectors from the table base to its member image (0 on a single-game disc).
    pub member_base: u32,
    /// What the image is.
    pub kind: Kind,
    /// Overlay: how it is kept. `None` for a Program.
    pub class: Option<Class>,
    /// Overlay: its Program's image index. Reserved for a Program: written 0, not read by the core.
    pub owner: u8,
    /// Program: CD-DA tracks of the members ahead of its own (0 on a single-game disc).
    pub cdda_delta: u8,
}

impl ImageRow {
    /// The image's RAM range `load_addr..load_addr + size`.
    pub const fn span(&self) -> Span {
        Span {
            start: self.load_addr,
            end: self.load_addr.wrapping_add(self.size),
        }
    }

    /// Write the row's 32 bytes.
    pub fn encode(&self, out: &mut [u8; ROW_BYTES]) {
        *out = [0; ROW_BYTES];
        put32(out, 0x00, self.lba);
        put32(out, 0x04, self.size);
        put32(out, 0x08, self.load_addr);
        put32(out, 0x0c, self.checksum);
        put32(out, 0x10, self.core_hash);
        put32(out, 0x14, self.evicts);
        put32(out, 0x18, self.member_base);
        out[0x1c] = self.kind as u8;
        out[0x1d] = self.class.map_or(0, |class| class as u8);
        out[0x1e] = self.owner;
        out[0x1f] = self.cdda_delta;
    }

    /// Read a row; `None` for an unknown kind, an Overlay without a known class or a Program
    /// with one.
    pub fn decode(bytes: &[u8; ROW_BYTES]) -> Option<Self> {
        let kind = Kind::from_u8(bytes[0x1c])?;
        let class = match (kind, bytes[0x1d]) {
            (Kind::Program, 0) => None,
            (Kind::Overlay, byte) => Some(Class::from_u8(byte)?),
            (Kind::Program, _) => return None,
        };
        Some(ImageRow {
            lba: get32(bytes, 0x00),
            size: get32(bytes, 0x04),
            load_addr: get32(bytes, 0x08),
            checksum: get32(bytes, 0x0c),
            core_hash: get32(bytes, 0x10),
            evicts: get32(bytes, 0x14),
            member_base: get32(bytes, 0x18),
            kind,
            class,
            owner: bytes[0x1e],
            cdda_delta: bytes[0x1f],
        })
    }
}

/// Why a table could not be written or read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableError {
    /// The magic is `PSXSCN01`: a version 1 table.
    Version1,
    /// The magic is neither version's.
    Magic,
    /// The version field is not [`VERSION`].
    Version(u16),
    /// The row size or sector count is not this layout's.
    Shape,
    /// No scenes.
    NoScenes,
    /// Image count outside `1..=MAX_IMAGES`.
    ImageCount(usize),
    /// More than [`MAX_ROWS`] rows, scenes and images together.
    TooManyRows(usize),
    /// A scene's id is not its index.
    IdNotIndex {
        /// The row's index.
        index: u16,
        /// The id it carries.
        id: u16,
    },
    /// A scene names an image the table does not have.
    NoImage {
        /// The scene.
        scene: u16,
        /// The image index it names.
        image: u8,
    },
    /// The boot scene id names no scene.
    NoBootScene(u16),
    /// The output buffer or the table read is shorter than one sector.
    ShortBuffer,
    /// The rows' checksum is not the header's.
    Checksum,
    /// A compact copy's scene ids are not strictly ascending, or one names an image row that is
    /// absent from the copy.
    Compact,
}

impl TableError {
    /// The status the core returns for a table refused this way, `None` for an error only the
    /// build can make.
    pub const fn status(&self) -> Option<u32> {
        match self {
            TableError::Version1
            | TableError::Magic
            | TableError::Version(_)
            | TableError::Shape
            | TableError::Checksum
            | TableError::Compact => Some(status::TABLE),
            _ => None,
        }
    }
}

const fn check_counts(scenes: usize, images: usize) -> Result<(), TableError> {
    if scenes == 0 {
        return Err(TableError::NoScenes);
    }
    if images == 0 || images > MAX_IMAGES {
        return Err(TableError::ImageCount(images));
    }
    if scenes + images > MAX_ROWS {
        return Err(TableError::TooManyRows(scenes + images));
    }
    Ok(())
}

/// Word FNV over `bytes` read as little-endian words; a trailing partial
/// word is not hashed (every input here is whole words).
pub fn word_fnv(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for word in bytes.chunks_exact(4) {
        let word = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
        hash = (hash ^ word).wrapping_mul(0x0100_0193);
    }
    hash
}

/// Write the whole table into the first sector of `out` (zero-filled) and return its byte
/// length, one sector. Scene rows first, then image rows; the header's checksum is the word FNV
/// of the rows as written. Only the table's own consistency is checked here (counts, ids, image
/// indices, the boot scene); what the rows mean is the build's to check.
pub fn encode_table(
    scenes: &[SceneRow],
    images: &[ImageRow],
    boot_scene: u16,
    out: &mut [u8],
) -> Result<usize, TableError> {
    check_counts(scenes.len(), images.len())?;
    if boot_scene as usize >= scenes.len() {
        return Err(TableError::NoBootScene(boot_scene));
    }
    let out = out.get_mut(..SECTOR_BYTES).ok_or(TableError::ShortBuffer)?;
    out.fill(0);
    for (index, scene) in scenes.iter().enumerate() {
        if scene.id as usize != index {
            return Err(TableError::IdNotIndex {
                index: index as u16,
                id: scene.id,
            });
        }
        if scene.image as usize >= images.len() {
            return Err(TableError::NoImage {
                scene: scene.id,
                image: scene.image,
            });
        }
        let at = scene_row_offset(scene.id);
        let mut row = [0u8; ROW_BYTES];
        scene.encode(&mut row);
        out[at..at + ROW_BYTES].copy_from_slice(&row);
    }
    for (index, image) in images.iter().enumerate() {
        let at = image_row_offset(scenes.len() as u16, index as u8);
        let mut row = [0u8; ROW_BYTES];
        image.encode(&mut row);
        out[at..at + ROW_BYTES].copy_from_slice(&row);
    }
    let rows_end = HEADER_BYTES + (scenes.len() + images.len()) * ROW_BYTES;
    let header = Header {
        image_count: images.len() as u16,
        scene_count: scenes.len() as u16,
        boot_scene,
        checksum: word_fnv(&out[HEADER_BYTES..rows_end]),
        compact: false,
    };
    let mut head = [0u8; HEADER_BYTES];
    header.encode(&mut head);
    out[..HEADER_BYTES].copy_from_slice(&head);
    Ok(SECTOR_BYTES)
}

/// Read and check a whole table or a compact copy: the header, then the rows' checksum, and for
/// a compact copy that its scene ids ascend and name images it holds. `table` may be just the
/// copy's bytes ([`compact_bytes`]).
pub fn decode_header_checked(table: &[u8]) -> Result<Header, TableError> {
    let head: &[u8; HEADER_BYTES] = table
        .get(..HEADER_BYTES)
        .ok_or(TableError::ShortBuffer)?
        .try_into()
        .expect("64 bytes");
    let header = Header::decode(head)?;
    let rows = header.scene_count as usize + header.image_count as usize;
    let rows_end = HEADER_BYTES + rows * ROW_BYTES;
    let rows_bytes = table
        .get(HEADER_BYTES..rows_end)
        .ok_or(TableError::ShortBuffer)?;
    if word_fnv(rows_bytes) != header.checksum {
        return Err(TableError::Checksum);
    }
    if header.compact {
        let mut last: Option<u16> = None;
        for slot in 0..header.scene_count {
            let scene = scene_at(table, slot);
            let ascending = last.is_none_or(|id| scene.id > id);
            let held = u16::from(scene.image) < header.image_count
                && !image_row_absent(table, &header, scene.image);
            if !ascending || !held {
                return Err(TableError::Compact);
            }
            last = Some(scene.id);
        }
    }
    Ok(header)
}

fn scene_at(table: &[u8], slot: u16) -> SceneRow {
    let at = scene_row_offset(slot);
    SceneRow::decode(table[at..at + ROW_BYTES].try_into().expect("32 bytes"))
}

fn image_row_absent(table: &[u8], header: &Header, index: u8) -> bool {
    let at = image_row_offset(header.scene_count, index);
    table[at..at + ROW_BYTES].iter().all(|&b| b == 0)
}

/// Scene `id` of a table: row `id` of the disc's table, or the row whose id field is `id` in a
/// compact copy. `None` for a scene the table does not hold.
pub fn table_scene(table: &[u8], header: &Header, id: u16) -> Option<SceneRow> {
    if !header.compact {
        if id >= header.scene_count {
            return None;
        }
        let at = scene_row_offset(id);
        return Some(SceneRow::decode(
            table.get(at..at + ROW_BYTES)?.try_into().ok()?,
        ));
    }
    let end = scene_row_offset(header.scene_count);
    table.get(..end)?;
    (0..header.scene_count)
        .map(|slot| scene_at(table, slot))
        .find(|scene| scene.id == id)
}

/// Image row `index` (its mask bit) of a table, `None` past the image count, for a row a compact
/// copy leaves out (all zero) or for a row that does not decode.
pub fn table_image(table: &[u8], header: &Header, index: u8) -> Option<ImageRow> {
    if u16::from(index) >= header.image_count {
        return None;
    }
    let at = image_row_offset(header.scene_count, index);
    let bytes: &[u8; ROW_BYTES] = table.get(at..at + ROW_BYTES)?.try_into().ok()?;
    if header.compact && bytes.iter().all(|&b| b == 0) {
        return None;
    }
    ImageRow::decode(bytes)
}

/// Bytes of a compact copy holding `scenes` scene rows and the images below the highest bit of
/// `images`: `64 + 32 * (scenes + highest bit + 1)`.
pub const fn compact_bytes(scenes: usize, images: u32) -> usize {
    let image_rows = (32 - images.leading_zeros()) as usize;
    HEADER_BYTES + (scenes + image_rows) * ROW_BYTES
}

/// Write the compact RAM copy of the disc's `table` that holds only the scenes in `scenes` (ids,
/// any order) and the images whose bits are set in `images`, and return its length,
/// [`compact_bytes`]. Image rows keep their index (the mask bit), so rows below the highest
/// reached one that are not reached are written as zeros; the scenes keep their ids. Every
/// reached scene's image must be reached too.
pub fn encode_compact(
    table: &[u8],
    scenes: &[u16],
    images: u32,
    out: &mut [u8],
) -> Result<usize, TableError> {
    let full = decode_header_checked(table)?;
    if full.compact {
        return Err(TableError::Compact);
    }
    let image_rows = (32 - images.leading_zeros()) as u16;
    if image_rows > full.image_count {
        return Err(TableError::ImageCount(image_rows as usize));
    }
    let mut ids: [u16; MAX_ROWS] = [0; MAX_ROWS];
    let ids = {
        let n = scenes.len();
        if n > MAX_ROWS {
            return Err(TableError::TooManyRows(n));
        }
        ids[..n].copy_from_slice(scenes);
        ids[..n].sort_unstable();
        &ids[..n]
    };
    check_counts(ids.len(), image_rows as usize)?;
    let len = compact_bytes(ids.len(), images);
    let out = out.get_mut(..len).ok_or(TableError::ShortBuffer)?;
    out.fill(0);
    for (slot, &id) in ids.iter().enumerate() {
        if slot > 0 && ids[slot - 1] == id {
            return Err(TableError::Compact);
        }
        let scene = table_scene(table, &full, id).ok_or(TableError::Compact)?;
        if images & (1 << scene.image) == 0 {
            return Err(TableError::NoImage {
                scene: id,
                image: scene.image,
            });
        }
        let at = scene_row_offset(slot as u16);
        let src = scene_row_offset(id);
        out[at..at + ROW_BYTES].copy_from_slice(&table[src..src + ROW_BYTES]);
    }
    for index in 0..image_rows as u8 {
        if images & (1 << index) != 0 {
            let at = image_row_offset(ids.len() as u16, index);
            let src = image_row_offset(full.scene_count, index);
            out[at..at + ROW_BYTES].copy_from_slice(&table[src..src + ROW_BYTES]);
        }
    }
    let header = Header {
        image_count: image_rows,
        scene_count: ids.len() as u16,
        boot_scene: full.boot_scene,
        checksum: word_fnv(&out[HEADER_BYTES..len]),
        compact: true,
    };
    let mut head = [0u8; HEADER_BYTES];
    header.encode(&mut head);
    out[..HEADER_BYTES].copy_from_slice(&head);
    Ok(len)
}

/// What the prebuilt core (table version 1, handoff block version 1) still reads, kept so
/// `psx-core` builds unchanged until it moves to version 2. Nothing new should use it.
pub mod v1 {
    pub use super::{Kind, BOOT_EXE_LBA, TABLE_LBA};

    /// The version 1 magic.
    pub const MAGIC: [u8; 8] = super::MAGIC_V1;
    /// Table version 1.
    pub const VERSION: u16 = 1;
    /// Bytes in the version 1 header.
    pub const HEADER_BYTES: usize = 64;
    /// Bytes per version 1 row.
    pub const ROW_BYTES: usize = 64;
    /// Handoff block version the version 1 core writes.
    pub const BLOCK_VERSION: u16 = 1;

    /// Version 1 rows a table of `sectors` sectors has room for.
    pub const fn rows_capacity(sectors: u16) -> usize {
        (sectors as usize * super::SECTOR_BYTES - HEADER_BYTES) / ROW_BYTES
    }
}

fn put16(out: &mut [u8], at: usize, value: u16) {
    out[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut [u8], at: usize, value: u32) {
    out[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn get16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn get32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

#[cfg(test)]
mod tests;
