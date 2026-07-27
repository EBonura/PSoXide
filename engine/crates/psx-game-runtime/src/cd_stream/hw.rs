//! CD-ROM controller driver: polled command/IRQ/DMA sequencing over the
//! raw `psx_io` MMIO primitives. Split by safety contract: register
//! polling and acknowledgement helpers are safe functions (fixed CD
//! register addresses, no caller obligations, single-threaded model --
//! the engine installs a VBlank-only exception handler so nothing
//! preempts these sequences); sector movers stay `unsafe fn` because
//! they write a caller-supplied raw buffer.

#[allow(unused_imports)]
use super::*;

#[cfg(target_arch = "mips")]
pub(super) enum WaitOutcome {
    Matched,
    CdError,
    Timeout,
}

/// Bring the controller into the manual-poll read state: mask CPU-level
/// CD IRQs, drain any boot leftovers (first call only), and set
/// double-speed 2048-byte data mode.
#[cfg(target_arch = "mips")]
pub(super) fn prepare_cd_read(cd: &mut CdController, polls: &mut u32) -> Result<(), u32> {
    // The engine installs a VBlank-only exception handler. After a real BIOS
    // disc boot, keep CD-ROM at the controller level and poll its IRQ flags
    // manually so DataReady cannot enter an unhandled CPU IRQ storm.
    psx_io::irq::set_mask(1 << psx_io::irq::source::VBLANK);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_enable_irqs();
    cd_ack_all();
    psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);
    if !cd.read_prepared {
        // BIOS disc boot has already finished its file load. Do not send Pause
        // before our first stream; on real BIOS boot paths some emulators have
        // no active read command to pause and never acknowledge it. Drain any
        // already-latched data/ack instead.
        drain_pending_irqs(cd, polls);
        cd_ack_all();
        cd.read_prepared = true;
    }
    if !send_command(
        cd,
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

/// Acknowledge (and for DataReady, drain) every already-latched CD IRQ,
/// bounded so a stuck flag cannot spin forever.
#[cfg(target_arch = "mips")]
pub(super) fn drain_pending_irqs(cd: &mut CdController, polls: &mut u32) {
    let mut i = 0;
    while i < 16 {
        let flag = cd_irq_flag();
        if flag == 0 {
            break;
        }
        ack_unexpected_irq(cd, flag, polls);
        i += 1;
    }
}

/// Seek to `lba` and start a ReadN stream (SetLoc + ReadN, both
/// ack-polled).
///
/// `lba` is relative to the start of this game's own disc image; on a
/// multi-program disc [`psx_io::disc_base`] shifts it to where that image
/// actually landed.
#[cfg(target_arch = "mips")]
pub(super) fn start_cd_read_at_lba(
    cd: &mut CdController,
    lba: u32,
    polls: &mut u32,
) -> Result<(), u32> {
    let (minute, second, frame) = lba_to_bcd_msf(psx_io::disc_base::shift_lba(lba));
    if !send_command(
        cd,
        CMD_SETLOC,
        &[minute, second, frame],
        IRQ_ACK,
        COMMAND_ACK_POLL_LIMIT,
        polls,
    ) {
        return Err(classify_command_failure(STATUS_SETLOC_TIMEOUT));
    }
    if !send_command(cd, CMD_READN, &[], IRQ_ACK, COMMAND_ACK_POLL_LIMIT, polls) {
        return Err(classify_command_failure(STATUS_READ_ACK_TIMEOUT));
    }
    cd_enable_irqs();
    Ok(())
}

/// Issue one CD command with parameters and wait (polled, bounded) for
/// `expected_irq`, preserving the caller-visible IRQ-enable mask.
#[cfg(target_arch = "mips")]
pub(super) fn send_command(
    cd: &mut CdController,
    command: u8,
    params: &[u8],
    expected_irq: u8,
    poll_limit: u32,
    polls: &mut u32,
) -> bool {
    let irq_enable = cd_irq_enable();
    cd_set_irq_enable(0);
    cd_ack_all();
    cd_write_index(0);
    drain_responses();
    cd_write_index(1);
    // SAFETY: CD_IRQ is a valid CD controller register; 0x40 resets the
    // parameter FIFO (index-1 interrupt-flag register write).
    unsafe { psx_io::write8(CD_IRQ, 0x40) };
    cd_write_index(0);
    for &param in params {
        if !wait_parameter_room(polls) {
            cd_set_irq_enable(irq_enable);
            cd_write_index(0);
            return false;
        }
        // SAFETY: CD_PARAM is the CD parameter-FIFO register (index 0).
        unsafe { psx_io::write8(CD_PARAM, param) };
    }
    // SAFETY: CD_RESPONSE at index 0 is the command register.
    unsafe { psx_io::write8(CD_RESPONSE, command) };
    let ok = match wait_irq(cd, expected_irq, poll_limit, polls) {
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
    };
    cd_set_irq_enable(irq_enable);
    cd_write_index(0);
    ok
}

/// Poll (bounded) until the parameter FIFO reports room.
#[cfg(target_arch = "mips")]
pub(super) fn wait_parameter_room(polls: &mut u32) -> bool {
    let mut i = 0;
    while i < PARAMETER_ROOM_POLL_LIMIT {
        // SAFETY: CD_STATUS is the CD index/status register (read-only here).
        if unsafe { psx_io::read8(CD_STATUS) } & STATUS_PARAMETER_FIFO_NOT_FULL != 0 {
            return true;
        }
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    false
}

/// Poll (bounded) for `expected`, acking and draining any other latched
/// IRQ along the way.
#[cfg(target_arch = "mips")]
pub(super) fn wait_irq(
    cd: &mut CdController,
    expected: u8,
    poll_limit: u32,
    polls: &mut u32,
) -> WaitOutcome {
    let mut i = 0;
    while i < poll_limit {
        let flag = cd_irq_flag();
        if flag == expected {
            return WaitOutcome::Matched;
        }
        if expected == IRQ_DATA_READY && cd_data_fifo_ready() {
            return WaitOutcome::Matched;
        }
        if flag == IRQ_ERROR {
            return WaitOutcome::CdError;
        }
        if flag != 0 {
            ack_unexpected_irq(cd, flag, polls);
        }
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    WaitOutcome::Timeout
}

/// Ack a stray IRQ; a stray DataReady is drained into the controller's
/// sector buffer so the data FIFO cannot wedge the stream.
#[cfg(target_arch = "mips")]
pub(super) fn ack_unexpected_irq(cd: &mut CdController, flag: u8, polls: &mut u32) {
    match flag {
        IRQ_DATA_READY => {
            let buffer = cd.sector_buffer.as_mut_ptr();
            // SAFETY: `buffer` is the controller-owned sector buffer, valid
            // for one whole-sector DMA write; single-threaded access.
            unsafe { dma_read_sector(buffer, polls) };
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

/// Blocking single-sector read for the benchmark stream: wait (bounded)
/// for DataReady, then DMA the sector into the controller's buffer.
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) fn read_stream_sector(cd: &mut CdController, polls: &mut u32) -> Result<(), u32> {
    match wait_irq(cd, IRQ_DATA_READY, DATA_READY_POLL_LIMIT, polls) {
        WaitOutcome::Matched => {}
        WaitOutcome::CdError => {
            drain_responses();
            cd_ack_all();
            return Err(STATUS_CD_ERROR);
        }
        WaitOutcome::Timeout => return Err(STATUS_DATA_TIMEOUT),
    }
    // SAFETY: the controller-owned sector buffer is valid for a sector write.
    unsafe { dma_read_sector(cd.sector_buffer.as_mut_ptr(), polls) };
    drain_responses();
    cd_ack(IRQ_DATA_READY);
    Ok(())
}

/// Blocking single-sector read. Waits for the next DataReady IRQ,
/// DMAs the sector into the controller's buffer, and acks. Mirrors the
/// benchmark's `read_stream_sector` but is available outside the
/// benchmark feature so the UI image loader can read whole UI.PAK
/// chunks one sector at a time.
#[cfg(target_arch = "mips")]
pub(super) fn read_one_sector_blocking(cd: &mut CdController, polls: &mut u32) -> Result<(), u32> {
    match wait_irq(cd, IRQ_DATA_READY, DATA_READY_BLOCKING_POLL_LIMIT, polls) {
        WaitOutcome::Matched => {}
        WaitOutcome::CdError => {
            drain_responses();
            cd_ack_all();
            return Err(STATUS_CD_ERROR);
        }
        WaitOutcome::Timeout => return Err(STATUS_DATA_TIMEOUT),
    }
    // SAFETY: the controller-owned sector buffer is valid for a sector write.
    unsafe { dma_read_sector(cd.sector_buffer.as_mut_ptr(), polls) };
    drain_responses();
    cd_ack(IRQ_DATA_READY);
    Ok(())
}

/// Non-blocking sector poll: `Ok(true)` when a sector landed in the
/// controller's buffer, `Ok(false)` when no data is ready yet.
#[cfg(target_arch = "mips")]
pub(super) fn try_read_stream_sector(cd: &mut CdController, polls: &mut u32) -> Result<bool, u32> {
    let flag = cd_irq_flag();
    match flag {
        IRQ_DATA_READY => {
            // SAFETY: the controller-owned sector buffer is valid for a
            // sector write.
            unsafe { dma_read_sector(cd.sector_buffer.as_mut_ptr(), polls) };
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
        _ if cd_data_fifo_ready() => {
            // SAFETY: the controller-owned sector buffer is valid for a
            // sector write.
            unsafe { dma_read_sector(cd.sector_buffer.as_mut_ptr(), polls) };
            drain_responses();
            cd_ack(IRQ_DATA_READY);
            Ok(true)
        }
        _ => {
            *polls = (*polls).saturating_add(1);
            Ok(false)
        }
    }
}

/// Stream and checksum the whole WORLD.PAK region for the benchmark,
/// validating its header out of the first sector.
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) fn stream_world_pack(cd: &mut CdController, result: &mut BenchResult) {
    telemetry::stage_begin(telemetry::stage::CD_WORLD_PACK_STREAM);
    result.world_status = STATUS_OK;
    let mut checksum = FNV_OFFSET;

    // SAFETY: the controller-owned sector buffer is valid for sector-sized
    // reads/writes; forwarded to the byte checkers below unchanged.
    unsafe {
        if let Err(status) = read_stream_sector(cd, &mut result.polls) {
            result.world_status = status;
            telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
            return;
        }
        let sector = cd.sector_buffer.as_ptr() as *const u8;
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
            if let Err(status) = read_stream_sector(cd, &mut result.polls) {
                result.world_status = status;
                break;
            }
            checksum = checksum_sector(cd.sector_buffer.as_ptr() as *const u8, checksum);
            result.world_bytes = result.world_bytes.saturating_add(SECTOR_BYTES as u32);
            result.world_sectors = result.world_sectors.saturating_add(1);
            sector_index += 1;
        }
    }
    result.world_checksum = checksum;
    telemetry::stage_end(telemetry::stage::CD_WORLD_PACK_STREAM);
}

/// DMA one 2048-byte sector from the CD data FIFO into `buffer`,
/// spinning (bounded) until channel 3 goes idle.
///
/// # Safety
/// `buffer` must be valid for a whole-sector (2048-byte) write.
#[cfg(target_arch = "mips")]
pub(super) unsafe fn dma_read_sector(buffer: *mut u32, polls: &mut u32) {
    cd_arm_data_transfer();
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

/// Request the data transfer (BFRD) so the sector FIFO is DMA-readable.
#[cfg(target_arch = "mips")]
pub(super) fn cd_arm_data_transfer() {
    cd_write_index(0);
    // SAFETY: CD_IRQ at index 0 is the request register; 0x80 sets BFRD.
    unsafe { psx_io::write8(CD_IRQ, 0x80) };
    cd_write_index(0);
}

/// Pause the drive and ack whatever the pause latched, restoring the
/// idle controller state after a stream (best-effort, bounded).
#[cfg(target_arch = "mips")]
pub(super) fn cleanup_read_stream(cd: &mut CdController, polls: &mut u32) {
    if send_command(cd, CMD_PAUSE, &[], IRQ_ACK, CLEANUP_POLL_LIMIT, polls) {
        let _ = wait_irq(cd, IRQ_COMPLETE, CLEANUP_POLL_LIMIT, polls);
        drain_responses();
        cd_ack(IRQ_COMPLETE);
    }
    cd_ack_all();
}

/// Refine a timeout status: a latched error IRQ means the drive
/// reported a real error rather than going silent.
#[cfg(target_arch = "mips")]
pub(super) fn classify_command_failure(timeout_status: u32) -> u32 {
    if cd_irq_flag() == IRQ_ERROR {
        STATUS_CD_ERROR
    } else {
        timeout_status
    }
}

/// Enable all five controller IRQ sources (index-1 enable register).
#[cfg(target_arch = "mips")]
pub(super) fn cd_enable_irqs() {
    cd_write_index(1);
    // SAFETY: CD_PARAM at index 1 is the interrupt-enable register.
    unsafe { psx_io::write8(CD_PARAM, 0x1F) };
    cd_write_index(0);
}

/// Read the controller IRQ-enable mask.
#[cfg(target_arch = "mips")]
pub(super) fn cd_irq_enable() -> u8 {
    cd_write_index(0);
    // SAFETY: CD_IRQ is a valid CD controller register for reads.
    let enable = unsafe { psx_io::read8(CD_IRQ) } & 0x1F;
    cd_write_index(0);
    enable
}

/// Write the controller IRQ-enable mask.
#[cfg(target_arch = "mips")]
pub(super) fn cd_set_irq_enable(mask: u8) {
    cd_write_index(1);
    // SAFETY: CD_PARAM at index 1 is the interrupt-enable register.
    unsafe { psx_io::write8(CD_PARAM, mask & 0x1F) };
    cd_write_index(0);
}

/// Ack every controller IRQ flag (and the CPU-level CDROM IRQ).
#[cfg(target_arch = "mips")]
pub(super) fn cd_ack_all() {
    cd_write_index(1);
    // SAFETY: CD_IRQ at index 1 is the interrupt-flag register; 0x5F
    // acks all flags and resets the parameter FIFO.
    unsafe { psx_io::write8(CD_IRQ, 0x5F) };
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_write_index(0);
}

/// Ack one controller IRQ flag (and the CPU-level CDROM IRQ).
#[cfg(target_arch = "mips")]
pub(super) fn cd_ack(irq: u8) {
    cd_write_index(1);
    // SAFETY: CD_IRQ at index 1 is the interrupt-flag register.
    unsafe { psx_io::write8(CD_IRQ, irq & 0x1F) };
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_write_index(0);
}

/// Read the latched controller IRQ flag (index-1 flag register).
#[cfg(target_arch = "mips")]
pub(super) fn cd_irq_flag() -> u8 {
    cd_write_index(1);
    // SAFETY: CD_IRQ is a valid CD controller register for reads.
    let flag = unsafe { psx_io::read8(CD_IRQ) } & 0x1F;
    cd_write_index(0);
    flag
}

/// Drain the response FIFO until the status register reports it empty.
#[cfg(target_arch = "mips")]
pub(super) fn drain_responses() {
    cd_write_index(0);
    // SAFETY: CD_STATUS/CD_RESPONSE are valid CD controller registers;
    // reading RESPONSE pops the FIFO, which is exactly the intent.
    unsafe {
        while psx_io::read8(CD_STATUS) & STATUS_RESPONSE_FIFO_NOT_EMPTY != 0 {
            let _ = psx_io::read8(CD_RESPONSE);
        }
    }
}

/// Whether the data FIFO reports a sector ready.
#[cfg(target_arch = "mips")]
pub(super) fn cd_data_fifo_ready() -> bool {
    cd_write_index(0);
    // SAFETY: CD_STATUS is a valid CD controller register for reads.
    let ready = unsafe { psx_io::read8(CD_STATUS) } & STATUS_DATA_FIFO_NOT_EMPTY != 0;
    cd_write_index(0);
    ready
}

/// Select the controller register bank (index 0..=3).
#[cfg(target_arch = "mips")]
pub(super) fn cd_write_index(index: u8) {
    // SAFETY: CD_STATUS is the index/status register; writing selects
    // the register bank, a side-effect-free controller state change.
    unsafe { psx_io::write8(CD_STATUS, index & 0x03) };
}

/// FNV-checksum one whole sector at `ptr`.
///
/// # Safety
/// `ptr` must be readable for [`SECTOR_BYTES`] bytes.
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn checksum_sector(ptr: *const u8, checksum: u32) -> u32 {
    // SAFETY: caller guarantees a whole sector is readable at `ptr`.
    unsafe { checksum_bytes(ptr, SECTOR_BYTES, checksum) }
}

/// FNV-checksum `len` bytes at `ptr` (volatile reads: the buffer is
/// DMA-written).
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[cfg(target_arch = "mips")]
pub(super) unsafe fn checksum_bytes(ptr: *const u8, len: usize, mut checksum: u32) -> u32 {
    let mut i = 0usize;
    while i < len {
        // SAFETY: caller guarantees `len` readable bytes at `ptr`.
        checksum ^= unsafe { core::ptr::read_volatile(ptr.add(i)) } as u32;
        checksum = checksum.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    checksum
}

/// Whether the sector at `ptr` opens with the stream-bench magic.
///
/// # Safety
/// `ptr` must be readable for the magic's length.
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn sector_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < CD_STREAM_BENCH_MAGIC.len() {
        // SAFETY: caller guarantees the magic's length readable at `ptr`.
        if unsafe { core::ptr::read_volatile(ptr.add(i)) } != CD_STREAM_BENCH_MAGIC[i] {
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
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn world_pack_magic_matches(ptr: *const u8) -> bool {
    let mut i = 0usize;
    while i < WORLD_PACK_MAGIC.len() {
        // SAFETY: caller guarantees the magic's length readable at `ptr`.
        if unsafe { core::ptr::read_volatile(ptr.add(i)) } != WORLD_PACK_MAGIC[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Read a little-endian u32 from `ptr` (volatile: DMA-written buffer).
///
/// # Safety
/// `ptr` must be readable for 4 bytes.
#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn read_le_u32(ptr: *const u8) -> u32 {
    // SAFETY: caller guarantees 4 readable bytes at `ptr`.
    unsafe {
        let b0 = core::ptr::read_volatile(ptr) as u32;
        let b1 = core::ptr::read_volatile(ptr.add(1)) as u32;
        let b2 = core::ptr::read_volatile(ptr.add(2)) as u32;
        let b3 = core::ptr::read_volatile(ptr.add(3)) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }
}

/// LBA to BCD-coded minute/second/frame (with the 150-sector lead-in).
#[cfg(target_arch = "mips")]
pub(super) fn lba_to_bcd_msf(lba: u32) -> (u8, u8, u8) {
    let abs = lba.saturating_add(150);
    let minute = (abs / (60 * 75)) as u8;
    let second = ((abs / 75) % 60) as u8;
    let frame = (abs % 75) as u8;
    (bin_to_bcd(minute), bin_to_bcd(second), bin_to_bcd(frame))
}

/// Binary 0..=99 to BCD.
#[cfg(target_arch = "mips")]
pub(super) const fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// Host-side reference checksum for the bench pattern.
#[cfg(feature = "cd-stream-benchmark")]
pub(super) fn expected_checksum(sectors: usize) -> u32 {
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
pub(super) const fn expected_byte(index: usize, sectors: usize) -> u8 {
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
