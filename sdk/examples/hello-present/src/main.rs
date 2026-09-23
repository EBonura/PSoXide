//! `hello-present` -- psx-rt's queued display flip waits for the frame's
//! closing GP0(1Fh), not for GPUSTAT bit 28.
//!
//! On silicon bit 28 rises when the DMA has pushed a list's last packet,
//! about one large primitive before the drawing ends (hardware-tests v1.24
//! cases 219-226), so a flip gated on it can show a frame one primitive
//! short. psx-rt's VBlank handler now applies a queued display start only
//! once GPUSTAT bit 24 is set, which GP0(1Fh) raises when the GPU reaches
//! it. This example checks that contract end to end:
//!
//! 1. Held: a frame kicked without GP0(1Fh) keeps its flip queued, even
//!    after `draw_sync` has seen the GPU idle (the old bit-28 test would
//!    have flipped at the next edge).
//! 2. Released: `signal_draw_done` then lets the flip land within a few
//!    VBlanks.
//! 3. Pipelined: `FRAMES` frames of varying cost, each an ordering table
//!    closed with `end_with_draw_done`, armed with `arm_draw_done` and
//!    kicked asynchronously, flip through the queue with no timeout, and
//!    every flip is seen with its frame's GP0(1Fh) executed.
//!
//! The verdict goes to the TTY (`PRESENT PASS ...` or `PRESENT FAIL ...`)
//! and on screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::framebuf::FrameBuffer;
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::TriGouraud;
use psx_gpu::{self as gpu, Resolution, VideoMode};
use psx_rt::interrupts;
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Pipelined frames to present.
const FRAMES: u32 = 120;
/// VBlanks a queued flip is given before it counts as a timeout.
const WAIT_LIMIT: u32 = 8;
/// Most triangles in one frame; frame `f` draws `f % (TRIS + 1)`.
const TRIS: usize = 12;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
static mut OT: OrderingTable<8> = OrderingTable::new();
static mut PACKETS: [TriGouraud; TRIS] = [EMPTY; TRIS];

/// Wait until the queued flip is applied or `limit` VBlanks pass.
/// Returns whether it was applied.
fn wait_flip(limit: u32) -> bool {
    let start = interrupts::vblank_count();
    while interrupts::gp1_queue_pending() {
        if interrupts::vblank_count().wrapping_sub(start) > limit {
            return false;
        }
    }
    true
}

/// Wait `n` VBlanks.
fn wait_vblanks(n: u32) {
    for _ in 0..n {
        interrupts::wait_vblank();
    }
}

/// Build frame `f` into the ordering table: `f % (TRIS + 1)` half-screen
/// Gouraud triangles, closed with GP0(1Fh) when `close` is set.
fn build_frame(ot: &mut OrderingTable<8>, packets: &mut [TriGouraud; TRIS], f: u32, close: bool) {
    ot.clear();
    if close {
        ot.end_with_draw_done();
    }
    let count = (f as usize) % (TRIS + 1);
    for (t, packet) in packets.iter_mut().take(count).enumerate() {
        let shade = ((t as u32 * 20 + f * 3) & 0x7F) as u8 + 0x20;
        let verts = if t & 1 == 0 {
            [(0, 0), (319, 0), (0, 239)]
        } else {
            [(319, 239), (0, 239), (319, 0)]
        };
        *packet = TriGouraud::new(verts, [(shade, 0, 40), (0, shade, 40), (40, 0, shade)]);
        ot.add(1 + t % 6, packet, TriGouraud::WORDS);
    }
}

/// `label=XXXXXXXX` for the TTY and the screen.
fn line(label: &str, value: u32) -> ([u8; 32], usize) {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut text = [b' '; 32];
    let label = &label.as_bytes()[..label.len().min(22)];
    text[..label.len()].copy_from_slice(label);
    text[label.len()] = b'=';
    for i in 0..8 {
        text[label.len() + 1 + i] = DIGITS[(value >> (28 - 4 * i) & 0xF) as usize];
    }
    (text, label.len() + 9)
}

fn as_str(text: &[u8]) -> &str {
    core::str::from_utf8(text).unwrap_or("?")
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    fb.apply_draw_target();
    // SAFETY: single-threaded; nothing else touches these statics, and the
    // DMA reading them is waited out before they are rebuilt.
    let (ot, packets) = unsafe { (&mut *addr_of_mut!(OT), &mut *addr_of_mut!(PACKETS)) };

    // 1. Held: no GP0(1Fh), so the flip must stay queued even once the GPU
    // is idle.
    fb.clear(8, 8, 24);
    build_frame(ot, packets, TRIS as u32, false);
    gpu::arm_draw_done();
    ot.submit_async();
    interrupts::queue_gp1_at_vblank(fb.begin_deferred_swap());
    gpu::draw_sync();
    wait_vblanks(4);
    let held = interrupts::gp1_queue_pending() && !gpu::draw_done();

    // 2. Released: GP0(1Fh) through the port lets it land.
    gpu::signal_draw_done();
    let released = wait_flip(4);
    if !released {
        let word = interrupts::take_pending_gp1();
        if word != 0 {
            psx_io::gpu::write_gp1(word);
        }
    }

    // 3. Pipelined frames through the queue.
    let mut timeouts = 0u32;
    let mut early = 0u32;
    let start = interrupts::vblank_count();
    for f in 0..FRAMES {
        fb.apply_draw_target();
        fb.clear(8, 8, 24);
        build_frame(ot, packets, f, true);
        gpu::arm_draw_done();
        ot.submit_async();
        interrupts::queue_gp1_at_vblank(fb.begin_deferred_swap());
        if !wait_flip(WAIT_LIMIT) {
            timeouts += 1;
            gpu::draw_sync();
            let word = interrupts::take_pending_gp1();
            if word != 0 {
                psx_io::gpu::write_gp1(word);
            }
        } else if !gpu::draw_done() {
            // The flag cannot clear before the next arm, so a flip seen
            // without it happened before the frame's GP0(1Fh) ran.
            early += 1;
        }
    }
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    gpu::draw_sync();

    let checks = [
        (held, "flip without GP0(1Fh) was applied"),
        (released, "GP0(1Fh) did not release the flip"),
        (timeouts == 0, "queued flips timed out"),
        (early == 0, "flip before the frame's GP0(1Fh)"),
        (vblanks >= FRAMES, "fewer VBlanks than frames"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("PRESENT check failed: ");
        tty::println(what);
    }
    let lines = [
        line("failures", failures),
        line("frames", FRAMES),
        line("vblanks", vblanks),
        line("timeouts", timeouts),
        line("early", early),
    ];
    let (banner, tint) = if failures == 0 {
        ("PRESENT PASS", GREEN)
    } else {
        ("PRESENT FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    loop {
        fb.apply_draw_target();
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "QUEUED FLIP WAITS FOR GP0(1FH)", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 126;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu::arm_draw_done();
        gpu::signal_draw_done();
        interrupts::queue_gp1_at_vblank(fb.begin_deferred_swap());
        wait_flip(WAIT_LIMIT);
    }
}
