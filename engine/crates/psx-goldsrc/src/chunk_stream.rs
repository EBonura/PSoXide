//! Cached WORLD.PAK reads and bounded incremental chunk sessions.
//!
//! The cache borrows a caller-owned render arena. Its invalidation contract is
//! explicit and statically dispatched; persistent entry metadata has const
//! capacity and never adds a second resident pack table.
use core::marker::PhantomData;
use psx_pack::{cd::SECTOR_WORDS, SECTOR_BYTES};

/// Fixed pack placement used by the existing GoldSrc disc recipes.
pub const PACK_LBA: u32 = psx_pack::cd::WORLD_PACK_DEFAULT_LBA;
/// One cached entry occupies exactly three u32 words in the caller arena.
pub const CACHE_ENTRY_BYTES: usize = 12;

/// Overlay storage policy. No storage or function pointer is kept in the reader.
/// # Safety
/// `storage` must return four-byte-aligned writable storage for at least
/// `CAPACITY * CACHE_ENTRY_BYTES` bytes. It remains exclusively available during
/// cache access. Rendering calls `invalidate_cache` before reclaiming it;
/// `invalidate_render_cache` retires retained packet metadata before writes.
/// Callbacks must not reenter this stream instance.
pub unsafe trait PacketArena {
    const CAPACITY: usize;
    /// # Safety
    /// Caller holds the exclusive arena lease described by this trait.
    unsafe fn storage() -> *mut u8;
    /// # Safety
    /// Caller owns the renderer metadata and must not reenter the stream.
    unsafe fn invalidate_render_cache();
}

/// Statically dispatched sector transport. Implementations preserve command and
/// acknowledgement ordering; readiness must leave a ready sector pending.
pub trait ChunkReader {
    // Each operation requires exclusive, serialized transport ownership.
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn prepare(&mut self) -> bool;
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn start_read(&mut self, lba: u32) -> bool;
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn read_sector(&mut self, dst: &mut [u32; SECTOR_WORDS]) -> bool;
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn stop(&mut self);
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn ready(&mut self) -> Result<bool, psx_io::cdrom::SectorPollError>;
    /// # Safety
    /// Caller exclusively owns the transport and serializes all CD operations.
    unsafe fn find_entry(
        &mut self,
        pack_lba: u32,
        id: u32,
        scratch: &mut [u32; SECTOR_WORDS],
    ) -> Option<psx_pack::PackEntry>;
}
#[cfg(target_arch = "mips")]
impl ChunkReader for psx_pack::cd::SectorReader {
    #[inline]
    unsafe fn prepare(&mut self) -> bool {
        unsafe { Self::prepare(self) }
    }
    #[inline]
    unsafe fn start_read(&mut self, lba: u32) -> bool {
        unsafe { Self::start_read(self, lba) }
    }
    #[inline]
    unsafe fn read_sector(&mut self, dst: &mut [u32; SECTOR_WORDS]) -> bool {
        unsafe { Self::read_sector(self, dst) }
    }
    #[inline]
    unsafe fn stop(&mut self) {
        unsafe { Self::stop(self) }
    }
    #[inline]
    unsafe fn ready(&mut self) -> Result<bool, psx_io::cdrom::SectorPollError> {
        psx_io::cdrom::poll_data_sector()
    }
    #[inline]
    unsafe fn find_entry(
        &mut self,
        pack_lba: u32,
        id: u32,
        scratch: &mut [u32; SECTOR_WORDS],
    ) -> Option<psx_pack::PackEntry> {
        psx_pack::cd::find_entry(self, pack_lba, id, scratch)
    }
}

#[derive(Clone, Copy)]
struct CachedEntry {
    id: u32,
    sector_offset: u32,
    byte_size: u32,
}
const PERSIST_NONE: u32 = u32::MAX;
const EMPTY_ENTRY: CachedEntry = CachedEntry {
    id: PERSIST_NONE,
    sector_offset: 0,
    byte_size: 0,
};
struct EntryCache<A: PacketArena, const P: usize> {
    len: i32,
    persist: [CachedEntry; P],
    arena: PhantomData<A>,
}
impl<A: PacketArena, const P: usize> EntryCache<A, P> {
    const fn new() -> Self {
        Self {
            len: -1,
            persist: [EMPTY_ENTRY; P],
            arena: PhantomData,
        }
    }
    #[inline(always)]
    unsafe fn ptr() -> *mut CachedEntry {
        A::storage().cast()
    }

    #[inline]
    unsafe fn persist_lookup(&mut self, chunk_id: u32) -> Option<(u32, usize)> {
        let e = self.persist[(chunk_id as usize) % P];
        if e.id == chunk_id {
            Some((e.sector_offset, e.byte_size as usize))
        } else {
            None
        }
    }

    #[inline]
    unsafe fn persist_store(&mut self, id: u32, sector_offset: u32, byte_size: u32) {
        if id != PERSIST_NONE {
            self.persist[(id as usize) % P] = CachedEntry {
                id,
                sector_offset,
                byte_size,
            };
        }
    }

    /// Read the pack header once and cache every entry, or mark the cache disabled
    /// (`-2`) if the pack has more chunks than the cache holds. The header sectors
    /// stream in a single ReadN pass (entries are laid out sequentially), parsed
    /// with the SDK's host-tested helpers; the entry that straddles two sectors is
    /// stitched through a 24-byte buffer, same as `psx_pack::cd`'s own scan.
    unsafe fn build_pack_cache<R: ChunkReader>(
        &mut self,
        rd: &mut R,
        scratch: &mut [u32; SECTOR_WORDS],
    ) {
        use psx_pack::{entry_location, parse_entry_at, parse_header, ENTRY_BYTES};
        // The exact-view world cache deliberately retains packet payloads in this
        // arena across visual frames. A mid-game stream (for example a cold weapon
        // chunk) can rebuild the WORLD.PAK table between those frames. Invalidate
        // the retained packet metadata before the first table entry overwrites it;
        // otherwise the next cache hit submits table bytes as textured polygons.
        A::invalidate_render_cache();
        if !rd.prepare() || !rd.start_read(PACK_LBA) || !rd.read_sector(scratch) {
            rd.stop();
            return; // leave state -1 so a later call retries
        }
        let Some(header) = parse_header(sector_bytes(scratch)) else {
            rd.stop();
            return;
        };
        if header.chunk_count as usize > A::CAPACITY {
            rd.stop();
            self.len = -2;
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
            Self::ptr().add(n).write(CachedEntry {
                id: e.chunk_id,
                sector_offset: e.sector_offset,
                byte_size: e.byte_size,
            });
            n += 1;
        }
        rd.stop();
        self.len = n as i32;
    }

    /// Resolve `chunk_id` to (sector_offset, byte_size), building the table cache
    /// on first use so later lookups touch no disc. A pack too big for the cache
    /// falls back to the SDK's sector-by-sector table scan.
    #[optimize(size)]
    unsafe fn lookup_entry<R: ChunkReader>(
        &mut self,
        rd: &mut R,
        scratch: &mut [u32; SECTOR_WORDS],
        chunk_id: u32,
    ) -> Option<(u32, usize)> {
        // Primed viewmodel entries survive the per-frame arena invalidation, so
        // a weapon switch (pump or blocking glock fallback) skips the header scan.
        if let Some(hit) = self.persist_lookup(chunk_id) {
            return Some(hit);
        }
        if self.len == -1 {
            self.build_pack_cache(rd, scratch);
        }
        if self.len >= 0 {
            let n = self.len as usize;
            let mut k = 0;
            while k < n {
                let e = Self::ptr().add(k).read();
                if e.id == chunk_id {
                    return Some((e.sector_offset, e.byte_size as usize));
                }
                k += 1;
            }
            return None;
        }
        rd.find_entry(PACK_LBA, chunk_id, scratch)
            .map(|e| (e.sector_offset, e.byte_size as usize))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkLoad {
    pub stored_len: usize,
    pub raw_len: usize,
}
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
struct ChunkStream {
    active: bool,
    just_started: bool,
    dst: *mut u32,
    dst_words: usize,
    byte_size: usize,
    sectors_done: usize,
    stall_ticks: u32,
}
const PROGRESS_SECTORS: usize = 8;
const PUMP_MAX_SECTORS: usize = 1;
const PUMP_WAIT_POLLS: u32 = 24_576;
const PUMP_IDLE_POLLS: u32 = 2_048;
const PUMP_STALL_TICKS: u32 = 64;

/// One exclusive reader/cache/pump owner. The caller supplies its existing
/// sector reader; the arena is borrowed through `A`, never allocated here.
/// There is no automatic `Drop` abort: the owner must call `stream_abort`
/// before relinquishing the transport or an active destination buffer.
pub struct CachedStreamer<R: ChunkReader, A: PacketArena, const P: usize> {
    reader: R,
    scratch: [u32; SECTOR_WORDS],
    cache: EntryCache<A, P>,
    stream: ChunkStream,
    hook: Option<fn(usize, usize)>,
}
impl<R: ChunkReader, A: PacketArena, const P: usize> CachedStreamer<R, A, P> {
    /// Empty cache and closed session. `P` is the persistent direct-mapped capacity.
    /// # Safety
    /// The caller is the sole stream owner of this transport and arena while
    /// methods run. Rendering must invalidate the table before reclaiming storage,
    /// and callbacks must not reenter this owner.
    pub const unsafe fn new(reader: R) -> Self {
        assert!(P > 0);
        Self {
            reader,
            scratch: [0; SECTOR_WORDS],
            cache: EntryCache::new(),
            stream: ChunkStream {
                active: false,
                just_started: false,
                dst: core::ptr::null_mut(),
                dst_words: 0,
                byte_size: 0,
                sectors_done: 0,
                stall_ticks: 0,
            },
            hook: None,
        }
    }

    /// Rendering is about to reuse the overlaid packet arena. The next disc stream
    /// must rebuild the table before consulting it.
    #[inline(always)]
    /// # Safety
    /// Call only between stream operations, before rendering reuses the arena.
    pub unsafe fn invalidate_cache(&mut self) {
        self.cache.len = -1;
    }

    /// Resolve `count` contiguous chunk ids starting at `first_id` into the
    /// persistent mini-cache. Call while the arena-overlay table is resident
    /// (mid map load) so the fill is pure RAM scans; entries the table cannot
    /// resolve are skipped and fall back to a lazy per-switch lookup.
    /// Call only during map loading with no incremental session active; a cache
    /// miss may issue header reads and would interrupt that session.
    pub fn prime_persistent_entries(&mut self, first_id: u32, count: usize) {
        unsafe {
            let rd = &mut *core::ptr::addr_of_mut!(self.reader);
            let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
            let mut i = 0usize;
            while i < count.min(P) {
                let id = first_id + i as u32;
                if self.cache.persist_lookup(id).is_none() {
                    if let Some((sector_offset, byte_size)) =
                        self.cache.lookup_entry(rd, scratch, id)
                    {
                        self.cache
                            .persist_store(id, sector_offset, byte_size as u32);
                    }
                }
                i += 1;
            }
        }
    }

    pub fn set_sector_hook(&mut self, hook: Option<fn(usize, usize)>) {
        self.hook = hook;
    }

    pub fn load_chunk(&mut self, chunk_id: u32, dst: &mut [u32]) -> Option<usize> {
        // The drive can hold one open pump session (weapon switch). Any blocking
        // load supersedes it: close the READN stream before issuing new commands.
        self.stream_abort();
        unsafe {
            let rd = &mut *core::ptr::addr_of_mut!(self.reader);
            let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
            let (sector_offset, byte_size) = self.cache.lookup_entry(rd, scratch, chunk_id)?;
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
                report_sectors(self.hook, s, needed);
            }
            rd.stop();
            Some(byte_size)
        }
    }

    /// Stream and, when framed, decompress one WORLD.PAK chunk in place. Raw
    /// chunks remain a zero-copy passthrough, so every caller can use this path and
    /// the disc packer is free to choose compression independently per payload.
    #[optimize(size)]
    pub fn load_chunk_decompressed(&mut self, chunk_id: u32, dst: &mut [u32]) -> Option<ChunkLoad> {
        let stored_len = self.load_chunk(chunk_id, dst)?;
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

    /// Open an incremental stream for `chunk_id`: resolve its pack entry (the
    /// persistent mini-cache first, so no header re-scan), seek, start the READN
    /// session, and leave it open for [`stream_pump`] to drain across ticks. Any
    /// stream already in flight is superseded (aborted) -- never queued.
    ///
    /// # Safety
    /// `dst` must be four-byte aligned, writable and exclusively available for
    /// `dst_words` words until `Done`/`Failed` or [`Self::stream_abort`]. It must
    /// not overlap this owner's sector scratch or the caller's cache arena.
    /// Only the owner may access the destination while the stream is active.
    /// Calls are serialized with all other CD MMIO and may not reenter.
    pub unsafe fn stream_begin(&mut self, chunk_id: u32, dst: *mut u32, dst_words: usize) -> bool {
        self.stream_abort();
        let rd = &mut *core::ptr::addr_of_mut!(self.reader);
        let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
        let Some((sector_offset, byte_size)) = self.cache.lookup_entry(rd, scratch, chunk_id)
        else {
            return false;
        };
        // Every switch after a cold miss resolves from RAM, even once the render
        // loop has invalidated the arena-overlay table again.
        self.cache
            .persist_store(chunk_id, sector_offset, byte_size as u32);
        if byte_size == 0 || byte_size > dst_words * 4 {
            return false;
        }
        if !rd.prepare() || !rd.start_read(PACK_LBA + sector_offset) {
            rd.stop();
            return false;
        }
        self.stream = ChunkStream {
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

    /// Advance the open stream by the bounded per-tick budget. On the completion
    /// call the session is paused and the payload decompressed in place (a raw
    /// chunk passes through untouched). Never blocks longer than the poll budget.
    ///
    /// # Safety
    /// Same contract as [`stream_begin`] (whose `dst` this writes through).
    pub unsafe fn stream_pump(&mut self) -> StreamPump {
        let st = &mut *core::ptr::addr_of_mut!(self.stream);
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
        let rd = &mut *core::ptr::addr_of_mut!(self.reader);
        let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
        let needed = st.byte_size.div_ceil(SECTOR_BYTES);
        let mut got = 0usize;
        let mut polls = 0u32;
        while got < PUMP_MAX_SECTORS && st.sectors_done < needed {
            match rd.ready() {
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

    /// Whether a pump session currently owns the drive (READN open). While true,
    /// nothing else may issue CD commands -- CDDA restarts wait for completion.
    pub fn stream_active(&mut self) -> bool {
        self.stream.active
    }

    /// Close an in-flight pump session (pause the drive, ack everything). Safe
    /// to call when idle. Runs automatically ahead of every blocking load.
    pub fn stream_abort(&mut self) {
        unsafe {
            if self.stream.active {
                self.stream.active = false;
                let rd = &mut *core::ptr::addr_of_mut!(self.reader);
                rd.stop();
            }
        }
    }
}

/// The scratch sector as bytes (little-endian DMA words are exactly the
/// on-disc byte order).
fn sector_bytes(scratch: &[u32; SECTOR_WORDS]) -> &[u8] {
    // SAFETY: a [u32; N] viewed as its own bytes; alignment only shrinks.
    unsafe { core::slice::from_raw_parts(scratch.as_ptr() as *const u8, SECTOR_BYTES) }
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
/// # Safety
/// No other user may access this payload while its in-place decode runs.
pub unsafe fn decompress_in_place(buf: &mut [u32], loaded: usize) -> usize {
    let bytes = core::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 4);
    psx_pack::decompress_hlzc_in_place(bytes, loaded).unwrap_or(0)
}

#[inline]
fn report_sectors(hook: Option<fn(usize, usize)>, done: usize, needed: usize) {
    if !done.is_multiple_of(PROGRESS_SECTORS) && done != needed {
        return;
    }
    if let Some(hook) = hook {
        hook(done, needed);
    }
}
