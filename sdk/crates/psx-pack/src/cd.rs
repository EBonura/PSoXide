// SPDX-License-Identifier: GPL-2.0-or-later
//! The pack-table scan and chunk streaming, on top of the polled sector
//! reader.
//!
//! The CD controller has one driver, `psx_io::cd`: the `Cd` token, its
//! register steps, and [`SectorReader`], the polled reader that sequences
//! SetMode, Setloc, ReadN and Pause the way the pack loader has run on
//! silicon. This module holds what the pack format adds: finding a chunk's
//! table entry and copying its sectors into a caller's buffer.
//!
//! Everything that touches the drive is `cfg(target_arch = "mips")`; on the
//! host only the constants and the [`SectorReader`] re-export compile, so
//! `cargo test` keeps covering the pure pieces.
//!
//! # What loading does to the machine
//!
//! `load_chunk` and `find_entry` (guest only) bracket each read with
//! [`SectorReader::prepare`] and [`SectorReader::stop`]. Between the two,
//! `I_MASK` is VBlank-only (the reader polls the controller's own IRQ flags,
//! so a CD-ROM CPU interrupt with no handler cannot storm). `stop` puts the
//! previous mask back, so a caller's timer or controller interrupts survive a
//! chunk load. The reader needs the `Cd` token: `SectorReader::with_cd`.
//!
//! No caching happens at this layer. hl-psx's 512-entry table cache (skip
//! re-scanning the header on every chunk load) is a game-side optimization:
//! keep it in the game, keyed to its own pack, where the entry count and the
//! RAM budget are known.

// The loader bodies are generic over `Transport` so host tests can count their seeks; on a
// host build outside the tests nothing calls them.
#![cfg_attr(not(any(target_arch = "mips", test)), allow(dead_code))]

use crate::{entry_location, parse_entry_at, parse_header, PackEntry, ENTRY_BYTES, SECTOR_BYTES};

/// Moved to `psx_io::cd::reader`. Kept at this path for one stage so
/// `psx_pack::cd::SectorReader` still resolves; a re-export cannot carry a
/// deprecation.
pub use psx_io::cd::reader::{
    SectorReader, DIAG_CD_ERROR, DIAG_PARAM_STUCK, DIAG_SITE_READ, DIAG_TIMEOUT,
};

/// One CD sector's user data in 32-bit words (`SECTOR_BYTES / 4`).
pub const SECTOR_WORDS: usize = SECTOR_BYTES / 4;

const _: () = assert!(SECTOR_WORDS == psx_io::cd::reader::SECTOR_WORDS);

/// Where `mkisopsx` / the editor's embedded Play place `WORLD.PAK`: the pack's
/// first sector, as an absolute data-track LBA. Mirrors
/// `psx_iso::WORLD_PACK_DEFAULT_START_LBA` (the writer reserves a fixed boot
/// area so runtime LBAs never depend on the boot EXE size); a host test in
/// this crate asserts the two constants stay equal.
pub const WORLD_PACK_DEFAULT_LBA: u32 = 1024;

/// The four drive steps the pack loader runs, so the loader's sector and seek
/// pattern is one generic body: [`SectorReader`] on the console, a counting
/// disc image in the host tests. Each `start_read` is one Setloc + ReadN (one
/// seek); the body never chains reads without the `stop` in between.
pub(crate) trait Transport {
    /// [`SectorReader::prepare`].
    fn prepare(&mut self) -> bool;
    /// [`SectorReader::start_read`].
    fn start_read(&mut self, lba: u32) -> bool;
    /// [`SectorReader::read_sector`].
    fn read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> bool;
    /// [`SectorReader::stop`].
    fn stop(&mut self);
}

impl Transport for SectorReader {
    #[inline(always)]
    fn prepare(&mut self) -> bool {
        SectorReader::prepare(self)
    }
    #[inline(always)]
    fn start_read(&mut self, lba: u32) -> bool {
        SectorReader::start_read(self, lba)
    }
    #[inline(always)]
    fn read_sector(&mut self, buffer: &mut [u32; SECTOR_WORDS]) -> bool {
        SectorReader::read_sector(self, buffer)
    }
    #[inline(always)]
    fn stop(&mut self) {
        SectorReader::stop(self)
    }
}

/// The sector currently in `scratch`, as bytes (little-endian DMA words are
/// exactly the on-disc byte order).
fn scratch_bytes(scratch: &[u32; SECTOR_WORDS]) -> &[u8] {
    // SAFETY: [u32; N] reinterpreted as its own bytes; alignment shrinks.
    unsafe { core::slice::from_raw_parts(scratch.as_ptr() as *const u8, SECTOR_BYTES) }
}

/// No sector: what a bounce buffer holds before its first read and after
/// anything else wrote it.
const NO_SECTOR: u32 = u32::MAX;

/// Read pack header sector `sector` (relative to `pack_lba`) into `scratch`,
/// unless `*loaded` (an absolute LBA) says it is already there. Each miss is a
/// full prepare/seek/read/stop cycle, which is why scans track `loaded`
/// (ports hl-psx's `load_pack_header_sector`).
fn load_header_sector<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    sector: u32,
) -> bool {
    let lba = pack_lba + sector;
    if *loaded == lba {
        return true;
    }
    // Whatever the read leaves in `scratch`, it is not the old sector.
    *loaded = NO_SECTOR;
    if !rd.prepare() || !rd.start_read(lba) {
        rd.stop();
        return false;
    }
    let ok = rd.read_sector(scratch);
    rd.stop();
    if ok {
        *loaded = lba;
    }
    ok
}

/// Read table entry `index` while scanning, stitching an entry that straddles
/// two header sectors (ports hl-psx's `read_pack_entry`; the sector math and
/// byte parsing are the crate's host-tested [`entry_location`] /
/// [`parse_entry_at`]).
fn read_entry<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    header_sectors: u32,
    index: u32,
) -> Option<PackEntry> {
    let (sector, within) = entry_location(index);
    if sector >= header_sectors {
        return None;
    }
    if !load_header_sector(rd, pack_lba, scratch, loaded, sector) {
        return None;
    }
    if within + ENTRY_BYTES <= SECTOR_BYTES {
        parse_entry_at(scratch_bytes(scratch), within)
    } else {
        // Entry spans this sector and the next; stitch the 24 bytes together.
        if sector + 1 >= header_sectors {
            return None;
        }
        let first = SECTOR_BYTES - within;
        let mut stitched = [0u8; ENTRY_BYTES];
        stitched[..first].copy_from_slice(&scratch_bytes(scratch)[within..]);
        if !load_header_sector(rd, pack_lba, scratch, loaded, sector + 1) {
            return None;
        }
        stitched[first..].copy_from_slice(&scratch_bytes(scratch)[..ENTRY_BYTES - first]);
        parse_entry_at(&stitched, 0)
    }
}

/// The two header fields a scan needs, remembered for one pack.
#[derive(Clone, Copy)]
struct TableShape {
    pack_lba: u32,
    chunk_count: u32,
    header_sectors: u32,
}

const NO_TABLE: TableShape = TableShape {
    pack_lba: NO_SECTOR,
    chunk_count: 0,
    header_sectors: 0,
};

/// A one-sector bounce buffer that remembers which sector it holds, so the
/// `_bounced` loads skip reading a sector that is already in it, and the
/// pack's header numbers, so a lookup whose entry is not in the first table
/// sector does not read that sector again just for them.
///
/// 2,060 bytes: the sector plus 12. It assumes the disc does not change under
/// it. Anything else written into the buffer must go through
/// [`sector_mut`](Self::sector_mut), which forgets what it held.
pub struct Bounce {
    sector: [u32; SECTOR_WORDS],
    holds: u32,
    table: TableShape,
}

impl Bounce {
    /// An empty buffer that holds no sector. `const` so it can be a `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            sector: [0; SECTOR_WORDS],
            holds: NO_SECTOR,
            table: NO_TABLE,
        }
    }

    /// The absolute LBA of the sector in the buffer, if any.
    #[must_use]
    pub fn holds(&self) -> Option<u32> {
        (self.holds != NO_SECTOR).then_some(self.holds)
    }

    /// Forget the sector and the pack header numbers (after a disc change).
    pub fn forget(&mut self) {
        self.holds = NO_SECTOR;
        self.table = NO_TABLE;
    }

    /// The buffer for another use; it no longer counts as holding a sector.
    pub fn sector_mut(&mut self) -> &mut [u32; SECTOR_WORDS] {
        self.holds = NO_SECTOR;
        &mut self.sector
    }
}

impl Default for Bounce {
    fn default() -> Self {
        Self::new()
    }
}

/// Scan the pack table at `pack_lba` for `chunk_id` and return its entry
/// (including `byte_size` and the FNV `checksum` the writer stored).
///
/// Reads the header/table sectors through `scratch`, one at a time. `None`
/// on read failure, bad magic/version, or id not present. `I_MASK` is
/// VBlank-only while it reads and restored when it returns.
#[cfg(target_arch = "mips")]
pub fn find_entry(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<PackEntry> {
    scan_entry(rd, pack_lba, chunk_id, scratch)
}

/// [`find_entry`] through a [`Bounce`]: a table sector it already holds is
/// not read again, and neither is the first table sector when only the
/// header numbers were needed from it.
#[cfg(target_arch = "mips")]
pub fn find_entry_bounced(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    bounce: &mut Bounce,
) -> Option<PackEntry> {
    scan_with(
        rd,
        pack_lba,
        chunk_id,
        &mut bounce.sector,
        &mut bounce.holds,
        &mut bounce.table,
    )
}

/// [`find_entry`] over any [`Transport`].
pub(crate) fn scan_entry<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<PackEntry> {
    crate::lock::check(chunk_id);
    // The plain scan keeps its own body: routed through `scan_with` it would
    // carry the held-sector pass (about 540 B of .text) into every program
    // that never uses a `Bounce`.
    let mut loaded = NO_SECTOR;
    if !load_header_sector(rd, pack_lba, scratch, &mut loaded, 0) {
        return None;
    }
    let header = parse_header(scratch_bytes(scratch))?;
    let mut index = 0u32;
    while index < header.chunk_count {
        let entry = read_entry(
            rd,
            pack_lba,
            scratch,
            &mut loaded,
            header.header_sectors,
            index,
        )?;
        if entry.chunk_id == chunk_id {
            return Some(entry);
        }
        index += 1;
    }
    None
}

/// The first table entry that starts in header sector `sector`.
fn first_entry_in(sector: u32) -> u32 {
    let start = (sector as usize * SECTOR_BYTES).saturating_sub(crate::HEADER_BYTES);
    start.div_ceil(ENTRY_BYTES) as u32
}

/// The table scan with what `scratch` holds (`loaded`) and the pack's header
/// numbers (`table`) carried in and out.
fn scan_with<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    table: &mut TableShape,
) -> Option<PackEntry> {
    crate::lock::check(chunk_id);
    if table.pack_lba != pack_lba {
        if !load_header_sector(rd, pack_lba, scratch, loaded, 0) {
            return None;
        }
        let header = parse_header(scratch_bytes(scratch))?;
        *table = TableShape {
            pack_lba,
            chunk_count: header.chunk_count,
            header_sectors: header.header_sectors,
        };
    }
    // A table sector already in `scratch` is searched first, at no read; the
    // full pass after it skips the entries that lie wholly inside it.
    let held = loaded
        .checked_sub(pack_lba)
        .filter(|&s| *loaded != NO_SECTOR && s < table.header_sectors);
    if let Some(sector) = held {
        let mut index = first_entry_in(sector);
        while index < table.chunk_count {
            let (s, within) = entry_location(index);
            if s != sector || within + ENTRY_BYTES > SECTOR_BYTES {
                break;
            }
            let entry = parse_entry_at(scratch_bytes(scratch), within)?;
            if entry.chunk_id == chunk_id {
                return Some(entry);
            }
            index += 1;
        }
    }
    let mut index = 0u32;
    while index < table.chunk_count {
        let (s, within) = entry_location(index);
        if Some(s) == held && within + ENTRY_BYTES <= SECTOR_BYTES {
            index += 1;
            continue;
        }
        let entry = read_entry(rd, pack_lba, scratch, loaded, table.header_sectors, index)?;
        if entry.chunk_id == chunk_id {
            return Some(entry);
        }
        index += 1;
    }
    None
}

/// Stream chunk `chunk_id` from the pack at `pack_lba` into `dst`.
///
/// Scans the table for the entry (sector-by-sector through `scratch`,
/// straddle-safe), verifies the payload fits `dst`, then seeks to the payload
/// and pops whole sectors straight into `dst`; only a partial last sector goes
/// through `scratch`. Returns the chunk's exact
/// byte size, or `None` (no disc / not found / too big / read error). The
/// payload lands at `dst[0]` still compressed if the writer compressed it;
/// see [`load_chunk_decompressed`].
///
/// Reads only as many sectors as `byte_size` needs: the table's padded
/// `sector_count` could be garbage and looping on it would hang the loader.
///
/// `I_MASK` is VBlank-only while the reads run and back to what it was when
/// this returns. No caching; scan cost is linear in the table, so games
/// loading many chunks should keep their own id table (see module doc).
#[cfg(target_arch = "mips")]
pub fn load_chunk(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let entry = scan_entry(rd, pack_lba, chunk_id, scratch)?;
    stream_payload(
        rd,
        pack_lba,
        entry.sector_offset,
        entry.byte_size,
        scratch,
        dst,
    )
}

/// [`load_chunk`] through a [`Bounce`]: the table scan skips sectors the
/// bounce already holds ([`find_entry_bounced`]), and a payload with a partial
/// last sector leaves that sector in it. A payload of whole sectors never
/// touches the bounce, so the table sector stays for the next lookup.
#[cfg(target_arch = "mips")]
pub fn load_chunk_bounced(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    bounce: &mut Bounce,
    dst: &mut [u32],
) -> Option<usize> {
    load_bounced(rd, pack_lba, chunk_id, bounce, dst)
}

/// [`load_chunk_bounced`] over any [`Transport`].
pub(crate) fn load_bounced<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    chunk_id: u32,
    bounce: &mut Bounce,
    dst: &mut [u32],
) -> Option<usize> {
    let entry = scan_with(
        rd,
        pack_lba,
        chunk_id,
        &mut bounce.sector,
        &mut bounce.holds,
        &mut bounce.table,
    )?;
    let tail = !(entry.byte_size as usize).is_multiple_of(SECTOR_BYTES);
    let bounced_lba =
        pack_lba + entry.sector_offset + (entry.byte_size as usize / SECTOR_BYTES) as u32;
    let fits = entry.byte_size as usize <= dst.len() * 4;
    if tail && fits {
        bounce.holds = NO_SECTOR;
    }
    let loaded = stream_payload(
        rd,
        pack_lba,
        entry.sector_offset,
        entry.byte_size,
        &mut bounce.sector,
        dst,
    );
    if tail && loaded.is_some() {
        bounce.holds = bounced_lba;
    }
    loaded
}

/// Read `byte_size` bytes of payload starting `sector_offset` sectors into the
/// pack at `pack_lba`, into `dst`: one seek, then the sectors. `None` when it
/// does not fit `dst` or a read fails.
pub(crate) fn stream_payload<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    sector_offset: u32,
    byte_size: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let byte_size = byte_size as usize;
    if byte_size > dst.len() * 4 {
        return None;
    }
    if !rd.prepare() || !rd.start_read(pack_lba + sector_offset) {
        rd.stop();
        return None;
    }
    let needed = byte_size.div_ceil(SECTOR_BYTES);
    let mut s = 0usize;
    while s < needed {
        let off = s * SECTOR_BYTES;
        let left = byte_size - off;
        // A whole sector of payload is popped straight into its place in
        // `dst`; only a partial tail goes through `scratch`, so no full
        // sector is copied twice. Same single read, no second seek.
        let whole = if left >= SECTOR_BYTES {
            dst[s * SECTOR_WORDS..].first_chunk_mut::<SECTOR_WORDS>()
        } else {
            None
        };
        if let Some(slot) = whole {
            if !rd.read_sector(slot) {
                rd.stop();
                return None;
            }
        } else {
            if !rd.read_sector(scratch) {
                rd.stop();
                return None;
            }
            // SAFETY: `off + left == byte_size <= dst.len() * 4` (checked
            // above), so the write stays inside `dst`; `left < SECTOR_BYTES`
            // fits `scratch`; the two are distinct borrows.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    scratch.as_ptr() as *const u8,
                    (dst.as_mut_ptr() as *mut u8).add(off),
                    left,
                );
            }
        }
        s += 1;
    }
    rd.stop();
    Some(byte_size)
}

/// One chunk's place in the pack, as a resident index keeps it: the three
/// fields a load needs, 12 bytes against the table's 24 (the sector count is
/// derived from `byte_size`, the checksum is left on the disc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    /// The chunk's id.
    pub chunk_id: u32,
    /// Payload start, in sectors from the beginning of the pack.
    pub sector_offset: u32,
    /// Exact payload length in bytes.
    pub byte_size: u32,
}

/// RAM one [`IndexEntry`] costs.
pub const INDEX_ENTRY_BYTES: usize = 12;
const _: () = assert!(core::mem::size_of::<IndexEntry>() == INDEX_ENTRY_BYTES);

impl IndexEntry {
    /// A slot holding no chunk, for initialising index storage.
    pub const EMPTY: Self = Self {
        chunk_id: u32::MAX,
        sector_offset: 0,
        byte_size: 0,
    };
}

/// The entry for `chunk_id` in an index [`read_index`] filled, by a linear
/// scan (a level's slice is small; keep it sorted and binary-search it in the
/// caller if it is not).
#[must_use]
pub fn find_indexed(index: &[IndexEntry], chunk_id: u32) -> Option<IndexEntry> {
    index.iter().copied().find(|e| e.chunk_id == chunk_id)
}

/// Read the pack table at `pack_lba` once and keep the entries `keep` accepts,
/// in table order, in `out`; returns how many were kept.
///
/// One read start for the whole table (header sectors in one ReadN, ended by
/// the reader's Pause), so every later [`load_indexed`] costs only its
/// payload's seek instead of [`load_chunk`]'s table scan of one seek per
/// table sector. `keep` = `|_| true` holds the whole table (12 bytes an
/// entry); a level's id list holds just its slice.
///
/// `None` on a read failure, a bad header, a table shorter than its count, or
/// more accepted entries than `out` holds: an index is never partial.
#[cfg(target_arch = "mips")]
pub fn read_index(
    rd: &mut SectorReader,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    out: &mut [IndexEntry],
    keep: impl FnMut(u32) -> bool,
) -> Option<usize> {
    read_index_from(rd, pack_lba, scratch, out, keep)
}

/// [`read_index`] over any [`Transport`].
pub(crate) fn read_index_from<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    out: &mut [IndexEntry],
    mut keep: impl FnMut(u32) -> bool,
) -> Option<usize> {
    crate::lock::check(crate::lock::TABLE_KEY);
    if !rd.prepare() || !rd.start_read(pack_lba) || !rd.read_sector(scratch) {
        rd.stop();
        return None;
    }
    let kept = scan_table_stream(rd, scratch, out, &mut keep);
    rd.stop();
    kept
}

/// The table walk of [`read_index_from`] with sector 0 in `scratch` and the
/// read running: later sectors are taken in order from the same read.
fn scan_table_stream<T: Transport>(
    rd: &mut T,
    scratch: &mut [u32; SECTOR_WORDS],
    out: &mut [IndexEntry],
    keep: &mut impl FnMut(u32) -> bool,
) -> Option<usize> {
    let header = parse_header(scratch_bytes(scratch))?;
    let mut kept = 0usize;
    let mut current = 0u32;
    let mut index = 0u32;
    while index < header.chunk_count {
        let (sector, within) = entry_location(index);
        if sector >= header.header_sectors {
            return None;
        }
        while current < sector {
            if !rd.read_sector(scratch) {
                return None;
            }
            current += 1;
        }
        let entry = if within + ENTRY_BYTES <= SECTOR_BYTES {
            parse_entry_at(scratch_bytes(scratch), within)?
        } else {
            if sector + 1 >= header.header_sectors {
                return None;
            }
            let first = SECTOR_BYTES - within;
            let mut stitched = [0u8; ENTRY_BYTES];
            stitched[..first].copy_from_slice(&scratch_bytes(scratch)[within..]);
            if !rd.read_sector(scratch) {
                return None;
            }
            current += 1;
            stitched[first..].copy_from_slice(&scratch_bytes(scratch)[..ENTRY_BYTES - first]);
            parse_entry_at(&stitched, 0)?
        };
        if keep(entry.chunk_id) {
            *out.get_mut(kept)? = IndexEntry {
                chunk_id: entry.chunk_id,
                sector_offset: entry.sector_offset,
                byte_size: entry.byte_size,
            };
            kept += 1;
        }
        index += 1;
    }
    Some(kept)
}

/// Stream the chunk `entry` names (from [`read_index`]) into `dst`: one seek to
/// its payload and its sectors, no table read. Same result and `I_MASK`
/// behaviour as [`load_chunk`].
#[cfg(target_arch = "mips")]
pub fn load_indexed(
    rd: &mut SectorReader,
    pack_lba: u32,
    entry: &IndexEntry,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    crate::lock::check(entry.chunk_id);
    stream_payload(
        rd,
        pack_lba,
        entry.sector_offset,
        entry.byte_size,
        scratch,
        dst,
    )
}

/// [`load_indexed`], then [`crate::decompress_hlzc_in_place`]; as
/// [`load_chunk_decompressed`].
#[cfg(target_arch = "mips")]
pub fn load_indexed_decompressed(
    rd: &mut SectorReader,
    pack_lba: u32,
    entry: &IndexEntry,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let loaded = load_indexed(rd, pack_lba, entry, scratch, dst)?;
    // SAFETY: [u32] viewed as its own bytes for the in-place decoder.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, dst.len() * 4) };
    crate::decompress_hlzc_in_place(bytes, loaded)
}

/// [`load_chunk`], then [`crate::decompress_hlzc_in_place`] on the result.
///
/// Returns the chunk's RAW byte length: the decompressed size for an `HLZC`
/// chunk, the stored size for an uncompressed one (raw passthrough). `None`
/// on any load or decode failure. `dst` must hold the raw payload plus the
/// in-place LZ4 margin (see the decompressor's docs); a too-small buffer
/// fails cleanly.
#[cfg(target_arch = "mips")]
pub fn load_chunk_decompressed(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let loaded = load_chunk(rd, pack_lba, chunk_id, scratch, dst)?;
    // SAFETY: [u32] viewed as its own bytes for the in-place decoder.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, dst.len() * 4) };
    crate::decompress_hlzc_in_place(bytes, loaded)
}

#[cfg(test)]
pub(crate) mod test_drive;

#[cfg(test)]
mod tests {
    use super::test_drive::ImageDrive;
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    const LBA: u32 = WORLD_PACK_DEFAULT_LBA;

    /// `count` chunks with ids 0.., chunk `i` holding `size(i)` bytes of
    /// a pattern unique to it, packed by the real writer.
    fn pack(count: u32, size: fn(u32) -> usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let bodies: Vec<Vec<u8>> = (0..count)
            .map(|i| {
                (0..size(i))
                    .map(|k| (k as u32 ^ i.wrapping_mul(97)) as u8)
                    .collect()
            })
            .collect();
        let refs: Vec<(u32, &[u8])> = bodies
            .iter()
            .enumerate()
            .map(|(i, b)| (i as u32, b.as_slice()))
            .collect();
        (psx_iso::build_world_pack(&refs), bodies)
    }

    fn words_as_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn a_load_pays_one_seek_per_table_sector_it_scans_plus_the_payload() {
        // 200 entries: 28 + 200 * 24 = 4828 bytes, three table sectors.
        let (image, bodies) = pack(200, |i| 1000 + i as usize * 37);
        let mut drive = ImageDrive::new(image, LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut dst = vec![0u32; 4096];
        for (id, table_seeks) in [(0u32, 1u32), (100, 2), (199, 3)] {
            drive.reset_counts();
            let got = scan_entry(&mut drive, LBA, id, &mut scratch).and_then(|e| {
                stream_payload(
                    &mut drive,
                    LBA,
                    e.sector_offset,
                    e.byte_size,
                    &mut scratch,
                    &mut dst,
                )
            });
            let len = got.expect("loaded");
            assert_eq!(&words_as_bytes(&dst)[..len], bodies[id as usize].as_slice());
            assert_eq!(drive.seeks(), table_seeks + 1, "id {id}");
        }
    }

    #[test]
    fn whole_sectors_land_in_place_and_only_a_partial_tail_bounces() {
        // Sizes: exact sectors, one byte over, one byte under, under a sector.
        let sizes = [4096usize, 4097, 6143, 100, 2048];
        let (image, bodies) = pack(sizes.len() as u32, |i| {
            [4096usize, 4097, 6143, 100, 2048][i as usize]
        });
        let mut drive = ImageDrive::new(image, LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        for (id, &size) in sizes.iter().enumerate() {
            let e = scan_entry(&mut drive, LBA, id as u32, &mut scratch).expect("entry");
            // dst exactly the payload, rounded up to a word: the tail must
            // not be written past `byte_size` even when it would fit.
            let mut dst = vec![0xdead_beefu32; size.div_ceil(4) + 1];
            drive.reset_counts();
            drive.watch(&dst);
            let len = stream_payload(
                &mut drive,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            );
            assert_eq!(len, Some(size));
            let bytes = words_as_bytes(&dst);
            assert_eq!(&bytes[..size], bodies[id].as_slice(), "size {size}");
            assert_eq!(
                *dst.last().expect("guard"),
                0xdead_beef,
                "wrote past the payload"
            );
            assert_eq!(
                drive.seeks(),
                1,
                "one read start, no second seek for the tail"
            );
            assert_eq!(drive.direct, (size / SECTOR_BYTES) as u32, "size {size}");
            assert_eq!(
                drive.bounced,
                u32::from(size % SECTOR_BYTES != 0),
                "size {size}"
            );
        }
    }

    #[test]
    fn a_failed_sector_fails_the_load_whether_it_lands_in_place_or_bounces() {
        let (image, _) = pack(1, |_| 5000);
        for bad in 0..3u32 {
            let mut drive = ImageDrive::new(image.clone(), LBA);
            let mut scratch = [0u32; SECTOR_WORDS];
            let e = scan_entry(&mut drive, LBA, 0, &mut scratch).expect("entry");
            drive.fail_lba = Some(LBA + e.sector_offset + bad);
            let mut dst = vec![0u32; 2048];
            assert_eq!(
                stream_payload(
                    &mut drive,
                    LBA,
                    e.sector_offset,
                    e.byte_size,
                    &mut scratch,
                    &mut dst
                ),
                None
            );
        }
    }

    #[test]
    fn the_default_pack_lba_seeks_where_the_disc_math_says() {
        // The default pack LBA: 1024 + 150 lead-in sectors = 1174 = 15 * 75 + 49,
        // so Setloc gets 00:15:49. (The BCD arithmetic itself is psx-io's, and
        // tested there.)
        assert_eq!(
            psx_io::cd::lba_to_bcd_msf(WORLD_PACK_DEFAULT_LBA),
            [0x00, 0x15, 0x49]
        );
    }

    #[test]
    fn default_pack_lba_matches_the_writer() {
        // psx-iso (the pack writer) is a host-only crate, so the guest-side
        // constant is mirrored here; this pins them together.
        assert_eq!(
            WORLD_PACK_DEFAULT_LBA,
            psx_iso::WORLD_PACK_DEFAULT_START_LBA
        );
    }

    #[test]
    fn an_index_is_the_whole_table_read_in_one_start() {
        // 714 entries: the Half-Life disc's table (9 header sectors).
        let (image, _) = pack(714, |i| 100 + (i as usize * 131) % 9000);
        let header = crate::parse_header(&image).expect("header");
        let mut drive = ImageDrive::new(image.clone(), LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut index = vec![IndexEntry::EMPTY; 714];
        let kept = read_index_from(&mut drive, LBA, &mut scratch, &mut index, |_| true);
        assert_eq!(kept, Some(714));
        assert_eq!(drive.seeks(), 1);
        assert_eq!(drive.sectors(), header.header_sectors);
        for id in 0..714u32 {
            let e = crate::find_chunk(&image, &header, id).expect("on disc");
            let i = find_indexed(&index, id).expect("indexed");
            assert_eq!(
                (i.sector_offset, i.byte_size),
                (e.sector_offset, e.byte_size)
            );
        }
        assert_eq!(find_indexed(&index, 714), None);
    }

    #[test]
    fn a_level_slice_keeps_only_its_ids_and_refuses_to_overflow() {
        let (image, bodies) = pack(300, |i| 500 + i as usize);
        let mut drive = ImageDrive::new(image, LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        let level = |id: u32| id % 25 == 3;
        let mut slice = [IndexEntry::EMPTY; 12];
        assert_eq!(
            read_index_from(&mut drive, LBA, &mut scratch, &mut slice, level),
            Some(12)
        );
        let mut dst = vec![0u32; 256];
        drive.reset_counts();
        for e in &slice {
            let len = stream_payload(
                &mut drive,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            );
            let len = len.expect("loads");
            assert_eq!(
                &words_as_bytes(&dst)[..len],
                bodies[e.chunk_id as usize].as_slice()
            );
        }
        assert_eq!(drive.seeks(), 12, "one seek a chunk, no table reads");
        let mut short = [IndexEntry::EMPTY; 11];
        assert_eq!(
            read_index_from(&mut drive, LBA, &mut scratch, &mut short, level),
            None
        );
    }

    #[test]
    fn a_failed_table_sector_leaves_no_index() {
        let (image, _) = pack(200, |_| 10);
        let mut drive = ImageDrive::new(image, LBA);
        drive.fail_lba = Some(LBA + 2);
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut index = vec![IndexEntry::EMPTY; 200];
        assert_eq!(
            read_index_from(&mut drive, LBA, &mut scratch, &mut index, |_| true),
            None
        );
    }

    /// Drive time of the reads `drive` recorded, from the residency
    /// simulator's silicon seek and read table (double speed, nominal seeks;
    /// a read starting where the last ended is charged no seek, which is
    /// kind to the table scan, since our reader Pauses between them).
    fn drive_micros(drive: &ImageDrive) -> u64 {
        let mut model = psx_residency_sim::DriveModel::double_speed();
        drive
            .runs
            .iter()
            .map(|&(lba, n)| model.read(lba, n).0)
            .sum()
    }

    /// Seeks, sectors and drive time to load `ids` one after another: by table
    /// scan (`load_chunk`), through a [`Bounce`] (`load_chunk_bounced`), and
    /// from an index read once (`read_index` + `load_indexed`), on `image`.
    fn compare(image: &[u8], ids: &[u32]) -> [(u32, u32, u64); 3] {
        let mut scratch = [0u32; SECTOR_WORDS];
        let header = crate::parse_header(image).expect("header");
        let mut dst = vec![0u32; header.total_sectors as usize * SECTOR_WORDS];
        let mut scan = ImageDrive::new(image.to_vec(), LBA);
        for &id in ids {
            let e = scan_entry(&mut scan, LBA, id, &mut scratch).expect("entry");
            stream_payload(
                &mut scan,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            )
            .expect("load");
        }
        let mut bounced = ImageDrive::new(image.to_vec(), LBA);
        let mut bounce = Bounce::new();
        for &id in ids {
            load_bounced(&mut bounced, LBA, id, &mut bounce, &mut dst).expect("load");
        }
        let mut indexed = ImageDrive::new(image.to_vec(), LBA);
        let mut index = vec![IndexEntry::EMPTY; header.chunk_count as usize];
        read_index_from(&mut indexed, LBA, &mut scratch, &mut index, |_| true).expect("index");
        for &id in ids {
            let e = find_indexed(&index, id).expect("entry");
            stream_payload(
                &mut indexed,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            )
            .expect("load");
        }
        [
            (scan.seeks(), scan.sectors(), drive_micros(&scan)),
            (bounced.seeks(), bounced.sectors(), drive_micros(&bounced)),
            (indexed.seeks(), indexed.sectors(), drive_micros(&indexed)),
        ]
    }

    #[test]
    fn a_bounce_finds_every_entry_the_scan_finds_in_any_order() {
        // 300 entries over 4 table sectors, straddles included.
        let (image, bodies) = pack(300, |i| [2048usize, 3000, 100][i as usize % 3]);
        let mut drive = ImageDrive::new(image, LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut bounce = Bounce::new();
        let mut dst = vec![0u32; 1024];
        for k in 0..900u32 {
            let id = (k * 7919) % 301; // 300 is absent
            let want = scan_entry(&mut drive, LBA, id, &mut scratch);
            let got = scan_with(
                &mut drive,
                LBA,
                id,
                &mut bounce.sector,
                &mut bounce.holds,
                &mut bounce.table,
            );
            assert_eq!(got, want, "id {id}");
            if k % 5 == 0 && id < 300 {
                let len = load_bounced(&mut drive, LBA, id, &mut bounce, &mut dst).expect("load");
                assert_eq!(&words_as_bytes(&dst)[..len], bodies[id as usize].as_slice());
            }
        }
    }

    #[test]
    fn a_lookup_then_a_load_of_the_same_chunk_reads_the_table_once() {
        // Quake's and hello-pack's pattern: find_entry, then load_chunk.
        let (image, _) = pack(200, |_| 4096);
        for id in [0u32, 100, 199] {
            let mut plain = ImageDrive::new(image.clone(), LBA);
            let mut scratch = [0u32; SECTOR_WORDS];
            let mut dst = vec![0u32; 1024];
            scan_entry(&mut plain, LBA, id, &mut scratch).expect("entry");
            let e = scan_entry(&mut plain, LBA, id, &mut scratch).expect("entry");
            stream_payload(
                &mut plain,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            )
            .expect("load");
            let mut drive = ImageDrive::new(image.clone(), LBA);
            let mut bounce = Bounce::new();
            scan_with(
                &mut drive,
                LBA,
                id,
                &mut bounce.sector,
                &mut bounce.holds,
                &mut bounce.table,
            )
            .expect("entry");
            let first = drive.seeks();
            load_bounced(&mut drive, LBA, id, &mut bounce, &mut dst).expect("load");
            std::println!(
                "A5 find+load id {id}: plain seeks {} bounced seeks {}",
                plain.seeks(),
                drive.seeks()
            );
            assert_eq!(drive.seeks(), first + 1, "the load re-read no table sector");
        }
    }

    #[test]
    fn measure_index_against_table_scan_on_synthetic_hl_geometry() {
        // 714 entries as on the HL disc; 40 loads spread over the table.
        let (image, _) = pack(714, |i| 2000 + (i as usize * 7919) % 30000);
        let ids: Vec<u32> = (0..40).map(|k| k * 17 + 3).collect();
        let [scan, bounced, indexed] = compare(&image, &ids);
        std::println!("A3/A5 synthetic 714 entries, 40 loads: scan {scan:?} bounced {bounced:?} indexed {indexed:?}");
        assert!(indexed.0 < scan.0);
        assert!(indexed.2 < scan.2);
    }

    /// Reads the WORLD.PAK of a real disc image (raw 2352-byte sectors) named by
    /// `PSX_PACK_MEASURE_DISC` and compares the two load paths for every chunk in
    /// table order. Lane measurement only: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_index_against_table_scan_on_a_disc() {
        let path = std::env::var("PSX_PACK_MEASURE_DISC").expect("PSX_PACK_MEASURE_DISC");
        let raw = std::fs::read(&path).expect("disc");
        let user = |lba: usize| &raw[lba * 2352 + 24..lba * 2352 + 24 + SECTOR_BYTES];
        let head = crate::parse_header(user(LBA as usize)).expect("pack header");
        let mut image = Vec::new();
        for s in 0..head.total_sectors as usize {
            image.extend_from_slice(user(LBA as usize + s));
        }
        let ids: Vec<u32> = (0..head.chunk_count)
            .map(|i| {
                let (s, w) = entry_location(i);
                let at = s as usize * SECTOR_BYTES + w;
                crate::parse_entry_at(&image[at..], 0)
                    .expect("entry")
                    .chunk_id
            })
            .collect();
        let [scan, bounced, indexed] = compare(&image, &ids);
        std::println!(
            "{path}: {} entries, {} header sectors; every chunk once in table order: \
             scan seeks {} sectors {} drive {} ms; bounced seeks {} sectors {} drive {} ms; indexed seeks {} sectors {} drive {} ms; \
             index RAM {} B",
            head.chunk_count,
            head.header_sectors,
            scan.0,
            scan.1,
            scan.2 / 1000,
            bounced.0,
            bounced.1,
            bounced.2 / 1000,
            indexed.0,
            indexed.1,
            indexed.2 / 1000,
            head.chunk_count as usize * INDEX_ENTRY_BYTES
        );
    }
}
