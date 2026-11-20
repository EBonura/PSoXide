//! Cached WORLD.PAK reads and interrupt-serviced chunk sessions.
//!
//! The cache borrows a caller-owned render arena. Its invalidation contract is
//! explicit and statically dispatched; persistent entry metadata has const
//! capacity and never adds a second resident pack table.
//!
//! Every CD command goes through the caller's polled reader. A session (a
//! weapon model that streams in while the world renders) starts its read the
//! same way and hands only the popping of sectors to the CD interrupt
//! (`psx_cdstream::drain` on the console); the reader's Pause ends it.
//!
//! A read that fails is tried once more from a fresh seek, and the pack table
//! is cached only when every entry it declares was read, so a read that dies
//! part way through never leaves a short table that later lookups trust.
use core::marker::PhantomData;
use psx_pack::{cd::SECTOR_WORDS, SECTOR_BYTES};

/// Fixed pack placement used by the existing GoldSrc disc recipes.
pub const PACK_LBA: u32 = psx_pack::cd::WORLD_PACK_DEFAULT_LBA;
/// One cached entry occupies exactly three u32 words in the caller arena.
pub const CACHE_ENTRY_BYTES: usize = 12;
/// Reads of one table, payload or page before the load gives up: the first and
/// one retry from a fresh prepare and seek.
pub const READ_ATTEMPTS: u32 = 2;
/// VBlanks a session may go without a sector before it fails and the caller
/// falls back to a blocking load. The polled reader's first-sector bound
/// (`psx_io::cd::wait::FIRST_SECTOR_WAIT`, 10 s) at 60 VBlanks a second: the
/// console has taken 4.4 s to a first sector after the drive sat idle.
pub const STREAM_TIMEOUT_VBLANKS: u32 =
    psx_io::cd::wait::FIRST_SECTOR_WAIT.millis().div_ceil(1000) * 60;

/// Where an interrupt-serviced read stands, as [`ChunkReader::drain_poll`]
/// reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainPoll {
    /// Sectors are still arriving.
    Running,
    /// Every sector landed.
    Done,
    /// The drive reported an error, a sector's data never came, or no sector
    /// arrived within the timeout; nothing was started.
    Failed,
}

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
    unsafe fn find_entry(
        &mut self,
        pack_lba: u32,
        id: u32,
        scratch: &mut [u32; SECTOR_WORDS],
    ) -> Option<psx_pack::PackEntry>;
    /// Hand the sectors of the read [`Self::start_read`] just started to the
    /// CD interrupt: `sectors` land back to back at `dst`, of which only
    /// `byte_len` bytes are written. `false` starts nothing.
    /// # Safety
    /// `dst` is word aligned and writable for `byte_len` bytes, rounded up to
    /// a word, and nothing else touches it or the drive until
    /// [`Self::drain_poll`] stops reporting `Running` or [`Self::drain_cancel`].
    unsafe fn drain_start(
        &mut self,
        dst: *mut u32,
        sectors: u32,
        byte_len: u32,
        timeout_vblanks: u32,
    ) -> bool;
    /// Where the interrupt-serviced read stands.
    fn drain_poll(&mut self) -> DrainPoll;
    /// Take the sectors back from the interrupt. The drive keeps reading
    /// until [`Self::stop`].
    fn drain_cancel(&mut self);
}
#[cfg(target_arch = "mips")]
impl ChunkReader for psx_pack::cd::SectorReader {
    #[inline]
    unsafe fn prepare(&mut self) -> bool {
        Self::prepare(self)
    }
    #[inline]
    unsafe fn start_read(&mut self, lba: u32) -> bool {
        Self::start_read(self, lba)
    }
    #[inline]
    unsafe fn read_sector(&mut self, dst: &mut [u32; SECTOR_WORDS]) -> bool {
        Self::read_sector(self, dst)
    }
    #[inline]
    unsafe fn stop(&mut self) {
        Self::stop(self)
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
    #[inline]
    unsafe fn drain_start(
        &mut self,
        dst: *mut u32,
        sectors: u32,
        byte_len: u32,
        timeout_vblanks: u32,
    ) -> bool {
        // SAFETY: the trait's contract is the drain's.
        unsafe { psx_cdstream::drain::start(dst, sectors, byte_len, timeout_vblanks) }
    }
    #[inline]
    fn drain_poll(&mut self) -> DrainPoll {
        use psx_cdstream::drain::{poll, DrainState};
        match poll() {
            DrainState::Running { .. } => DrainPoll::Running,
            DrainState::Done => DrainPoll::Done,
            // Failed, or a drain nobody started (Idle): either way no sectors come.
            _ => DrainPoll::Failed,
        }
    }
    #[inline]
    fn drain_cancel(&mut self) {
        psx_cdstream::drain::cancel();
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
    ///
    /// The table is cached only when all `chunk_count` entries were read: a read
    /// failure, a header too short for the count, or an unparsable entry leaves
    /// the cache unbuilt (`-1`), so the next lookup reads it again instead of
    /// trusting a short table. Returns whether the cache state is final.
    unsafe fn build_pack_cache<R: ChunkReader>(
        &mut self,
        rd: &mut R,
        scratch: &mut [u32; SECTOR_WORDS],
    ) -> bool {
        use psx_pack::{entry_location, parse_entry_at, parse_header, ENTRY_BYTES};
        // The exact-view world cache deliberately retains packet payloads in this
        // arena across visual frames. A mid-game stream (for example a cold weapon
        // chunk) can rebuild the WORLD.PAK table between those frames. Invalidate
        // the retained packet metadata before the first table entry overwrites it;
        // otherwise the next cache hit submits table bytes as textured polygons.
        A::invalidate_render_cache();
        if !rd.prepare() || !rd.start_read(PACK_LBA) || !rd.read_sector(scratch) {
            rd.stop();
            return false; // leave state -1 so a later call retries
        }
        let Some(header) = parse_header(sector_bytes(scratch)) else {
            rd.stop();
            return false;
        };
        if header.chunk_count as usize > A::CAPACITY {
            rd.stop();
            self.len = -2;
            return true;
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
        if n as u32 != header.chunk_count {
            return false; // short: leave state -1, never cache part of a table
        }
        self.len = n as i32;
        true
    }

    /// Resolve `chunk_id` to (sector_offset, byte_size), building the table cache
    /// on first use so later lookups touch no disc. A pack too big for the cache,
    /// or a table that could not be read whole in [`READ_ATTEMPTS`] tries, falls
    /// back to the SDK's sector-by-sector table scan for this lookup.
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
        let mut attempt = 0;
        while self.len == -1 && attempt < READ_ATTEMPTS {
            self.build_pack_cache(rd, scratch);
            attempt += 1;
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
/// The open interrupt-serviced session.
struct ChunkStream {
    active: bool,
    dst: *mut u32,
    dst_words: usize,
    byte_size: usize,
}
const PROGRESS_SECTORS: usize = 8;

/// One exclusive reader/cache/session owner. The caller supplies its existing
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
                dst: core::ptr::null_mut(),
                dst_words: 0,
                byte_size: 0,
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
    /// Call during map loading; a cache miss may issue header reads, so an open
    /// session is closed first.
    pub fn prime_persistent_entries(&mut self, first_id: u32, count: usize) {
        self.stream_abort();
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

    /// Read chunk `chunk_id`'s stored bytes into `dst` and return their length.
    /// A read that fails part way is tried again from a fresh seek, up to
    /// [`READ_ATTEMPTS`] in all. `None` when the chunk is unknown, too big for
    /// `dst`, or every attempt failed.
    pub fn load_chunk(&mut self, chunk_id: u32, dst: &mut [u32]) -> Option<usize> {
        // The drive can hold one open session (weapon switch). Any blocking
        // load supersedes it: close the READN stream before issuing new commands.
        self.stream_abort();
        unsafe {
            let rd = &mut *core::ptr::addr_of_mut!(self.reader);
            let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
            let (sector_offset, byte_size) = self.cache.lookup_entry(rd, scratch, chunk_id)?;
            if byte_size > dst.len() * 4 {
                return None;
            }
            let dst_ptr = dst.as_mut_ptr() as *mut u8;
            let mut attempt = 0;
            while attempt < READ_ATTEMPTS {
                if read_payload(rd, scratch, sector_offset, byte_size, dst_ptr, self.hook) {
                    return Some(byte_size);
                }
                attempt += 1;
            }
            None
        }
    }

    /// Read whole sector `index` of chunk `chunk_id`'s payload into `dst`: one
    /// page of a paged table whose entry must be exactly `expected_bytes`, a
    /// whole number of sectors. Tried again from a fresh seek on failure, up to
    /// [`READ_ATTEMPTS`] in all. `false` when the entry is unknown, its size is
    /// not `expected_bytes`, the sector lies outside it, or every attempt failed.
    pub fn load_entry_sector(
        &mut self,
        chunk_id: u32,
        expected_bytes: usize,
        index: u32,
        dst: &mut [u32; SECTOR_WORDS],
    ) -> bool {
        self.stream_abort();
        unsafe {
            let rd = &mut *core::ptr::addr_of_mut!(self.reader);
            let scratch = &mut *core::ptr::addr_of_mut!(self.scratch);
            let Some((sector_offset, byte_size)) = self.cache.lookup_entry(rd, scratch, chunk_id)
            else {
                return false;
            };
            if byte_size != expected_bytes
                || byte_size == 0
                || !byte_size.is_multiple_of(SECTOR_BYTES)
                || (index as usize) >= byte_size / SECTOR_BYTES
            {
                return false;
            }
            let lba = PACK_LBA + sector_offset + index;
            let mut attempt = 0;
            while attempt < READ_ATTEMPTS {
                let ok = rd.prepare() && rd.start_read(lba) && rd.read_sector(dst);
                rd.stop();
                if ok {
                    return true;
                }
                attempt += 1;
            }
            false
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

    /// Open an interrupt-serviced session for `chunk_id`: resolve its pack entry
    /// (the persistent mini-cache first, so no header re-scan), start the read
    /// with the polled reader, and hand its sectors to the CD interrupt, which
    /// stores each one at `dst` as it arrives while the frame loop runs. Ask
    /// [`Self::stream_pump`] once a tick. Any session already open is superseded
    /// (aborted), never queued. `false` starts nothing: the entry is unknown, empty
    /// or too big for `dst`, or the drive refused the read.
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
        let sectors = byte_size.div_ceil(SECTOR_BYTES) as u32;
        if !rd.prepare()
            || !rd.start_read(PACK_LBA + sector_offset)
            || !rd.drain_start(dst, sectors, byte_size as u32, STREAM_TIMEOUT_VBLANKS)
        {
            rd.stop();
            return false;
        }
        self.stream = ChunkStream {
            active: true,
            dst,
            dst_words,
            byte_size,
        };
        true
    }

    /// Where the open session stands; never waits. On the call that sees the
    /// last sector land the read is paused and the payload decompressed in place
    /// (a raw chunk passes through untouched). `Failed` closes the session: the
    /// drive reported an error, a sector's data never came, no sector arrived
    /// for [`STREAM_TIMEOUT_VBLANKS`], or the payload did not decode.
    ///
    /// # Safety
    /// Same contract as [`Self::stream_begin`] (whose `dst` this writes through).
    pub unsafe fn stream_pump(&mut self) -> StreamPump {
        if !self.stream.active {
            return StreamPump::Idle;
        }
        match self.reader.drain_poll() {
            DrainPoll::Running => StreamPump::InFlight,
            DrainPoll::Done => {
                self.stream_abort();
                let st = &self.stream;
                let buf = core::slice::from_raw_parts_mut(st.dst, st.dst_words);
                match decompress_in_place(buf, st.byte_size) {
                    0 => StreamPump::Failed,
                    raw_len => StreamPump::Done(ChunkLoad {
                        stored_len: st.byte_size,
                        raw_len,
                    }),
                }
            }
            DrainPoll::Failed => {
                self.stream_abort();
                StreamPump::Failed
            }
        }
    }

    /// Whether a session currently owns the drive (READN open). While true,
    /// nothing else may issue CD commands.
    pub fn stream_active(&mut self) -> bool {
        self.stream.active
    }

    /// Close the open session: take the sectors back from the interrupt, then
    /// pause the drive with the reader's Pause. Safe to call when idle. Runs
    /// automatically ahead of every blocking load.
    pub fn stream_abort(&mut self) {
        if self.stream.active {
            self.stream.active = false;
            self.reader.drain_cancel();
            // SAFETY: the session owned the drive and has just given it up; this
            // owner serializes every CD operation.
            unsafe { self.reader.stop() };
        }
    }
}

/// Read a chunk's payload, `byte_size` bytes from `sector_offset` in the pack,
/// into `dst` through `scratch`, reporting progress to `hook`. One attempt: a
/// prepare, a seek and ReadN, the sectors, and the reader's Pause.
///
/// Reads only as many sectors as `byte_size` needs; the table's padded
/// `sector_count` could be garbage and looping on it would hang the loader.
///
/// # Safety
/// `dst` is writable for `byte_size` bytes; the caller exclusively owns the
/// transport.
unsafe fn read_payload<R: ChunkReader>(
    rd: &mut R,
    scratch: &mut [u32; SECTOR_WORDS],
    sector_offset: u32,
    byte_size: usize,
    dst: *mut u8,
    hook: Option<fn(usize, usize)>,
) -> bool {
    if !rd.prepare() || !rd.start_read(PACK_LBA + sector_offset) {
        rd.stop();
        return false;
    }
    let needed = byte_size.div_ceil(SECTOR_BYTES);
    let mut s = 0usize;
    while s < needed {
        if !rd.read_sector(scratch) {
            rd.stop();
            return false;
        }
        let off = s * SECTOR_BYTES;
        let copy = byte_size.saturating_sub(off).min(SECTOR_BYTES);
        if copy > 0 {
            core::ptr::copy_nonoverlapping(scratch.as_ptr() as *const u8, dst.add(off), copy);
        }
        s += 1;
        report_sectors(hook, s, needed);
    }
    rd.stop();
    true
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
