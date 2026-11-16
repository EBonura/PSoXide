// SPDX-License-Identifier: GPL-2.0-or-later
//! `hello-flipcap` -- the presentation cap: no display flip comes sooner than
//! two VBlanks after the one before (30 fps NTSC), unless the game opts out by
//! name.
//!
//! Every phase presents as fast as the CPU lets it, so each would flip every
//! VBlank without the cap:
//!
//! 1. Queued: tiny frames drawn through the port and handed over with
//!    `Presenter::present_drawn` (the one present path). The handler holds
//!    each flip until the cap allows it.
//! 2. Direct: `DoubleBuffer::swap` in a loop with no VBlank wait of its own.
//!    The swap waits for its slot.
//! 3. Opt-out: `present::set_cap(PresentCap::UNCAPPED)` and the same queued
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
use psx_rt::present::{self, PresentCap, Presenter};
use psx_rt::{interrupts, tty, Peripherals};
use psx_tick::FlipHistogram;
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Flips each phase makes.
const FLIPS: u32 = 90;

/// Frames of the long run that must leave the presenter in signal mode.
const LONG_FRAMES: u32 = 1500;

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

/// Present `FLIPS` empty frames, as fast as the queue releases them; counts the
/// present stalls (a flip that never landed).
fn queued(gpu: &mut Gpu, fb: &mut DoubleBuffer, presenter: &mut Presenter) -> u32 {
    let before = present::stats().recovery_count;
    for n in 0..FLIPS {
        presenter.begin_frame(fb);
        // A different picture every frame, so a frame-hash replay can count the flips.
        fb.clear(gpu, (8, 8, 24 + (n % 8) as u8 * 12));
        presenter.present_drawn(fb);
    }
    present::wait_idle();
    present::stats().recovery_count.wrapping_sub(before)
}

/// `frames` empty frames where the signal is taken away on every frame `lose` names: after the
/// frame is drawn (its GP0(1Fh) has raised GPUSTAT bit 24) the program acknowledges bit 24
/// itself, so the presenter sees a frame whose signal never arrived. Nothing waits for a
/// flip here, so the run costs what the cap lets it.
fn lossy(
    gpu: &mut Gpu,
    fb: &mut DoubleBuffer,
    presenter: &mut Presenter,
    frames: u32,
    lose: impl Fn(u32) -> bool,
) {
    for n in 0..frames {
        presenter.begin_frame(fb);
        fb.clear(gpu, (24, 8 + (n % 8) as u8 * 12, 8));
        presenter.present_drawn(fb);
        if lose(n) {
            gpu.wait_idle();
            gpu.arm_draw_done();
        }
    }
    present::wait_idle();
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
    let default_cap = present::cap();
    let mut presenter = Presenter::new(None);

    let capped_queue = measure(|| queued(&mut gpu, &mut fb, &mut presenter));
    let missed_capped = interrupts::draw_done_fallback_count();
    let distrust_capped = interrupts::flip_signal_distrusted();
    let capped_direct = measure(|| direct(&mut gpu, &mut fb));
    present::set_cap(PresentCap::UNCAPPED);
    let uncapped = measure(|| queued(&mut gpu, &mut fb, &mut presenter));
    present::set_cap(PresentCap::HZ30);
    gpu.wait_idle();
    // The signal's trust, on a target where the signal arrived in the phases above (a PS1 and the
    // models that raise it): a lone lost signal in eight must not move the presenter to the
    // conventional test, a burst of lost signals must, and a long run with the signal must bring
    // it back. A target that never raised it skips these (the phases above showed it).
    let signal_works = interrupts::draw_done_fallback_count() == 0;
    present::set_cap(PresentCap::UNCAPPED);
    let (mut isolated, mut burst, mut recovered) = (None, None, None);
    let mut long = None;
    if signal_works {
        // A long run with the signal on every frame: the mode must never move.
        lossy(&mut gpu, &mut fb, &mut presenter, LONG_FRAMES, |_| false);
        long = Some((
            interrupts::draw_done_fallback_count(),
            interrupts::flip_signal_distrusted(),
        ));
        let before = interrupts::draw_done_fallback_count();
        lossy(&mut gpu, &mut fb, &mut presenter, 64, |n| n % 8 == 3);
        isolated = Some((
            interrupts::draw_done_fallback_count() - before,
            interrupts::flip_signal_distrusted(),
        ));
        let before = interrupts::draw_done_fallback_count();
        lossy(&mut gpu, &mut fb, &mut presenter, 6, |_| true);
        burst = Some((
            interrupts::draw_done_fallback_count() - before,
            interrupts::flip_signal_distrusted(),
        ));
        let before = interrupts::draw_done_fallback_count();
        lossy(&mut gpu, &mut fb, &mut presenter, 40, |_| false);
        recovered = Some((
            interrupts::draw_done_fallback_count() - before,
            interrupts::flip_signal_distrusted(),
        ));
    }
    present::set_cap(PresentCap::HZ30);
    gpu.wait_idle();
    let missed_end = interrupts::draw_done_fallback_count();
    let distrust_end = interrupts::flip_signal_distrusted();

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
        (capped_queue.timeouts == 0, "queued: present stalled"),
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
        (
            long.is_none_or(|(missed, distrust)| missed == 0 && !distrust),
            "trust: the signal missed or the mode moved in a long run",
        ),
        (
            isolated.is_none_or(|(missed, distrust)| missed == 8 && !distrust),
            "trust: a lone lost signal in eight changed the mode",
        ),
        (
            burst.is_none_or(|(missed, distrust)| missed == 6 && distrust),
            "trust: a burst of lost signals did not switch the mode",
        ),
        (
            recovered.is_none_or(|(missed, distrust)| missed == 0 && !distrust),
            "trust: the signal did not win the mode back",
        ),
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
        // The signal: bit 24 missed on this many frames, and whether the presenter had stopped
        // waiting for it. A PS1 reads 0 and 0 on every row.
        line("missed capped", missed_capped),
        line("distrust capped", u32::from(distrust_capped)),
        line("missed end", missed_end),
        line("distrust end", u32::from(distrust_end)),
        // Lost-signal phases (a program-made loss): frames missed and the mode after each, or
        // FFFFFFFF where the target never raised the signal and the phase did not run.
        line("long missed", long.map_or(u32::MAX, |p| p.0)),
        line("lone missed", isolated.map_or(u32::MAX, |p| p.0)),
        line("burst missed", burst.map_or(u32::MAX, |p| p.0)),
        line("burst distrust", burst.map_or(u32::MAX, |p| u32::from(p.1))),
        line(
            "regain distrust",
            recovered.map_or(u32::MAX, |p| u32::from(p.1)),
        ),
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
            font.draw_text(8, 36 + 10 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 224;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu.wait_idle();
        fb.swap(&mut gpu);
    }
}
