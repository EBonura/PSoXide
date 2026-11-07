// SPDX-License-Identifier: GPL-2.0-or-later
//! Display modes, measured rather than looked at.
//!
//! The SDK's GPU-01 fix programs each horizontal resolution's dot clock, so a
//! width spans the whole picture instead of a quarter of it. For each of
//! 256, 320, 368, 384, 512 and 640 pixels this puts the mode on the display,
//! lets it settle and records what the hardware says about itself: the
//! GPUSTAT mode bits, and the number of dot-clock ticks root counter 0 counts
//! in 64 scanlines (root counter 1 on the HBlank clock is the ruler). A mode
//! whose dot clock is wrong shows as a different tick count, with no TV in the
//! loop. 256, 320, 512 and 640 go through the SDK's `DisplayConfig`; it has no
//! preset for 368 or 384, so those two start from the 320 `DisplayConfig` and
//! then write GP1(08h) with the 368-mode bit and GP1(06h) with the 7-clock dot
//! clock themselves (a 384-pixel window is the same mode with a wider range,
//! an assumption these records test rather than know).
//!
//! The interlace case sets 640x480 through `DisplayConfig::R640X480` and
//! counts, over a fixed number of frames, how often the field bit changed and
//! how often GPUSTAT said 480 lines and interlaced.
//!
//! What cannot be measured from the console is whether a TV shows the whole
//! picture; that is on the list of things only a person can judge.

use crate::console_tests::{
    next_frame, record, INTERLACE_COUNT, INTERLACE_RECORD, WIDTH_COUNT, WIDTH_RECORD,
};
use crate::TimingRecord;
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_hw::gpu::gp1;
use psx_io::gpu as gpu_io;
use psx_io::timers::{self, Timer};

/// GPU clocks per pixel and picture width, in screen order.
const WIDTHS: [(u16, i32); WIDTH_COUNT] =
    [(256, 10), (320, 8), (368, 7), (384, 7), (512, 5), (640, 4)];
/// Standard GP1(06h) left edge, and the centre of the standard window.
const X1_STANDARD: i32 = 0x260;
const X_CENTRE: i32 = X1_STANDARD + 1280;
/// GP1(08h) bit 6: the 368-pixel horizontal mode.
const MODE_368: u32 = 0x40;
/// Scanlines the dot clock is counted over.
const LINES: u16 = 64;
/// Frames the interlace case watches.
const FRAMES: u32 = 30;

/// The GP1(06h) window of width index `i`, from the dot clock.
fn window(i: usize) -> (i32, i32) {
    let (pixels, divider) = WIDTHS[i];
    let span = pixels as i32 * divider;
    let x1 = X_CENTRE - span / 2;
    (x1, x1 + span)
}

/// Put width `i` on the display.
fn show_width(gpu: &mut Gpu, i: usize) {
    let config = |r| DisplayConfig::new(VideoMode::Ntsc, r);
    match WIDTHS[i].0 {
        256 => gpu.set_display(config(Resolution::R256X240)),
        320 => gpu.set_display(config(Resolution::R320X240)),
        512 => gpu.set_display(config(Resolution::R512X240)),
        640 => gpu.set_display(config(Resolution::R640X240)),
        _ => {
            gpu.set_display(config(Resolution::R320X240));
            let (x1, x2) = window(i);
            gpu_io::write_display_control(0x0800_0000 | MODE_368);
            gpu_io::write_display_control(gp1::h_display_range(x1 as u32, x2 as u32));
        }
    }
}

/// Dot-clock ticks root counter 0 counts while root counter 1 counts `LINES`
/// HBlanks. Both counters are freed from sync modes first.
fn dots_per_lines() -> u32 {
    timers::set_mode(Timer::Timer0, crate::TIMER_MODE_CLOCK_SOURCE_1);
    timers::set_mode(Timer::Timer1, crate::TIMER_MODE_CLOCK_SOURCE_1);
    timers::set_counter(Timer::Timer0, 0);
    timers::set_counter(Timer::Timer1, 0);
    let mut guard = 0u32;
    while timers::counter(Timer::Timer1) < LINES && guard < 2_000_000 {
        guard += 1;
    }
    let dots = timers::counter(Timer::Timer0) as u32;
    timers::set_mode(Timer::Timer0, 0);
    timers::set_mode(Timer::Timer1, 0);
    if guard >= 2_000_000 {
        0xFFFF
    } else {
        dots
    }
}

/// Step through the six widths and record what each reports.
/// min = GPUSTAT bits 16-31, med = dot-clock ticks in 64 lines, max = the
/// GP1(06h) window span in GPU clocks.
#[inline(never)]
pub(crate) fn run_widths(gpu: &mut Gpu) -> [TimingRecord; WIDTH_COUNT] {
    core::array::from_fn(|k| {
        gpu.set_display_enabled(false);
        show_width(gpu, k);
        gpu.set_display_start((0, 0));
        gpu.set_display_enabled(true);
        for _ in 0..3 {
            next_frame();
        }
        let status = gpu_io::status().bits();
        let dots = dots_per_lines();
        let (x1, x2) = window(k);
        record(
            WIDTH_RECORD + k as u16,
            status >> 16,
            dots,
            (x2 - x1) as u32,
        )
    })
}

/// GPUSTAT bits the interlace case reads: 19 vertical resolution (480
/// lines), 22 interlace, 31 even/odd field.
const STAT_480: u32 = 1 << 19;
const STAT_INTERLACE: u32 = 1 << 22;
const STAT_FIELD: u32 = 1 << 31;

/// 640x480 interlaced, watched for `FRAMES` frames.
/// 0x2D6: GPUSTAT high half, field changes, frames watched.
/// 0x2D7: frames interlaced, frames 480 lines, GPUSTAT low half.
#[inline(never)]
pub(crate) fn run_interlace(gpu: &mut Gpu) -> [TimingRecord; INTERLACE_COUNT] {
    gpu.set_display_enabled(false);
    gpu.set_display(DisplayConfig::new(VideoMode::Ntsc, Resolution::R640X480));
    gpu.set_display_start((0, 0));
    gpu.set_display_enabled(true);
    for _ in 0..3 {
        next_frame();
    }
    let (mut flips, mut interlaced, mut tall) = (0u32, 0u32, 0u32);
    let mut previous = gpu_io::status().bits() & STAT_FIELD;
    let mut status = 0;
    for _ in 0..FRAMES {
        next_frame();
        status = gpu_io::status().bits();
        flips += (status & STAT_FIELD != previous) as u32;
        previous = status & STAT_FIELD;
        interlaced += (status & STAT_INTERLACE != 0) as u32;
        tall += (status & STAT_480 != 0) as u32;
    }
    [
        record(INTERLACE_RECORD, status >> 16, flips, FRAMES),
        record(INTERLACE_RECORD + 1, interlaced, tall, status & 0xFFFF),
    ]
}
