//! CD-ROM streaming for the game runtime: the multi-room WORLD.PAK read job the
//! streamed-room scheduler drives, blocking UI.PAK chunk readers for menu images
//! and the sky, and the `cd-stream-benchmark` throughput probe.
//!
//! Every read goes through the shared `psx-cdstream` transport: an interrupt
//! handler pops one sector per CD interrupt (PIO, seek first, abortable at the
//! next sector) into a small ring of staging windows, and the functions here
//! consume the sectors in order, copying and checksumming them into their final
//! homes. The transport also arbitrates the drive with CD-DA music: a read
//! displaces music through [`psx_engine::cd_drive`], which pauses the track
//! and later resumes it where it stood. The staging ring is in `ring`; host
//! (non-MIPS) builds compile the same API with every read reporting
//! unsupported, and the unit tests drive it against a scripted drive.

#![cfg_attr(not(target_arch = "mips"), allow(dead_code))]

use psx_engine::telemetry;
use psx_level::LevelWorldPackEntryRecord;

// Streamed-world region reads: compiled for the `world-stream` feature (and
// for tests), so a build that does not stream carries none of it.
#[cfg(any(test, feature = "world-stream"))]
mod region;
mod ring;
#[cfg(any(test, feature = "world-stream"))]
pub use self::region::{RegionRead, RegionReadProgress};
#[cfg(target_arch = "mips")]
use self::ring::Hardware as Console;
#[cfg(not(target_arch = "mips"))]
use self::ring::Unavailable as Console;
use self::ring::{CdRun, CdStage, Poll, Transport};

#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_LBA: u32 = 992;
#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_SECTORS: usize = 32;
#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_MAGIC: [u8; 8] = *b"PSOXSTRM";
#[cfg(feature = "cd-stream-benchmark")]
const WORLD_PACK_MAGIC: [u8; 8] = *b"PSOXWPAK";
#[cfg(feature = "cd-stream-benchmark")]
const WORLD_PACK_MAX_SECTORS: u32 = 512;
/// One raw CD-ROM Mode 2 data sector. Stream residency uses this as its
/// allocation quantum so a sector can land directly in its final RAM page.
pub const SECTOR_BYTES: usize = 2048;

/// CD frames (sectors) per second at single speed. A PS1 spec figure: the
/// drive's sector clock is `psx_clock / 75`.
pub const CD_SECTORS_PER_SECOND_1X: usize = 75;

/// Sectors per second at the double speed the runtime reads at.
pub const CD_SECTORS_PER_SECOND_2X: usize = CD_SECTORS_PER_SECOND_1X * 2;

/// Display fields per second, NTSC. Rounded from 59.94 in the pessimistic
/// direction for a per-tick budget: assuming slightly more ticks per second
/// than reality understates the sectors each one may drain.
pub const DISPLAY_FIELDS_PER_SECOND: usize = 60;

/// A seek costs four single-speed sector periods, so at double speed it is
/// worth reading and discarding up to this many gap sectors rather than
/// reseeking past them. Beyond it, seeking is cheaper.
///
/// `4 * (1 / 75) / (1 / 150) == 8`.
pub const SEEK_BREAK_EVEN_SECTORS: usize = 8;

/// Sectors the drive can deliver between two background pump ticks.
///
/// The pump runs on alternate simulation ticks, so its period is two display
/// fields. At double speed that is `150 / 60 * 2 == 5` sectors. A pump's
/// budget is a DRAIN ceiling, not a request, so it must be at least this or
/// sectors that already arrived would be left for the next tick and the
/// streamer would fall behind the drive it is reading from.
pub const fn drive_sectors_per_background_tick() -> usize {
    // Integer arithmetic, rounded up, to avoid understating the delivery.
    (CD_SECTORS_PER_SECOND_2X * 2).div_ceil(DISPLAY_FIELDS_PER_SECOND)
}

/// Destination used by the incremental WORLD.PAK reader.
///
/// Keeping this interface sector-oriented lets the room streamer choose its
/// RAM layout (fixed rows, sector pages, or a future shared asset cache)
/// without teaching the CD state machine about that layout.
pub trait WorldChunkDestination {
    /// Writable byte capacity currently reserved for `slot`.
    fn slot_capacity_bytes(&self, slot: usize) -> usize;

    /// Copy one portion of a chunk into its reserved slot.
    fn write_chunk_bytes(&mut self, slot: usize, offset: usize, bytes: &[u8]) -> bool;
}
const SECTOR_WORDS: usize = SECTOR_BYTES / 4;
const FNV_OFFSET: u32 = 0x811C_9DC5;
const FNV_PRIME: u32 = 0x0100_0193;

const STATUS_OK: u32 = 0;
const STATUS_READ_ACK_TIMEOUT: u32 = 3;
const STATUS_DATA_TIMEOUT: u32 = 4;
const STATUS_CD_ERROR: u32 = 5;
#[cfg(feature = "cd-stream-benchmark")]
const STATUS_MAGIC_MISMATCH: u32 = 6;
const STATUS_CHECKSUM_MISMATCH: u32 = 7;
const STATUS_UNSUPPORTED: u32 = 8;
#[cfg(feature = "cd-stream-benchmark")]
const STATUS_HEADER_INVALID: u32 = 9;
const STATUS_CHUNK_NOT_FOUND: u32 = 10;
const STATUS_DEST_TOO_SMALL: u32 = 11;

/// Status value reported by chunk reads that completed and verified.
pub const ROOM_CHUNK_STATUS_OK: u32 = STATUS_OK;

/// How long a pump may keep finding no sector before the read is declared
/// dead, in VBlanks counted from the last sector that landed (or the group's
/// start).
///
/// This is a duration, not a count of pumps: pumps run once or twice per
/// simulation tick, and how many land inside a given wait depends on the
/// frame rate and on what else the tick does, so a pump count turns the same
/// drive latency into a pass or a failure depending on the build. Silicon
/// worst cases (hardware tests v1.28): the first sector after a music
/// hand-off 945 ms, a read on a stopped drive 1951 ms, a read issued while a
/// Stop is still spinning down 2721 ms
/// ([`psx_engine::cd_drive::MOTOR_RESTART_MS`]). The limit is the transport's
/// own no-progress watchdog, so the foreground never gives up before the
/// transport does.
pub(crate) const STALL_LIMIT_VBLANKS: u32 = psx_cdstream::Config::DEFAULT.timeout_vblanks;
const _: () = assert!(
    STALL_LIMIT_VBLANKS * 1000 / 60 >= 2 * psx_engine::cd_drive::MOTOR_RESTART_MS,
    "the stall limit must clear the slowest silicon drive transition twice over"
);
/// Times the job restarts a group after the run under it failed, before the
/// failure becomes the job's. A restart costs a seek and loses no sector (the
/// group resumes at the first sector not yet delivered).
const GROUP_RETRIES: u8 = 3;
/// How long a blocking read waits for one sector before giving up, in VBlanks:
/// the transport's own no-progress watchdog. It has to outlast a handoff from
/// music, after which the first sector takes about a second on a console
/// ([`psx_engine::cd_drive::FIRST_SECTOR_AFTER_AUDIO_MS`]) where the emulator
/// takes a few milliseconds. It also bounds a read whose request never started
/// (nothing owns the drive).
pub(crate) const BLOCKING_READ_DEADLINE_VBLANKS: u32 =
    psx_cdstream::Config::DEFAULT.timeout_vblanks;
const _: () = assert!(BLOCKING_READ_DEADLINE_VBLANKS > psx_engine::cd_drive::HANDOFF_VBLANKS);
/// Backstop for a blocking read when the display clock is not running: spins,
/// not time, so it is set far past any real wait rather than tuned.
const DATA_READY_BLOCKING_POLL_LIMIT: u32 = 50_000_000;
/// Spins to wait for a sector already on its way. One arrives every 6.7 ms at
/// double speed; this is well past that and far short of a hang. It is a
/// courtesy wait inside a pump; the first sector after a music handoff (about
/// a second on a console) is carried by [`STALL_LIMIT_VBLANKS`] instead.
const SECTOR_ARRIVAL_SPIN_LIMIT: u32 = 200_000;

/// Why a sector did not arrive.
#[derive(Clone, Copy)]
enum Stall {
    /// Nothing failed; it has not landed (yet).
    Slow,
    /// The transport gave up, with this `STATUS_*` code.
    Failed(u32),
}

impl Stall {
    fn status(self) -> u32 {
        match self {
            Stall::Slow => STATUS_DATA_TIMEOUT,
            Stall::Failed(status) => status,
        }
    }
}

/// Owned CD read state: the staging ring the transport writes into and the run
/// being read from it; consumers read each sector in place. The
/// game keeps one instance in its runtime arenas and threads it into every CD
/// read entry point. All bytes zero, so it lives in `.bss`.
pub struct CdController {
    stage: CdStage,
    run: CdRun,
}

impl CdController {
    /// All-zero state (link-time `.bss`-safe).
    pub const fn zeroed() -> Self {
        Self {
            stage: CdStage::zeroed(),
            run: CdRun::ZERO,
        }
    }

    /// Start reading `sectors` sectors from `lba`.
    fn begin_run<T: Transport>(&mut self, transport: &mut T, lba: u32, sectors: u32) {
        self.run.begin(transport, &mut self.stage, lba, sectors);
    }

    /// The next sector of the run, in `sector_buffer` when it has landed.
    fn next_sector<T: Transport>(&mut self, transport: &mut T) -> Poll {
        self.run.next_sector(transport, &mut self.stage)
    }

    /// Stop the run and wait for the staging RAM to be free.
    fn abort_run<T: Transport>(&mut self, transport: &mut T) {
        self.run.abort(transport);
    }

    /// Wait for the next sector, bounded; a blocking read's single failure code.
    fn wait_sector<T: Transport>(&mut self, transport: &mut T) -> Result<(), u32> {
        let start = transport.vblank_count();
        let mut spins = 0u32;
        loop {
            match self.try_sector(transport) {
                Err(Stall::Slow) => {
                    spins += 1;
                    if spins > DATA_READY_BLOCKING_POLL_LIMIT
                        || transport.vblank_count().wrapping_sub(start)
                            > BLOCKING_READ_DEADLINE_VBLANKS
                    {
                        return Err(Stall::Slow.status());
                    }
                    transport.service();
                }
                other => return other.map_err(Stall::status),
            }
        }
    }

    /// The next sector if it has landed, without waiting.
    fn try_sector<T: Transport>(&mut self, transport: &mut T) -> Result<(), Stall> {
        match self.next_sector(transport) {
            Poll::Sector => Ok(()),
            Poll::Pending => Err(Stall::Slow),
            Poll::Done => Err(Stall::Failed(STATUS_DATA_TIMEOUT)),
            Poll::Failed(status) => Err(Stall::Failed(status)),
        }
    }

    /// Wait up to `limit` foreground spins for the next sector.
    fn wait_sector_within<T: Transport>(
        &mut self,
        transport: &mut T,
        limit: u32,
    ) -> Result<(), Stall> {
        let mut spins = 0u32;
        loop {
            match self.try_sector(transport) {
                Err(Stall::Slow) => {
                    spins += 1;
                    if spins > limit {
                        return Err(Stall::Slow);
                    }
                    transport.service();
                }
                other => return other,
            }
        }
    }

    /// The sector the last successful read landed: [`SECTOR_BYTES`] readable
    /// bytes, valid until the next read call.
    fn sector_bytes(&self) -> *const u8 {
        self.run.sector_ptr(&self.stage)
    }
}

#[cfg(feature = "cd-stream-benchmark")]
#[derive(Clone, Copy)]
struct BenchResult {
    status: u32,
    bytes: u32,
    sectors: u32,
    steady_bytes: u32,
    steady_sectors: u32,
    polls: u32,
    checksum: u32,
    expected_checksum: u32,
    world_bytes: u32,
    world_sectors: u32,
    world_chunks: u32,
    world_checksum: u32,
    world_status: u32,
}

/// Outcome of one chunk read (or one read job's aggregate).
#[derive(Clone, Copy)]
pub struct RoomChunkLoadResult {
    /// [`ROOM_CHUNK_STATUS_OK`] or the first error encountered.
    pub status: u32,
    /// Verified payload bytes delivered.
    pub bytes: usize,
    /// CD sectors consumed (including any padding tail).
    pub sectors: u32,
}

/// One pack chunk's location and verification data, from the cooked TOC.
#[derive(Clone, Copy)]
pub struct WorldChunkInfo {
    /// First sector of the chunk, relative to the pack's start LBA.
    pub sector_offset: u32,
    /// Whole sectors the chunk occupies on disc.
    pub sector_count: u32,
    /// Unpadded payload byte count.
    pub byte_size: usize,
    /// Expected FNV checksum of the payload bytes.
    pub checksum: u32,
}

impl WorldChunkInfo {
    /// Zero-sector placeholder entry.
    pub const EMPTY: Self = Self {
        sector_offset: 0,
        sector_count: 0,
        byte_size: 0,
        checksum: 0,
    };
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorldRoomSlotsReadState {
    Idle,
    Ready,
    Reading,
    Done,
}

/// The single in-flight multi-room CD read: up to `N` chunks resolved
/// from the WORLD.PAK TOC, read as contiguous disc groups and committed
/// per chunk with byte-count and checksum verification. Owned by the
/// streamed-room scheduler; pumped incrementally by [`Self::poll_into`].
pub struct WorldRoomSlotsReadJob<const N: usize> {
    entries: [WorldChunkInfo; N],
    slot_indices: [usize; N],
    byte_counts: [usize; N],
    statuses: [u32; N],
    checksums: [u32; N],
    processed: [bool; N],
    group_entries: [bool; N],
    count: usize,
    valid_count: usize,
    processed_count: usize,
    group_start: u32,
    group_end: u32,
    sector_offset: u32,
    /// Whether the current wait for a sector has begun, and when (VBlank).
    stalled: bool,
    stall_since: u32,
    /// Restarts left for the current group.
    retries: u8,
    /// Whether this read may stay with the drive between sectors. See
    /// [`WorldRoomSlotsRead::set_wait_for_sectors`].
    wait_for_sectors: bool,
    world_pack_lba: u32,
    result: RoomChunkLoadResult,
    state: WorldRoomSlotsReadState,
}

impl<const N: usize> Default for WorldRoomSlotsReadJob<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> WorldRoomSlotsReadJob<N> {
    /// All-zero-bytes idle placeholder (for the scheduler's `zeroed`
    /// arena constructor); differs from [`Self::new`] only in the slot
    /// and checksum sentinels, which `count: 0` / `state: Idle` keep
    /// unread. `start` assigns `Self::new()` over it before any read.
    pub(crate) const fn zeroed() -> Self {
        Self {
            entries: [WorldChunkInfo::EMPTY; N],
            slot_indices: [0; N],
            byte_counts: [0; N],
            statuses: [STATUS_OK; N],
            checksums: [0; N],
            processed: [false; N],
            group_entries: [false; N],
            count: 0,
            valid_count: 0,
            processed_count: 0,
            group_start: 0,
            group_end: 0,
            sector_offset: 0,
            stalled: false,
            stall_since: 0,
            retries: 0,
            wait_for_sectors: false,
            world_pack_lba: 0,
            result: RoomChunkLoadResult {
                status: STATUS_OK,
                bytes: 0,
                sectors: 0,
            },
            state: WorldRoomSlotsReadState::Idle,
        }
    }

    /// Idle job with no chunks resolved.
    pub const fn new() -> Self {
        Self {
            entries: [WorldChunkInfo::EMPTY; N],
            slot_indices: [usize::MAX; N],
            byte_counts: [0; N],
            statuses: [STATUS_OK; N],
            checksums: [FNV_OFFSET; N],
            processed: [false; N],
            group_entries: [false; N],
            count: 0,
            valid_count: 0,
            processed_count: 0,
            group_start: 0,
            group_end: 0,
            sector_offset: 0,
            stalled: false,
            stall_since: 0,
            retries: 0,
            wait_for_sectors: false,
            world_pack_lba: 0,
            result: RoomChunkLoadResult {
                status: STATUS_OK,
                bytes: 0,
                sectors: 0,
            },
            state: WorldRoomSlotsReadState::Idle,
        }
    }

    /// Resolve `room_ids` against `toc` and arm the read. Chunks that are
    /// missing or exceed their pre-reserved destination fail immediately with a per-entry
    /// status; the rest stream on subsequent [`Self::poll_into`] calls.
    pub fn start(
        &mut self,
        world_pack_lba: u32,
        toc: &[LevelWorldPackEntryRecord],
        room_ids: &[u16],
        slot_indices: &[usize],
        slot_capacities: &[usize],
    ) {
        *self = Self::new();
        self.count = room_ids
            .len()
            .min(slot_indices.len())
            .min(slot_capacities.len())
            .min(N);
        self.world_pack_lba = world_pack_lba;
        if self.count == 0 {
            self.state = WorldRoomSlotsReadState::Done;
            return;
        }
        telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_LOADS, self.count as u32);

        let mut i = 0usize;
        while i < self.count {
            let dst_slot = slot_indices[i];
            self.slot_indices[i] = dst_slot;
            match world_pack_entry_from_toc(toc, room_ids[i] as u32) {
                Some(_) if dst_slot >= N => {
                    self.statuses[i] = STATUS_DEST_TOO_SMALL;
                    self.result.status =
                        first_status_error(self.result.status, STATUS_DEST_TOO_SMALL);
                }
                Some(entry) if entry.byte_size <= slot_capacities[i] => {
                    self.entries[i] = entry;
                    self.valid_count += 1;
                }
                Some(_) => {
                    self.statuses[i] = STATUS_DEST_TOO_SMALL;
                    self.result.status =
                        first_status_error(self.result.status, STATUS_DEST_TOO_SMALL);
                }
                None => {
                    self.statuses[i] = STATUS_CHUNK_NOT_FOUND;
                    self.result.status =
                        first_status_error(self.result.status, STATUS_CHUNK_NOT_FOUND);
                }
            }
            i += 1;
        }

        if self.valid_count == 0 {
            self.state = WorldRoomSlotsReadState::Done;
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
        } else {
            self.state = WorldRoomSlotsReadState::Ready;
        }
    }

    /// Drain at the drive's rate rather than the frame's, for a load with
    /// nothing to stay responsive for.
    ///
    /// The transport fills its staging ring on its own, so a pump that finds
    /// nothing landed can simply return. A loading screen that has nothing
    /// else to do waits instead for the sector already on its way (bounded),
    /// which keeps the ring drained at the drive's rate and the load as short
    /// as the drive allows.
    pub fn set_wait_for_sectors(&mut self, wait: bool) {
        self.wait_for_sectors = wait;
    }

    /// Advance the in-flight read, moving any completed sectors into `dst`.
    pub fn poll_into(
        &mut self,
        cd: &mut CdController,
        dst: &mut impl WorldChunkDestination,
        max_sectors: usize,
    ) -> RoomChunkLoadResult {
        self.poll_into_with(&mut Console, cd, dst, max_sectors)
    }

    fn poll_into_with<T: Transport>(
        &mut self,
        transport: &mut T,
        cd: &mut CdController,
        dst: &mut impl WorldChunkDestination,
        max_sectors: usize,
    ) -> RoomChunkLoadResult {
        if self.state == WorldRoomSlotsReadState::Idle
            || self.state == WorldRoomSlotsReadState::Done
            || max_sectors == 0
        {
            return self.result;
        }
        if !transport.available() {
            self.fail_all(STATUS_UNSUPPORTED);
            self.state = WorldRoomSlotsReadState::Done;
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
            return self.result;
        }

        telemetry::stage_begin(telemetry::stage::CD_ROOM_CHUNK_LOAD);
        let before_sectors = self.result.sectors;
        let mut sectors_this_poll = 0usize;
        while sectors_this_poll < max_sectors && self.state != WorldRoomSlotsReadState::Done {
            if self.state == WorldRoomSlotsReadState::Ready && !self.begin_next_group(transport, cd)
            {
                break;
            }
            if self.state != WorldRoomSlotsReadState::Reading {
                break;
            }

            let landed = if self.wait_for_sectors {
                cd.wait_sector_within(transport, SECTOR_ARRIVAL_SPIN_LIMIT)
            } else {
                cd.try_sector(transport)
            };
            match landed {
                Ok(()) => {}
                Err(Stall::Slow) => {
                    // Merely slow: the display clock decides when to give up,
                    // measured from the last sector (or the group's start).
                    let now = transport.vblank_count();
                    if !self.stalled {
                        self.stalled = true;
                        self.stall_since = now;
                    } else if now.wrapping_sub(self.stall_since) > STALL_LIMIT_VBLANKS {
                        self.fail_all(STATUS_DATA_TIMEOUT);
                        cd.abort_run(transport);
                        self.state = WorldRoomSlotsReadState::Done;
                    }
                    break;
                }
                Err(Stall::Failed(status)) => {
                    cd.abort_run(transport);
                    if self.retries > 0 {
                        // Drop the run and start the group again at the first
                        // sector not yet delivered (`begin_next_group` resumes).
                        self.retries -= 1;
                        self.stalled = false;
                        self.state = WorldRoomSlotsReadState::Ready;
                    } else {
                        self.fail_all(status);
                        self.state = WorldRoomSlotsReadState::Done;
                    }
                    break;
                }
            }
            self.stalled = false;
            self.retries = GROUP_RETRIES;
            // SAFETY: the sector buffer holds the sector the read above just
            // landed; SECTOR_BYTES are readable behind the pointer.
            unsafe {
                copy_window_info_sector(
                    cd.sector_bytes(),
                    self.sector_offset,
                    &self.entries[..self.count],
                    &self.slot_indices[..self.count],
                    dst,
                    &mut self.byte_counts,
                    &mut self.checksums,
                );
            }
            self.result.sectors = self.result.sectors.saturating_add(1);
            sectors_this_poll += 1;
            self.sector_offset = self.sector_offset.saturating_add(1);

            if self.sector_offset >= self.group_end {
                // The group's run is fully consumed; the transport pauses the
                // drive itself.
                self.mark_group_processed();
                if self.processed_count >= self.valid_count {
                    self.finish();
                } else {
                    self.state = WorldRoomSlotsReadState::Ready;
                }
            }
        }
        let sector_delta = self.result.sectors.saturating_sub(before_sectors);
        if sector_delta > 0 {
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_SECTORS, sector_delta);
        }
        if self.state == WorldRoomSlotsReadState::Done {
            telemetry::counter(
                telemetry::counter::CD_ROOM_CHUNK_BYTES,
                self.result.bytes as u32,
            );
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
        }
        telemetry::stage_end(telemetry::stage::CD_ROOM_CHUNK_LOAD);
        self.result
    }

    /// Whether the job still has groups armed or streaming.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            WorldRoomSlotsReadState::Ready | WorldRoomSlotsReadState::Reading
        )
    }

    /// Stop the read in flight (the transport pauses the drive at the next
    /// sector) and reset the job to idle.
    pub fn abort(&mut self, cd: &mut CdController) {
        self.abort_with(&mut Console, cd);
    }

    fn abort_with<T: Transport>(&mut self, transport: &mut T, cd: &mut CdController) {
        if self.is_active() {
            cd.abort_run(transport);
        }
        *self = Self::new();
    }

    /// Whether the job has delivered (or failed) every armed chunk.
    pub fn is_done(&self) -> bool {
        matches!(self.state, WorldRoomSlotsReadState::Done)
    }

    /// Delivered payload bytes per armed entry.
    pub fn byte_counts(&self) -> &[usize; N] {
        &self.byte_counts
    }

    /// Per-entry status ([`ROOM_CHUNK_STATUS_OK`] or the first error).
    pub fn statuses(&self) -> &[u32; N] {
        &self.statuses
    }

    /// Per-entry "delivered and checksum-verified" flags, computable
    /// before the whole job finishes (early per-chunk commit).
    pub fn completed_entries(&self) -> [bool; N] {
        let mut completed = [false; N];
        let mut i = 0usize;
        while i < self.count.min(N) {
            let entry = self.entries[i];
            completed[i] = self.statuses[i] == STATUS_OK
                && entry.byte_size > 0
                && self.byte_counts[i] == entry.byte_size
                && self.checksums[i] == entry.checksum;
            i += 1;
        }
        completed
    }

    fn fail_all(&mut self, status: u32) {
        let mut i = 0usize;
        while i < self.count {
            self.statuses[i] = status;
            i += 1;
        }
        self.result.status = status;
    }

    fn begin_next_group<T: Transport>(&mut self, transport: &mut T, cd: &mut CdController) -> bool {
        let resuming = self.sector_offset < self.group_end;
        let (read_start, group_end, group_entries) = if resuming {
            (self.sector_offset, self.group_end, self.group_entries)
        } else {
            let Some((group_start, group_end, group_entries)) = next_world_pack_info_read_group(
                &self.entries,
                &self.statuses,
                &self.processed,
                self.count,
            ) else {
                self.finish();
                return false;
            };
            (group_start, group_end, group_entries)
        };
        cd.begin_run(
            transport,
            self.world_pack_lba.saturating_add(read_start),
            group_end - read_start,
        );
        self.stalled = false;
        if !resuming {
            self.group_start = read_start;
            self.retries = GROUP_RETRIES;
        }
        self.group_end = group_end;
        self.sector_offset = read_start;
        self.group_entries = group_entries;
        self.state = WorldRoomSlotsReadState::Reading;
        true
    }

    fn mark_group_processed(&mut self) {
        let mut i = 0usize;
        while i < self.count.min(N) {
            if self.group_entries[i] && !self.processed[i] {
                self.processed[i] = true;
                self.processed_count += 1;
            }
            i += 1;
        }
        self.group_entries = [false; N];
    }

    fn finish(&mut self) {
        self.result.bytes = 0;
        let mut k = 0usize;
        while k < self.count.min(N) {
            let entry = self.entries[k];
            if self.statuses[k] == STATUS_OK {
                if self.byte_counts[k] != entry.byte_size {
                    self.statuses[k] = STATUS_DATA_TIMEOUT;
                } else if self.checksums[k] != entry.checksum {
                    self.statuses[k] = STATUS_CHECKSUM_MISMATCH;
                } else {
                    self.result.bytes = self.result.bytes.saturating_add(self.byte_counts[k]);
                }
                self.result.status = first_status_error(self.result.status, self.statuses[k]);
            }
            k += 1;
        }
        self.state = WorldRoomSlotsReadState::Done;
    }
}

/// Run the CD throughput probe (raw stream + WORLD.PAK walk) and emit
/// its results as telemetry counters.
#[cfg(feature = "cd-stream-benchmark")]
pub fn run_benchmark(cd: &mut CdController) {
    telemetry::stage_begin(telemetry::stage::CD_STREAM_BENCH);
    let result = run_benchmark_inner(cd);
    telemetry::counter(telemetry::counter::CD_STREAM_BENCH_BYTES, result.bytes);
    telemetry::counter(telemetry::counter::CD_STREAM_BENCH_SECTORS, result.sectors);
    telemetry::counter(telemetry::counter::CD_STREAM_BENCH_POLLS, result.polls);
    telemetry::counter(
        telemetry::counter::CD_STREAM_BENCH_CHECKSUM,
        result.checksum,
    );
    telemetry::counter(
        telemetry::counter::CD_STREAM_BENCH_EXPECTED_CHECKSUM,
        result.expected_checksum,
    );
    telemetry::counter(telemetry::counter::CD_STREAM_BENCH_STATUS, result.status);
    telemetry::counter(
        telemetry::counter::CD_STREAM_STEADY_BYTES,
        result.steady_bytes,
    );
    telemetry::counter(
        telemetry::counter::CD_STREAM_STEADY_SECTORS,
        result.steady_sectors,
    );
    telemetry::counter(telemetry::counter::CD_WORLD_PACK_BYTES, result.world_bytes);
    telemetry::counter(
        telemetry::counter::CD_WORLD_PACK_SECTORS,
        result.world_sectors,
    );
    telemetry::counter(
        telemetry::counter::CD_WORLD_PACK_CHUNKS,
        result.world_chunks,
    );
    telemetry::counter(
        telemetry::counter::CD_WORLD_PACK_CHECKSUM,
        result.world_checksum,
    );
    telemetry::counter(
        telemetry::counter::CD_WORLD_PACK_STATUS,
        result.world_status,
    );
    telemetry::stage_end(telemetry::stage::CD_STREAM_BENCH);
}

#[cfg(feature = "cd-stream-benchmark")]
fn run_benchmark_inner(cd: &mut CdController) -> BenchResult {
    run_benchmark_with(&mut Console, cd)
}

/// The raw stream (checksummed against its generated pattern), then the WORLD.PAK
/// that follows it.
#[cfg(feature = "cd-stream-benchmark")]
fn run_benchmark_with<T: Transport>(transport: &mut T, cd: &mut CdController) -> BenchResult {
    let mut result = BenchResult {
        status: STATUS_OK,
        bytes: 0,
        sectors: 0,
        steady_bytes: 0,
        steady_sectors: 0,
        polls: 0,
        checksum: FNV_OFFSET,
        expected_checksum: expected_checksum(CD_STREAM_BENCH_SECTORS),
        world_bytes: 0,
        world_sectors: 0,
        world_chunks: 0,
        world_checksum: 0,
        world_status: STATUS_UNSUPPORTED,
    };
    if !transport.available() {
        result.status = STATUS_UNSUPPORTED;
        return result;
    }

    // The WORLD.PAK header sector follows the stream, so one run covers both.
    cd.begin_run(
        transport,
        CD_STREAM_BENCH_LBA,
        CD_STREAM_BENCH_SECTORS as u32 + 1,
    );
    let mut sector = 0usize;
    let mut steady_stage_open = false;
    while sector < CD_STREAM_BENCH_SECTORS {
        if sector == 2 {
            telemetry::stage_begin(telemetry::stage::CD_STREAM_STEADY);
            steady_stage_open = true;
        }
        if let Err(status) = cd.wait_sector(transport) {
            if steady_stage_open {
                telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
            }
            cd.abort_run(transport);
            result.status = status;
            return result;
        }
        // SAFETY: the sector buffer holds a whole sector.
        result.checksum =
            unsafe { checksum_bytes(cd.sector_bytes(), SECTOR_BYTES, result.checksum) };
        result.sectors = result.sectors.saturating_add(1);
        result.bytes = result.bytes.saturating_add(SECTOR_BYTES as u32);
        if steady_stage_open {
            result.steady_sectors = result.steady_sectors.saturating_add(1);
            result.steady_bytes = result.steady_bytes.saturating_add(SECTOR_BYTES as u32);
        }
        // SAFETY: as above.
        if sector == 0 && !unsafe { sector_magic_matches(cd.sector_bytes()) } {
            result.status = STATUS_MAGIC_MISMATCH;
            break;
        }
        sector += 1;
    }
    if steady_stage_open {
        telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
    }

    if result.status == STATUS_OK {
        stream_world_pack(transport, cd, &mut result);
    } else {
        cd.abort_run(transport);
    }

    if result.status == STATUS_OK && result.checksum != result.expected_checksum {
        result.status = STATUS_CHECKSUM_MISMATCH;
    }
    result
}

/// Stream and checksum the whole WORLD.PAK region for the benchmark,
/// validating its header out of the first sector. The header sector is the one
/// the raw-stream run has already queued after its last sector.
#[cfg(feature = "cd-stream-benchmark")]
fn stream_world_pack<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    result: &mut BenchResult,
) {
    telemetry::stage_begin(telemetry::stage::CD_WORLD_PACK_STREAM);
    result.world_status = STATUS_OK;
    let mut checksum = FNV_OFFSET;
    let finish = |result: &mut BenchResult, checksum: u32| {
        result.world_checksum = checksum;
        telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
    };

    if let Err(status) = cd.wait_sector(transport) {
        result.world_status = status;
        cd.abort_run(transport);
        finish(result, checksum);
        return;
    }
    let sector = cd.sector_bytes();
    // SAFETY: the sector buffer holds a whole sector.
    let total_sectors = unsafe {
        checksum = checksum_bytes(sector, SECTOR_BYTES, checksum);
        result.world_bytes = result.world_bytes.saturating_add(SECTOR_BYTES as u32);
        result.world_sectors = result.world_sectors.saturating_add(1);

        if !world_pack_magic_matches(sector) {
            result.world_status = STATUS_MAGIC_MISMATCH;
            cd.abort_run(transport);
            finish(result, checksum);
            return;
        }

        let version = read_le_u32(sector.add(8));
        let chunk_count = read_le_u32(sector.add(12));
        let total_sectors = read_le_u32(sector.add(16));
        let header_sectors = read_le_u32(sector.add(20));
        let table_bytes = read_le_u32(sector.add(24));
        if version != 1
            || chunk_count == 0
            || total_sectors == 0
            || total_sectors > WORLD_PACK_MAX_SECTORS
            || header_sectors == 0
            || header_sectors > total_sectors
            || table_bytes == 0
        {
            result.world_status = STATUS_HEADER_INVALID;
            cd.abort_run(transport);
            finish(result, checksum);
            return;
        }
        result.world_chunks = chunk_count;
        total_sectors
    };

    // The rest of the pack is a second run from the sector after the header.
    if total_sectors > 1 {
        cd.begin_run(
            transport,
            CD_STREAM_BENCH_LBA + CD_STREAM_BENCH_SECTORS as u32 + 1,
            total_sectors - 1,
        );
    }
    let mut sector_index = 1;
    while sector_index < total_sectors {
        if let Err(status) = cd.wait_sector(transport) {
            result.world_status = status;
            cd.abort_run(transport);
            break;
        }
        // SAFETY: the sector buffer holds a whole sector.
        checksum = unsafe { checksum_bytes(cd.sector_bytes(), SECTOR_BYTES, checksum) };
        result.world_bytes = result.world_bytes.saturating_add(SECTOR_BYTES as u32);
        result.world_sectors = result.world_sectors.saturating_add(1);
        sector_index += 1;
    }
    finish(result, checksum);
}

fn first_status_error(current: u32, next: u32) -> u32 {
    if current == STATUS_OK {
        next
    } else {
        current
    }
}

/// Synchronously read one UI.PAK chunk through a small rolling window,
/// handing the caller each run of bytes as it lands instead of staging the
/// whole chunk in RAM.
///
/// This exists for assets that are far larger than the RAM we want to reserve
/// for them. The cube sky is 196,828 bytes and was staged whole, which sized
/// the load/render arena overlay at 192 KiB for one texture that is copied
/// straight to VRAM and never read again. Streaming it needs a window of a few
/// KiB instead.
///
/// `on_bytes` is called with the window's unconsumed prefix after each sector
/// and returns how many leading bytes it took. Whatever it leaves is moved to
/// the front and grows with the next sector, so a consumer can wait for a whole
/// texture row without caring where sector boundaries fall. Returning zero
/// forever is a caller bug and shows up as `STATUS_DEST_TOO_SMALL` once the
/// window fills.
///
/// The FNV checksum still covers every byte of the chunk in order, so integrity
/// is verified exactly as it is for a whole-chunk read.
#[allow(clippy::too_many_arguments)]
pub fn read_chunk_banded(
    cd: &mut CdController,
    pack_lba: u32,
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
    window: &mut [u32],
    on_bytes: impl FnMut(usize, &[u8]) -> usize,
) -> RoomChunkLoadResult {
    read_chunk_banded_with(&mut Console, cd, pack_lba, toc, chunk_id, window, on_bytes)
}

fn read_chunk_banded_with<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    pack_lba: u32,
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
    window: &mut [u32],
    mut on_bytes: impl FnMut(usize, &[u8]) -> usize,
) -> RoomChunkLoadResult {
    let mut result = RoomChunkLoadResult {
        status: STATUS_OK,
        bytes: 0,
        sectors: 0,
    };

    let Some(entry) = world_pack_entry_from_toc(toc, chunk_id) else {
        result.status = STATUS_CHUNK_NOT_FOUND;
        return result;
    };

    let window_bytes = window.len().saturating_mul(4);
    if window_bytes < SECTOR_BYTES * 2 {
        // One sector of new data plus whatever the consumer is still holding.
        result.status = STATUS_DEST_TOO_SMALL;
        return result;
    }
    if !transport.available() {
        result.status = STATUS_UNSUPPORTED;
        return result;
    }

    cd.begin_run(
        transport,
        pack_lba.saturating_add(entry.sector_offset),
        entry.sector_count,
    );

    let window_ptr = window.as_mut_ptr().cast::<u8>();
    let mut checksum = FNV_OFFSET;
    let mut held = 0usize;
    let mut consumed_total = 0usize;
    let mut sector = 0u32;
    while sector < entry.sector_count {
        if let Err(status) = cd.wait_sector(transport) {
            result.status = status;
            break;
        }
        let chunk_byte_offset = (sector as usize).saturating_mul(SECTOR_BYTES);
        let remaining = entry.byte_size.saturating_sub(chunk_byte_offset);
        let copy_len = remaining.min(SECTOR_BYTES);
        if copy_len > 0 {
            if held + copy_len > window_bytes {
                result.status = STATUS_DEST_TOO_SMALL;
                break;
            }
            let buffer = cd.sector_bytes();
            // SAFETY: `held + copy_len <= window_bytes` was just checked, so
            // the destination stays inside `window`, and the controller's
            // sector buffer cannot overlap it.
            unsafe {
                core::ptr::copy_nonoverlapping(buffer, window_ptr.add(held), copy_len);
                checksum = checksum_bytes(buffer, copy_len, checksum);
            }
            held += copy_len;
            result.bytes = result.bytes.saturating_add(copy_len);

            // SAFETY: `held` bytes were just written and stay borrowed only
            // for this call.
            let view = unsafe { core::slice::from_raw_parts(window_ptr, held) };
            let took = on_bytes(consumed_total, view).min(held);
            if took > 0 {
                consumed_total += took;
                held -= took;
                if held > 0 {
                    // SAFETY: source and destination are both inside the
                    // window; the ranges may overlap, hence `copy`.
                    unsafe { core::ptr::copy(window_ptr.add(took), window_ptr, held) };
                }
            }
        }
        result.sectors = result.sectors.saturating_add(1);
        sector += 1;
    }
    // Give the consumer its tail: the trailing CLUT lands here.
    if result.status == STATUS_OK && held > 0 {
        // SAFETY: as above.
        let view = unsafe { core::slice::from_raw_parts(window_ptr, held) };
        let _ = on_bytes(consumed_total, view);
    }
    if result.status != STATUS_OK {
        cd.abort_run(transport);
    }

    if result.status == STATUS_OK {
        if result.bytes != entry.byte_size {
            result.status = STATUS_DATA_TIMEOUT;
        } else if checksum != entry.checksum {
            result.status = STATUS_CHECKSUM_MISMATCH;
        }
    }
    result
}

/// Synchronously read one UI.PAK chunk into `dst`. Looks the chunk
/// up in `toc` by `chunk_id` (the streamed asset index), reads its
/// sector run from `pack_lba + entry.sector_offset`, copies the
/// unpadded bytes into `dst`, and verifies the FNV checksum. Used by
/// the streamed-asset loaders (menu UI images and the gameplay sky),
/// which load one chunk at a time into a shared staging buffer, so a
/// blocking read is the simplest correct shape. Non-mips builds return
/// `STATUS_UNSUPPORTED`.
pub fn read_chunk_blocking(
    cd: &mut CdController,
    pack_lba: u32,
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
    dst: &mut [u32],
) -> RoomChunkLoadResult {
    read_chunk_blocking_with(&mut Console, cd, pack_lba, toc, chunk_id, dst)
}

fn read_chunk_blocking_with<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    pack_lba: u32,
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
    dst: &mut [u32],
) -> RoomChunkLoadResult {
    let mut result = RoomChunkLoadResult {
        status: STATUS_OK,
        bytes: 0,
        sectors: 0,
    };

    let Some(entry) = world_pack_entry_from_toc(toc, chunk_id) else {
        result.status = STATUS_CHUNK_NOT_FOUND;
        return result;
    };

    let dst_bytes = dst.len().saturating_mul(4);
    if entry.byte_size > dst_bytes {
        result.status = STATUS_DEST_TOO_SMALL;
        return result;
    }
    if !transport.available() {
        result.status = STATUS_UNSUPPORTED;
        return result;
    }

    cd.begin_run(
        transport,
        pack_lba.saturating_add(entry.sector_offset),
        entry.sector_count,
    );

    let dst_ptr = dst.as_mut_ptr().cast::<u8>();
    let mut checksum = FNV_OFFSET;
    let mut sector = 0u32;
    while sector < entry.sector_count {
        if let Err(status) = cd.wait_sector(transport) {
            result.status = status;
            break;
        }
        let chunk_byte_offset = (sector as usize).saturating_mul(SECTOR_BYTES);
        let remaining = entry.byte_size.saturating_sub(chunk_byte_offset);
        let copy_len = remaining.min(SECTOR_BYTES);
        if copy_len > 0 {
            let buffer = cd.sector_bytes();
            // SAFETY: the sector buffer holds `copy_len <= SECTOR_BYTES`
            // readable bytes; the destination range stays inside `dst`
            // (`entry.byte_size <= dst` was checked above) and cannot
            // overlap the controller's own sector buffer.
            unsafe {
                core::ptr::copy_nonoverlapping(buffer, dst_ptr.add(chunk_byte_offset), copy_len);
                checksum = checksum_bytes(buffer, copy_len, checksum);
            }
            result.bytes = result.bytes.saturating_add(copy_len);
        }
        result.sectors = result.sectors.saturating_add(1);
        sector += 1;
    }
    if result.status != STATUS_OK {
        cd.abort_run(transport);
    }

    if result.status == STATUS_OK {
        if result.bytes != entry.byte_size {
            result.status = STATUS_DATA_TIMEOUT;
        } else if checksum != entry.checksum {
            result.status = STATUS_CHECKSUM_MISMATCH;
        }
    }
    result
}

/// One UI.PAK chunk to read in a contiguous batch: where it sits on disc
/// (`sector_offset` / `sector_count` within UI.PAK) and where its unpadded
/// bytes go in the caller's flat cache (`cache_word_start`, in u32 words).
#[derive(Copy, Clone)]
pub struct UiChunkPlan {
    /// First sector of the chunk, relative to UI.PAK's start LBA.
    pub sector_offset: u32,
    /// Whole sectors the chunk occupies on disc.
    pub sector_count: u32,
    /// Unpadded payload byte count.
    pub byte_size: usize,
    /// Expected FNV checksum of the payload bytes.
    pub checksum: u32,
    /// Destination u32-word offset in the caller's flat cache.
    pub cache_word_start: usize,
}

impl UiChunkPlan {
    /// Zero-sector placeholder plan.
    pub const EMPTY: Self = Self {
        sector_offset: 0,
        sector_count: 0,
        byte_size: 0,
        checksum: 0,
        cache_word_start: 0,
    };
}

/// Read several CONTIGUOUS UI.PAK chunks as ONE run: a single seek to the
/// first chunk, then every chunk's sectors back to back (the transport chains
/// the staging windows without pausing). This replaces N separate seeks, and
/// each of those forces a real CD-R drive to stop, seek, and re-acquire the
/// stream, which is the menu's HUGE boot delay on hardware (cheap only in the
/// emulator, which has no seek/spin model). `plans` must be in ascending
/// `sector_offset` (disc) order; a gap of up to [`SEEK_BREAK_EVEN_SECTORS`]
/// between chunks is read and discarded so the stream stays aligned, and a chunk
/// behind a longer gap is not read (its status is a timeout): a longer gap means
/// the caller's chunk ordering is wrong for this read, and discarding it would
/// cost more than reseeking. Each chunk's status lands in `out_status[i]`.
pub fn read_chunks_contiguous(
    cd: &mut CdController,
    pack_lba: u32,
    plans: &[UiChunkPlan],
    cache: &mut [u32],
    out_status: &mut [u32],
) {
    read_chunks_contiguous_with(&mut Console, cd, pack_lba, plans, cache, out_status);
}

fn read_chunks_contiguous_with<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    pack_lba: u32,
    plans: &[UiChunkPlan],
    cache: &mut [u32],
    out_status: &mut [u32],
) {
    for status in out_status.iter_mut() {
        *status = STATUS_OK;
    }
    if plans.is_empty() {
        return;
    }
    if !transport.available() {
        let n = plans.len().min(out_status.len());
        for status in out_status.iter_mut().take(n) {
            *status = STATUS_UNSUPPORTED;
        }
        return;
    }

    // One run covers the first chunk through the end of the last chunk that
    // sits within the break-even gap of its predecessor.
    let first = plans[0].sector_offset;
    let mut end = first;
    let mut i = 0usize;
    while i < plans.len() {
        let plan = plans[i];
        if plan.sector_offset.saturating_sub(end) as usize <= SEEK_BREAK_EVEN_SECTORS {
            end = end.max(plan.sector_offset.saturating_add(plan.sector_count));
        }
        i += 1;
    }
    cd.begin_run(transport, pack_lba.saturating_add(first), end - first);

    let cache_ptr = cache.as_mut_ptr() as *mut u8;
    let cache_bytes = cache.len().saturating_mul(4);
    let mut cur = first;
    let mut aborted = false;
    let mut i = 0usize;
    while i < plans.len() {
        if aborted {
            out_status[i] = STATUS_DATA_TIMEOUT;
            i += 1;
            continue;
        }
        let plan = plans[i];
        if plan.sector_offset.saturating_sub(cur) as usize > SEEK_BREAK_EVEN_SECTORS {
            out_status[i] = STATUS_DATA_TIMEOUT;
            i += 1;
            continue;
        }
        while cur < plan.sector_offset {
            if cd.wait_sector(transport).is_err() {
                aborted = true;
                break;
            }
            cur = cur.saturating_add(1);
        }
        if aborted {
            out_status[i] = STATUS_DATA_TIMEOUT;
            i += 1;
            continue;
        }
        let dst_base = plan.cache_word_start.saturating_mul(4);
        let mut checksum = FNV_OFFSET;
        let mut bytes = 0usize;
        let mut sector = 0u32;
        while sector < plan.sector_count {
            if let Err(status) = cd.wait_sector(transport) {
                out_status[i] = status;
                aborted = true;
                break;
            }
            cur = cur.saturating_add(1);
            let off = (sector as usize).saturating_mul(SECTOR_BYTES);
            let remaining = plan.byte_size.saturating_sub(off);
            let copy_len = remaining.min(SECTOR_BYTES);
            let dst_off = dst_base.saturating_add(off);
            if copy_len > 0 && dst_off.saturating_add(copy_len) <= cache_bytes {
                let buffer = cd.sector_bytes();
                // SAFETY: `copy_len <= SECTOR_BYTES` readable bytes sit
                // in the sector buffer; the destination range was bounds-
                // checked against `cache` just above and cannot overlap
                // the controller's own sector buffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(buffer, cache_ptr.add(dst_off), copy_len);
                    checksum = checksum_bytes(buffer, copy_len, checksum);
                }
                bytes = bytes.saturating_add(copy_len);
            }
            sector += 1;
        }
        if !aborted {
            if bytes != plan.byte_size {
                out_status[i] = STATUS_DATA_TIMEOUT;
            } else if checksum != plan.checksum {
                out_status[i] = STATUS_CHECKSUM_MISMATCH;
            }
        }
        i += 1;
    }
    if aborted {
        cd.abort_run(transport);
    }
}

/// Where chunk `chunk_id` of a pack sits, from its cooked table of contents.
#[cfg(feature = "world-stream")]
pub fn world_pack_chunk(
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
) -> Option<WorldChunkInfo> {
    world_pack_entry_from_toc(toc, chunk_id)
}

fn world_pack_entry_from_toc(
    toc: &[LevelWorldPackEntryRecord],
    chunk_id: u32,
) -> Option<WorldChunkInfo> {
    let mut i = 0usize;
    while i < toc.len() {
        let entry = toc[i];
        if entry.room.raw() as u32 == chunk_id {
            return Some(WorldChunkInfo {
                sector_offset: entry.sector_offset,
                sector_count: entry.sector_count,
                byte_size: entry.byte_size as usize,
                checksum: entry.checksum,
            });
        }
        i += 1;
    }
    None
}

fn next_world_pack_info_read_group<const N: usize>(
    entries: &[WorldChunkInfo; N],
    statuses: &[u32; N],
    processed: &[bool; N],
    count: usize,
) -> Option<(u32, u32, [bool; N])> {
    let limit = count.min(N);
    let mut first_index = usize::MAX;
    let mut first_sector = u32::MAX;
    let mut i = 0usize;
    while i < limit {
        let entry = entries[i];
        if !processed[i]
            && statuses[i] == STATUS_OK
            && entry.sector_count > 0
            && entry.sector_offset < first_sector
        {
            first_index = i;
            first_sector = entry.sector_offset;
        }
        i += 1;
    }
    if first_index == usize::MAX {
        return None;
    }

    let mut group_entries = [false; N];
    group_entries[first_index] = true;
    let mut group_start = entries[first_index].sector_offset;
    let mut group_end = entries[first_index]
        .sector_offset
        .saturating_add(entries[first_index].sector_count);

    let mut changed = true;
    while changed {
        changed = false;
        let mut candidate = 0usize;
        while candidate < limit {
            let entry = entries[candidate];
            if group_entries[candidate]
                || processed[candidate]
                || statuses[candidate] != STATUS_OK
                || entry.sector_count == 0
            {
                candidate += 1;
                continue;
            }
            let entry_end = entry.sector_offset.saturating_add(entry.sector_count);
            if entry.sector_offset <= group_end && entry_end >= group_start {
                group_entries[candidate] = true;
                group_start = group_start.min(entry.sector_offset);
                group_end = group_end.max(entry_end);
                changed = true;
            }
            candidate += 1;
        }
    }

    Some((group_start, group_end, group_entries))
}

/// Land the freshly read sector at `sector_offset` into every armed
/// chunk it belongs to, advancing that chunk's byte count and checksum.
///
/// # Safety
/// `sector_ptr` must be readable for [`SECTOR_BYTES`] bytes. Entries'
/// `byte_size` must fit its destination allocation (`start` checked the
/// supplied per-slot capacities when arming the job).
unsafe fn copy_window_info_sector<const N: usize>(
    sector_ptr: *const u8,
    sector_offset: u32,
    entries: &[WorldChunkInfo],
    slot_indices: &[usize],
    dst: &mut impl WorldChunkDestination,
    byte_counts: &mut [usize; N],
    checksums: &mut [u32; N],
) {
    let mut i = 0usize;
    while i < entries.len() && i < slot_indices.len() && i < N {
        let entry = entries[i];
        let chunk_end = entry.sector_offset.saturating_add(entry.sector_count);
        if sector_offset >= entry.sector_offset && sector_offset < chunk_end {
            let dst_slot = slot_indices[i];
            if dst_slot >= N {
                i += 1;
                continue;
            }
            let chunk_sector = sector_offset.saturating_sub(entry.sector_offset) as usize;
            let chunk_byte_offset = chunk_sector.saturating_mul(SECTOR_BYTES);
            let remaining = entry.byte_size.saturating_sub(chunk_byte_offset);
            let copy_len = remaining.min(SECTOR_BYTES);
            if copy_len > 0 {
                // SAFETY: caller guarantees a readable sector at
                // `sector_ptr` and that `byte_size` (hence
                // `chunk_byte_offset + copy_len`) fits the slot row; the
                // sector buffer and slot rows never overlap.
                // SAFETY: the caller guarantees `sector_ptr` is readable for
                // a complete sector. `copy_len` is bounded by that sector.
                let source = unsafe { core::slice::from_raw_parts(sector_ptr, copy_len) };
                if dst.write_chunk_bytes(dst_slot, chunk_byte_offset, source) {
                    checksums[i] = unsafe { checksum_bytes(sector_ptr, copy_len, checksums[i]) };
                    byte_counts[i] = byte_counts[i].saturating_add(copy_len);
                }
            }
        }
        i += 1;
    }
}

/// FNV-checksum `len` bytes at `ptr`.
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
unsafe fn checksum_bytes(ptr: *const u8, len: usize, mut checksum: u32) -> u32 {
    let mut i = 0usize;
    while i < len {
        // SAFETY: caller guarantees `len` readable bytes at `ptr`.
        checksum ^= unsafe { *ptr.add(i) } as u32;
        checksum = checksum.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    checksum
}

/// Whether the sector at `ptr` opens with the stream-bench magic.
///
/// # Safety
/// `ptr` must be readable for the magic's length.
#[cfg(feature = "cd-stream-benchmark")]
unsafe fn sector_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < CD_STREAM_BENCH_MAGIC.len() {
        // SAFETY: caller guarantees the magic's length readable at `ptr`.
        if unsafe { *ptr.add(i) } != CD_STREAM_BENCH_MAGIC[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether the sector at `ptr` opens with the WORLD.PAK magic.
///
/// # Safety
/// `ptr` must be readable for the magic's length.
#[cfg(feature = "cd-stream-benchmark")]
unsafe fn world_pack_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < WORLD_PACK_MAGIC.len() {
        // SAFETY: caller guarantees the magic's length readable at `ptr`.
        if unsafe { *ptr.add(i) } != WORLD_PACK_MAGIC[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Read a little-endian u32 from `ptr`.
///
/// # Safety
/// `ptr` must be readable for 4 bytes.
#[cfg(feature = "cd-stream-benchmark")]
unsafe fn read_le_u32(ptr: *const u8) -> u32 {
    // SAFETY: caller guarantees 4 readable bytes at `ptr`.
    unsafe {
        u32::from(*ptr)
            | (u32::from(*ptr.add(1)) << 8)
            | (u32::from(*ptr.add(2)) << 16)
            | (u32::from(*ptr.add(3)) << 24)
    }
}

/// Host-side reference checksum for the bench pattern.
#[cfg(feature = "cd-stream-benchmark")]
fn expected_checksum(sectors: usize) -> u32 {
    let mut checksum = FNV_OFFSET;
    let mut i = 0usize;
    while i < sectors * SECTOR_BYTES {
        checksum ^= expected_byte(i, sectors) as u32;
        checksum = checksum.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    checksum
}

/// The bench disc's generated byte pattern at `index`.
#[cfg(feature = "cd-stream-benchmark")]
const fn expected_byte(index: usize, sectors: usize) -> u8 {
    if index < CD_STREAM_BENCH_MAGIC.len() {
        CD_STREAM_BENCH_MAGIC[index]
    } else if index < 12 {
        ((sectors as u32).to_le_bytes())[index - 8]
    } else {
        let mixed = (index as u32)
            .wrapping_mul(37)
            .wrapping_add((index as u32) >> 3)
            .wrapping_add(0x5D);
        mixed as u8
    }
}
