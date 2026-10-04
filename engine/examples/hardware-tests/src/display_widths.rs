// SPDX-License-Identifier: GPL-2.0-or-later
//! CONSOLE TESTS: DISPLAY WIDTHS and 480I INTERLACE.
//!
//! The SDK's GPU-01 fix programs each horizontal resolution's dot clock, so a
//! width spans the whole picture instead of a quarter of it. The emulator
//! cannot show that bug, so this is for a TV: a pattern per width with
//! markers on the extreme left and right pixels, 1-pixel stripes beside them,
//! and a ruler along the top and bottom, so cropping or black bands at
//! either edge can be read off the picture.
//!
//! The pattern steps through 256, 320, 368, 384, 512 and 640 pixels, five
//! seconds each. 256, 320, 512 and 640 go through the SDK's
//! `psx_gpu::display::DisplayConfig`. It has no preset for 368 or 384, so
//! those two start from the 320 `DisplayConfig` and then write GP1(08h) with
//! the 368-mode bit and GP1(06h) with the 7-clock dot clock themselves (368
//! mode is 7 GPU clocks a pixel; a 384-pixel window is the same mode with a
//! wider GP1(06h) range, an assumption this case tests rather than knows).
//!
//! The interlace case shows 640x480 through `DisplayConfig::R640X480`:
//! alternate lines red and blue, so a display that shows both fields shows
//! both colours and one that shows a single field shows one, plus live
//! GPUSTAT bits and a count of field-parity changes at VBlank.

use crate::console_tests::{
    next_frame, number, record, text, Buttons, INTERLACE_COUNT, INTERLACE_RECORD, WIDTH_COUNT,
    WIDTH_RECORD,
};
use crate::TimingRecord;
use psx_engine::button;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::display::{DisplayConfig, Resolution, VideoMode};
use psx_gpu::prim::RectFlat;
use psx_gpu::Gpu;
use psx_hw::gpu::gp1;
use psx_io::gpu as gpu_io;
use psx_vram::{Clut, TextureDepth, TexturePage};

/// The font lives right of the widest picture, out of every framebuffer.
const FONT_TPAGE: TexturePage = TexturePage::new(640, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(640, 256);

/// GPU clocks per pixel and picture width, in screen order.
const WIDTHS: [(u16, i32); WIDTH_COUNT] =
    [(256, 10), (320, 8), (368, 7), (384, 7), (512, 5), (640, 4)];
/// Standard GP1(06h) left edge, and the centre of the standard window.
const X1_STANDARD: i32 = 0x260;
const X_CENTRE: i32 = X1_STANDARD + 1280;
/// GP1(08h) bit 6: the 368-pixel horizontal mode.
const MODE_368: u32 = 0x40;
const DWELL_FRAMES: u32 = 300;

type Rgb = (u8, u8, u8);

#[inline(never)]
fn rect(gpu: &mut Gpu, x: i32, y: i32, w: u32, h: u32, c: Rgb) {
    gpu.draw(&RectFlat::new(
        x as i16, y as i16, w as u16, h as u16, c.0, c.1, c.2,
    ));
}

/// The GP1(06h) window of width index `i`, from the dot clock.
fn window(i: usize) -> (i32, i32) {
    let (pixels, divider) = WIDTHS[i];
    let span = pixels as i32 * divider;
    let x1 = X_CENTRE - span / 2;
    (x1, x1 + span)
}

/// Put width `i` on the display. Returns whether it went through
/// `DisplayConfig` alone.
fn show_width(gpu: &mut Gpu, i: usize) -> bool {
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
            return false;
        }
    }
    true
}

const BACKGROUND: Rgb = (16, 18, 48);
const WHITE: Rgb = (255, 255, 255);
const GREY: Rgb = (96, 104, 130);

/// Edge markers, stripes, frame lines and ruler for a `w` by `h` picture.
#[inline(never)]
fn draw_frame(gpu: &mut Gpu, font: &FontAtlas, w: i32, h: i32) {
    rect(gpu, 0, 0, w as u32, h as u32, BACKGROUND);
    // Insets 8, 16 and 24 pixels in, so a crop's depth can be counted.
    for inset in [8, 16, 24] {
        let (iw, ih) = ((w - 2 * inset) as u32, (h - 2 * inset) as u32);
        rect(gpu, inset, inset, iw, 1, GREY);
        rect(gpu, inset, h - inset - 1, iw, 1, GREY);
        rect(gpu, inset, inset, 1, ih, GREY);
        rect(gpu, w - inset - 1, inset, 1, ih, GREY);
    }
    // One-pixel-wide stripes beside each edge: a wrong dot clock smears them.
    for k in 0..16 {
        rect(gpu, 5 + 2 * k, 40, 1, (h - 80) as u32, WHITE);
        rect(gpu, w - 6 - 2 * k, 40, 1, (h - 80) as u32, WHITE);
    }
    // The outermost lines, then the bars on the extreme left and right.
    rect(gpu, 0, 0, w as u32, 1, WHITE);
    rect(gpu, 0, h - 1, w as u32, 1, WHITE);
    rect(gpu, 0, 0, 4, h as u32, (255, 32, 32));
    rect(gpu, w - 4, 0, 4, h as u32, (32, 255, 32));
    // Ruler: a tick every 16 pixels, a long one and a number every 64.
    for x in (0..=w).step_by(16) {
        let long = x % 64 == 0;
        let len = if long { 12 } else { 6 };
        let x = x.min(w - 1);
        rect(gpu, x, 1, 1, len, WHITE);
        rect(gpu, x, h - 1 - len as i32, 1, len, WHITE);
        if long && x > 8 && x < w - 24 {
            let mut digits = [0u8; 10];
            text(
                font,
                x as i16 - 8,
                14,
                number(&mut digits, x as u32),
                (200, 210, 230),
            );
        }
    }
    // Centre cross.
    rect(gpu, w / 2, 26, 1, (h - 52) as u32, GREY);
    rect(gpu, 26, h / 2, (w - 52) as u32, 1, GREY);
}

fn label(font: &FontAtlas, y: i16, words: &str) {
    text(font, 44, y, words, (230, 236, 248));
}

fn label_number(font: &FontAtlas, x: i16, y: i16, value: u32) {
    let mut digits = [0u8; 10];
    text(font, x, y, number(&mut digits, value), (255, 216, 96));
}

/// The text block of width `i` (the picture is already drawn).
#[inline(never)]
fn draw_text_block(font: &FontAtlas, i: usize, preset: bool, status: u32) {
    let (pixels, divider) = WIDTHS[i];
    let (x1, x2) = window(i);
    label(font, 50, "WIDTH");
    label_number(font, 92, 50, pixels as u32);
    label(font, 62, "DOT CLOCK /");
    label_number(font, 140, 62, divider as u32);
    label(
        font,
        74,
        if preset {
            "SDK PRESET"
        } else {
            "RAW GP1(08)(06)"
        },
    );
    label(font, 86, "X1");
    label_number(font, 64, 86, x1 as u32);
    label(font, 86 + 12, "X2");
    label_number(font, 64, 98, x2 as u32);
    label(font, 114, "GPUSTAT");
    let mut buf = [0u8; 8];
    text(
        font,
        44,
        126,
        crate::console_tests::hex(&mut buf, status),
        (255, 216, 96),
    );
    label_number(font, 44, 144, i as u32 + 1);
    label(font, 144, "  OF 6   5 S EACH");
    label(font, 160, "L/R STEP  START END");
}

/// Show width `i` and return its GPUSTAT once the display has settled.
#[inline(never)]
fn show(gpu: &mut Gpu, font: &FontAtlas, i: usize) -> u32 {
    let (pixels, _) = WIDTHS[i];
    gpu.set_display_enabled(false);
    let preset = show_width(gpu, i);
    gpu.set_display_start((0, 0));
    gpu.set_draw_area((0, 0), (pixels - 1, 239));
    gpu.set_draw_offset((0, 0));
    draw_frame(gpu, font, pixels as i32, 240);
    gpu.wait_idle();
    next_frame();
    next_frame();
    let status = gpu_io::status().bits();
    draw_text_block(font, i, preset, status);
    gpu.wait_idle();
    gpu.set_display_enabled(true);
    status
}

/// Step through the six widths, five seconds each, until START or TRIANGLE.
/// RIGHT or CROSS skips ahead, LEFT goes back.
#[inline(never)]
pub(crate) fn run_widths(gpu: &mut Gpu) -> [TimingRecord; WIDTH_COUNT] {
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    let mut status: [Option<u32>; WIDTH_COUNT] = [None; WIDTH_COUNT];
    let mut buttons = Buttons::new();
    let mut i = 0usize;
    'steps: loop {
        status[i] = Some(show(gpu, &font, i));
        for _ in 0..DWELL_FRAMES {
            next_frame();
            let pressed = buttons.poll();
            if pressed.has(button::START) || pressed.has(button::TRIANGLE) {
                break 'steps;
            }
            if pressed.has(button::LEFT) {
                i = (i + WIDTH_COUNT - 1) % WIDTH_COUNT;
                continue 'steps;
            }
            if pressed.has(button::RIGHT) || pressed.has(button::CROSS) {
                break;
            }
        }
        i = (i + 1) % WIDTH_COUNT;
    }
    core::array::from_fn(|k| {
        let id = WIDTH_RECORD + k as u16;
        match status[k] {
            Some(s) => record(id, s >> 16, window(k).0 as u32, window(k).1 as u32),
            None => record(id, 0xFFFF, 0, 0),
        }
    })
}

/// GPUSTAT bits the interlace case reads: 19 vertical resolution (480
/// lines), 22 interlace, 31 even/odd field.
const STAT_480: u32 = 1 << 19;
const STAT_INTERLACE: u32 = 1 << 22;
const STAT_FIELD: u32 = 1 << 31;

/// Red on even lines, blue on odd, on the left; white line patterns at 1, 2
/// and 4 pixels pitch on the right.
#[inline(never)]
fn draw_interlace_pattern(gpu: &mut Gpu) {
    rect(gpu, 0, 0, 640, 480, (0, 0, 0));
    rect(gpu, 0, 64, 320, 384, (40, 40, 255));
    for y in (64..448).step_by(2) {
        rect(gpu, 0, y, 320, 1, (255, 40, 40));
    }
    for (x, pitch) in [(336, 1), (432, 2), (528, 4)] {
        for y in (64..448).step_by(2 * pitch as usize) {
            rect(gpu, x, y, 80, pitch, WHITE);
        }
    }
    rect(gpu, 0, 0, 640, 1, WHITE);
    rect(gpu, 0, 479, 640, 1, WHITE);
    rect(gpu, 0, 0, 8, 8, WHITE);
    rect(gpu, 632, 0, 8, 8, WHITE);
    rect(gpu, 0, 472, 8, 8, WHITE);
    rect(gpu, 632, 472, 8, 8, WHITE);
}

#[inline(never)]
fn draw_interlace_header(gpu: &mut Gpu, font: &FontAtlas, status: u32, flips: u32, frames: u32) {
    rect(gpu, 8, 20, 624, 40, (0, 0, 0));
    text(
        font,
        16,
        24,
        "640X480 INTERLACED   RED = EVEN LINES  BLUE = ODD",
        WHITE,
    );
    let mut buf = [0u8; 8];
    text(font, 16, 36, "GPUSTAT", (200, 210, 230));
    text(
        font,
        80,
        36,
        crate::console_tests::hex(&mut buf, status),
        (255, 216, 96),
    );
    text(
        font,
        160,
        36,
        if status & STAT_480 != 0 {
            "480 LINES"
        } else {
            "240 LINES"
        },
        WHITE,
    );
    text(
        font,
        240,
        36,
        if status & STAT_INTERLACE != 0 {
            "INTERLACED"
        } else {
            "PROGRESSIVE"
        },
        WHITE,
    );
    text(font, 16, 48, "FIELD CHANGES", (200, 210, 230));
    label_number(font, 128, 48, flips);
    text(font, 168, 48, "IN", (200, 210, 230));
    label_number(font, 192, 48, frames);
    text(font, 232, 48, "FRAMES   START ENDS", (200, 210, 230));
}

/// Show the 640x480 interlaced picture until CROSS, START or TRIANGLE.
#[inline(never)]
pub(crate) fn run_interlace(gpu: &mut Gpu) -> [TimingRecord; INTERLACE_COUNT] {
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    gpu.set_display_enabled(false);
    gpu.set_display(DisplayConfig::new(VideoMode::Ntsc, Resolution::R640X480));
    gpu.set_display_start((0, 0));
    gpu.set_draw_area((0, 0), (639, 479));
    gpu.set_draw_offset((0, 0));
    draw_interlace_pattern(gpu);
    gpu.wait_idle();
    gpu.set_display_enabled(true);
    let mut buttons = Buttons::new();
    let (mut frames, mut flips, mut interlaced, mut tall) = (0u32, 0u32, 0u32, 0u32);
    let mut previous = gpu_io::status().bits() & STAT_FIELD;
    let mut status: u32;
    loop {
        next_frame();
        status = gpu_io::status().bits();
        frames += 1;
        flips += (status & STAT_FIELD != previous) as u32;
        previous = status & STAT_FIELD;
        interlaced += (status & STAT_INTERLACE != 0) as u32;
        tall += (status & STAT_480 != 0) as u32;
        if frames % 4 == 1 {
            draw_interlace_header(gpu, &font, status, flips, frames);
        }
        if buttons.poll().exit() {
            break;
        }
    }
    [
        record(INTERLACE_RECORD, status >> 16, flips, frames),
        record(INTERLACE_RECORD + 1, interlaced, tall, status & 0xFFFF),
    ]
}
