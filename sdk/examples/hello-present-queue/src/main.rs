//! `hello-present-queue` -- whole frames through psx-rt's VBlank-kicked
//! present queue (`psx_rt::present`, the `present-queue` feature).
//!
//! `FRAMES` frames of varying cost are each published as one chain: a
//! recorded preamble (draw target and clear), the ordering table, a recorded
//! HUD band, then GP0(1Fh). The CPU never kicks or flips: the VBlank handler
//! shows the previous frame and kicks the next chain on the first edge that
//! finds the one before it drawn. Everything a chain links is double
//! buffered, and `wait_arena_free` keeps the CPU off the side a walk may
//! still read.
//!
//! Every frame must be kicked exactly once, with no stall recovery and no
//! recording overflow. The verdict goes to the TTY
//! (`PRESENT-QUEUE PASS ...` or `PRESENT-QUEUE FAIL ...`) and on screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use core::ptr::addr_of_mut;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::chain::DRAW_DONE_NODE;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::{FillRect, TriGouraud};
use psx_gpu::Gpu;
use psx_io::gpu::{begin_recording_raw, end_recording};
use psx_rt::{interrupts, present, tty};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Frames to publish.
const FRAMES: u32 = 120;
/// VBlanks the final screen's flip is given.
const WAIT_LIMIT: u32 = 8;
/// Most triangles in one frame; frame `f` draws `f % (TRIS + 1)`.
const TRIS: usize = 12;

const EMPTY: TriGouraud = TriGouraud::new([(0, 0); 3], [(0, 0, 0); 3]);
// Frame N is built while N-1 may still walk, so every buffer a chain links
// comes in two.
static mut TABLES: [OrderingTable<8>; 2] = [OrderingTable::new(), OrderingTable::new()];
static mut PACKETS: [[TriGouraud; TRIS]; 2] = [[EMPTY; TRIS], [EMPTY; TRIS]];
static mut PREAMBLES: [[u32; 64]; 2] = [[0; 64]; 2];
static mut HUDS: [[u32; 1024]; 2] = [[0; 1024]; 2];

/// Wait until the queued flip is applied or `limit` VBlanks pass.
fn wait_flip(limit: u32) -> bool {
    let start = interrupts::vblank_count();
    while interrupts::is_display_control_queued() {
        if interrupts::vblank_count().wrapping_sub(start) > limit {
            return false;
        }
    }
    true
}

/// Build frame `f`'s triangles: `f % (TRIS + 1)` half-screen Gouraud
/// triangles in slots 1..=6, leaving slot 0 for the HUD link.
fn build_frame(table: &mut OrderingTable<8>, packets: &mut [TriGouraud; TRIS], f: u32) {
    let mut frame = table.frame();
    let count = (f as usize) % (TRIS + 1);
    for (t, packet) in packets.iter_mut().take(count).enumerate() {
        let shade = ((t as u32 * 20 + f * 3) & 0x7F) as u8 + 0x20;
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
    let mut gpu = Gpu::new(
        psx_rt::Peripherals::take()
            .expect("peripherals are taken once")
            .gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    fb.apply_draw_target(&mut gpu);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    gpu.wait_idle();

    let before = present::stats();
    present::start();
    let mut overflows = 0u32;
    // GP1(05h) word of the last published frame; 0 before the first.
    let mut shown = 0u32;
    let start = interrupts::vblank_count();
    for f in 0..FRAMES {
        let side = (f & 1) as usize;
        // The chain two frames back linked this side's buffers.
        present::wait_arena_free();
        // SAFETY: `wait_arena_free` guarantees no walk reads this side, and
        // these are the only references made to it this frame.
        let (table, packets, preamble, hud) = unsafe {
            (
                &mut (*addr_of_mut!(TABLES))[side],
                &mut (*addr_of_mut!(PACKETS))[side],
                &mut (*addr_of_mut!(PREAMBLES))[side],
                &mut (*addr_of_mut!(HUDS))[side],
            )
        };
        build_frame(table, packets, f);

        // SAFETY: `hud` is static, and is not written again until
        // `wait_arena_free` says its walk has ended.
        unsafe { begin_recording_raw(hud.as_mut_ptr(), hud.len()) };
        gpu.draw(&FillRect::new(
            (0, fb.draw_origin().1 + 200),
            (320, 40),
            (20, 24, 60),
        ));
        font.draw_text(8, 212, "QUEUED HUD", WHITE);
        match end_recording() {
            // SAFETY: the recording and the static GP0(1Fh) node live as long
            // as `hud`; the table's slot 0 is still empty.
            Ok(Some(recording)) => unsafe {
                recording.link_to(DRAW_DONE_NODE.as_ptr());
                table.end_with_chain(recording.head());
            },
            _ => {
                overflows += 1;
                table.end_with_draw_done();
            }
        }

        // SAFETY: as for `hud`.
        unsafe { begin_recording_raw(preamble.as_mut_ptr(), preamble.len()) };
        fb.apply_draw_target(&mut gpu);
        fb.clear(&mut gpu, (8, 24, 8));
        let Ok(Some(preamble)) = end_recording() else {
            overflows += 1;
            continue;
        };
        // SAFETY: the table is static and stays untouched until its walk ends.
        unsafe { preamble.link_to(table.submit_head()) };

        // Show the frame before this one when this one starts drawing.
        let display = shown;
        shown = fb.begin_deferred_swap();
        present::wait_slot_empty();
        // SAFETY: the slot is empty; the chain (preamble, table, packets,
        // HUD, GP0(1Fh)) is static and not rebuilt before `wait_arena_free`
        // clears its side two frames from now.
        unsafe { present::publish_raw(preamble.head(), display) };
    }
    present::wait_idle();
    psx_io::gpu::write_display_control(shown);
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    let after = present::stats();
    let kicks = after.kick_count.wrapping_sub(before.kick_count);
    let skips = after.skip_count.wrapping_sub(before.skip_count);
    let recoveries = after.recovery_count.wrapping_sub(before.recovery_count);

    let checks = [
        (kicks == FRAMES, "queued frames not each kicked once"),
        (recoveries == 0, "present queue stalled"),
        (overflows == 0, "recording overflowed"),
        (vblanks >= FRAMES, "fewer VBlanks than frames"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("PRESENT-QUEUE check failed: ");
        tty::println(what);
    }
    let lines = [
        line("failures", failures),
        line("frames", FRAMES),
        line("kicks", kicks),
        line("skips", skips),
        line("vblanks", vblanks),
    ];
    let (banner, tint) = if failures == 0 {
        ("PRESENT-QUEUE PASS", GREEN)
    } else {
        ("PRESENT-QUEUE FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    loop {
        fb.apply_draw_target(&mut gpu);
        fb.clear(&mut gpu, (10, 12, 20));
        font.draw_text(8, 6, "WHOLE FRAMES KICKED AT VBLANK", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 126;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu.arm_draw_done();
        gpu.signal_draw_done();
        interrupts::queue_display_control_at_vblank(fb.begin_deferred_swap());
        wait_flip(WAIT_LIMIT);
    }
}
