//! Stream a cooked map (`.hlm`) from the disc's WORLD.PAK (fixed LBA 1024)
//! into a RAM buffer. The CD-ROM DMA sector-read state machine, the pack
//! header/table parsing, and the HLZC/LZ4 decoder were extracted from this
//! file into the SDK's `psx_pack` crate (`psx_pack::cd` is the same
//! silicon-proven command sequence, repackaged from module statics into
//! `SectorReader`). What stays here is the hl-psx-specific pack-entry cache
//! plus thin wrappers preserving the game-facing API (`load_chunk`,
//! `decompress_in_place`).
//!
//! Pack layout: see `psx_pack`'s crate docs (28-byte "PSOXWPAK" header,
//! 24-byte table entries, sector-aligned payloads). hl-psx stores menu room N
//! as two chunks: resident world data in room_<2N>.psxc and temporary texture
//! data in room_<2N+1>.psxc.

#![allow(dead_code)]

#[cfg(target_arch = "mips")]
use psx_pack::cd::{SectorReader, SECTOR_WORDS};
#[cfg(target_arch = "mips")]
use psx_pack::SECTOR_BYTES;

/// WORLD.PAK's fixed start LBA (mkisopsx's default, pinned by the SDK).
pub const PACK_LBA: u32 = psx_pack::cd::WORLD_PACK_DEFAULT_LBA;

/// One successfully streamed chunk. `stored_len` is the number of bytes (and
/// therefore sectors) read from CD; `raw_len` is the usable payload length
/// after an optional HLZC decode. Keeping both prevents compressed chunks from
/// being reported as if their uncompressed bytes had crossed the CD bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLoad {
    pub stored_len: usize,
    pub raw_len: usize,
}

/// The one CD sector reader (it owns the "boot IRQs drained" flag and the
/// bounce buffer that unexpected DataReady sectors drain into) plus the
/// scratch sector every header scan and payload copy stages through.
/// Single-threaded polled MMIO, same contract as the statics they replace.
#[cfg(target_arch = "mips")]
static mut READER: SectorReader = SectorReader::new();
#[cfg(target_arch = "mips")]
static mut SECTOR_BUF: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

/// Parsed WORLD.PAK table, cached on the first `load_chunk` so later loads skip
/// re-reading + re-scanning the header. Loading a map streams many chunks;
/// without the cache each one seeks back to the pack LBA and rescans the whole
/// table -- the dominant cost of a map load. A larger pack falls back to the
/// SDK's on-disc scan (PACK_CACHE_LEN = -2).
///
/// Streaming and rendering never overlap on this single-threaded PS1 runtime,
/// so the table borrows the start of the 112 KiB primitive-packet arena. Keeping
/// a second resident table cost 5.25 KiB and pushed a full campaign recook over
/// the console's two-megabyte RAM limit. Ownership is bidirectional: the render
/// loop invalidates this table before drawing, and rebuilding the table must
/// invalidate the persistent world-packet cache before writing into its arena.
#[cfg(target_arch = "mips")]
const PACK_CACHE_MAX: usize = crate::room_budget::PACK_CACHE_ENTRIES;

#[cfg(target_arch = "mips")]
#[derive(Clone, Copy)]
struct CachedEntry {
    id: u32,
    sector_offset: u32,
    byte_size: u32,
}

#[cfg(target_arch = "mips")]
// -1 = not built yet, -2 = pack too big for the cache (use the disk scan),
// >= 0 = number of cached entries.
#[cfg(target_arch = "mips")]
static mut PACK_CACHE_LEN: i32 = -1;

#[cfg(target_arch = "mips")]
const _: () = assert!(
    core::mem::size_of::<crate::RenderPacketScratch>()
        >= PACK_CACHE_MAX * core::mem::size_of::<CachedEntry>()
);

/// Pointer to the temporary pack table overlaid on the render packet arena.
/// The arena is four-byte aligned and `CachedEntry` contains only `u32`s.
#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn pack_cache_ptr() -> *mut CachedEntry {
    core::ptr::addr_of_mut!(crate::PRIMITIVE_PACKETS).cast::<CachedEntry>()
}

/// Rendering is about to reuse the overlaid packet arena. The next disc stream
/// must rebuild the table before consulting it.
#[cfg(target_arch = "mips")]
#[inline(always)]
pub unsafe fn invalidate_cache() {
    PACK_CACHE_LEN = -1;
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
pub unsafe fn invalidate_cache() {}

/// Persistent chunk-entry mini-cache for the weapon-switch pump. The render
/// loop invalidates the arena-overlay table every frame, so without this a
/// mid-gameplay switch would re-read the pack header sectors before its
/// payload. Sixteen direct-mapped slots (id % 16) cover the contiguous
/// viewmodel chunk-id range; reads verify the id so colliding ids simply
/// miss to the normal lookup. Entries are immutable disc facts, so this
/// cache is never invalidated. Written only by [`prime_persistent_entries`]
/// and [`stream_begin`] -- routing every lookup through it would let room
/// chunk ids churn the sixteen slots.
#[cfg(target_arch = "mips")]
const PERSIST_ENTRIES: usize = 16;
#[cfg(target_arch = "mips")]
const PERSIST_NONE: u32 = u32::MAX;
#[cfg(target_arch = "mips")]
static mut PERSIST_CACHE: [CachedEntry; PERSIST_ENTRIES] = [CachedEntry {
    id: PERSIST_NONE,
    sector_offset: 0,
    byte_size: 0,
}; PERSIST_ENTRIES];

#[cfg(target_arch = "mips")]
#[inline]
unsafe fn persist_lookup(chunk_id: u32) -> Option<(u32, usize)> {
    let e = PERSIST_CACHE[(chunk_id as usize) % PERSIST_ENTRIES];
    if e.id == chunk_id {
        Some((e.sector_offset, e.byte_size as usize))
    } else {
        None
    }
}

#[cfg(target_arch = "mips")]
#[inline]
unsafe fn persist_store(id: u32, sector_offset: u32, byte_size: u32) {
    if id != PERSIST_NONE {
        PERSIST_CACHE[(id as usize) % PERSIST_ENTRIES] = CachedEntry {
            id,
            sector_offset,
            byte_size,
        };
    }
}

/// Resolve `count` contiguous chunk ids starting at `first_id` into the
/// persistent mini-cache. Call while the arena-overlay table is resident
/// (mid map load) so the fill is pure RAM scans; entries the table cannot
/// resolve are skipped and fall back to a lazy per-switch lookup.
#[cfg(target_arch = "mips")]
pub fn prime_persistent_entries(first_id: u32, count: usize) {
    unsafe {
        let rd = &mut *core::ptr::addr_of_mut!(READER);
        let scratch = &mut *core::ptr::addr_of_mut!(SECTOR_BUF);
        let mut i = 0usize;
        while i < count.min(PERSIST_ENTRIES) {
            let id = first_id + i as u32;
            if persist_lookup(id).is_none() {
                if let Some((sector_offset, byte_size)) = lookup_entry(rd, scratch, id) {
                    persist_store(id, sector_offset, byte_size as u32);
                }
            }
            i += 1;
        }
    }
}

#[cfg(not(target_arch = "mips"))]
pub fn prime_persistent_entries(_first_id: u32, _count: usize) {}

/// The scratch sector as bytes (little-endian DMA words are exactly the
/// on-disc byte order).
#[cfg(target_arch = "mips")]
fn sector_bytes(scratch: &[u32; SECTOR_WORDS]) -> &[u8] {
    // SAFETY: a [u32; N] viewed as its own bytes; alignment only shrinks.
    unsafe { core::slice::from_raw_parts(scratch.as_ptr() as *const u8, SECTOR_BYTES) }
}

/// Read the pack header once and cache every entry, or mark the cache disabled
/// (`-2`) if the pack has more chunks than the cache holds. The header sectors
/// stream in a single ReadN pass (entries are laid out sequentially), parsed
/// with the SDK's host-tested helpers; the entry that straddles two sectors is
/// stitched through a 24-byte buffer, same as `psx_pack::cd`'s own scan.
#[cfg(target_arch = "mips")]
unsafe fn build_pack_cache(rd: &mut SectorReader, scratch: &mut [u32; SECTOR_WORDS]) {
    use psx_pack::{entry_location, parse_entry_at, parse_header, ENTRY_BYTES};
    // The exact-view world cache deliberately retains packet payloads in this
    // arena across visual frames. A mid-game stream (for example a cold weapon
    // chunk) can rebuild the WORLD.PAK table between those frames. Invalidate
    // the retained packet metadata before the first table entry overwrites it;
    // otherwise the next cache hit submits table bytes as textured polygons.
    crate::invalidate_world_packet_cache();
    if !rd.prepare() || !rd.start_read(PACK_LBA) || !rd.read_sector(scratch) {
        rd.stop();
        return; // leave state -1 so a later call retries
    }
    let Some(header) = parse_header(sector_bytes(scratch)) else {
        rd.stop();
        return;
    };
    if header.chunk_count as usize > PACK_CACHE_MAX {
        rd.stop();
        PACK_CACHE_LEN = -2;
        return;
    }
    let mut n = 0usize;
    let mut cur_sector = 0u32;
    'scan: while (n as u32) < header.chunk_count {
        let (sector, within) = entry_location(n as u32);
        if sector >= header.header_sectors {
            break;
        }
        while cur_sector < sector {
            if !rd.read_sector(scratch) {
                break 'scan;
            }
            cur_sector += 1;
        }
        let entry = if within + ENTRY_BYTES <= SECTOR_BYTES {
            parse_entry_at(sector_bytes(scratch), within)
        } else {
            // Entry spans this sector and the next; stitch the 24 bytes together.
            if sector + 1 >= header.header_sectors {
                break;
            }
            let first = SECTOR_BYTES - within;
            let mut stitched = [0u8; ENTRY_BYTES];
            stitched[..first].copy_from_slice(&sector_bytes(scratch)[within..]);
            if !rd.read_sector(scratch) {
                break;
            }
            cur_sector += 1;
            stitched[first..].copy_from_slice(&sector_bytes(scratch)[..ENTRY_BYTES - first]);
            parse_entry_at(&stitched, 0)
        };
        let Some(e) = entry else { break };
        pack_cache_ptr().add(n).write(CachedEntry {
            id: e.chunk_id,
            sector_offset: e.sector_offset,
            byte_size: e.byte_size,
        });
        n += 1;
    }
    rd.stop();
    PACK_CACHE_LEN = n as i32;
}

/// Resolve `chunk_id` to (sector_offset, byte_size), building the table cache
/// on first use so later lookups touch no disc. A pack too big for the cache
/// falls back to the SDK's sector-by-sector table scan.
#[cfg(target_arch = "mips")]
#[optimize(size)]
unsafe fn lookup_entry(
    rd: &mut SectorReader,
    scratch: &mut [u32; SECTOR_WORDS],
    chunk_id: u32,
) -> Option<(u32, usize)> {
    // Primed viewmodel entries survive the per-frame arena invalidation, so
    // a weapon switch (pump or blocking glock fallback) skips the header scan.
    if let Some(hit) = persist_lookup(chunk_id) {
        return Some(hit);
    }
    if PACK_CACHE_LEN == -1 {
        build_pack_cache(rd, scratch);
    }
    if PACK_CACHE_LEN >= 0 {
        let n = PACK_CACHE_LEN as usize;
        let mut k = 0;
        while k < n {
            let e = pack_cache_ptr().add(k).read();
            if e.id == chunk_id {
                return Some((e.sector_offset, e.byte_size as usize));
            }
            k += 1;
        }
        return None;
    }
    psx_pack::cd::find_entry(rd, PACK_LBA, chunk_id, scratch)
        .map(|e| (e.sector_offset, e.byte_size as usize))
}

/// Stream chunk `chunk_id` from WORLD.PAK into `dst`. Returns the chunk's byte
/// size on success, or `None` (no disc / not found / too big / read error).
#[cfg(target_arch = "mips")]
/// Called every `PROGRESS_SECTORS` sectors of a blocking payload read, with
/// (sectors done, sectors needed). The loader is otherwise a 2.5-second freeze
/// with nothing to redraw its own screen: the CPU spends it waiting on the
/// drive, so handing that wait back is what lets the loading strip animate and
/// show real progress rather than ticking five times at stage boundaries.
static mut ON_SECTOR: Option<fn(usize, usize)> = None;
const PROGRESS_SECTORS: usize = 8;

pub fn set_sector_hook(hook: Option<fn(usize, usize)>) {
    unsafe { ON_SECTOR = hook };
}

#[inline]
fn report_sectors(done: usize, needed: usize) {
    if done % PROGRESS_SECTORS != 0 && done != needed {
        return;
    }
    if let Some(hook) = unsafe { ON_SECTOR } {
        hook(done, needed);
    }
}

pub fn load_chunk(chunk_id: u32, dst: &mut [u32]) -> Option<usize> {
    // The drive can hold one open pump session (weapon switch). Any blocking
    // load supersedes it: close the READN stream before issuing new commands.
    stream_abort();
    unsafe {
        let rd = &mut *core::ptr::addr_of_mut!(READER);
        let scratch = &mut *core::ptr::addr_of_mut!(SECTOR_BUF);
        let (sector_offset, byte_size) = lookup_entry(rd, scratch, chunk_id)?;
        if byte_size > dst.len() * 4 {
            return None;
        }
        // Payload: read the chunk's sectors into dst. This is the cached-entry
        // twin of psx_pack::cd::load_chunk's payload loop (the SDK version
        // rescans the table per call, which the cache exists to avoid).
        if !rd.prepare() || !rd.start_read(PACK_LBA + sector_offset) {
            rd.stop();
            return None;
        }
        let dst_ptr = dst.as_mut_ptr() as *mut u8;
        // Read only as many sectors as `byte_size` needs; the table's padded
        // sector_count could be garbage and looping on it would hang the loader.
        let needed = byte_size.div_ceil(SECTOR_BYTES);
        let mut s = 0usize;
        while s < needed {
            if !rd.read_sector(scratch) {
                rd.stop();
                return None;
            }
            let off = s * SECTOR_BYTES;
            let copy = byte_size.saturating_sub(off).min(SECTOR_BYTES);
            if copy > 0 {
                core::ptr::copy_nonoverlapping(
                    scratch.as_ptr() as *const u8,
                    dst_ptr.add(off),
                    copy,
                );
            }
            s += 1;
            report_sectors(s, needed);
        }
        rd.stop();
        Some(byte_size)
    }
}

#[cfg(not(target_arch = "mips"))]
pub fn load_chunk(_chunk_id: u32, _dst: &mut [u32]) -> Option<usize> {
    None
}

/// LZ4-wrapped chunk support ("HLZC" | u32 raw_len | LZ4 block), decoded in
/// place by the SDK's `psx_pack::decompress_hlzc_in_place` (extracted from
/// this file's original decoder).
///
/// `load_chunk` leaves the (compressed) payload at buf[0..loaded]; the SDK
/// stages it at the END of the buffer and decodes the LZ4 stream back to the
/// head. Safe in place: build.rs runs this same decoder over mkisopsx's exact
/// compressed streams, sizes MAP_WORDS to the maximum accepted capacity plus
/// a guard, and reruns after asset changes. The guest still checks every step
/// and fails cleanly instead of corrupting if a pack/build mismatch occurs.
///
/// Returns the decompressed length, `loaded` unchanged for non-HLZC chunks
/// (raw passthrough -- models/SFX/old packs), or 0 on a corrupt stream.
pub unsafe fn decompress_in_place(buf: &mut [u32], loaded: usize) -> usize {
    let bytes = core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 4);
    psx_pack::decompress_hlzc_in_place(bytes, loaded).unwrap_or(0)
}

/// Stream and, when framed, decompress one WORLD.PAK chunk in place. Raw
/// chunks remain a zero-copy passthrough, so every caller can use this path and
/// the disc packer is free to choose compression independently per payload.
#[optimize(size)]
pub fn load_chunk_decompressed(chunk_id: u32, dst: &mut [u32]) -> Option<ChunkLoad> {
    let stored_len = load_chunk(chunk_id, dst)?;
    let raw_len = unsafe { decompress_in_place(dst, stored_len) };
    if raw_len == 0 {
        None
    } else {
        Some(ChunkLoad {
            stored_len,
            raw_len,
        })
    }
}

// ---------------------------------------------------------------------------
// Incremental chunk stream: the weapon-switch pump (OC-02).
//
// One READN session stays open across pump calls WITHIN a single chunk, so
// each fixed-update tick drains the sectors the drive delivered while the
// previous frame rendered instead of blocking a whole tick on the payload
// (~38 sectors froze every render for ~15 vblanks). This is the engine room
// streamer's proven session shape: only cross-CHUNK open sessions were the
// M26 loss (mid-session realignment fights the FIFO model); sequential
// sectors of one chunk across ticks are exactly what it ships.
// ---------------------------------------------------------------------------

/// Result of one [`stream_pump`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamPump {
    /// No stream is in flight.
    Idle,
    /// Sectors are still landing; call again next tick.
    InFlight,
    /// The whole chunk landed (and decompressed when framed) this call.
    Done(ChunkLoad),
    /// The stream died (CD error, silent drive, corrupt frame); the session
    /// is closed and the destination contents are undefined.
    Failed,
}

/// Pump budgets, sized against the 20 Hz tick (~1.69 M CPU cycles). Up to
/// one acknowledged sector per call; at most ~24 k empty flag polls (~250 k cycles)
/// waiting on the in-cadence next sector, and an early yield when the tick
/// opens with no data at all. A stream that makes zero progress for 64
/// consecutive pumps (~3 s) fails into the blocking correctness retry.
#[cfg(target_arch = "mips")]
const PUMP_MAX_SECTORS: usize = 1;
#[cfg(target_arch = "mips")]
const PUMP_WAIT_POLLS: u32 = 24_576;
#[cfg(target_arch = "mips")]
const PUMP_IDLE_POLLS: u32 = 2_048;
#[cfg(target_arch = "mips")]
const PUMP_STALL_TICKS: u32 = 64;

#[cfg(target_arch = "mips")]
struct ChunkStream {
    active: bool,
    just_started: bool,
    dst: *mut u32,
    dst_words: usize,
    byte_size: usize,
    sectors_done: usize,
    stall_ticks: u32,
}

#[cfg(target_arch = "mips")]
static mut CHUNK_STREAM: ChunkStream = ChunkStream {
    active: false,
    just_started: false,
    dst: core::ptr::null_mut(),
    dst_words: 0,
    byte_size: 0,
    sectors_done: 0,
    stall_ticks: 0,
};

// Readiness must leave INT1 pending: SectorReader owns the subsequent DMA
// and acknowledgement, including the compiler-visible mutable bounce buffer.
#[cfg(target_arch = "mips")]
use psx_io::cdrom::poll_data_sector as try_sector_ready;

/// Open an incremental stream for `chunk_id`: resolve its pack entry (the
/// persistent mini-cache first, so no header re-scan), seek, start the READN
/// session, and leave it open for [`stream_pump`] to drain across ticks. Any
/// stream already in flight is superseded (aborted) -- never queued.
///
/// # Safety
/// `dst` must stay valid for `dst_words` words until the stream reports
/// `Done`/`Failed` or [`stream_abort`] runs; single-threaded polled MMIO
/// (same contract as `load_chunk`).
#[cfg(target_arch = "mips")]
pub unsafe fn stream_begin(chunk_id: u32, dst: *mut u32, dst_words: usize) -> bool {
    stream_abort();
    let rd = &mut *core::ptr::addr_of_mut!(READER);
    let scratch = &mut *core::ptr::addr_of_mut!(SECTOR_BUF);
    let Some((sector_offset, byte_size)) = lookup_entry(rd, scratch, chunk_id) else {
        return false;
    };
    // Every switch after a cold miss resolves from RAM, even once the render
    // loop has invalidated the arena-overlay table again.
    persist_store(chunk_id, sector_offset, byte_size as u32);
    if byte_size == 0 || byte_size > dst_words * 4 {
        return false;
    }
    if !rd.prepare() || !rd.start_read(PACK_LBA + sector_offset) {
        rd.stop();
        return false;
    }
    CHUNK_STREAM = ChunkStream {
        active: true,
        just_started: true,
        dst,
        dst_words,
        byte_size,
        sectors_done: 0,
        stall_ticks: 0,
    };
    true
}

#[cfg(not(target_arch = "mips"))]
pub unsafe fn stream_begin(_chunk_id: u32, _dst: *mut u32, _dst_words: usize) -> bool {
    false
}

/// Advance the open stream by the bounded per-tick budget. On the completion
/// call the session is paused and the payload decompressed in place (a raw
/// chunk passes through untouched). Never blocks longer than the poll budget.
///
/// # Safety
/// Same contract as [`stream_begin`] (whose `dst` this writes through).
#[cfg(target_arch = "mips")]
pub unsafe fn stream_pump() -> StreamPump {
    let st = &mut *core::ptr::addr_of_mut!(CHUNK_STREAM);
    if !st.active {
        return StreamPump::Idle;
    }
    // `start_read` acknowledges ReadN, but the first DataReady can race an
    // immediate pump in the same fixed-update tick (especially on a fast HLE
    // frontend). Let one normal render/update interval establish the sector
    // cadence. This costs no busy-wait and also mirrors the real drive's
    // unavoidable seek/rotation latency.
    if st.just_started {
        st.just_started = false;
        return StreamPump::InFlight;
    }
    let rd = &mut *core::ptr::addr_of_mut!(READER);
    let scratch = &mut *core::ptr::addr_of_mut!(SECTOR_BUF);
    let needed = st.byte_size.div_ceil(SECTOR_BYTES);
    let mut got = 0usize;
    let mut polls = 0u32;
    while got < PUMP_MAX_SECTORS && st.sectors_done < needed {
        match try_sector_ready() {
            Ok(true) => {
                // Readiness is already latched, so the SDK's bounded wait
                // returns immediately and performs the silicon-proven DMA +
                // acknowledgement sequence. Passing `&mut scratch` through
                // this opaque call also makes the external DMA mutation
                // visible to the optimizer before the ordinary copy below.
                if !rd.read_sector(scratch) {
                    rd.stop();
                    st.active = false;
                    return StreamPump::Failed;
                }
                let off = st.sectors_done * SECTOR_BYTES;
                let copy = st.byte_size.saturating_sub(off).min(SECTOR_BYTES);
                if copy > 0 {
                    core::ptr::copy_nonoverlapping(
                        scratch.as_ptr() as *const u8,
                        (st.dst as *mut u8).add(off),
                        copy,
                    );
                }
                st.sectors_done += 1;
                got += 1;
            }
            Ok(false) => {
                polls += 1;
                if (got == 0 && polls >= PUMP_IDLE_POLLS) || polls >= PUMP_WAIT_POLLS {
                    break;
                }
            }
            Err(_) => {
                rd.stop();
                st.active = false;
                return StreamPump::Failed;
            }
        }
    }
    if st.sectors_done >= needed {
        rd.stop();
        st.active = false;
        let buf = core::slice::from_raw_parts_mut(st.dst, st.dst_words);
        let raw_len = decompress_in_place(buf, st.byte_size);
        return if raw_len == 0 {
            StreamPump::Failed
        } else {
            StreamPump::Done(ChunkLoad {
                stored_len: st.byte_size,
                raw_len,
            })
        };
    }
    if got == 0 {
        st.stall_ticks += 1;
        if st.stall_ticks >= PUMP_STALL_TICKS {
            rd.stop();
            st.active = false;
            return StreamPump::Failed;
        }
    } else {
        st.stall_ticks = 0;
    }
    StreamPump::InFlight
}

#[cfg(not(target_arch = "mips"))]
pub unsafe fn stream_pump() -> StreamPump {
    StreamPump::Idle
}

/// Whether a pump session currently owns the drive (READN open). While true,
/// nothing else may issue CD commands -- CDDA restarts wait for completion.
#[cfg(target_arch = "mips")]
pub fn stream_active() -> bool {
    unsafe { CHUNK_STREAM.active }
}

#[cfg(not(target_arch = "mips"))]
pub fn stream_active() -> bool {
    false
}

/// Close an in-flight pump session (pause the drive, ack everything). Safe
/// to call when idle. Runs automatically ahead of every blocking load.
#[cfg(target_arch = "mips")]
pub fn stream_abort() {
    unsafe {
        if CHUNK_STREAM.active {
            CHUNK_STREAM.active = false;
            let rd = &mut *core::ptr::addr_of_mut!(READER);
            rd.stop();
        }
    }
}

#[cfg(not(target_arch = "mips"))]
pub fn stream_abort() {}
