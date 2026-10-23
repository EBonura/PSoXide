// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-flipcap` -- the presentation cap: no display flip comes sooner than
//! two VBlanks after the one before (30 fps NTSC), unless the game opts out by
//! name.
//!
//! Every phase presents as fast as the CPU lets it, so each would flip every
//! VBlank without the cap:
//!
//! 1. Queued: tiny frames, each flip queued with
//!    `queue_display_control_at_vblank` (the path hl-psx, cs-psx and the other
//!    decoupled-present games use). The handler holds each flip until the cap
//!    allows it.
//! 2. Direct: `DoubleBuffer::swap` in a loop with no VBlank wait of its own.
//!    The swap waits for its slot.
//! 3. Opt-out: `set_min_flip_vblanks(PresentCap::UNCAPPED.min_vblanks())` and the same queued
//!    loop flips on every VBlank again.
//!
//! The flip-interval histogram (`interrupts::flip_histogram`) is read after
//! each phase; the capped ones must hold no interval under 2 and run at
//! about one flip per two VBlanks, the opt-out must reach intervals of 1. The
//! verdict goes to the TTY (`FLIPCAP PASS ...` or `FLIPCAP FAIL ...`) and on
//! screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_rt::{interrupts, tty, Peripherals};
use psx_tick::{FlipHistogram, PresentCap};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Flips each phase makes.
const FLIPS: u32 = 90;
/// VBlanks a queued flip is given before it counts as a timeout.
const WAIT_LIMIT: u32 = 8;

/// What one phase measured.
#[derive(Clone, Copy)]
struct Phase {
    /// Flips counted in the histogram.
    flips: u32,
    /// Flips that came sooner than the capped minimum (2 VBlanks) after the one before.
    fast: u32,
    /// Flips exactly one VBlank after the one before.
    single: u32,
    /// VBlanks the phase took.
    vblanks: u32,
    /// Queued flips that timed out.
    timeouts: u32,
}

fn measure(run: impl FnOnce() -> u32) -> Phase {
    interrupts::reset_flip_histogram();
    let start = interrupts::vblank_count();
    let timeouts = run();
    let histogram = FlipHistogram::from_buckets(interrupts::flip_histogram());
    Phase {
        flips: histogram.total(),
        fast: histogram.below(2),
        single: histogram.buckets()[1],
        vblanks: interrupts::vblank_count().wrapping_sub(start),
        timeouts,
    }
}

/// Queue `FLIPS` flips of an empty frame, as fast as the queue releases them.
fn queued(gpu: &mut Gpu, fb: &mut DoubleBuffer) -> u32 {
    let mut timeouts = 0;
    for _ in 0..FLIPS {
        fb.apply_draw_target(gpu);
        fb.clear(gpu, (8, 8, 24));
        gpu.arm_draw_done();
        gpu.signal_draw_done();
        interrupts::queue_display_control_at_vblank(fb.begin_deferred_swap());
        let start = interrupts::vblank_count();
        while interrupts::is_display_control_queued() {
            if interrupts::vblank_count().wrapping_sub(start) > WAIT_LIMIT {
                timeouts += 1;
                let word = interrupts::take_queued_display_control();
                if word != 0 {
                    psx_io::gpu::write_display_control(word);
                }
                break;
            }
        }
    }
    timeouts
}

/// `FLIPS` direct flips with nothing between them but a clear.
fn direct(gpu: &mut Gpu, fb: &mut DoubleBuffer) -> u32 {
    for _ in 0..FLIPS {
        fb.clear(gpu, (24, 8, 8));
        gpu.wait_idle();
        fb.swap(gpu);
    }
    0
}

/// `label=XXXXXXXX` for the TTY and the screen.
fn line(label: &str, value: u32) -> ([u8; 32], usize) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = [b' '; 32];
    let label = &label.as_bytes()[..label.len().min(22)];
    text[..label.len()].copy_from_slice(label);
    text[label.len()] = b'=';
    for i in 0..8 {
        text[label.len() + 1 + i] = DIGITS[(value >> (28 - 4 * i) & 0xf) as usize];
    }
    (text, label.len() + 9)
}

fn as_str(text: &[u8]) -> &str {
    core::str::from_utf8(text).unwrap_or("?")
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    let Some(peripherals) = Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    fb.apply_draw_target(&mut gpu);
    let default_cap = PresentCap::every_vblanks(interrupts::min_flip_vblanks());

    let capped_queue = measure(|| queued(&mut gpu, &mut fb));
    let capped_direct = measure(|| direct(&mut gpu, &mut fb));
    interrupts::set_min_flip_vblanks(PresentCap::UNCAPPED.min_vblanks());
    let uncapped = measure(|| queued(&mut gpu, &mut fb));
    interrupts::set_min_flip_vblanks(PresentCap::HZ30.min_vblanks());
    gpu.wait_idle();

    // Two VBlanks per flip, with a little slack at both ends of the phase.
    let paced = |p: &Phase| p.vblanks >= 2 * FLIPS - 4 && p.vblanks <= 2 * FLIPS + 8;
    let checks = [
        (
            default_cap == PresentCap::HZ30,
            "the default cap is not 30 fps",
        ),
        (capped_queue.flips == FLIPS, "queued: flips counted"),
        (
            capped_queue.fast == 0,
            "queued: a flip came under 2 VBlanks",
        ),
        (paced(&capped_queue), "queued: not one flip per 2 VBlanks"),
        (capped_queue.timeouts == 0, "queued: flips timed out"),
        (capped_direct.flips == FLIPS, "direct: flips counted"),
        (
            capped_direct.fast == 0,
            "direct: a flip came under 2 VBlanks",
        ),
        (paced(&capped_direct), "direct: not one flip per 2 VBlanks"),
        (uncapped.flips == FLIPS, "opt-out: flips counted"),
        (
            uncapped.single >= FLIPS - 4,
            "opt-out: not flipping every VBlank",
        ),
        (uncapped.vblanks <= FLIPS + 8, "opt-out: slower than 60 fps"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("FLIPCAP check failed: ");
        tty::println(what);
    }
    let lines = [
        line("failures", failures),
        line("queued vblanks", capped_queue.vblanks),
        line("queued fast", capped_queue.fast),
        line("direct vblanks", capped_direct.vblanks),
        line("direct fast", capped_direct.fast),
        line("optout vblanks", uncapped.vblanks),
        line("optout at 1", uncapped.single),
    ];
    let (banner, tint) = if failures == 0 {
        ("FLIPCAP PASS", GREEN)
    } else {
        ("FLIPCAP FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    loop {
        fb.apply_draw_target(&mut gpu);
        fb.clear(&mut gpu, (10, 12, 20));
        font.draw_text(8, 6, "PRESENTATION CAP: 30 FPS", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 150;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu.wait_idle();
        fb.swap(&mut gpu);
    }
}
