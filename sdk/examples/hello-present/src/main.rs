//! `hello-present` -- frames drawn through the GP0 port, presented with
//! `psx_rt::present::Presenter::present_drawn`, mixed with chain frames.
//!
//! psx-rt's VBlank handler applies a queued display start only once GPUSTAT
//! bit 24 is set, which GP0(1Fh) raises when the GPU reaches it. On silicon
//! bit 28 rises when the DMA has pushed a list's last packet, about one large
//! primitive before the drawing ends (hardware-tests v1.24 cases 219-226), so a
//! flip gated on it could show a frame one primitive short. `present_drawn`
//! acknowledges the flag, sends GP0(1Fh) behind the frame's drawing, queues the
//! flip and returns; the next frame's first direct access waits in the guard
//! until the flip has landed. This example checks that contract end to end:
//!
//! 1. Drawn: `FRAMES` frames, each cleared and drawn through the port and an
//!    ordering table kicked asynchronously (`FrameStorage::draw_async`), then
//!    `present_drawn`. The next frame's clear runs the guard, after which the
//!    flip has landed and bit 24 is still up: a flip seen without it happened
//!    before the frame's GP0(1Fh) ran.
//! 2. Mixed: every fourth frame is a chain handed to `Presenter::submit`
//!    instead, so a drawn frame follows a chain frame and the other way round.
//! 3. Every frame is flipped once, none sooner than the cap (30 fps).
//!
//! The verdict goes to the TTY (`PRESENT PASS ...` or `PRESENT FAIL ...`)
//! and on screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::frame::{FrameStorage, OtFrame};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::TriGouraud;
use psx_gpu::{is_draw_done, Gpu};
use psx_rt::present::{self, Part, Presenter};
use psx_rt::{interrupts, tty, Peripherals};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Frames to present.
const FRAMES: u32 = 120;
/// Most triangles in one frame; frame `f` draws `f % (TRIS + 1)`.
const TRIS: usize = 12;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
static mut FRAME: FrameStorage<8, [TriGouraud; TRIS]> = FrameStorage::new([EMPTY; TRIS]);
// A chain frame is walked after `submit` returns, so its table and packets
// come in two.
static mut TABLES: [OrderingTable<8>; 2] = [OrderingTable::new(), OrderingTable::new()];
static mut PACKETS: [[TriGouraud; TRIS]; 2] = [[EMPTY; TRIS], [EMPTY; TRIS]];

/// Build frame `f`: `f % (TRIS + 1)` half-screen Gouraud triangles.
fn build_frame<'f>(frame: &mut OtFrame<'f, 8>, packets: &'f mut [TriGouraud; TRIS], f: u32) {
    let count = (f as usize) % (TRIS + 1);
    for (t, packet) in packets.iter_mut().take(count).enumerate() {
        let shade = ((t as u32 * 20 + f * 3) & 0x7f) as u8 + 0x20;
        let verts = if t & 1 == 0 {
            [(0, 0), (319, 0), (0, 239)]
        } else {
            [(319, 239), (0, 239), (319, 0)]
        };
        *packet = TriGouraud::new(verts, [(shade, 0, 40), (0, shade, 40), (40, 0, shade)]);
        frame.add(1 + t % 6, packet);
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
    // The token moves into each in-flight frame and comes back from its
    // wait; between frames `Gpu::from_dma_mut` lends the driver.
    let display = DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240);
    let mut dma = Gpu::new(peripherals.gpu_dma, display).release();
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    fb.apply_draw_target(Gpu::from_dma_mut(&mut dma));
    // SAFETY: the only reference ever made to FRAME.
    let mut storage = unsafe { &mut *addr_of_mut!(FRAME) };
    let mut presenter = Presenter::new(Some((8, 8, 24)));

    let flips_before: u32 = interrupts::flip_histogram().iter().sum();
    let fast_before: u32 = interrupts::flip_histogram()[..2].iter().sum();
    let stats_before = present::stats();
    let start = interrupts::vblank_count();
    let mut early = 0u32;
    let mut chains = 0u32;
    for f in 0..FRAMES {
        if f % 4 == 3 {
            present::wait_arena_free();
            let side = (f & 1) as usize;
            // SAFETY: `wait_arena_free` guarantees no walk reads this side, and
            // these are the only references made to it this frame.
            let (table, packets) = unsafe {
                (
                    &mut (*addr_of_mut!(TABLES))[side],
                    &mut (*addr_of_mut!(PACKETS))[side],
                )
            };
            let mut frame = table.frame();
            frame.end_with_draw_done();
            build_frame(&mut frame, packets, f);
            // SAFETY: the table and packets are static and not rebuilt before
            // `wait_arena_free` frees this side two frames from now; the table
            // ends on GP0(1Fh).
            unsafe { presenter.submit(&mut fb, table.submit_head(), Part::Final) };
            chains += 1;
            continue;
        }
        // The clear is the frame's first direct access: the guard waits for
        // the last frame's flip, which needs that frame's GP0(1Fh) to have run.
        let gpu = Gpu::from_dma_mut(&mut dma);
        fb.clear(gpu, (8, 8, 24));
        if f > 0 && !is_draw_done() {
            early += 1;
        }
        let (in_flight, ()) =
            storage.draw_async(dma, |frame, packets| build_frame(frame, packets, f));
        (storage, dma) = in_flight.wait();
        presenter.present_drawn(&mut fb);
    }
    present::wait_idle();
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    let flips = interrupts::flip_histogram()
        .iter()
        .sum::<u32>()
        .wrapping_sub(flips_before);
    let fast = interrupts::flip_histogram()[..2]
        .iter()
        .sum::<u32>()
        .wrapping_sub(fast_before);
    let stats = present::stats();
    let recoveries = stats
        .recovery_count
        .wrapping_sub(stats_before.recovery_count);
    let kicks = stats.kick_count.wrapping_sub(stats_before.kick_count);
    let gpu = Gpu::from_dma_mut(&mut dma);

    let checks = [
        (flips == FRAMES, "frames not each flipped once"),
        (kicks == chains, "chain frames not each kicked once"),
        (early == 0, "flip before the frame's GP0(1Fh)"),
        (fast == 0, "a flip came under 2 VBlanks after the last"),
        (recoveries == 0, "present stalled"),
        (
            vblanks >= 2 * (FRAMES - 1),
            "fewer VBlanks than the cap allows",
        ),
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
        line("flips", flips),
        line("chains", chains),
        line("vblanks", vblanks),
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
        fb.clear(gpu, (10, 12, 20));
        font.draw_text(8, 6, "FRAMES DRAWN THROUGH THE PORT", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 138;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        presenter.present_blocking(&mut fb);
    }
}
