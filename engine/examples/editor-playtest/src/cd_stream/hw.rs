#[allow(unused_imports)]
use super::*;

#[cfg(target_arch = "mips")]
pub(super) enum WaitOutcome {
    Matched,
    CdError,
    Timeout,
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn prepare_cd_read(polls: &mut u32) -> Result<(), u32> {
    // The engine installs a VBlank-only exception handler. After a real BIOS
    // disc boot, keep CD-ROM at the controller level and poll its IRQ flags
    // manually so DataReady cannot enter an unhandled CPU IRQ storm.
    psx_io::irq::set_mask(1 << psx_io::irq::source::VBLANK);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_enable_irqs();
    cd_ack_all();
    psx_io::dma::enable_channel(psx_io::dma::Channel::Cdrom);
    if !CD_READ_PREPARED {
        // BIOS disc boot has already finished its file load. Do not send Pause
        // before our first stream; on real BIOS boot paths some emulators have
        // no active read command to pause and never acknowledge it. Drain any
        // already-latched data/ack instead.
        drain_pending_irqs(polls);
        cd_ack_all();
        CD_READ_PREPARED = true;
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
pub(super) unsafe fn drain_pending_irqs(polls: &mut u32) {
    let mut i = 0;
    while i < 16 {
        let flag = cd_irq_flag();
        if flag == 0 {
            break;
        }
        ack_unexpected_irq(flag, polls);
        i += 1;
    }
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn start_cd_read_at_lba(lba: u32, polls: &mut u32) -> Result<(), u32> {
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
    cd_enable_irqs();
    Ok(())
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn send_command(
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
    psx_io::write8(CD_IRQ, 0x40);
    cd_write_index(0);
    for &param in params {
        if !wait_parameter_room(polls) {
            cd_set_irq_enable(irq_enable);
            cd_write_index(0);
            return false;
        }
        psx_io::write8(CD_PARAM, param);
    }
    psx_io::write8(CD_RESPONSE, command);
    let ok = match wait_irq(expected_irq, poll_limit, polls) {
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

#[cfg(target_arch = "mips")]
pub(super) unsafe fn wait_parameter_room(polls: &mut u32) -> bool {
    let mut i = 0;
    while i < PARAMETER_ROOM_POLL_LIMIT {
        if psx_io::read8(CD_STATUS) & STATUS_PARAMETER_FIFO_NOT_FULL != 0 {
            return true;
        }
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    false
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn wait_irq(expected: u8, poll_limit: u32, polls: &mut u32) -> WaitOutcome {
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
            ack_unexpected_irq(flag, polls);
        }
        *polls = (*polls).saturating_add(1);
        i += 1;
    }
    WaitOutcome::Timeout
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn ack_unexpected_irq(flag: u8, polls: &mut u32) {
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
pub(super) unsafe fn read_stream_sector(buffer: *mut u32, polls: &mut u32) -> Result<(), u32> {
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

/// Blocking single-sector read. Waits for the next DataReady IRQ,
/// DMAs the sector into `buffer`, and acks. Mirrors the benchmark's
/// `read_stream_sector` but is available outside the benchmark
/// feature so the UI image loader can read whole UI.PAK chunks one
/// sector at a time.
#[cfg(target_arch = "mips")]
pub(super) unsafe fn read_one_sector_blocking(buffer: *mut u32, polls: &mut u32) -> Result<(), u32> {
    match wait_irq(IRQ_DATA_READY, DATA_READY_BLOCKING_POLL_LIMIT, polls) {
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
pub(super) unsafe fn try_read_stream_sector(
    buffer: *mut u32,
    polls: &mut u32,
) -> Result<bool, u32> {
    let flag = cd_irq_flag();
    match flag {
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
        _ if cd_data_fifo_ready() => {
            dma_read_sector(buffer, polls);
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

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn stream_world_pack(buffer: *mut u32, result: &mut BenchResult) {
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

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_arm_data_transfer() {
    cd_write_index(0);
    psx_io::write8(CD_IRQ, 0x80);
    cd_write_index(0);
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cleanup_read_stream(polls: &mut u32) {
    if send_command(CMD_PAUSE, &[], IRQ_ACK, CLEANUP_POLL_LIMIT, polls) {
        let _ = wait_irq(IRQ_COMPLETE, CLEANUP_POLL_LIMIT, polls);
        drain_responses();
        cd_ack(IRQ_COMPLETE);
    }
    cd_ack_all();
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn classify_command_failure(timeout_status: u32) -> u32 {
    if cd_irq_flag() == IRQ_ERROR {
        STATUS_CD_ERROR
    } else {
        timeout_status
    }
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_enable_irqs() {
    cd_write_index(1);
    psx_io::write8(CD_PARAM, 0x1F);
    cd_write_index(0);
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_irq_enable() -> u8 {
    cd_write_index(0);
    let enable = psx_io::read8(CD_IRQ) & 0x1F;
    cd_write_index(0);
    enable
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_set_irq_enable(mask: u8) {
    cd_write_index(1);
    psx_io::write8(CD_PARAM, mask & 0x1F);
    cd_write_index(0);
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_ack_all() {
    cd_write_index(1);
    psx_io::write8(CD_IRQ, 0x5F);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_write_index(0);
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_ack(irq: u8) {
    cd_write_index(1);
    psx_io::write8(CD_IRQ, irq & 0x1F);
    psx_io::irq::ack(1 << psx_io::irq::source::CDROM);
    cd_write_index(0);
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_irq_flag() -> u8 {
    cd_write_index(1);
    let flag = psx_io::read8(CD_IRQ) & 0x1F;
    cd_write_index(0);
    flag
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn drain_responses() {
    cd_write_index(0);
    while psx_io::read8(CD_STATUS) & STATUS_RESPONSE_FIFO_NOT_EMPTY != 0 {
        let _ = psx_io::read8(CD_RESPONSE);
    }
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_data_fifo_ready() -> bool {
    cd_write_index(0);
    let ready = psx_io::read8(CD_STATUS) & STATUS_DATA_FIFO_NOT_EMPTY != 0;
    cd_write_index(0);
    ready
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn cd_write_index(index: u8) {
    psx_io::write8(CD_STATUS, index & 0x03);
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn checksum_sector(ptr: *const u8, checksum: u32) -> u32 {
    checksum_bytes(ptr, SECTOR_BYTES, checksum)
}

#[cfg(target_arch = "mips")]
pub(super) unsafe fn checksum_bytes(ptr: *const u8, len: usize, mut checksum: u32) -> u32 {
    let mut i = 0usize;
    while i < len {
        checksum ^= core::ptr::read_volatile(ptr.add(i)) as u32;
        checksum = checksum.wrapping_mul(FNV_PRIME);
        i += 1;
    }
    checksum
}

#[cfg(all(feature = "cd-stream-benchmark", target_arch = "mips"))]
pub(super) unsafe fn sector_magic_matches(ptr: *const u8) -> bool {
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
pub(super) unsafe fn world_pack_magic_matches(ptr: *const u8) -> bool {
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
pub(super) unsafe fn read_le_u32(ptr: *const u8) -> u32 {
    let b0 = core::ptr::read_volatile(ptr) as u32;
    let b1 = core::ptr::read_volatile(ptr.add(1)) as u32;
    let b2 = core::ptr::read_volatile(ptr.add(2)) as u32;
    let b3 = core::ptr::read_volatile(ptr.add(3)) as u32;
    b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
}

#[cfg(target_arch = "mips")]
pub(super) fn lba_to_bcd_msf(lba: u32) -> (u8, u8, u8) {
    let abs = lba.saturating_add(150);
    let minute = (abs / (60 * 75)) as u8;
    let second = ((abs / 75) % 60) as u8;
    let frame = (abs % 75) as u8;
    (bin_to_bcd(minute), bin_to_bcd(second), bin_to_bcd(frame))
}

#[cfg(target_arch = "mips")]
pub(super) const fn bin_to_bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

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
