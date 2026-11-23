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
//! - the `SCENES.TBL` wire format ([`Header`], [`Row`], [`encode_table`]).
//!
//! It is the layout of the day-0 page, `CORE-ABI.md` (scene-core lane,
//! 2026-11-23), sections 0 to 3. Items that page marks provisional are marked
//! **provisional** here too and can still move after the P0 and P1 builds;
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
/// other call on the caller's stack. Cap 10,240 B, target 8,192 B; P0
/// reports where the build lands, and this constant is the one place that
/// moves if it does.
pub const CORE_WINDOW: Span = Span {
    start: 0x801f_d700,
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
/// a row whose `t_addr + t_size` is above it (stage 0x24).
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
}

/// The handoff block's fields, as byte offsets from [`HANDOFF_BLOCK`]`.start`.
pub mod block {
    /// `"PSCB"` read as a little-endian word.
    pub const MAGIC: u32 = 0x4243_5350;
    /// `!MAGIC`, the block's last word, written last.
    pub const MAGIC_INV: u32 = !MAGIC;
    /// Block version this layout describes.
    pub const VERSION: u16 = 1;
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
    /// u32 flags, see [`FLAG_CORE_ENTRY`] and [`FLAG_NESTED`].
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
}

/// User-data bytes per CD sector.
pub const SECTOR_BYTES: usize = 2048;

/// Member-relative LBA of `SCENES.TBL`: the file right after `SYSTEM.CNF`
/// (LBA 21). On a nested disc the core reads `TABLE_LBA + lba_offset`.
pub const TABLE_LBA: u32 = 22;

/// Member-relative LBA of the boot exe, right after a one-sector table.
pub const BOOT_EXE_LBA: u32 = TABLE_LBA + 1;

/// File name the disc tools give the table in the ISO 9660 root.
pub const TABLE_FILE_NAME: &str = "SCENES.TBL";

/// Identifies a scene table. A changed field meaning bumps it (`PSXSCN02`).
pub const MAGIC: [u8; 8] = *b"PSXSCN01";

/// Table version this layout describes. A new field in a reserved slot keeps it.
pub const VERSION: u16 = 1;

/// Bytes in the header, at offset 0.
pub const HEADER_BYTES: usize = 64;

/// Bytes per row. Rows start at [`HEADER_BYTES`], so sector 0 holds 31 and
/// every later sector 32, and no row crosses a sector.
pub const ROW_BYTES: usize = 64;

/// Sectors a table may take. P1 writes 1; up to 4 is **provisional**. A table
/// of more than one sector moves the boot exe past [`BOOT_EXE_LBA`].
pub const MAX_TABLE_SECTORS: u16 = 4;

/// Bytes reserved for a row's name.
pub const NAME_BYTES: usize = 20;

/// The `return_scene` of a row that has none.
pub const NO_SCENE: u16 = 0xffff;

/// Rows a table of `sectors` sectors has room for.
pub const fn rows_capacity(sectors: u16) -> usize {
    (sectors as usize * SECTOR_BYTES - HEADER_BYTES) / ROW_BYTES
}

/// Sectors a table of `rows` rows takes, or `None` past [`MAX_TABLE_SECTORS`].
pub const fn table_sectors_for(rows: usize) -> Option<u16> {
    let sectors = (HEADER_BYTES + rows * ROW_BYTES).div_ceil(SECTOR_BYTES);
    let sectors = if sectors == 0 { 1 } else { sectors };
    if sectors > MAX_TABLE_SECTORS as usize {
        None
    } else {
        Some(sectors as u16)
    }
}

/// Where row `index` sits: `(sector within the table, byte offset in it)`.
/// The core reads only that sector at a switch.
pub const fn row_location(index: u16) -> (u32, usize) {
    let byte = HEADER_BYTES + index as usize * ROW_BYTES;
    ((byte / SECTOR_BYTES) as u32, byte % SECTOR_BYTES)
}

/// What a row describes. **Provisional**: P1 implements [`Kind::Program`] only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    /// A whole PS-X EXE the core switches to.
    Program = 0,
    /// A fixed-address image a running base reads in.
    Image = 1,
    /// Plain data read through the core.
    Data = 2,
    /// A relocatable module.
    Module = 3,
    /// A program called and returned from.
    Call = 4,
    /// A boot-only program.
    Boot = 5,
}

impl Kind {
    /// The kind for a wire byte, `None` for one this layout does not know.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Kind::Program,
            1 => Kind::Image,
            2 => Kind::Data,
            3 => Kind::Module,
            4 => Kind::Call,
            5 => Kind::Boot,
            _ => return None,
        })
    }
}

/// Row flag bit 0: the boot scene shows the partner credit (**provisional**).
pub const ROW_FLAG_PARTNER_CREDIT: u8 = 1 << 0;

/// The table header, 64 bytes at offset 0.
///
/// ```text
/// 0x00  8  magic "PSXSCN01"
/// 0x08  2  table version, 1
/// 0x0A  2  row count
/// 0x0C  2  row bytes, 64
/// 0x0E  2  table sectors (1..4)
/// 0x10  2  boot scene id (the row of the exe at LBA 23)
/// 0x12  2  reserved, 0
/// 0x14  4  word FNV of all rows (row_count * 16 words)
/// 0x18 40  reserved, 0
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Table version.
    pub version: u16,
    /// Rows in the table.
    pub row_count: u16,
    /// Sectors the table takes, 1..=[`MAX_TABLE_SECTORS`].
    pub table_sectors: u16,
    /// Scene id of the boot exe's row.
    pub boot_scene: u16,
    /// Word FNV over every row's bytes.
    pub checksum: u32,
}

impl Header {
    /// Write the header's 64 bytes, reserved bytes zero.
    pub fn encode(&self, out: &mut [u8; HEADER_BYTES]) {
        *out = [0; HEADER_BYTES];
        out[0x00..0x08].copy_from_slice(&MAGIC);
        put16(out, 0x08, self.version);
        put16(out, 0x0a, self.row_count);
        put16(out, 0x0c, ROW_BYTES as u16);
        put16(out, 0x0e, self.table_sectors);
        put16(out, 0x10, self.boot_scene);
        put32(out, 0x14, self.checksum);
    }

    /// Read a header. `None` for a wrong magic, a row size other than
    /// [`ROW_BYTES`], a sector count outside 1..=[`MAX_TABLE_SECTORS`] or a
    /// row count the sectors cannot hold. The version is returned as read:
    /// the caller decides what it accepts.
    pub fn decode(bytes: &[u8; HEADER_BYTES]) -> Option<Self> {
        if bytes[0x00..0x08] != MAGIC || get16(bytes, 0x0c) != ROW_BYTES as u16 {
            return None;
        }
        let header = Header {
            version: get16(bytes, 0x08),
            row_count: get16(bytes, 0x0a),
            table_sectors: get16(bytes, 0x0e),
            boot_scene: get16(bytes, 0x10),
            checksum: get32(bytes, 0x14),
        };
        let sectors_ok = header.table_sectors >= 1 && header.table_sectors <= MAX_TABLE_SECTORS;
        if !sectors_ok || header.row_count as usize > rows_capacity(header.table_sectors) {
            return None;
        }
        Some(header)
    }
}

/// One scene, 64 bytes.
///
/// ```text
/// 0x00  2  scene id, equal to the row index
/// 0x02  1  kind (provisional)
/// 0x03  1  flags: bit 0 show partner credit (provisional)
/// 0x04  4  LBA of the file (the PS-X EXE header sector for a Program), member-relative
/// 0x08  4  sectors of the file, header included
/// 0x0C  4  t_addr
/// 0x10  4  t_size, a multiple of 2048
/// 0x14  4  entry PC
/// 0x18  4  payload checksum: word FNV over the t_size bytes
/// 0x1C  4  core hash the program carries
/// 0x20  4  initial SP
/// 0x24  2  return scene id, 0xFFFF none (provisional)
/// 0x26  1  read attempts, 0 means 3
/// 0x27  1  minimum handoff block version
/// 0x28  2  partner credit id, 0 none (provisional)
/// 0x2A  2  reserved, 0
/// 0x2C 20  name, ASCII, NUL-padded
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Row {
    /// Scene id; [`encode_table`] checks it equals the row's index.
    pub id: u16,
    /// What the row describes.
    pub kind: Kind,
    /// [`ROW_FLAG_PARTNER_CREDIT`] and room for later bits.
    pub flags: u8,
    /// Member-relative LBA of the file's first sector.
    pub lba: u32,
    /// Sectors of the file, header included.
    pub sectors: u32,
    /// Load address, equal to the PS-X EXE header's.
    pub t_addr: u32,
    /// Stored bytes after the header, a multiple of 2048.
    pub t_size: u32,
    /// Entry PC, equal to the header's.
    pub pc: u32,
    /// Word FNV over the `t_size` payload bytes.
    pub checksum: u32,
    /// Core hash the program carries.
    pub core_hash: u32,
    /// Initial SP, equal to the header's.
    pub sp: u32,
    /// Return scene id, [`NO_SCENE`] for none (**provisional**).
    pub return_scene: u16,
    /// Read attempts; 0 means 3.
    pub read_attempts: u8,
    /// Minimum handoff block version the program needs.
    pub min_block_version: u8,
    /// Partner credit id, 0 none (**provisional**).
    pub partner_credit: u16,
    /// ASCII name, NUL-padded.
    pub name: [u8; NAME_BYTES],
}

impl Row {
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

    /// Write the row's 64 bytes, reserved bytes zero.
    pub fn encode(&self, out: &mut [u8; ROW_BYTES]) {
        *out = [0; ROW_BYTES];
        put16(out, 0x00, self.id);
        out[0x02] = self.kind as u8;
        out[0x03] = self.flags;
        put32(out, 0x04, self.lba);
        put32(out, 0x08, self.sectors);
        put32(out, 0x0c, self.t_addr);
        put32(out, 0x10, self.t_size);
        put32(out, 0x14, self.pc);
        put32(out, 0x18, self.checksum);
        put32(out, 0x1c, self.core_hash);
        put32(out, 0x20, self.sp);
        put16(out, 0x24, self.return_scene);
        out[0x26] = self.read_attempts;
        out[0x27] = self.min_block_version;
        put16(out, 0x28, self.partner_credit);
        out[0x2c..0x2c + NAME_BYTES].copy_from_slice(&self.name);
    }

    /// Read a row; `None` for a kind this layout does not know.
    pub fn decode(bytes: &[u8; ROW_BYTES]) -> Option<Self> {
        let mut name = [0u8; NAME_BYTES];
        name.copy_from_slice(&bytes[0x2c..0x2c + NAME_BYTES]);
        Some(Row {
            id: get16(bytes, 0x00),
            kind: Kind::from_u8(bytes[0x02])?,
            flags: bytes[0x03],
            lba: get32(bytes, 0x04),
            sectors: get32(bytes, 0x08),
            t_addr: get32(bytes, 0x0c),
            t_size: get32(bytes, 0x10),
            pc: get32(bytes, 0x14),
            checksum: get32(bytes, 0x18),
            core_hash: get32(bytes, 0x1c),
            sp: get32(bytes, 0x20),
            return_scene: get16(bytes, 0x24),
            read_attempts: bytes[0x26],
            min_block_version: bytes[0x27],
            partner_credit: get16(bytes, 0x28),
            name,
        })
    }
}

/// Why [`encode_table`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableError {
    /// No rows.
    Empty,
    /// More rows than [`MAX_TABLE_SECTORS`] hold.
    TooManyRows(usize),
    /// A row's id is not its index.
    IdNotIndex {
        /// The row's index.
        index: u16,
        /// The id it carries.
        id: u16,
    },
    /// The boot scene id names no row.
    NoBootRow(u16),
    /// The output buffer is shorter than the table's sectors.
    ShortBuffer {
        /// Bytes the table needs.
        needed: usize,
    },
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

/// Write the whole table into `out` (zero-filled to its sectors) and return
/// its byte length, a whole number of sectors. The header's checksum is the
/// word FNV of the rows as written.
pub fn encode_table(rows: &[Row], boot_scene: u16, out: &mut [u8]) -> Result<usize, TableError> {
    if rows.is_empty() {
        return Err(TableError::Empty);
    }
    let sectors = table_sectors_for(rows.len()).ok_or(TableError::TooManyRows(rows.len()))?;
    if boot_scene as usize >= rows.len() {
        return Err(TableError::NoBootRow(boot_scene));
    }
    let bytes = sectors as usize * SECTOR_BYTES;
    if out.len() < bytes {
        return Err(TableError::ShortBuffer { needed: bytes });
    }
    let out = &mut out[..bytes];
    out.fill(0);
    for (index, row) in rows.iter().enumerate() {
        if row.id as usize != index {
            return Err(TableError::IdNotIndex {
                index: index as u16,
                id: row.id,
            });
        }
        let at = HEADER_BYTES + index * ROW_BYTES;
        let mut slot = [0u8; ROW_BYTES];
        row.encode(&mut slot);
        out[at..at + ROW_BYTES].copy_from_slice(&slot);
    }
    let rows_end = HEADER_BYTES + rows.len() * ROW_BYTES;
    let header = Header {
        version: VERSION,
        row_count: rows.len() as u16,
        table_sectors: sectors,
        boot_scene,
        checksum: word_fnv(&out[HEADER_BYTES..rows_end]),
    };
    let mut head = [0u8; HEADER_BYTES];
    header.encode(&mut head);
    out[..HEADER_BYTES].copy_from_slice(&head);
    Ok(bytes)
}

/// Read and check a whole table: the header, then the rows' checksum. `None`
/// for a bad header, a table shorter than its sectors, or a wrong checksum.
pub fn decode_header_checked(table: &[u8]) -> Option<Header> {
    let head: &[u8; HEADER_BYTES] = table.get(..HEADER_BYTES)?.try_into().ok()?;
    let header = Header::decode(head)?;
    if table.len() < header.table_sectors as usize * SECTOR_BYTES {
        return None;
    }
    let rows_end = HEADER_BYTES + header.row_count as usize * ROW_BYTES;
    (word_fnv(&table[HEADER_BYTES..rows_end]) == header.checksum).then_some(header)
}

/// Row `index` of a whole table, `None` past the row count or for an unknown kind.
pub fn table_row(table: &[u8], header: &Header, index: u16) -> Option<Row> {
    if index >= header.row_count {
        return None;
    }
    let at = HEADER_BYTES + index as usize * ROW_BYTES;
    Row::decode(table.get(at..at + ROW_BYTES)?.try_into().ok()?)
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
