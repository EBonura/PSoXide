#![cfg_attr(not(target_arch = "mips"), allow(dead_code))]

use psx_engine::telemetry;
use psx_level::LevelWorldPackEntryRecord;

const CD_BASE: u32 = 0x1F80_1800;
const CD_STATUS: u32 = CD_BASE;
const CD_RESPONSE: u32 = CD_BASE + 1;
const CD_PARAM: u32 = CD_BASE + 2;
const CD_IRQ: u32 = CD_BASE + 3;

const STATUS_RESPONSE_FIFO_NOT_EMPTY: u8 = 1 << 5;

const IRQ_DATA_READY: u8 = 1;
const IRQ_COMPLETE: u8 = 2;
const IRQ_ACK: u8 = 3;
const IRQ_DATA_END: u8 = 4;
const IRQ_ERROR: u8 = 5;

const CMD_GETSTAT: u8 = 0x01;
const CMD_SETLOC: u8 = 0x02;
const CMD_READN: u8 = 0x06;
const CMD_PAUSE: u8 = 0x09;
const CMD_SETMODE: u8 = 0x0E;

const DRIVE_STATUS_MOTOR_ON: u8 = 1 << 1;
const DRIVE_STATUS_SHELL_OPEN: u8 = 1 << 4;

const CD_MODE_DOUBLE_SPEED_2048: u8 = 0x80;
#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_LBA: u32 = 22;
#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_SECTORS: usize = 32;
#[cfg(feature = "cd-stream-benchmark")]
const CD_STREAM_BENCH_MAGIC: [u8; 8] = *b"PSOXSTRM";
#[cfg(feature = "cd-stream-benchmark")]
const WORLD_PACK_MAGIC: [u8; 8] = *b"PSOXWPAK";
#[cfg(feature = "cd-stream-benchmark")]
const WORLD_PACK_MAX_SECTORS: u32 = 512;
const SECTOR_BYTES: usize = 2048;
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

pub const ROOM_CHUNK_STATUS_OK: u32 = STATUS_OK;

const COMMAND_ACK_POLL_LIMIT: u32 = 16_384;
#[cfg(feature = "cd-stream-benchmark")]
const DATA_READY_POLL_LIMIT: u32 = 1_000_000;
#[cfg(target_arch = "mips")]
const DATA_READY_STALL_POLL_LIMIT: u32 = 4096;
const DMA_POLL_LIMIT: u32 = 65_536;
const CLEANUP_POLL_LIMIT: u32 = 16_384;

static mut CD_STREAM_SECTOR_BUFFER: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
#[cfg(target_arch = "mips")]
static mut CD_READ_PREPARED: bool = false;

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

#[derive(Clone, Copy)]
pub struct RoomChunkLoadResult {
    pub status: u32,
    pub bytes: usize,
    pub sectors: u32,
}

#[derive(Clone, Copy)]
pub struct WorldChunkInfo {
    pub sector_offset: u32,
    pub sector_count: u32,
    pub byte_size: usize,
    pub checksum: u32,
}

impl WorldChunkInfo {
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
    data_wait_polls: u32,
    world_pack_lba: u32,
    result: RoomChunkLoadResult,
    state: WorldRoomSlotsReadState,
}

impl<const N: usize> WorldRoomSlotsReadJob<N> {
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
            data_wait_polls: 0,
            world_pack_lba: 0,
            result: RoomChunkLoadResult {
                status: STATUS_OK,
                bytes: 0,
                sectors: 0,
            },
            state: WorldRoomSlotsReadState::Idle,
        }
    }

    pub fn start<const SLOT_BYTES: usize>(
        &mut self,
        world_pack_lba: u32,
        toc: &[LevelWorldPackEntryRecord],
        room_ids: &[u16],
        slot_indices: &[usize],
    ) {
        *self = Self::new();
        self.count = room_ids.len().min(slot_indices.len()).min(N);
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
            return;
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
                    Some(entry) if entry.byte_size as usize <= SLOT_BYTES => {
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

    pub fn poll_words<const SLOT_WORDS: usize>(
        &mut self,
        dst: &mut [[u32; SLOT_WORDS]; N],
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
            let _ = dst;
            self.fail_all(STATUS_UNSUPPORTED);
            self.state = WorldRoomSlotsReadState::Done;
            telemetry::counter(telemetry::counter::CD_ROOM_CHUNK_STATUS, self.result.status);
            return self.result;
        }

        #[cfg(target_arch = "mips")]
        unsafe {
            telemetry::stage_begin(telemetry::stage::CD_ROOM_CHUNK_LOAD);
            let before_sectors = self.result.sectors;
            let mut polls = 0;
            let mut sectors_this_poll = 0usize;
            while sectors_this_poll < max_sectors && self.state != WorldRoomSlotsReadState::Done {
                if self.state == WorldRoomSlotsReadState::Ready {
                    if !self.begin_next_group(&mut polls) {
                        break;
                    }
                    if sectors_this_poll == 0 {
                        break;
                    }
                }

                if self.state != WorldRoomSlotsReadState::Reading {
                    break;
                }

                let buffer = core::ptr::addr_of_mut!(CD_STREAM_SECTOR_BUFFER) as *mut u32;
                match try_read_stream_sector(buffer, &mut polls) {
                    Ok(true) => {
                        self.data_wait_polls = 0;
                    }
                    Ok(false) => {
                        self.data_wait_polls = self.data_wait_polls.saturating_add(1);
                        if self.data_wait_polls > DATA_READY_STALL_POLL_LIMIT {
                            self.fail_all(STATUS_DATA_TIMEOUT);
                            cleanup_read_stream(&mut polls);
                            self.state = WorldRoomSlotsReadState::Done;
                        }
                        break;
                    }
                    Err(status) => {
                        self.fail_all(status);
                        cleanup_read_stream(&mut polls);
                        self.state = WorldRoomSlotsReadState::Done;
                        break;
                    }
                }
                copy_window_info_sector(
                    buffer as *const u8,
                    self.sector_offset,
                    &self.entries[..self.count],
                    &self.slot_indices[..self.count],
                    dst,
                    &mut self.byte_counts,
                    &mut self.checksums,
                );
                self.result.sectors = self.result.sectors.saturating_add(1);
                sectors_this_poll += 1;
                self.sector_offset = self.sector_offset.saturating_add(1);

                if self.sector_offset >= self.group_end {
                    cleanup_read_stream(&mut polls);
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
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            WorldRoomSlotsReadState::Ready | WorldRoomSlotsReadState::Reading
        )
    }

    pub fn abort(&mut self) {
        if self.is_active() {
            #[cfg(target_arch = "mips")]
            unsafe {
                let mut polls = 0;
                cleanup_read_stream(&mut polls);
            }
        }
        *self = Self::new();
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, WorldRoomSlotsReadState::Done)
    }

    pub fn byte_counts(&self) -> &[usize; N] {
        &self.byte_counts
    }

    pub fn statuses(&self) -> &[u32; N] {
        &self.statuses
    }

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
    unsafe fn begin_next_group(&mut self, polls: &mut u32) -> bool {
        let Some((group_start, group_end, group_entries)) = next_world_pack_info_read_group(
            &self.entries,
            &self.statuses,
            &self.processed,
            self.count,
        ) else {
            self.finish();
            return false;
        };
        if let Err(status) = prepare_cd_read(polls) {
            self.fail_all(status);
            self.state = WorldRoomSlotsReadState::Done;
            return false;
        }
        if let Err(status) =
            start_cd_read_at_lba(self.world_pack_lba.saturating_add(group_start), polls)
        {
            self.fail_all(status);
            self.state = WorldRoomSlotsReadState::Done;
            return false;
        }
        self.group_start = group_start;
        self.group_end = group_end;
        self.sector_offset = group_start;
        self.data_wait_polls = 0;
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

#[cfg(feature = "cd-stream-benchmark")]
pub fn run_benchmark() {
    telemetry::stage_begin(telemetry::stage::CD_STREAM_BENCH);
    let result = run_benchmark_inner();
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
fn run_benchmark_inner() -> BenchResult {
    BenchResult::unsupported()
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
fn run_benchmark_inner() -> BenchResult {
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

    unsafe {
        cd_enable_irqs();
        cd_ack_all();
        let Some(stat) = get_stat(&mut result.polls) else {
            result.status = STATUS_CD_ERROR;
            return result;
        };
        if stat & DRIVE_STATUS_MOTOR_ON == 0 || stat & DRIVE_STATUS_SHELL_OPEN != 0 {
            result.status = STATUS_CD_ERROR;
            return result;
        }
        if !send_command(
            CMD_SETMODE,
            &[CD_MODE_DOUBLE_SPEED_2048],
            IRQ_ACK,
            COMMAND_ACK_POLL_LIMIT,
            &mut result.polls,
        ) {
            result.status = classify_command_failure(STATUS_SETMODE_TIMEOUT);
            return result;
        }

        let (minute, second, frame) = lba_to_bcd_msf(CD_STREAM_BENCH_LBA);
        if !send_command(
            CMD_SETLOC,
            &[minute, second, frame],
            IRQ_ACK,
            COMMAND_ACK_POLL_LIMIT,
            &mut result.polls,
        ) {
            result.status = classify_command_failure(STATUS_SETLOC_TIMEOUT);
            return result;
        }

        if !send_command(
            CMD_READN,
            &[],
            IRQ_ACK,
            COMMAND_ACK_POLL_LIMIT,
            &mut result.polls,
        ) {
            result.status = classify_command_failure(STATUS_READ_ACK_TIMEOUT);
            return result;
        }

        let buffer = core::ptr::addr_of_mut!(CD_STREAM_SECTOR_BUFFER) as *mut u32;
        psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);

        let mut sector = 0usize;
        let mut steady_stage_open = false;
        while sector < CD_STREAM_BENCH_SECTORS {
            if sector == 2 {
                telemetry::stage_begin(telemetry::stage::CD_STREAM_STEADY);
                steady_stage_open = true;
            }
            if let Err(status) = read_stream_sector(buffer, &mut result.polls) {
                if steady_stage_open {
                    telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
                }
                result.status = status;
                return result;
            }

            result.checksum = checksum_sector(buffer as *const u8, result.checksum);
            result.sectors = result.sectors.saturating_add(1);
            result.bytes = result.bytes.saturating_add(SECTOR_BYTES as u32);
            if steady_stage_open {
                result.steady_sectors = result.steady_sectors.saturating_add(1);
                result.steady_bytes = result.steady_bytes.saturating_add(SECTOR_BYTES as u32);
            }

            if sector == 0 && !sector_magic_matches(buffer as *const u8) {
                result.status = STATUS_MAGIC_MISMATCH;
                break;
            }
            sector += 1;
        }
        if steady_stage_open {
            telemetry::stage_end(telemetry::stage::CD_STREAM_STEADY);
        }

        if result.status == STATUS_OK {
            stream_world_pack(buffer, &mut result);
        }

        cleanup_read_stream(&mut result.polls);
    }

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

#[cfg(target_arch = "mips")]
unsafe fn copy_window_info_sector<const N: usize, const SLOT_WORDS: usize>(
    sector_ptr: *const u8,
    sector_offset: u32,
    entries: &[WorldChunkInfo],
    slot_indices: &[usize],
    dst: &mut [[u32; SLOT_WORDS]; N],
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
                let dst_ptr = dst[dst_slot]
                    .as_mut_ptr()
                    .cast::<u8>()
                    .add(chunk_byte_offset);
                core::ptr::copy_nonoverlapping(sector_ptr, dst_ptr, copy_len);
                checksums[i] = checksum_bytes(sector_ptr, copy_len, checksums[i]);
                byte_counts[i] = byte_counts[i].saturating_add(copy_len);
            }
        }
        i += 1;
    }
}

#[cfg(target_arch = "mips")]
enum WaitOutcome {
    Matched,
    CdError,
    Timeout,
}

#[cfg(target_arch = "mips")]
unsafe fn prepare_cd_read(polls: &mut u32) -> Result<(), u32> {
    cd_enable_irqs();
    cd_ack_all();
    psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);
    if !CD_READ_PREPARED {
        // The BIOS/fast-boot EXE loader can leave an old CD read
        // stream or data-ready state behind. Stop it once before
        // our first direct sector read so random WORLD.PAK loads do
        // not consume stale sector data.
        cleanup_read_stream(polls);
        cd_ack_all();
        CD_READ_PREPARED = true;
    }
    let Some(stat) = get_stat(polls) else {
        return Err(STATUS_CD_ERROR);
    };
    if stat & DRIVE_STATUS_MOTOR_ON == 0 || stat & DRIVE_STATUS_SHELL_OPEN != 0 {
        return Err(STATUS_CD_ERROR);
    }
    if !send_command(
        CMD_SETMODE,
        &[CD_MODE_DOUBLE_SPEED_2048],
        IRQ_ACK,
        COMMAND_ACK_POLL_LIMIT,
        polls,
    ) {
        return Err(classify_command_failure(STATUS_SETMODE_TIMEOUT));
    }
    Ok(())
}

#[cfg(target_arch = "mips")]
unsafe fn start_cd_read_at_lba(lba: u32, polls: &mut u32) -> Result<(), u32> {
    let (minute, second, frame) = lba_to_bcd_msf(lba);
    if !send_command(
        CMD_SETLOC,
        &[minute, second, frame],
        IRQ_ACK,
        COMMAND_ACK_POLL_LIMIT,
        polls,
    ) {
        return Err(classify_command_failure(STATUS_SETLOC_TIMEOUT));
    }
    if !send_command(CMD_READN, &[], IRQ_ACK, COMMAND_ACK_POLL_LIMIT, polls) {
        return Err(classify_command_failure(STATUS_READ_ACK_TIMEOUT));
    }
    Ok(())
}

#[cfg(target_arch = "mips")]
unsafe fn get_stat(polls: &mut u32) -> Option<u8> {
    cd_write_index(0);
    psx_io::write8(CD_IRQ, 0x40);
    psx_io::write8(CD_RESPONSE, CMD_GETSTAT);
    match wait_irq(IRQ_ACK, COMMAND_ACK_POLL_LIMIT, polls) {
        WaitOutcome::Matched => {
            cd_write_index(0);
            let stat = if psx_io::read8(CD_STATUS) & STATUS_RESPONSE_FIFO_NOT_EMPTY != 0 {
                Some(psx_io::read8(CD_RESPONSE))
            } else {
                None
            };
            drain_responses();
            cd_ack(IRQ_ACK);
            stat
        }
        WaitOutcome::CdError | WaitOutcome::Timeout => {
            drain_responses();
            cd_ack_all();
            None
        }
    }
}

#[cfg(target_arch = "mips")]
unsafe fn send_command(
    command: u8,
    params: &[u8],
    expected_irq: u8,
    poll_limit: u32,
    polls: &mut u32,
) -> bool {
    cd_write_index(0);
    psx_io::write8(CD_IRQ, 0x40);
    for &param in params {
        psx_io::write8(CD_PARAM, param);
    }
    psx_io::write8(CD_RESPONSE, command);
    match wait_irq(expected_irq, poll_limit, polls) {
        WaitOutcome::Matched => {
            drain_responses();
            cd_ack(expected_irq);
            true
        }
        WaitOutcome::CdError => {
            drain_responses();
            cd_ack_all();
            false
        }
        WaitOutcome::Timeout => false,
    }
}

#[cfg(target_arch = "mips")]
unsafe fn wait_irq(expected: u8, poll_limit: u32, polls: &mut u32) -> WaitOutcome {
    let mut i = 0;
    while i < poll_limit {
        let flag = cd_irq_flag();
        if flag == expected {
            return WaitOutcome::Matched;
        }
        if flag == IRQ_ERROR {
            return WaitOutcome::CdError;
        }
        if flag != 0 {
            ack_unexpected_irq(flag, polls);
        }
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    WaitOutcome::Timeout
}

#[cfg(target_arch = "mips")]
unsafe fn ack_unexpected_irq(flag: u8, polls: &mut u32) {
    match flag {
        IRQ_DATA_READY => {
            let buffer = core::ptr::addr_of_mut!(CD_STREAM_SECTOR_BUFFER) as *mut u32;
            dma_read_sector(buffer, polls);
            drain_responses();
            cd_ack(IRQ_DATA_READY);
        }
        IRQ_COMPLETE | IRQ_ACK | IRQ_DATA_END => {
            drain_responses();
            cd_ack(flag);
        }
        _ => {
            drain_responses();
            cd_ack_all();
        }
    }
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn read_stream_sector(buffer: *mut u32, polls: &mut u32) -> Result<(), u32> {
    match wait_irq(IRQ_DATA_READY, DATA_READY_POLL_LIMIT, polls) {
        WaitOutcome::Matched => {}
        WaitOutcome::CdError => {
            drain_responses();
            cd_ack_all();
            return Err(STATUS_CD_ERROR);
        }
        WaitOutcome::Timeout => return Err(STATUS_DATA_TIMEOUT),
    }
    dma_read_sector(buffer, polls);
    drain_responses();
    cd_ack(IRQ_DATA_READY);
    Ok(())
}

#[cfg(target_arch = "mips")]
unsafe fn try_read_stream_sector(buffer: *mut u32, polls: &mut u32) -> Result<bool, u32> {
    match cd_irq_flag() {
        IRQ_DATA_READY => {
            dma_read_sector(buffer, polls);
            drain_responses();
            cd_ack(IRQ_DATA_READY);
            Ok(true)
        }
        IRQ_ERROR => {
            drain_responses();
            cd_ack_all();
            Err(STATUS_CD_ERROR)
        }
        IRQ_ACK | IRQ_COMPLETE => {
            // A late command acknowledgement/completion can otherwise keep the
            // drive IRQ flag occupied forever and starve the pending DataReady.
            let stale_irq = cd_irq_flag();
            drain_responses();
            cd_ack(stale_irq);
            Ok(false)
        }
        _ => {
            *polls = (*polls).saturating_add(1);
            Ok(false)
        }
    }
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn stream_world_pack(buffer: *mut u32, result: &mut BenchResult) {
    telemetry::stage_begin(telemetry::stage::CD_WORLD_PACK_STREAM);
    result.world_status = STATUS_OK;
    let mut checksum = FNV_OFFSET;

    if let Err(status) = read_stream_sector(buffer, &mut result.polls) {
        result.world_status = status;
        telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
        return;
    }
    let sector = buffer as *const u8;
    checksum = checksum_sector(sector, checksum);
    result.world_bytes = result.world_bytes.saturating_add(SECTOR_BYTES as u32);
    result.world_sectors = result.world_sectors.saturating_add(1);

    if !world_pack_magic_matches(sector) {
        result.world_status = STATUS_MAGIC_MISMATCH;
        result.world_checksum = checksum;
        telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
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
        result.world_checksum = checksum;
        telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
        return;
    }
    result.world_chunks = chunk_count;

    let mut sector_index = 1;
    while sector_index < total_sectors {
        if let Err(status) = read_stream_sector(buffer, &mut result.polls) {
            result.world_status = status;
            break;
        }
        checksum = checksum_sector(buffer as *const u8, checksum);
        result.world_bytes = result.world_bytes.saturating_add(SECTOR_BYTES as u32);
        result.world_sectors = result.world_sectors.saturating_add(1);
        sector_index += 1;
    }
    result.world_checksum = checksum;
    telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
}

#[cfg(target_arch = "mips")]
unsafe fn dma_read_sector(buffer: *mut u32, polls: &mut u32) {
    psx_io::dma::set_madr(psx_io::dma::Channel::Cdrom, buffer as u32);
    psx_io::dma::set_bcr_manual(psx_io::dma::Channel::Cdrom, SECTOR_WORDS as u16);
    // Matches the BIOS-style burst control word that the emulator
    // models at Redux's quarter-rate CD DMA completion cadence.
    psx_io::dma::set_chcr(psx_io::dma::Channel::Cdrom, 0x1140_0100);
    let mut i = 0;
    while psx_io::dma::is_busy(psx_io::dma::Channel::Cdrom) && i < DMA_POLL_LIMIT {
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    psx_io::irq::ack(1 << psx_io::irq::source::DMA);
}

#[cfg(target_arch = "mips")]
unsafe fn cleanup_read_stream(polls: &mut u32) {
    if send_command(CMD_PAUSE, &[], IRQ_ACK, CLEANUP_POLL_LIMIT, polls) {
        let _ = wait_irq(IRQ_COMPLETE, CLEANUP_POLL_LIMIT, polls);
        drain_responses();
        cd_ack(IRQ_COMPLETE);
    }
    cd_ack_all();
}

#[cfg(target_arch = "mips")]
unsafe fn classify_command_failure(timeout_status: u32) -> u32 {
    if cd_irq_flag() == IRQ_ERROR {
        STATUS_CD_ERROR
    } else {
        timeout_status
    }
}

#[cfg(target_arch = "mips")]
unsafe fn cd_enable_irqs() {
    cd_write_index(1);
    psx_io::write8(CD_PARAM, 0x1F);
}

#[cfg(target_arch = "mips")]
unsafe fn cd_ack_all() {
    cd_write_index(1);
    psx_io::write8(CD_IRQ, 0x5F);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
}

#[cfg(target_arch = "mips")]
unsafe fn cd_ack(irq: u8) {
    cd_write_index(1);
    psx_io::write8(CD_IRQ, irq & 0x1F);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
}

#[cfg(target_arch = "mips")]
unsafe fn cd_irq_flag() -> u8 {
    cd_write_index(1);
    psx_io::read8(CD_IRQ) & 0x1F
}

#[cfg(target_arch = "mips")]
unsafe fn drain_responses() {
    cd_write_index(0);
    while psx_io::read8(CD_STATUS) & STATUS_RESPONSE_FIFO_NOT_EMPTY != 0 {
        let _ = psx_io::read8(CD_RESPONSE);
    }
}

#[cfg(target_arch = "mips")]
unsafe fn cd_write_index(index: u8) {
    psx_io::write8(CD_STATUS, index & 0x03);
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn checksum_sector(ptr: *const u8, checksum: u32) -> u32 {
    checksum_bytes(ptr, SECTOR_BYTES, checksum)
}

#[cfg(target_arch = "mips")]
unsafe fn checksum_bytes(ptr: *const u8, len: usize, mut checksum: u32) -> u32 {
    let mut i = 0usize;
    while i < len {
        checksum ^= core::ptr::read_volatile(ptr.add(i)) as u32;
        checksum = checksum.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    checksum
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn sector_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < CD_STREAM_BENCH_MAGIC.len() {
        if core::ptr::read_volatile(ptr.add(i)) != CD_STREAM_BENCH_MAGIC[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn world_pack_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < WORLD_PACK_MAGIC.len() {
        if core::ptr::read_volatile(ptr.add(i)) != WORLD_PACK_MAGIC[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
unsafe fn read_le_u32(ptr: *const u8) -> u32 {
    let b0 = core::ptr::read_volatile(ptr) as u32;
    let b1 = core::ptr::read_volatile(ptr.add(1)) as u32;
    let b2 = core::ptr::read_volatile(ptr.add(2)) as u32;
    let b3 = core::ptr::read_volatile(ptr.add(3)) as u32;
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

#[cfg(target_arch = "mips")]
fn lba_to_bcd_msf(lba: u32) -> (u8, u8, u8) {
    let abs = lba.saturating_add(150);
    let minute = (abs / (60 * 75)) as u8;
    let second = ((abs / 75) % 60) as u8;
    let frame = (abs % 75) as u8;
    (bin_to_bcd(minute), bin_to_bcd(second), bin_to_bcd(frame))
}

#[cfg(target_arch = "mips")]
const fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

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
