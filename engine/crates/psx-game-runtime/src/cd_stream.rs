//! Polled CD-ROM streaming: the multi-room WORLD.PAK read job the
//! streamed-room scheduler drives, blocking UI.PAK chunk readers for
//! menu images and the sky, and the `cd-stream-benchmark` throughput
//! probe. The register-level command/IRQ/DMA sequencing lives in the
//! private `hw` submodule; host (non-MIPS) builds compile the same API
//! with every read reporting unsupported.

#![cfg_attr(not(target_arch = "mips"), allow(dead_code))]

use psx_engine::telemetry;
use psx_level::LevelWorldPackEntryRecord;

mod hw;
#[allow(unused_imports)]
use self::hw::*;

const CD_BASE: u32 = 0x1F80_1800;
const CD_STATUS: u32 = CD_BASE;
const CD_RESPONSE: u32 = CD_BASE + 1;
const CD_PARAM: u32 = CD_BASE + 2;
const CD_IRQ: u32 = CD_BASE + 3;

const STATUS_RESPONSE_FIFO_NOT_EMPTY: u8 = 1 << 5;
const STATUS_PARAMETER_FIFO_NOT_FULL: u8 = 1 << 4;
const STATUS_DATA_FIFO_NOT_EMPTY: u8 = 1 << 6;

const IRQ_DATA_READY: u8 = 1;
const IRQ_COMPLETE: u8 = 2;
const IRQ_ACK: u8 = 3;
const IRQ_DATA_END: u8 = 4;
const IRQ_ERROR: u8 = 5;

const CMD_SETLOC: u8 = 0x02;
const CMD_READN: u8 = 0x06;
const CMD_PAUSE: u8 = 0x09;
const CMD_SETMODE: u8 = 0x0E;

const CD_MODE_DOUBLE_SPEED_2048: u8 = 0x80;
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
const STATUS_SETMODE_TIMEOUT: u32 = 1;
const STATUS_SETLOC_TIMEOUT: u32 = 2;
const STATUS_READ_ACK_TIMEOUT: u32 = 3;
const STATUS_DATA_TIMEOUT: u32 = 4;
const STATUS_CD_ERROR: u32 = 5;
#[cfg(feature = "cd-stream-benchmark")]
const STATUS_MAGIC_MISMATCH: u32 = 6;
const STATUS_CHECKSUM_MISMATCH: u32 = 7;
#[cfg(any(not(target_arch = "mips"), feature = "cd-stream-benchmark"))]
const STATUS_UNSUPPORTED: u32 = 8;
#[cfg(feature = "cd-stream-benchmark")]
const STATUS_HEADER_INVALID: u32 = 9;
const STATUS_CHUNK_NOT_FOUND: u32 = 10;
const STATUS_DEST_TOO_SMALL: u32 = 11;

/// Status value reported by chunk reads that completed and verified.
pub const ROOM_CHUNK_STATUS_OK: u32 = STATUS_OK;

const COMMAND_ACK_POLL_LIMIT: u32 = 16_384;
const PARAMETER_ROOM_POLL_LIMIT: u32 = 16_384;
#[cfg(feature = "cd-stream-benchmark")]
const DATA_READY_POLL_LIMIT: u32 = 1_000_000;
/// Consecutive `poll_into` calls that may find no sector waiting before the
/// read is declared dead.
///
/// The unit is PUMP CALLS, not spin-polls: `try_read_stream_sector` checks the
/// IRQ flag once and returns, and the caller breaks out of its loop on the
/// first empty result, so at most one increment happens per `poll_into`. The
/// old 4096 was written as if it counted spins; at one pump per two sim ticks
/// that was a 136-second budget, which is why a disc with nothing at the pack
/// LBA hung an 8117-tick headless run without ever tripping the detector.
///
/// 256 pumps is ~8.5 s at 30 pumps/second, well past any real seek and
/// re-acquire, and short enough that a dead read reports instead of hanging.
#[cfg(target_arch = "mips")]
const EMPTY_PUMP_STALL_LIMIT: u32 = 256;
/// Spin budget for one blocking UI image sector read.
#[cfg(target_arch = "mips")]
const DATA_READY_BLOCKING_POLL_LIMIT: u32 = 1_000_000;
const DMA_POLL_LIMIT: u32 = 65_536;
/// Spins to wait for a sector already on its way. One arrives every 6.7 ms at
/// double speed; this is well past that and far short of a hang.
#[cfg(target_arch = "mips")]
const SECTOR_ARRIVAL_SPIN_LIMIT: u32 = 200_000;
const CLEANUP_POLL_LIMIT: u32 = 16_384;

/// Owned CD-ROM controller driver state: the one-sector DMA bounce
/// buffer and the first-read preparation latch (formerly the module's
/// `CD_STREAM_SECTOR_BUFFER`/`CD_READ_PREPARED` statics, retired in
/// phase 2 per the phase-1.5 note). The game keeps one instance in its
/// runtime arenas and threads it into every CD read entry point.
pub struct CdController {
    #[cfg_attr(not(target_arch = "mips"), allow(dead_code))]
    sector_buffer: [u32; SECTOR_WORDS],
    #[cfg_attr(not(target_arch = "mips"), allow(dead_code))]
    read_prepared: bool,
}

impl CdController {
    /// All-zero state (link-time `.bss`-safe); matches the old statics'
    /// initial state exactly (zeroed buffer, not yet prepared).
    pub const fn zeroed() -> Self {
        Self {
            sector_buffer: [0; SECTOR_WORDS],
            read_prepared: false,
        }
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

#[cfg(feature = "cd-stream-benchmark")]
impl BenchResult {
    /// Host stub result: no CD hardware.
    #[cfg(not(target_arch = "mips"))]
    const fn unsupported() -> Self {
        Self {
            status: STATUS_UNSUPPORTED,
            bytes: 0,
            sectors: 0,
            steady_bytes: 0,
            steady_sectors: 0,
            polls: 0,
            checksum: 0,
            expected_checksum: 0,
            world_bytes: 0,
            world_sectors: 0,
            world_chunks: 0,
            world_checksum: 0,
            world_status: STATUS_UNSUPPORTED,
        }
    }
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
    empty_pumps: u32,
    /// Whether this read may stay with the drive between sectors. See
    /// [`WorldRoomSlotsRead::set_wait_for_sectors`].
    wait_for_sectors: bool,
    /// End an in-flight read after a productive budget slice so other work
    /// may run without the drive advancing past uncollected sectors. The next
    /// poll resumes at `sector_offset`, not at the beginning of the group.
    pause_at_poll_boundary: bool,
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
            empty_pumps: 0,
            wait_for_sectors: false,
            pause_at_poll_boundary: false,
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
            empty_pumps: 0,
            wait_for_sectors: false,
            pause_at_poll_boundary: false,
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

        #[cfg(not(target_arch = "mips"))]
        {
            let _ = toc;
            self.fail_all(STATUS_UNSUPPORTED);
            self.state = WorldRoomSlotsReadState::Done;
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
        }

        #[cfg(target_arch = "mips")]
        {
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
                    Some(entry) if entry.byte_size as usize <= slot_capacities[i] => {
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
    }

    /// Advance the armed read by up to `max_sectors` sectors, landing
    /// each chunk's unpadded bytes in `dst[slot]` and tracking per-chunk
    /// byte counts and running checksums.
    /// Drain at the drive's rate rather than the frame's, for a load with
    /// nothing to stay responsive for.
    ///
    /// The controller steps over sectors that arrive while software is away,
    /// and a pump that returns the moment nothing is buffered is away for the
    /// rest of the frame. At double speed a sector lands every 6.7 ms and a
    /// frame is 16.7 ms, so returning early loses most of them. Waiting for
    /// the one already on its way costs a bounded spin and keeps the stream
    /// whole.
    pub fn set_wait_for_sectors(&mut self, wait: bool) {
        self.wait_for_sectors = wait;
    }

    /// Pause a productive read at the poll budget boundary. Use this when a
    /// visual frame must run between synchronous CD slices: leaving ReadN open
    /// while rendering lets the one-sector controller advance underneath the
    /// collector and corrupts the payload. A zero-sector poll remains armed so
    /// the initial command can be collected immediately by the next call.
    pub fn set_pause_at_poll_boundary(&mut self, pause: bool) {
        self.pause_at_poll_boundary = pause;
    }

    /// Advance the in-flight read, moving any completed sectors into `dst`.
    pub fn poll_into(
        &mut self,
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

        #[cfg(not(target_arch = "mips"))]
        {
            let _ = (cd, dst);
            self.fail_all(STATUS_UNSUPPORTED);
            self.state = WorldRoomSlotsReadState::Done;
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
            self.result
        }

        #[cfg(target_arch = "mips")]
        {
            telemetry::stage_begin(telemetry::stage::CD_ROOM_CHUNK_LOAD);
            let before_sectors = self.result.sectors;
            let mut polls = 0;
            let mut sectors_this_poll = 0usize;
            while sectors_this_poll < max_sectors && self.state != WorldRoomSlotsReadState::Done {
                if self.state == WorldRoomSlotsReadState::Ready {
                    if !self.begin_next_group(cd, &mut polls) {
                        break;
                    }
                    if sectors_this_poll == 0 {
                        break;
                    }
                }

                if self.state != WorldRoomSlotsReadState::Reading {
                    break;
                }

                match try_read_stream_sector(cd, &mut polls) {
                    Ok(true) => {
                        self.empty_pumps = 0;
                    }
                    Ok(false) => {
                        // Nothing buffered yet. If this read may block, the
                        // next sector is already on its way, so wait for it
                        // rather than hand the frame back and let it land
                        // unattended.
                        if self.wait_for_sectors {
                            let mut spins = 0u32;
                            let mut landed = false;
                            while spins < SECTOR_ARRIVAL_SPIN_LIMIT {
                                spins += 1;
                                match try_read_stream_sector(cd, &mut polls) {
                                    Ok(true) => {
                                        landed = true;
                                        break;
                                    }
                                    Ok(false) => continue,
                                    Err(status) => {
                                        self.fail_all(status);
                                        cleanup_read_stream(cd, &mut polls);
                                        self.state = WorldRoomSlotsReadState::Done;
                                        break;
                                    }
                                }
                            }
                            if landed {
                                self.empty_pumps = 0;
                            } else {
                                self.empty_pumps = self.empty_pumps.saturating_add(1);
                                if self.empty_pumps > EMPTY_PUMP_STALL_LIMIT {
                                    self.fail_all(STATUS_DATA_TIMEOUT);
                                    cleanup_read_stream(cd, &mut polls);
                                    self.state = WorldRoomSlotsReadState::Done;
                                }
                                break;
                            }
                        } else {
                            self.empty_pumps = self.empty_pumps.saturating_add(1);
                            if self.empty_pumps > EMPTY_PUMP_STALL_LIMIT {
                                self.fail_all(STATUS_DATA_TIMEOUT);
                                cleanup_read_stream(cd, &mut polls);
                                self.state = WorldRoomSlotsReadState::Done;
                            }
                            break;
                        }
                    }
                    Err(status) => {
                        self.fail_all(status);
                        cleanup_read_stream(cd, &mut polls);
                        self.state = WorldRoomSlotsReadState::Done;
                        break;
                    }
                }
                // SAFETY: the sector buffer holds the sector the read above
                // just landed; SECTOR_BYTES are readable behind the pointer.
                unsafe {
                    copy_window_info_sector(
                        cd.sector_buffer.as_ptr() as *const u8,
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
                    cleanup_read_stream(cd, &mut polls);
                    self.mark_group_processed();
                    if self.processed_count >= self.valid_count {
                        self.finish();
                    } else {
                        self.state = WorldRoomSlotsReadState::Ready;
                    }
                }
            }
            if self.pause_at_poll_boundary
                && sectors_this_poll > 0
                && self.state == WorldRoomSlotsReadState::Reading
            {
                cleanup_read_stream(cd, &mut polls);
                self.state = WorldRoomSlotsReadState::Ready;
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
    }

    /// Whether the job still has groups armed or streaming.
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            WorldRoomSlotsReadState::Ready | WorldRoomSlotsReadState::Reading
        )
    }

    /// Pause the drive if a read is in flight and reset the job to idle.
    pub fn abort(&mut self, cd: &mut CdController) {
        if self.is_active() {
            #[cfg(target_arch = "mips")]
            {
                let mut polls = 0;
                cleanup_read_stream(cd, &mut polls);
            }
            #[cfg(not(target_arch = "mips"))]
            let _ = cd;
        }
        #[cfg(not(target_arch = "mips"))]
        let _ = cd;
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

    #[cfg(target_arch = "mips")]
    fn begin_next_group(&mut self, cd: &mut CdController, polls: &mut u32) -> bool {
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
        if let Err(status) = prepare_cd_read(cd, polls) {
            self.fail_all(status);
            self.state = WorldRoomSlotsReadState::Done;
            return false;
        }
        if let Err(status) =
            start_cd_read_at_lba(cd, self.world_pack_lba.saturating_add(read_start), polls)
        {
            self.fail_all(status);
            self.state = WorldRoomSlotsReadState::Done;
            return false;
        }
        if !resuming {
            self.group_start = read_start;
            self.empty_pumps = 0;
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

#[cfg(all(feature = "cd-stream-benchmark", not(target_arch = "mips")))]
fn run_benchmark_inner(cd: &mut CdController) -> BenchResult {
    let _ = cd;
    BenchResult::unsupported()
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
fn run_benchmark_inner(cd: &mut CdController) -> BenchResult {
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

    if let Err(status) = prepare_cd_read(cd, &mut result.polls) {
        result.status = status;
        return result;
    }
    if let Err(status) = start_cd_read_at_lba(cd, CD_STREAM_BENCH_LBA, &mut result.polls) {
        result.status = status;
        cleanup_read_stream(cd, &mut result.polls);
        return result;
    }

    // SAFETY: the controller-owned sector buffer is valid for
    // whole-sector DMA writes and sector-length reads throughout.
    unsafe {
        let mut sector = 0usize;
        let mut steady_stage_open = false;
        while sector < CD_STREAM_BENCH_SECTORS {
            if sector == 2 {
                telemetry::stage_begin(telemetry::stage::CD_STREAM_STEADY);
                steady_stage_open = true;
            }
            if let Err(status) = read_stream_sector(cd, &mut result.polls) {
                if steady_stage_open {
                    telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
                }
                result.status = status;
                return result;
            }

            result.checksum =
                checksum_sector(cd.sector_buffer.as_ptr() as *const u8, result.checksum);
            result.sectors = result.sectors.saturating_add(1);
            result.bytes = result.bytes.saturating_add(SECTOR_BYTES as u32);
            if steady_stage_open {
                result.steady_sectors = result.steady_sectors.saturating_add(1);
                result.steady_bytes = result.steady_bytes.saturating_add(SECTOR_BYTES as u32);
            }

            if sector == 0 && !sector_magic_matches(cd.sector_buffer.as_ptr() as *const u8) {
                result.status = STATUS_MAGIC_MISMATCH;
                break;
            }
            sector += 1;
        }
        if steady_stage_open {
            telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
        }

        if result.status == STATUS_OK {
            stream_world_pack(cd, &mut result);
        }
    }
    cleanup_read_stream(cd, &mut result.polls);

    if result.status == STATUS_OK && result.checksum != result.expected_checksum {
        result.status = STATUS_CHECKSUM_MISMATCH;
    }
    result
}

fn first_status_error(current: u32, next: u32) -> u32 {
    if current == STATUS_OK {
        next
    } else {
        current
    }
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

    #[cfg(not(target_arch = "mips"))]
    {
        let _ = (cd, pack_lba);
        result.status = STATUS_UNSUPPORTED;
        result
    }

    #[cfg(target_arch = "mips")]
    {
        let mut polls = 0u32;
        if let Err(status) = prepare_cd_read(cd, &mut polls) {
            result.status = status;
            return result;
        }
        if let Err(status) =
            start_cd_read_at_lba(cd, pack_lba.saturating_add(entry.sector_offset), &mut polls)
        {
            result.status = status;
            cleanup_read_stream(cd, &mut polls);
            return result;
        }

        let dst_ptr = dst.as_mut_ptr().cast::<u8>();
        let mut checksum = FNV_OFFSET;
        let mut sector = 0u32;
        while sector < entry.sector_count {
            if let Err(status) = read_one_sector_blocking(cd, &mut polls) {
                result.status = status;
                break;
            }
            let chunk_byte_offset = (sector as usize).saturating_mul(SECTOR_BYTES);
            let remaining = entry.byte_size.saturating_sub(chunk_byte_offset);
            let copy_len = remaining.min(SECTOR_BYTES);
            if copy_len > 0 {
                let buffer = cd.sector_buffer.as_ptr();
                // SAFETY: the sector buffer holds `copy_len <= SECTOR_BYTES`
                // readable bytes; the destination range stays inside `dst`
                // (`entry.byte_size <= dst` was checked above) and cannot
                // overlap the controller's own sector buffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buffer as *const u8,
                        dst_ptr.add(chunk_byte_offset),
                        copy_len,
                    );
                    checksum = checksum_bytes(buffer as *const u8, copy_len, checksum);
                }
                result.bytes = result.bytes.saturating_add(copy_len);
            }
            result.sectors = result.sectors.saturating_add(1);
            sector += 1;
        }
        cleanup_read_stream(cd, &mut polls);

        if result.status == STATUS_OK {
            if result.bytes != entry.byte_size {
                result.status = STATUS_DATA_TIMEOUT;
            } else if checksum != entry.checksum {
                result.status = STATUS_CHECKSUM_MISMATCH;
            }
        }
        result
    }
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

/// Read several CONTIGUOUS UI.PAK chunks in a SINGLE ReadN session: one
/// SetMode + SetLoc + ReadN at the first chunk, stream every chunk's sectors
/// back-to-back (the drive reads sequentially through the run), and one Pause
/// at the end. This replaces N separate `SetLoc + ReadN + Pause` cycles -- and
/// each of those forces a real CD-R drive to stop, seek, and re-acquire the
/// stream, which is the menu's HUGE boot delay on hardware (cheap only in the
/// emulator, which has no seek/spin model). `plans` must be in ascending
/// `sector_offset` (disc) order; any gap between chunks is read and discarded
/// so the stream stays aligned. Each chunk's status lands in `out_status[i]`.
#[cfg(target_arch = "mips")]
pub fn read_chunks_contiguous(
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
    let mut polls = 0u32;
    if let Err(status) = prepare_cd_read(cd, &mut polls) {
        for s in out_status.iter_mut() {
            *s = status;
        }
        return;
    }
    let first = plans[0].sector_offset;
    if let Err(status) = start_cd_read_at_lba(cd, pack_lba.saturating_add(first), &mut polls) {
        for s in out_status.iter_mut() {
            *s = status;
        }
        cleanup_read_stream(cd, &mut polls);
        return;
    }

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
        // Read and discard any gap sectors before this chunk so the single
        // continuous ReadN stays aligned to chunk boundaries. This trades
        // bandwidth for a seek and only wins while the gap is under the
        // break-even; a longer gap means the caller's chunk ordering is wrong
        // for this read, and discarding it would cost more than reseeking.
        if plan.sector_offset.saturating_sub(cur) as usize > SEEK_BREAK_EVEN_SECTORS {
            out_status[i] = STATUS_DATA_TIMEOUT;
            i += 1;
            continue;
        }
        while cur < plan.sector_offset {
            if read_one_sector_blocking(cd, &mut polls).is_err() {
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
            if let Err(status) = read_one_sector_blocking(cd, &mut polls) {
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
                let buffer = cd.sector_buffer.as_ptr();
                // SAFETY: `copy_len <= SECTOR_BYTES` readable bytes sit
                // in the sector buffer; the destination range was bounds-
                // checked against `cache` just above and cannot overlap
                // the controller's own sector buffer.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        buffer as *const u8,
                        cache_ptr.add(dst_off),
                        copy_len,
                    );
                    checksum = checksum_bytes(buffer as *const u8, copy_len, checksum);
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
    cleanup_read_stream(cd, &mut polls);
}

/// Host stub: no CD hardware, so streamed UI is unsupported (matches
/// `read_chunk_blocking`).
#[cfg(not(target_arch = "mips"))]
pub fn read_chunks_contiguous(
    _cd: &mut CdController,
    _pack_lba: u32,
    plans: &[UiChunkPlan],
    _cache: &mut [u32],
    out_status: &mut [u32],
) {
    let n = plans.len().min(out_status.len());
    for status in out_status.iter_mut().take(n) {
        *status = STATUS_UNSUPPORTED;
    }
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
#[cfg(target_arch = "mips")]
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
