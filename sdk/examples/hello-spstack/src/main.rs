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
//! `stack-guard` checks the linked call tree fits the region.
//!
//! With the `chained-vector` feature the exception vector jumps to a
//! handler of the example's own that hands straight on to psx-rt's, the
//! way hk-psx's CD wrapper does, declared with
//! `interrupts::declare_stack_safe_handler`; the run must still pass.

#![no_std]
#![no_main]
#![cfg_attr(
    any(feature = "chained-vector", feature = "window"),
    feature(asm_experimental_arch)
)]

extern crate psx_rt;

use core::hint::black_box;
use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_rt::scratchpad::{self, assert_disjoint, Region, ScratchpadStack};
use psx_rt::{interrupts, tty};
use psx_vram::{Clut, TextureDepth, TexturePage};

const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
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
    (i as u32).wrapping_mul(0x9e37_79b9) ^ 0x0bad_f00d
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
        *word = seed.wrapping_mul(0x2545_f491).rotate_left(i as u32) ^ t;
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
    let entered = scratchpad::is_on_scratchpad_stack();
    let start = interrupts::vblank_count();
    let mut checksum = 0x1234_5678u32;
    for round in 0..rounds {
        checksum ^= level1(checksum.wrapping_add(round));
    }
    Outcome {
        checksum,
        irqs: interrupts::vblank_count().wrapping_sub(start),
        on_scratchpad: [entered, scratchpad::is_on_scratchpad_stack()],
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

/// The window workload: a recursion that is deeper than the scratchpad window, so the guard has to hop back.
#[cfg(feature = "window")]
mod window {
    use super::*;

    /// First scratchpad byte the window may use: everything below is the table.
    pub const FLOOR: usize = TABLE.len();
    /// Levels of recursion per call: about 80 bytes each, so the tail does not fit the 720-byte window.
    pub const DEPTH: u32 = 18;
    pub static mut LEVELS_ON_SCRATCHPAD: u32 = 0;
    pub static mut LEVELS_ON_RAM: u32 = 0;
    pub static mut NESTED_RUN_ON_SCRATCHPAD: u32 = 0;

    /// The recursion, once as the window target (`W` true: calls go through the guard) and once as the plain RAM
    /// reference (`W` false: ordinary calls), so the control run never calls a stubbed function directly.
    #[inline(never)]
    unsafe fn deep<const W: bool>(n: u32, seed: u32) -> u32 {
        let mut local = [0u32; 12];
        for (i, word) in local.iter_mut().enumerate() {
            let t = unsafe {
                table()
                    .add((seed as usize + i) % TABLE_WORDS)
                    .read_volatile()
            };
            *word = seed.wrapping_mul(0x2545_f491).rotate_left(i as u32) ^ t;
        }
        if W {
            unsafe {
                if scratchpad::is_on_scratchpad_stack() {
                    LEVELS_ON_SCRATCHPAD += 1;
                } else {
                    LEVELS_ON_RAM += 1;
                    // A nested run from a hopped-back region must run in place on the RAM stack.
                    let inside = WorkStack::run(|| scratchpad::is_on_scratchpad_stack());
                    NESTED_RUN_ON_SCRATCHPAD += u32::from(inside);
                }
            }
        }
        let local = black_box(&mut local);
        let below = if n == 0 {
            seed
        } else if W {
            unsafe { win_deep(n - 1, seed ^ local[3]) }
        } else {
            unsafe { deep::<false>(n - 1, seed ^ local[3]) }
        };
        local.iter().fold(below, |acc, &w| acc.rotate_left(3) ^ w)
    }

    #[inline(never)]
    unsafe fn entry<const W: bool>(depth: u32, seed: u32) -> u32 {
        let mut local = [0u32; 6];
        for (i, word) in local.iter_mut().enumerate() {
            *word = seed.wrapping_add(i as u32 * 0x1234_5679);
        }
        let local = black_box(&mut local);
        let below = if W {
            unsafe { win_deep(depth, seed) }
        } else {
            unsafe { deep::<false>(depth, seed) }
        };
        local
            .iter()
            .fold(below, |acc, &w| acc.wrapping_mul(31).wrapping_add(w))
    }

    #[inline(never)]
    unsafe fn deep_impl(n: u32, seed: u32) -> u32 {
        unsafe { deep::<true>(n, seed) }
    }

    #[inline(never)]
    unsafe fn entry_impl(depth: u32, seed: u32) -> u32 {
        unsafe { entry::<true>(depth, seed) }
    }

    // `need` comes from `window-gate --emit`; the gate fails the build when it is too small.
    psx_rt::window_fn!(guard FLOOR + 160, true, "__spwin_deep", ext_deep, win_deep, deep_impl,
        (n: u32, seed: u32) -> u32);
    psx_rt::window_fn!(level1, true, "__spwin_entry", ext_entry, win_entry, entry_impl,
        (depth: u32, seed: u32) -> u32);

    /// Plain run: the guard stubs see a RAM `$sp` and call straight through.
    #[inline(never)]
    pub fn ram_workload(rounds: u32) -> u32 {
        let mut checksum = 0x1234_5678u32;
        for round in 0..rounds {
            checksum ^= unsafe { entry::<false>(DEPTH, checksum.wrapping_add(round)) };
        }
        checksum
    }

    /// Window run: every call switches `$sp` onto the scratchpad and the deep tail hops back.
    #[inline(never)]
    pub fn window_workload(rounds: u32) -> (u32, u32) {
        let start = interrupts::vblank_count();
        let mut checksum = 0x1234_5678u32;
        for round in 0..rounds {
            checksum ^= unsafe { win_entry(DEPTH, checksum.wrapping_add(round)) };
        }
        (checksum, interrupts::vblank_count().wrapping_sub(start))
    }
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
        psx_rt::cache::flush_instruction_cache();
        interrupts::declare_stack_safe_handler(hello_spstack_vector);
    }
    for i in 0..TABLE_WORDS {
        unsafe { table().add(i).write_volatile(table_word(i)) };
    }
    // A RAM frame the scratchpad run must leave alone.
    let ram_frame = black_box([0xa5a5_0000u32 | 0x1111, 0x2222, 0x3333, 0x4444]);

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
            nested = WorkStack::run(|| [scratchpad::is_on_scratchpad_stack(), true]);
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
        (!scratchpad::is_on_scratchpad_stack(), "$sp not restored"),
        (
            black_box(ram_frame) == [0xa5a5_1111, 0x2222, 0x3333, 0x4444],
            "RAM frame changed",
        ),
        (
            (0..TABLE_WORDS).all(|i| unsafe { table().add(i).read_volatile() } == table_word(i)),
            "table below the stack changed",
        ),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
    ];
    #[cfg(feature = "window")]
    let window_failures = {
        let ram_sum = window::ram_workload(black_box(ROUNDS * 8));
        let (win_sum, win_irqs) = window::window_workload(black_box(ROUNDS * 8));
        let (spad_levels, ram_levels, nested) = unsafe {
            (
                window::LEVELS_ON_SCRATCHPAD,
                window::LEVELS_ON_RAM,
                window::NESTED_RUN_ON_SCRATCHPAD,
            )
        };
        let window_checks = [
            (
                ram_sum == win_sum,
                "window checksum differs from the RAM run",
            ),
            (spad_levels > 0, "window run never used the scratchpad"),
            (
                ram_levels > ROUNDS,
                "guard never hopped back to the RAM stack",
            ),
            (
                win_irqs >= MIN_IRQS_ON_STACK,
                "too few IRQs during the window run",
            ),
            (
                nested == 0,
                "a nested run inside a hopped-back region left the RAM stack",
            ),
            (
                !scratchpad::is_on_scratchpad_stack() && !psx_rt::scratchpad_window::is_active(),
                "window left $sp or its state behind",
            ),
        ];
        for (ok, what) in window_checks.iter() {
            if !*ok {
                tty::print("SPSTACK window check failed: ");
                tty::println(what);
            }
        }
        tty::print("SPSTACK window");
        for (label, v) in [
            ("sum", win_sum),
            ("irqs", win_irqs),
            ("spad_levels", spad_levels),
            ("ram_levels", ram_levels),
        ] {
            let (text, len) = line(label, v);
            tty::print(" ");
            tty::print(as_str(&text[..len]));
        }
        tty::println("");
        window_checks.iter().filter(|c| !c.0).count() as u32
    };
    #[cfg(not(feature = "window"))]
    let window_failures = 0u32;
    let failures = checks.iter().filter(|check| !check.0).count() as u32 + window_failures;
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
    loop {
        fb.clear(&mut gpu, (10, 12, 20));
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
        gpu.wait_idle();
        interrupts::wait_vblank();
        fb.swap(&mut gpu);
    }
}
