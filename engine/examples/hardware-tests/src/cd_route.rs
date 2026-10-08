// SPDX-License-Identifier: GPL-2.0-or-later
//! CD data reads against the SPU's CD audio route (PA1).
//!
//! Five stages reproduce the game state that produced the real-PS1 noise
//! (data being read while CD audio could reach the mixer), then change one
//! routing control at a time: the drive idle; a read with the game's route
//! off (SPUCNT bit 0 clear, full CD volume); the same with the drive's own
//! mute; the SPU's CD mix bit on; paused again. In each the SPU's CD capture
//! buffer (SPU RAM 0 to 7FFh, left then right) is read back and summarised, so
//! whether CD data leaks into the mixer is a number, not something heard.
//!
//! Records `0x510 + stage`: capture peak left, capture peak right, and a
//! packed state word (bits 0-3 command results, bit 4 SPUCNT bit 0, bits 8-15
//! data sectors serviced). Stage 0 is the control: the drive idle, nothing in
//! the capture buffer, so its peaks are the silence floor.

use crate::console_tests::record;
use crate::{spu_dma_read, TimingRecord};
use psx_io::dma;
use psx_rt::interrupts;
use psx_spu::{self as spu, CdVolume};

/// rec cd_route_stage: capture_peak_left, capture_peak_right, state_word (five records, 0x510-0x514)
pub(crate) const CD_ROUTE_RECORD: u16 = 0x510;

const STAGE_COUNT: usize = 5;
/// Frames a stage runs, and the frame its capture buffer is read.
const STAGE_FRAMES: u16 = 40;
const CAPTURE_FRAME: u16 = 30;
const CDTEST_LBA: u32 = 564;
const SECTOR_WORDS: usize = 2048 / 4;
const CAPTURE_WORDS: usize = 0x800 / 4;
const CAPTURE_HALF_WORDS: usize = 0x400 / 4;

const CD_STATUS: u32 = psx_hw::cd::BASE;
const CD_REQUEST_IRQ: u32 = psx_hw::cd::BASE + 3;
const CD_STATUS_DATA_READY: u8 = 1 << 6;
const CD_IRQ_DATA_READY: u8 = 1;

static mut SECTOR_BUFFER: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];
static mut CAPTURE_BUFFER: [u32; CAPTURE_WORDS] = [0; CAPTURE_WORDS];

struct State {
    command_state: u8,
    sectors: u16,
}

fn apply_stage(stage: usize) -> u8 {
    match stage {
        0 => {
            spu::enable_cd_audio(false);
            let _ = psx_io::cd::try_unmute(200_000);
            1
        }
        1 => {
            spu::enable_cd_audio(false);
            let demute = psx_io::cd::try_unmute(200_000).is_some();
            let mode = psx_io::cd::try_set_mode(psx_hw::cd::MODE_DOUBLE_SPEED, 200_000).is_some();
            let loc = psx_io::cd::try_set_target_lba(CDTEST_LBA, 200_000).is_some();
            let read = psx_io::cd::try_start_reading(200_000).is_some();
            demute as u8 | ((mode as u8) << 1) | ((loc as u8) << 2) | ((read as u8) << 3)
        }
        2 => {
            spu::enable_cd_audio(false);
            psx_io::cd::try_mute(200_000).is_some() as u8
        }
        3 => {
            let demute = psx_io::cd::try_unmute(200_000).is_some();
            spu::enable_cd_audio(true);
            demute as u8
        }
        _ => {
            spu::enable_cd_audio(false);
            let paused = psx_io::cd::try_pause_until_complete(200_000);
            let demute = psx_io::cd::try_unmute(200_000).is_some();
            paused as u8 | ((demute as u8) << 1)
        }
    }
}

fn service_cd_sector(state: &mut State) {
    let irq = cd_irq_flag();
    // SAFETY: plain controller status read.
    let status = unsafe { psx_io::read_u8(CD_STATUS) };
    if irq != CD_IRQ_DATA_READY && status & CD_STATUS_DATA_READY == 0 {
        return;
    }
    cd_select(0);
    // SAFETY: controller register write.
    unsafe { psx_io::write_u8(CD_REQUEST_IRQ, 0x80) };
    dma::enable_channel(dma::Channel::Cd);
    // SAFETY: the transfer touches only memory this probe owns, which stays
    // live until the channel is waited idle below.
    unsafe {
        dma::raw::set_address(
            dma::Channel::Cd,
            core::ptr::addr_of_mut!(SECTOR_BUFFER) as u32,
        );
        dma::raw::set_size(dma::Channel::Cd, dma::size_words(SECTOR_WORDS as u16));
        dma::raw::set_control(dma::Channel::Cd, 0x1140_0100);
    }
    let mut guard = 0u32;
    while dma::is_busy(dma::Channel::Cd) && guard < 1_000_000 {
        guard += 1;
    }
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::DMA);
    cd_select(1);
    // SAFETY: controller register write.
    unsafe { psx_io::write_u8(CD_REQUEST_IRQ, CD_IRQ_DATA_READY) };
    psx_io::irq::acknowledge(1 << psx_hw::irq::source::CDROM);
    cd_select(0);
    state.sectors = state.sectors.saturating_add(1);
}

/// Peak magnitude of a capture half.
fn peak(words: &[u32]) -> u32 {
    let mut peak = 0u32;
    for &word in words {
        for raw in [(word & 0xFFFF) as u16, (word >> 16) as u16] {
            peak = peak.max((raw as i16 as i32).unsigned_abs());
        }
    }
    peak
}

/// Run the five stages.
pub(crate) fn run() -> [TimingRecord; STAGE_COUNT] {
    let mut out = [record(CD_ROUTE_RECORD, 0, 0, 0); STAGE_COUNT];
    spu::set_cd_volume(CdVolume::MAX, CdVolume::MAX);
    spu::enable_cd_audio(false);
    let _ = psx_io::cd::try_pause_until_complete(200_000);
    let _ = psx_io::cd::try_unmute(200_000);
    let mut state = State {
        command_state: 0,
        sectors: 0,
    };
    for (stage, slot) in out.iter_mut().enumerate() {
        state.command_state = apply_stage(stage);
        let mut left = 0;
        let mut right = 0;
        let mut cnt = 0;
        for frame in 0..STAGE_FRAMES {
            if (1..=3).contains(&stage) {
                service_cd_sector(&mut state);
            }
            if frame == CAPTURE_FRAME {
                // SAFETY: the buffer is this probe's.
                let buffer = unsafe { &mut *core::ptr::addr_of_mut!(CAPTURE_BUFFER) };
                // One contiguous DMA read avoids an artificial boundary
                // between the adjacent CD-L and CD-R capture rings.
                spu_dma_read(0, buffer);
                left = peak(&buffer[..CAPTURE_HALF_WORDS]);
                right = peak(&buffer[CAPTURE_HALF_WORDS..]);
                // SAFETY: plain SPU register read.
                cnt = unsafe { psx_io::read_u16(psx_hw::spu::SPUCNT) };
            }
            interrupts::wait_vblank();
        }
        *slot = record(
            CD_ROUTE_RECORD + stage as u16,
            left,
            right,
            (state.command_state & 0xF) as u32
                | (((cnt & 1) as u32) << 4)
                | ((state.sectors.min(255) as u32) << 8),
        );
    }
    let _ = psx_io::cd::try_pause_until_complete(200_000);
    spu::enable_cd_audio(false);
    out
}

fn cd_select(index: u8) {
    // SAFETY: controller index select.
    unsafe { psx_io::write_u8(CD_STATUS, index & 3) };
}

fn cd_irq_flag() -> u8 {
    cd_select(1);
    // SAFETY: controller flag read.
    let flag = unsafe { psx_io::read_u8(CD_REQUEST_IRQ) } & 0x1F;
    cd_select(0);
    flag
}
