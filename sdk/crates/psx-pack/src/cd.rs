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
        // The BIOS bracket (Setloc, SeekL waited to completion, SetMode, ReadN) the
        // resident core and the chain loader use, instead of the implicit seek of a
        // bare Setloc + ReadN that reader.rs warns corrupts sustained console
        // streams. The seek is bounded like a first sector (10 s, a drive spinning up).
        SectorReader::start_read_seek_first(self, lba, psx_io::cd::wait::FIRST_SECTOR_WAIT)
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
/// and copies it in whole sectors via `scratch`. Returns the chunk's exact
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
    let ok = pop_payload(rd, byte_size, scratch, dst);
    rd.stop();
    ok.then_some(byte_size)
}

/// Pop the next `byte_size.div_ceil(2048)` sectors of the running read
/// through `scratch` into `dst` (`byte_size <= dst.len() * 4`, checked by the
/// caller). `false` when a sector read fails; the read is left for the
/// caller to stop.
#[inline(always)]
fn pop_payload<T: Transport>(
    rd: &mut T,
    byte_size: usize,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> bool {
    let dst_ptr = dst.as_mut_ptr() as *mut u8;
    let needed = byte_size.div_ceil(SECTOR_BYTES);
    let mut s = 0usize;
    while s < needed {
        if !rd.read_sector(scratch) {
            return false;
        }
        let off = s * SECTOR_BYTES;
        let copy = byte_size.saturating_sub(off).min(SECTOR_BYTES);
        if copy > 0 {
            // SAFETY: `off + copy <= byte_size <= dst.len() * 4` (the
            // caller's check), so the write stays inside `dst`; `scratch` is
            // a whole sector and `copy <= SECTOR_BYTES`; distinct borrows.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    scratch.as_ptr() as *const u8,
                    dst_ptr.add(off),
                    copy,
                );
            }
        }
        s += 1;
    }
    true
}

/// A gap of up to this many sectors between two chunks of a run is read and
/// thrown away instead of ending the read: it costs less than the Pause,
/// seek and first-sector wait of a new read. The game runtime's UI.PAK
/// planner (`SEEK_BREAK_EVEN_SECTORS`) uses the same number.
pub const RUN_GAP_SECTORS: u32 = 8;

/// A run (one read start, then the Pause) grows to at most this many sectors;
/// the next chunk starts a new read. The resident core pauses between its
/// 256-sector chunks for the same reason, and a run keeps to it. A single
/// chunk longer than this is read in one read, as `load_chunk` reads it.
pub const RUN_MAX_SECTORS: u32 = 256;

/// One chunk of a [`load_run`]: its table entry and where its payload goes.
#[non_exhaustive]
pub struct RunItem<'a> {
    /// The chunk's entry, from [`find_entry`] or the caller's own table copy
    /// (only `sector_offset` and `byte_size` are used).
    pub entry: PackEntry,
    /// Its destination; holds at least `entry.byte_size` bytes.
    pub dst: &'a mut [u32],
}

impl<'a> RunItem<'a> {
    /// Chunk `entry`, to land in `dst`.
    #[must_use]
    pub fn new(entry: PackEntry, dst: &'a mut [u32]) -> Self {
        Self { entry, dst }
    }
}

/// Load several chunks with as few read starts as the disc layout allows:
/// `items` is sorted by disc position, chunks no more than
/// [`RUN_GAP_SECTORS`] apart share one read (the gap is read and thrown
/// away) up to [`RUN_MAX_SECTORS`], and each read ends with the reader's
/// Pause, as every load does. Returns the number of read starts, or `None`
/// when a chunk does not fit its `dst` (nothing read) or a read failed
/// (destinations of that read and after undefined).
///
/// It never issues a read while another runs: each run is one read start
/// over sectors that lie back to back on the disc, then the Pause, exactly
/// like one `load_chunk` of a chunk that long.
#[cfg(target_arch = "mips")]
pub fn load_run(
    rd: &mut SectorReader,
    pack_lba: u32,
    items: &mut [RunItem<'_>],
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<u32> {
    load_run_with(rd, pack_lba, items, scratch)
}

/// [`load_run`] over any [`Transport`].
pub(crate) fn load_run_with<T: Transport>(
    rd: &mut T,
    pack_lba: u32,
    items: &mut [RunItem<'_>],
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<u32> {
    let sectors = |e: &PackEntry| (e.byte_size as usize).div_ceil(SECTOR_BYTES) as u32;
    for item in items.iter() {
        if item.entry.byte_size as usize > item.dst.len() * 4 {
            return None;
        }
    }
    items.sort_unstable_by_key(|item| item.entry.sector_offset);
    let mut starts = 0u32;
    let mut i = 0usize;
    while i < items.len() {
        // The run: items[i..j], sectors first..end.
        let first = items[i].entry.sector_offset;
        let mut end = first + sectors(&items[i].entry);
        let mut j = i + 1;
        while j < items.len() {
            let at = items[j].entry.sector_offset;
            let next_end = at + sectors(&items[j].entry);
            if at < end || at - end > RUN_GAP_SECTORS || next_end - first > RUN_MAX_SECTORS {
                break;
            }
            end = next_end;
            j += 1;
        }
        if !rd.prepare() || !rd.start_read(pack_lba + first) {
            rd.stop();
            return None;
        }
        starts += 1;
        let mut head = first;
        let mut k = i;
        while k < j {
            let item = &mut items[k];
            while head < item.entry.sector_offset {
                if !rd.read_sector(scratch) {
                    rd.stop();
                    return None;
                }
                head += 1;
            }
            if !pop_payload(rd, item.entry.byte_size as usize, scratch, item.dst) {
                rd.stop();
                return None;
            }
            head += sectors(&item.entry);
            k += 1;
        }
        rd.stop();
        i = j;
    }
    Some(starts)
}

/// [`load_chunk`] through a [`Bounce`]: the table scan skips sectors the
/// bounce already holds ([`find_entry_bounced`]). The payload streams
/// through the bounce as through any scratch sector, so afterwards it holds
/// the payload's last sector, not the table's.
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
    let sectors = (entry.byte_size as usize).div_ceil(SECTOR_BYTES) as u32;
    let fits = entry.byte_size as usize <= dst.len() * 4;
    if fits && sectors > 0 {
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
    if loaded.is_some() && sectors > 0 {
        bounce.holds = pack_lba + entry.sector_offset + sectors - 1;
    }
    loaded
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
mod test_drive;

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
    // 900 table scans over a 300-entry pack: a sweep, native only (sdk/docs/MIRI.md).
    #[cfg_attr(miri, ignore)]
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

    /// Drive time of the reads `drive` recorded, from the residency
    /// simulator's silicon seek and read table (double speed, nominal seeks;
    /// a read starting where the last ended is charged no seek).
    fn drive_micros(drive: &ImageDrive) -> u64 {
        let mut model = psx_residency_sim::DriveModel::double_speed();
        drive
            .runs
            .iter()
            .map(|&(lba, n)| model.read(lba, n).0)
            .sum()
    }

    /// The WORLD.PAK of the raw disc image `PSX_PACK_MEASURE_DISC`, and the ids
    /// in `PSX_PACK_MEASURE_IDS` (comma separated).
    fn disc_pack() -> (Vec<u8>, Vec<u32>) {
        let path = std::env::var("PSX_PACK_MEASURE_DISC").expect("PSX_PACK_MEASURE_DISC");
        let ids = std::env::var("PSX_PACK_MEASURE_IDS")
            .expect("PSX_PACK_MEASURE_IDS")
            .split(',')
            .map(|s| s.trim().parse().expect("id"))
            .collect();
        let raw = std::fs::read(&path).expect("disc");
        let user = |lba: usize| &raw[lba * 2352 + 24..lba * 2352 + 24 + SECTOR_BYTES];
        let head = crate::parse_header(user(LBA as usize)).expect("pack header");
        let mut image = Vec::new();
        for s in 0..head.total_sectors as usize {
            image.extend_from_slice(user(LBA as usize + s));
        }
        (image, ids)
    }

    /// `load_chunk` against `load_chunk_bounced` for a real disc's id list.
    /// Lane measurement: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_bounce_against_scan_on_a_disc() {
        let (image, ids) = disc_pack();
        let header = crate::parse_header(&image).expect("header");
        let mut dst = vec![0u32; header.total_sectors as usize * SECTOR_WORDS];
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut scan = ImageDrive::new(image.clone(), LBA);
        for &id in &ids {
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
        let mut bounced = ImageDrive::new(image, LBA);
        let mut bounce = Bounce::new();
        for &id in &ids {
            load_bounced(&mut bounced, LBA, id, &mut bounce, &mut dst).expect("load");
        }
        std::println!(
            "{} chunks: scan starts {} sectors {} drive {} ms; bounced starts {} sectors {} drive {} ms",
            ids.len(),
            scan.seeks(),
            scan.sectors(),
            drive_micros(&scan) / 1000,
            bounced.seeks(),
            bounced.sectors(),
            drive_micros(&bounced) / 1000
        );
    }

    /// Load `ids` from `image` one chunk at a time and as runs, the entries
    /// found first (not counted); returns (read starts, sectors, drive micros)
    /// for each way, after checking the run put every chunk's bytes in place.
    fn compare_run(image: &[u8], ids: &[u32]) -> [(u32, u32, u64); 2] {
        let mut scratch = [0u32; SECTOR_WORDS];
        let mut probe = ImageDrive::new(image.to_vec(), LBA);
        let entries: Vec<PackEntry> = ids
            .iter()
            .map(|&id| scan_entry(&mut probe, LBA, id, &mut scratch).expect("entry"))
            .collect();
        let mut single = ImageDrive::new(image.to_vec(), LBA);
        let mut want: Vec<Vec<u32>> = Vec::new();
        for e in &entries {
            let mut dst = vec![0u32; (e.byte_size as usize).div_ceil(4)];
            stream_payload(
                &mut single,
                LBA,
                e.sector_offset,
                e.byte_size,
                &mut scratch,
                &mut dst,
            )
            .expect("load");
            want.push(dst);
        }
        let mut dsts: Vec<Vec<u32>> = entries
            .iter()
            .map(|e| vec![0u32; (e.byte_size as usize).div_ceil(4)])
            .collect();
        let mut run = ImageDrive::new(image.to_vec(), LBA);
        {
            let mut items: Vec<RunItem<'_>> = entries
                .iter()
                .zip(dsts.iter_mut())
                .map(|(&entry, dst)| RunItem {
                    entry,
                    dst: dst.as_mut_slice(),
                })
                .collect();
            let starts = load_run_with(&mut run, LBA, &mut items, &mut scratch).expect("run");
            assert_eq!(starts, run.seeks());
        }
        assert_eq!(dsts, want, "every chunk landed whole in its own buffer");
        [
            (single.seeks(), single.sectors(), drive_micros(&single)),
            (run.seeks(), run.sectors(), drive_micros(&run)),
        ]
    }

    #[test]
    // Packs of 40 and 100 chunks loaded both ways: a sweep, native only (sdk/docs/MIRI.md).
    #[cfg_attr(miri, ignore)]
    fn a_run_reads_neighbours_in_one_start_and_keeps_to_the_cap() {
        // 40 chunks of 3 sectors, back to back in the pack.
        let (image, _) = pack(40, |_| 3 * SECTOR_BYTES - 100);
        // Every other chunk: gaps of 3 sectors, read through.
        let ids: Vec<u32> = (0..40).step_by(2).collect();
        let [single, run] = compare_run(&image, &ids);
        assert_eq!(single.0, 20);
        assert_eq!(
            run.0, 1,
            "20 chunks x 3 + 19 gaps x 3 = 117 sectors, one read"
        );
        let all: Vec<u32> = (0..40).collect();
        assert_eq!(compare_run(&image, &all)[1].0, 1);
        // 100 chunks x 3 sectors = 300: capped at 256, so two reads.
        let (big, _) = pack(100, |_| 3 * SECTOR_BYTES);
        let all: Vec<u32> = (0..100).rev().collect();
        assert_eq!(
            compare_run(&big, &all)[1].0,
            2,
            "85 chunks (255 sectors) then 15"
        );
        // Far apart: no sharing.
        let (sparse, _) = pack(40, |i| if i % 2 == 0 { 2000 } else { 20 * SECTOR_BYTES });
        let ids: Vec<u32> = (0..40).step_by(2).collect();
        assert_eq!(compare_run(&sparse, &ids)[1].0, 20);
    }

    #[test]
    fn a_run_refuses_a_destination_too_small_before_reading() {
        let (image, _) = pack(3, |_| 5000);
        let mut drive = ImageDrive::new(image, LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        let e0 = scan_entry(&mut drive, LBA, 0, &mut scratch).expect("entry");
        let e1 = scan_entry(&mut drive, LBA, 1, &mut scratch).expect("entry");
        let reads = drive.seeks();
        let (mut a, mut b) = (vec![0u32; 1250], vec![0u32; 1000]);
        let mut items = [
            RunItem {
                entry: e0,
                dst: &mut a,
            },
            RunItem {
                entry: e1,
                dst: &mut b,
            },
        ];
        assert_eq!(
            load_run_with(&mut drive, LBA, &mut items, &mut scratch),
            None
        );
        assert_eq!(drive.seeks(), reads, "nothing read");
    }

    /// The run loader against per-chunk loads for a real disc's id list.
    /// Lane measurement: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn measure_run_against_per_chunk_on_a_disc() {
        let (image, ids) = disc_pack();
        let [single, run] = compare_run(&image, &ids);
        std::println!(
            "{} chunks: per chunk starts {} sectors {} drive {} ms; run starts {} sectors {} drive {} ms",
            ids.len(),
            single.0,
            single.1,
            single.2 / 1000,
            run.0,
            run.1,
            run.2 / 1000
        );
    }
}
