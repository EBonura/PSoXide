// SPDX-License-Identifier: GPL-2.0-or-later
//! The run's progress screen: which area and step is running, and a bar.
//!
//! A run blocks the frame loop, so nothing here goes through the engine's
//! double buffer. Every write is an absolute-coordinate GP0 fill or a font
//! draw into BOTH 240-line buffers (rows 0-239 and 240-479), because which one
//! is on show depends on how many swaps have happened. The step name is drawn
//! BEFORE the step runs, so a hang leaves it on screen.

use crate::{FONT_CLUT, FONT_TPAGE, SUITE_VERSION};
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::Gpu;
use psx_io::gpu as gpu_io;

const BAR_X: u32 = 24;
const BAR_W: u32 = 272;
const BAR_H: u32 = 8;
/// Rows of the text lines and the bar inside one 240-line buffer.
const ROW_AREA: u32 = 146;
const ROW_STEP: u32 = 158;
const ROW_DETAIL: u32 = 170;
const ROW_TEXT_END: u32 = 186;
const ROW_BAR: u32 = 200;
const BUFFERS: [u32; 2] = [0, 240];

static mut STEP_INDEX: u32 = 0;
static mut STEPS_TOTAL: u32 = 1;
static mut SUB_DONE: u32 = 0;
static mut SUB_TOTAL: u32 = 1;

/// A fresh font atlas in its VRAM place.
pub(crate) fn upload_font() -> FontAtlas {
    FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT)
}

fn fill(y: u32, x: u32, w: u32, h: u32, rgb: u32) {
    for base in BUFFERS {
        gpu_io::write_command(0x0200_0000 | rgb);
        gpu_io::write_command(((base + y) << 16) | x);
        gpu_io::write_command((h << 16) | w);
    }
}

fn text(font: &FontAtlas, gpu: &mut Gpu, x: i16, y: i16, s: &str, colour: (u8, u8, u8)) {
    gpu.set_draw_area((0, 0), (1023, 511));
    gpu.set_draw_offset((0, 0));
    for base in BUFFERS {
        font.draw_text(x, base as i16 + y, s, colour);
    }
}

/// Start a run of `total_steps` steps and draw the header on a clear picture.
pub(crate) fn begin_run(font: &FontAtlas, total_steps: usize) {
    // SAFETY: single thread; plain statics.
    unsafe {
        STEP_INDEX = 0;
        STEPS_TOTAL = total_steps.max(1) as u32;
        SUB_DONE = 0;
        SUB_TOTAL = 1;
    }
    gpu_io::wait_command_ready();
    fill(0, 0, 320, 240, 0x0012_0806);
    banner(font);
}

/// The static header, redrawn after a GPU reset.
pub(crate) fn banner(font: &FontAtlas) {
    probe_gpu!(gpu);
    gpu_io::wait_command_ready();
    text(font, gpu, 8, 8, "PS1 HARDWARE TEST", (232, 236, 244));
    text(
        font,
        gpu,
        320 - 8 - SUITE_VERSION.len() as i16 * 8,
        8,
        SUITE_VERSION,
        (112, 136, 170),
    );
    text(
        font,
        gpu,
        8,
        24,
        "ONE LINEAR RUN. HANDS OFF UNLESS ASKED.",
        (150, 170, 200),
    );
    text(
        font,
        gpu,
        8,
        36,
        "IF IT STOPS, THE LINES BELOW SAY WHERE.",
        (150, 170, 200),
    );
    gpu.wait_idle();
    bar();
}

/// Clear the picture and draw the header again, keeping the step counters:
/// for a step that painted over the screen (the GPU load behind the SIO
/// measurements).
pub(crate) fn repaint(font: &FontAtlas) {
    gpu_io::wait_command_ready();
    fill(0, 0, 320, 240, 0x0012_0806);
    banner(font);
}

/// Name the area and step about to run, and move the bar to the step's start.
pub(crate) fn begin_step(
    font: &FontAtlas,
    area_no: usize,
    area_count: usize,
    area_name: &str,
    index: usize,
    step_name: &str,
) {
    probe_gpu!(gpu);
    // SAFETY: single thread; plain statics.
    unsafe {
        STEP_INDEX = index as u32;
        SUB_DONE = 0;
        SUB_TOTAL = 1;
    }
    gpu_io::wait_command_ready();
    fill(ROW_AREA, 8, 304, ROW_TEXT_END - ROW_AREA, 0x0012_0806);
    let mut line = Line::new();
    line.s("AREA ")
        .u(area_no as u32 + 1)
        .s(" OF ")
        .u(area_count as u32)
        .s("  ")
        .s(area_name);
    text(
        font,
        gpu,
        8,
        ROW_AREA as i16,
        line.as_str(),
        (255, 232, 128),
    );
    let mut line = Line::new();
    line.s("STEP ").u(index as u32 + 1).s("  ").s(step_name);
    text(font, gpu, 8, ROW_STEP as i16, line.as_str(), (255, 216, 96));
    gpu.wait_idle();
    bar();
}

/// The line under the step name: the case or record in flight.
pub(crate) fn detail(font: &FontAtlas, group: &str, name: &str) {
    probe_gpu!(gpu);
    gpu_io::wait_command_ready();
    fill(ROW_DETAIL, 8, 304, 12, 0x0012_0806);
    let mut line = Line::new();
    line.s(group).s(": ").s(name);
    text(
        font,
        gpu,
        8,
        ROW_DETAIL as i16,
        line.as_str(),
        (220, 224, 230),
    );
    gpu.wait_idle();
}

/// Progress inside the current step (tests run, records taken).
pub(crate) fn sub(done: usize, total: usize) {
    // SAFETY: single thread; plain statics.
    unsafe {
        SUB_DONE = done as u32;
        SUB_TOTAL = total.max(1) as u32;
    }
    bar();
}

/// Paint the bar from the step index and the sub-progress.
fn bar() {
    // SAFETY: single thread; plain statics.
    let (index, total, done, sub_total) = unsafe { (STEP_INDEX, STEPS_TOTAL, SUB_DONE, SUB_TOTAL) };
    let units = total * 256;
    let position = index * 256 + (done.min(sub_total) * 256 / sub_total);
    let filled = (BAR_W * position / units).min(BAR_W);
    gpu_io::wait_command_ready();
    fill(ROW_BAR, BAR_X, BAR_W, BAR_H, 0x0020_2020);
    if filled != 0 {
        fill(ROW_BAR, BAR_X, filled, BAR_H, 0x00FF_C850);
    }
    gpu_io::wait_command_ready();
}

/// Blink a marker beside the bar once per timing sample, so slow and stuck
/// look different.
pub(crate) fn heartbeat(on: bool) {
    gpu_io::wait_command_ready();
    fill(
        ROW_BAR + 4,
        304,
        16,
        8,
        if on { 0x0040_E0FF } else { 0x0020_2020 },
    );
}

/// Paint the id of the record being measured as sixteen bit-cells under the
/// bar, most significant bit first: bright yellow = 1, dark = 0. A photo of a
/// frozen screen then names the exact record, not just "roughly 40%".
pub(crate) fn record_id(id: u16) {
    for bit in 0..16u32 {
        let rgb = if id & (0x8000 >> bit) != 0 {
            0x0040_E0FF
        } else {
            0x0020_2020
        };
        gpu_io::wait_command_ready();
        // GP0(02h) snaps X and width to 16, so the cells are 16 wide.
        fill(ROW_BAR + 12, 32 + bit * 16, 16, 8, rgb);
    }
    gpu_io::wait_command_ready();
}

/// A line of text built in place.
pub(crate) struct Line {
    buf: [u8; 44],
    len: usize,
}

impl Line {
    pub(crate) const fn new() -> Self {
        Self {
            buf: [0; 44],
            len: 0,
        }
    }

    pub(crate) fn s(&mut self, t: &str) -> &mut Self {
        for b in t.bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }

    pub(crate) fn u(&mut self, mut v: u32) -> &mut Self {
        let mut digits = [0u8; 10];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        for &b in &digits[at..] {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        self
    }

    pub(crate) fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("")
    }
}
