//! `hello-spstack` -- does code run correctly with its stack in the
//! scratchpad while VBlank interrupts keep arriving?
//!
//! A deterministic, stack-heavy workload (three levels of calls, each holding
//! an array in its frame, reading a table kept in the scratchpad below the
//! stack) runs once on the RAM stack and once through
//! `psx_rt::scratchpad::ScratchpadStack::run`, long enough that many VBlank
//! IRQs land while `$sp` is in the scratchpad. The two results must match,
//! the workload must have seen the IRQs from the scratchpad stack, the table
//! and the caller's RAM frame must be intact, and a nested call must run in
//! place. psx-rt's `scratchpad-stack-check` feature is on, so the region's
//! canary and the exception-vector check run around every call too.
//!
//! The verdict goes to the TTY (`SPSTACK PASS ...` or `SPSTACK FAIL ...`)
//! and on screen, so the same disc reads out on a console.
//! `tools/stack_guard.py` checks the linked call tree fits the region.
//!
//! With the `chained-vector` feature the exception vector jumps to a
//! handler of the example's own that hands straight on to psx-rt's, the
//! way hk-psx's CD wrapper does, declared with
//! `interrupts::declare_stack_safe_handler`; the run must still pass.

#![no_std]
#![no_main]
#![cfg_attr(feature = "chained-vector", feature(asm_experimental_arch))]

extern crate psx_rt;

use core::hint::black_box;
use psx_font::{fonts::BASIC, FontAtlas};
use psx_gpu::{self as gpu, framebuf::FrameBuffer, Resolution, VideoMode};
use psx_rt::interrupts;
use psx_rt::scratchpad::{self, assert_disjoint, Region, ScratchpadStack};
use psx_rt::tty;
use psx_vram::{Clut, TexDepth, Tpage};

const FONT_TPAGE: Tpage = Tpage::new(320, 0, TexDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Read by the workload while it runs; must survive the stack below it.
const TABLE: Region = Region::new(0, 256);
/// The workload's stack: everything above the table.
type WorkStack = ScratchpadStack<256, 1024>;
const _: () = assert_disjoint(&[TABLE, WorkStack::REGION]);

const TABLE_WORDS: usize = TABLE.len() / 4;
/// Enough rounds to span a good number of VBlanks.
const ROUNDS: u32 = 96;
/// The scratchpad run must see at least this many IRQs from inside.
const MIN_IRQS_ON_STACK: u32 = 8;

fn table() -> *mut u32 {
    TABLE.addr() as *mut u32
}

fn table_word(i: usize) -> u32 {
    (i as u32).wrapping_mul(0x9E37_79B9) ^ 0x0BAD_F00D
}

#[inline(never)]
fn level3(seed: u32) -> u32 {
    let mut local = [0u32; 24];
    for (i, word) in local.iter_mut().enumerate() {
        let t = unsafe {
            table()
                .add((seed as usize + i) % TABLE_WORDS)
                .read_volatile()
        };
        *word = seed.wrapping_mul(0x2545_F491).rotate_left(i as u32) ^ t;
    }
    // Keep the array in the frame: an IRQ between the fill and the fold must
    // not disturb it.
    let local = black_box(&mut local);
    let mut acc = seed;
    for word in local.iter().rev() {
        acc = acc.rotate_left(5) ^ word;
    }
    acc
}

#[inline(never)]
fn level2(seed: u32) -> u32 {
    let mut local = [0u32; 16];
    for (i, word) in local.iter_mut().enumerate() {
        *word = level3(seed ^ (i as u32).wrapping_mul(0x0101_0101));
    }
    let local = black_box(&mut local);
    local
        .iter()
        .fold(seed, |acc, &w| acc.wrapping_mul(31).wrapping_add(w))
}

#[inline(never)]
fn level1(seed: u32) -> u32 {
    let mut local = [0u32; 8];
    for (i, word) in local.iter_mut().enumerate() {
        *word = level2(seed.wrapping_add(i as u32 * 0x1234_5679));
    }
    let local = black_box(&mut local);
    local.iter().fold(seed, |acc, &w| acc.rotate_left(3) ^ w)
}

/// What one run of the workload saw.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Outcome {
    checksum: u32,
    /// VBlanks counted between the workload's first and last instruction.
    irqs: u32,
    /// `$sp` was in the scratchpad on entry and exit.
    on_scratchpad: [bool; 2],
}

#[inline(never)]
fn workload(rounds: u32) -> Outcome {
    let entered = scratchpad::on_scratchpad_stack();
    let start = interrupts::vblank_count();
    let mut checksum = 0x1234_5678u32;
    for round in 0..rounds {
        checksum ^= level1(checksum.wrapping_add(round));
    }
    Outcome {
        checksum,
        irqs: interrupts::vblank_count().wrapping_sub(start),
        on_scratchpad: [entered, scratchpad::on_scratchpad_stack()],
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

// A handler of the game's own in the vector: it leaves $sp alone and
// chains to psx-rt's.
#[cfg(feature = "chained-vector")]
core::arch::global_asm!(
    ".set noreorder",
    ".section .text.hello_spstack_vector",
    ".globl hello_spstack_vector",
    "hello_spstack_vector:",
    "j __psx_rt_exception_handler",
    "nop",
    ".set reorder"
);

#[cfg(feature = "chained-vector")]
extern "C" {
    fn hello_spstack_vector();
}

#[no_mangle]
fn main() {
    interrupts::install_vblank_counter();
    #[cfg(feature = "chained-vector")]
    // SAFETY: the vector is one `j` and a nop; the handler touches no
    // register but psx-rt's own $k0/$k1.
    unsafe {
        let handler = hello_spstack_vector as *const () as usize as u32;
        (0x8000_0080 as *mut u32).write_volatile(0x0800_0000 | ((handler >> 2) & 0x03ff_ffff));
        (0x8000_0084 as *mut u32).write_volatile(0);
        psx_rt::cache::flush_i_cache();
        interrupts::declare_stack_safe_handler(hello_spstack_vector);
    }
    for i in 0..TABLE_WORDS {
        unsafe { table().add(i).write_volatile(table_word(i)) };
    }
    // A RAM frame the scratchpad run must leave alone.
    let ram_frame = black_box([0xA5A5_0000u32 | 0x1111, 0x2222, 0x3333, 0x4444]);

    let ram = workload(black_box(ROUNDS));

    #[cfg(feature = "panic-on-stack")]
    unsafe {
        WorkStack::run::<(), _>(|| {
            if level1(black_box(1)) != 0 {
                panic!("deliberate panic on the scratchpad stack");
            }
        })
    };

    let start = interrupts::vblank_count();
    let mut nested = [false; 2];
    let spad = unsafe {
        WorkStack::run(|| {
            // A nested call runs in place, still on the scratchpad.
            nested = WorkStack::run(|| [scratchpad::on_scratchpad_stack(), true]);
            workload(black_box(ROUNDS))
        })
    };
    let spad_vblanks = interrupts::vblank_count().wrapping_sub(start);

    let checks = [
        (ram.checksum == spad.checksum, "checksums differ"),
        (
            ram.on_scratchpad == [false, false],
            "RAM run saw the scratchpad",
        ),
        (
            spad.on_scratchpad == [true, true],
            "run did not use the scratchpad",
        ),
        (
            spad.irqs >= MIN_IRQS_ON_STACK,
            "too few IRQs on the scratchpad stack",
        ),
        (nested == [true, true], "nested run did not run in place"),
        (!scratchpad::on_scratchpad_stack(), "$sp not restored"),
        (
            black_box(ram_frame) == [0xA5A5_1111, 0x2222, 0x3333, 0x4444],
            "RAM frame changed",
        ),
        (
            (0..TABLE_WORDS).all(|i| unsafe { table().add(i).read_volatile() } == table_word(i)),
            "table below the stack changed",
        ),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    let failures = checks.iter().filter(|check| !check.0).count() as u32;
    for (_, what) in checks.iter().filter(|check| !check.0) {
        tty::print("SPSTACK check failed: ");
        tty::println(what);
    }
    let lines = [
        line("failures", failures),
        line("checksum", spad.checksum),
        line("irqs_on_stack", spad.irqs),
        line("vblanks_ram", ram.irqs),
        line("vblanks_spad", spad_vblanks),
    ];
    let (banner, tint) = if failures == 0 {
        ("SPSTACK PASS", GREEN)
    } else {
        ("SPSTACK FAIL", RED)
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
        font.draw_text(8, 6, "SCRATCHPAD STACK UNDER VBLANK IRQS", WHITE);
        font.draw_text(8, 30, banner, tint);
        for (row, (text, len)) in lines.iter().enumerate() {
            font.draw_text(8, 54 + 12 * row as i16, as_str(&text[..*len]), WHITE);
        }
        let mut y = 126;
        for (_, what) in checks.iter().filter(|check| !check.0) {
            font.draw_text(8, y, what, RED);
            y += 12;
        }
        gpu::draw_sync();
        interrupts::wait_vblank();
        fb.swap();
    }
}
