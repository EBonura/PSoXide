//! `hello-gteirq` -- does a GTE command run exactly once when a VBlank
//! interrupt lands on it?
//!
//! On silicon an interrupt taken on a GTE command lets the command run and
//! still reports EPC at it, so a handler that returns to EPC runs it twice
//! (hardware-tests v1.24, case 0xC9: 38 of 38). psx-rt's handler steps over
//! a GTE command at EPC instead (case 0xCB: 61 skips, none doubled or lost).
//! This example runs the v1.24 probe's pattern under psx-rt's own handler:
//! load SXY0-2 with three markers, RTPS, wait out its latency, read SXY0.
//! One RTPS shifts the screen-XY FIFO once, so SXY0 must hold the second
//! marker; two shifts leave the third, none the first. The markers' high
//! halves lie outside the range RTPS saturates to, so no projection can
//! pass for one. The loop runs for `VBLANKS` VBlank interrupts.
//!
//! The verdict goes to the TTY (`GTEIRQ PASS ...` or `GTEIRQ FAIL ...`) and
//! on screen. `gte_skips` counts the interrupts the handler stepped over a
//! GTE command for: about one in forty VBlanks on silicon, where the loop is
//! mostly RTPS-adjacent instructions, and always zero in PSoXide, which
//! never takes an interrupt in front of a GTE command, so the emulator
//! passes whether or not the fix is present. Only a console run tests it.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

extern crate psx_rt;

use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_rt::interrupts;
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Written to SXY0/SXY1/SXY2 before each RTPS. After one RTPS SXY0 holds
/// B, after two C, after none A.
const SXY_A: u32 = 0x5A5A_1111;
const SXY_B: u32 = 0x5A5A_2222;
const SXY_C: u32 = 0x5A5A_3333;
/// Iterations between checks of the VBlank count.
const CHUNK: u32 = 4096;
/// Interrupts to run under: about five seconds on NTSC.
const VBLANKS: u32 = 300;

#[derive(Clone, Copy, Default)]
struct Counts {
    ok: u32,
    doubled: u32,
    missing: u32,
    other: u32,
}

/// `iterations` x (reload SXY0-2, RTPS, wait it out, classify SXY0).
///
/// The RTPS never sits in a delay slot (psx-rt's fix cannot cover one).
/// Twenty nops cover its latency before the read, so a stale read cannot
/// pass for a missing command.
fn gte_loop(iterations: u32, counts: &mut Counts) {
    let (mut ok, mut doubled, mut missing, mut other) =
        (counts.ok, counts.doubled, counts.missing, counts.other);
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            "1:",
            ".word 0x48896000", // mtc2 $9, SXY0
            ".word 0x488A6800", // mtc2 $10, SXY1
            ".word 0x488B7000", // mtc2 $11, SXY2
            "nop",
            "nop",
            ".word 0x4A180001", // RTPS
            ".rept 20",
            "nop",
            ".endr",
            ".word 0x480C6000", // mfc2 $12, SXY0
            "nop",
            "beq $12, $10, 2f",
            "addiu $8, $8, -1",
            "beq $12, $11, 3f",
            "nop",
            "beq $12, $9, 4f",
            "nop",
            "addiu $24, $24, 1",
            "b 5f",
            "nop",
            "2:",
            "addiu $13, $13, 1",
            "b 5f",
            "nop",
            "3:",
            "addiu $14, $14, 1",
            "b 5f",
            "nop",
            "4:",
            "addiu $15, $15, 1",
            "5:",
            "bnez $8, 1b",
            "nop",
            ".set reorder",
            inout("$8") iterations => _,
            in("$9") SXY_A,
            in("$10") SXY_B,
            in("$11") SXY_C,
            lateout("$12") _,
            inout("$13") ok,
            inout("$14") doubled,
            inout("$15") missing,
            inout("$24") other,
            options(nostack)
        );
    }
    counts.ok = ok;
    counts.doubled = doubled;
    counts.missing = missing;
    counts.other = other;
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
    // Also sets SR.CU2, which the GTE needs.
    interrupts::install_vblank_counter();

    let skips_before = interrupts::gte_skip_count();
    let start = interrupts::vblank_count();
    let mut counts = Counts::default();
    let mut iterations = 0u32;
    while interrupts::vblank_count().wrapping_sub(start) < VBLANKS {
        gte_loop(CHUNK, &mut counts);
        iterations = iterations.wrapping_add(CHUNK);
    }
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    let skips = interrupts::gte_skip_count().wrapping_sub(skips_before);

    let checks = [
        (counts.doubled == 0, "an RTPS ran twice"),
        (counts.missing == 0, "an RTPS was lost"),
        (counts.other == 0, "SXY0 held no marker"),
        (counts.ok == iterations, "not every iteration counted"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("GTEIRQ check failed: ");
        tty::println(what);
    }
    let lines = [
        line("failures", failures),
        line("iterations", iterations),
        line("vblanks", vblanks),
        line("gte_skips", skips),
        line("doubled", counts.doubled),
        line("missing", counts.missing),
    ];
    let (banner, tint) = if failures == 0 {
        ("GTEIRQ PASS", GREEN)
    } else {
        ("GTEIRQ FAIL", RED)
    };
    tty::print(banner);
    for (text, len) in &lines {
        tty::print(" ");
        tty::print(as_str(&text[..*len]));
    }
    tty::println("");

    gpu::init(VideoMode::Ntsc, Resolution::R320X240);
    let mut fb = FrameBuffer::new(320, 240);
    gpu::set_draw_area(0, 0, 319, 239);
    gpu::set_draw_offset(0, 0);
    let font = FontAtlas::upload(&BASIC, FONT_TPAGE, FONT_CLUT);
    loop {
        fb.clear(10, 12, 20);
        font.draw_text(8, 6, "GTE COMMANDS UNDER VBLANK IRQS", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 138;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu::draw_sync();
        interrupts::wait_vblank();
        fb.swap();
    }
}
