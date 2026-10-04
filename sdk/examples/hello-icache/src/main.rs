//! `hello-icache` -- does `psx_rt::cache::flush_instruction_cache` make the
//! CPU fetch code that was just rewritten in RAM?
//!
//! A three-word function in cached RAM returns a constant:
//! `addiu $v0, $zero, K; jr $ra; nop`. The probe:
//!
//! 1. writes it with K = 1, flushes, calls it (expects 1; the call also
//!    loads the line into the instruction cache);
//! 2. rewrites K = 2 with a plain store and calls it again without a
//!    flush. A store doesn't update the instruction cache, so a cache that
//!    still holds the line runs the old word and returns 1;
//! 3. flushes and calls once more (expects 2).
//!
//! `ICACHE PASS` needs 1, 1, 2: step 2 proves the line was really cached,
//! so step 3 proves the flush dropped it. If step 2 already returns 2 the
//! run says `ICACHE INCONCLUSIVE`: nothing was stale, so the flush wasn't
//! tested. The verdict goes to the TTY and the screen.

#![no_std]
#![no_main]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_rt::{cache::flush_instruction_cache, tty};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);

/// `addiu $v0, $zero, 0` (opcode 9, rt = 2); the low 16 bits are the
/// immediate.
const ADDIU_V0_ZERO: u32 = 0x2402_0000;
/// `jr $ra`.
const JR_RA: u32 = 0x03E0_0008;

/// One cache line (16 bytes) of code, line aligned so the function sits
/// in a single line.
#[repr(C, align(16))]
struct Line([u32; 4]);

static mut CODE: Line = Line([ADDIU_V0_ZERO | 1, JR_RA, 0, 0]);

/// Store a new immediate into the function's first word.
fn patch(value: u16) {
    // SAFETY: CODE is only touched from this single-threaded program.
    unsafe { (&raw mut CODE.0[0]).write_volatile(ADDIU_V0_ZERO | u32::from(value)) };
}

/// Call the function in CODE.
#[inline(never)]
fn call() -> u32 {
    // SAFETY: CODE holds a complete O32 leaf function (load $v0, return).
    unsafe {
        let f: extern "C" fn() -> u32 = core::mem::transmute(&raw const CODE);
        f()
    }
}

#[no_mangle]
fn main() {
    patch(1);
    flush_instruction_cache();
    let first = call();
    patch(2);
    let stale = call();
    flush_instruction_cache();
    let fresh = call();

    let (verdict, tint) = if first == 1 && stale == 1 && fresh == 2 {
        ("ICACHE PASS", (80, 220, 100))
    } else if first == 1 && stale == 2 && fresh == 2 {
        ("ICACHE INCONCLUSIVE", (230, 200, 80))
    } else {
        ("ICACHE FAIL", (230, 80, 80))
    };
    tty::print(verdict);
    for (label, value) in [(" first=", first), (" stale=", stale), (" fresh=", fresh)] {
        tty::print(label);
        tty::print_hex_u32(value);
    }
    tty::println("");

    let Some(peripherals) = psx_rt::Peripherals::take() else {
        return;
    };
    let mut gpu = Gpu::new(
        peripherals.gpu_dma,
        DisplayConfig::new(VideoMode::Ntsc, Resolution::R320X240),
    );
    let mut fb = DoubleBuffer::new(Resolution::R320X240);
    gpu.set_draw_area((0, 0), (319, 239));
    gpu.set_draw_offset((0, 0));
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    let digit = |v: u32| -> &'static str {
        match v {
            1 => "1",
            2 => "2",
            _ => "?",
        }
    };
    loop {
        fb.clear(&mut gpu, (10, 12, 20));
        font.draw_text(8, 6, "I-CACHE FLUSH PROBE", (220, 220, 230));
        font.draw_text(8, 26, "first", (220, 220, 230));
        font.draw_text(80, 26, digit(first), tint);
        font.draw_text(8, 38, "stale", (220, 220, 230));
        font.draw_text(80, 38, digit(stale), tint);
        font.draw_text(8, 50, "fresh", (220, 220, 230));
        font.draw_text(80, 50, digit(fresh), tint);
        font.draw_text(8, 70, verdict, tint);
        gpu.wait_idle();
        psx_rt::interrupts::wait_vblank();
        fb.swap(&mut gpu);
    }
}
