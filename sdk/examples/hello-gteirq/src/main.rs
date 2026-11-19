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
//! A second phase runs the same loop under root counter 2 interrupts that psx-rt's
//! dispatcher serves (`psx_rt::dispatch::attach`, the path every driver's
//! interrupts now take), at 160 consecutive periods so the interrupt
//! lands on every phase of the loop, including the RTPS itself. NCLIP is not
//! probed: it is idempotent, so a repeat cannot be seen in its result.
//!
//! Before either phase the dispatcher's order is checked: two VBlank hooks and a
//! handler on every source record who ran, the same assembly an interrupt uses
//! runs for a set of pending masks (`dispatch_for_test`), and the record must
//! equal `dispatch::plan`, the pure reference. The two hooks then stay
//! registered through the VBlank phase and must each run once per VBlank, in order.
//!
//! The foreground also holds a known value in `$28` (the global pointer, which the compiler
//! uses as a general register, so a handler may leave it changed and the interrupted code
//! may hold a live value in it) across a loop, while a VBlank hook and the root counter 2
//! handler each overwrite `$28`. The dispatcher saves and restores it; the check fails if
//! the foreground ever sees it changed.
//!
//! The verdict goes to the TTY (`GTEIRQ PASS ...` or `GTEIRQ FAIL ...`) and
//! on screen. `gte_skips` counts the interrupts the handler stepped over a
//! GTE command for: about one in forty VBlanks on silicon, where the loop is
//! mostly RTPS-adjacent instructions. Since emulator commit c7e3ea8 PSoXide
//! also takes an interrupt with EPC on a GTE command, as silicon does, so
//! the probe can fail there too; a console run remains the reference.

#![no_std]
#![no_main]
#![feature(asm_experimental_arch)]

extern crate psx_rt;

use psx_font::fonts::BASIC;
use psx_font::FontAtlas;
use psx_gpu::display::{DisplayConfig, DoubleBuffer, Resolution, VideoMode};
use psx_gpu::Gpu;
use psx_hw::irq::source;
use psx_io::irq;
use psx_io::timers::{self, Timer};
use psx_rt::{dispatch, interrupts, tty};
use psx_vram::{Clut, TextureDepth, TexturePage};

/// Root counter 2 mode bits: reset at target, interrupt at target, repeat.
const RESET_AT_TARGET: u16 = 1 << 3;
const IRQ_ON_TARGET: u16 = 1 << 4;
const IRQ_REPEAT: u16 = 1 << 6;
const FONT_TPAGE: TexturePage = TexturePage::new(320, 0, TextureDepth::Bit4);
const FONT_CLUT: Clut = Clut::new(320, 256);
const WHITE: (u8, u8, u8) = (220, 220, 230);
const GREEN: (u8, u8, u8) = (80, 220, 100);
const RED: (u8, u8, u8) = (230, 80, 80);

/// Written to SXY0/SXY1/SXY2 before each RTPS. After one RTPS SXY0 holds
/// B, after two C, after none A.
const SXY_A: u32 = 0x5a5a_1111;
const SXY_B: u32 = 0x5a5a_2222;
const SXY_C: u32 = 0x5a5a_3333;
/// Phase 2: periods of root counter 2 in system clocks, `PERIOD_FIRST` up, one
/// more each step, so the interrupt walks every instruction of the loop (it
/// is 50 to 80 clocks long).
const PERIOD_FIRST: u16 = 2000;
const PERIOD_STEPS: u16 = 160;
/// Iterations at each period.
const PERIOD_ITERATIONS: u32 = 320;

static mut TIMER_IRQS: u32 = 0;

/// What the foreground keeps in `$28` while the handlers run.
const GP_SENTINEL: u32 = 0x600d_cafe;
/// What the `$28` handlers leave in it.
const GP_CLOBBER: u32 = 0xbad0_0028;
static mut GP_HOOK_RUNS: u32 = 0;
static mut GP_TIMER_RUNS: u32 = 0;

/// Overwrite `$28` the way compiled code may, with no save: the dispatcher has to put it back.
#[inline(always)]
fn clobber_gp() {
    // SAFETY: the point of the call; nothing after it in the handler reads `$28`.
    unsafe { core::arch::asm!("li $28, {v}", v = const GP_CLOBBER, options(nomem, nostack)) };
}

/// A VBlank hook that leaves `$28` changed.
#[inline(never)]
fn gp_hook() {
    // SAFETY: runs with interrupts off; the foreground only reads it.
    unsafe { *(&raw mut GP_HOOK_RUNS) += 1 };
    clobber_gp();
}

/// The foreground side: hold `GP_SENTINEL` in `$28` for `iterations` loop passes and count the
/// passes that found it changed (each is put back, so one interrupt counts once). The whole
/// loop is one asm block, so no compiled code of ours has a live value in `$28` meanwhile; it
/// is saved before and restored after.
#[inline(never)]
fn gp_loop(iterations: u32) -> u32 {
    let mut changed = 0u32;
    let _saved: u32;
    // SAFETY: restores `$28` before it ends; touches no memory.
    unsafe {
        core::arch::asm!(
            ".set noreorder",
            "move {saved}, $28",
            "move $28, {sentinel}",
            "1:",
            "beq $28, {sentinel}, 2f",
            "nop",
            "addiu {changed}, {changed}, 1",
            "move $28, {sentinel}",
            "2:",
            "addiu {left}, {left}, -1",
            "bnez {left}, 1b",
            "nop",
            "move $28, {saved}",
            ".set reorder",
            saved = out(reg) _saved,
            sentinel = in(reg) GP_SENTINEL,
            changed = inout(reg) changed,
            left = inout(reg) iterations => _,
            options(nomem, nostack)
        );
    }
    changed
}

/// The dispatcher's handler for root counter 2.
fn on_timer() {
    irq::acknowledge(1 << source::TIMER2);
    // SAFETY: runs with interrupts off; the foreground only reads it.
    unsafe {
        *(&raw mut TIMER_IRQS) += 1;
        *(&raw mut GP_TIMER_RUNS) += 1;
    }
    clobber_gp();
}

/// Who ran during an order check, one code per call.
static mut LOG: [u8; 16] = [0; 16];
static mut LOG_LEN: usize = 0;
/// Hook runs counted in the VBlank phase and hook B runs that followed hook A's.
static mut HOOK_A: u32 = 0;
static mut HOOK_B: u32 = 0;
static mut HOOK_ORDER_ERRORS: u32 = 0;
static mut LOGGING: bool = false;

fn record(code: u8) {
    // SAFETY: handlers run with interrupts off and the foreground reads only
    // while they cannot.
    unsafe {
        if *(&raw const LOGGING) && *(&raw const LOG_LEN) < 16 {
            LOG[*(&raw const LOG_LEN)] = code;
            *(&raw mut LOG_LEN) += 1;
        }
    }
}

fn hook_a() {
    record(0x80);
    // SAFETY: as above.
    unsafe { *(&raw mut HOOK_A) += 1 };
}

fn hook_b() {
    record(0x81);
    // SAFETY: as above.
    unsafe {
        if *(&raw const HOOK_A) == *(&raw const HOOK_B) + 1 {
            *(&raw mut HOOK_B) += 1;
        } else {
            *(&raw mut HOOK_ORDER_ERRORS) += 1;
        }
    }
}

/// A handler that records its source's `I_STAT` bit.
fn rec<const BIT: u8>() {
    record(BIT);
}

const ORDER_SOURCES: [(dispatch::Source, dispatch::Handler); 4] = [
    (dispatch::Source::Cdrom, rec::<2>),
    (dispatch::Source::Dma, rec::<3>),
    (dispatch::Source::Timer1, rec::<5>),
    (dispatch::Source::Timer2, rec::<6>),
];

/// Pending masks the order check runs: everything, single sources, VBlank
/// alone and with others, sources nothing is registered for (bits 4, 7 and 9),
/// bit 1 (no source), and nothing.
const ORDER_MASKS: [u32; 10] = [
    0x26d, 0x004, 0x001, 0x201, 0x090, 0x040, 0x002, 0, 0x2ad, 0x028,
];

/// Run `ORDER_MASKS` through the assembly and the reference; the number that
/// disagreed.
fn order_mismatches() -> u32 {
    let mut known = [(0u32, 0u8); 6];
    known[0] = (dispatch::address_of(hook_a), 0x80);
    known[1] = (dispatch::address_of(hook_b), 0x81);
    let mut n = 2;
    for (source, handler) in ORDER_SOURCES {
        known[n] = (dispatch::address_of(handler), source.bit() as u8);
        n += 1;
    }
    let mut bad = 0;
    for mask in ORDER_MASKS {
        let mut expected = [0u8; 16];
        let mut expected_len = 0;
        dispatch::plan(mask, |address| {
            let code = known.iter().find(|k| k.0 == address).map_or(0xff, |k| k.1);
            expected[expected_len] = code;
            expected_len += 1;
        });
        // SAFETY: interrupts are off for the whole check and every handler logs only.
        let ran = irq::without_interrupts(|| unsafe {
            LOG_LEN = 0;
            LOGGING = true;
            dispatch::dispatch_for_test(mask);
            LOGGING = false;
            (LOG, LOG_LEN)
        });
        if ran.0[..ran.1] != expected[..expected_len] {
            bad += 1;
        }
    }
    bad
}

/// Iterations between checks of the VBlank count.
const CHUNK: u32 = 4096;
/// Loop passes of one `$28` check (five instructions each).
const GP_ITERATIONS: u32 = 20_000;
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
        text[label.len() + 1 + i] = DIGITS[(value >> (28 - 4 * i) & 0xf) as usize];
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

    // The order check, then the two hooks that stay for the VBlank phase.
    let mut hooks_ok = dispatch::on_vblank(hook_a).is_ok() && dispatch::on_vblank(hook_b).is_ok();
    for (source, handler) in ORDER_SOURCES {
        dispatch::attach(source, handler);
    }
    let order_bad = order_mismatches();
    for (source, _) in ORDER_SOURCES {
        dispatch::detach(source);
    }
    let mask_after_detach = dispatch::registered_sources();
    // SAFETY: foreground, interrupts on, the VBlank hooks run only at VBlank.
    unsafe {
        HOOK_A = 0;
        HOOK_B = 0;
        HOOK_ORDER_ERRORS = 0;
    }
    hooks_ok &= mask_after_detach == 1;
    // The `$28` hook joins after the order check, which counts only hooks A and B.
    hooks_ok &= dispatch::on_vblank(gp_hook).is_ok();
    let mut gp_changed = 0u32;

    let skips_before = interrupts::gte_skip_count();
    let start = interrupts::vblank_count();
    let mut counts = Counts::default();
    let mut iterations = 0u32;
    while interrupts::vblank_count().wrapping_sub(start) < VBLANKS {
        gte_loop(CHUNK, &mut counts);
        iterations = iterations.wrapping_add(CHUNK);
        gp_changed += gp_loop(GP_ITERATIONS);
    }
    let vblanks = interrupts::vblank_count().wrapping_sub(start);
    // SAFETY: plain reads of counters the hooks wrote.
    let (hook_a_runs, hook_b_runs, hook_order_errors) =
        unsafe { (HOOK_A, HOOK_B, HOOK_ORDER_ERRORS) };
    // SAFETY: as above.
    let gp_hook_runs = unsafe { core::ptr::read_volatile(&raw const GP_HOOK_RUNS) };

    // Phase 2: root counter 2 through the dispatcher.
    let mut timer_counts = Counts::default();
    let mut timer_iterations = 0u32;
    dispatch::attach(dispatch::Source::Timer2, on_timer);
    let skips_mid = interrupts::gte_skip_count();
    let irqs_before = unsafe { core::ptr::read_volatile(&raw const TIMER_IRQS) };
    for step in 0..PERIOD_STEPS {
        timers::set_mode(Timer::Timer2, 0);
        timers::set_target(Timer::Timer2, PERIOD_FIRST + step);
        timers::set_counter(Timer::Timer2, 0);
        irq::acknowledge(1 << source::TIMER2);
        irq::set_mask(irq::mask() | (1 << source::TIMER2));
        timers::set_mode(Timer::Timer2, RESET_AT_TARGET | IRQ_ON_TARGET | IRQ_REPEAT);
        gte_loop(PERIOD_ITERATIONS, &mut timer_counts);
        timer_iterations += PERIOD_ITERATIONS;
        gp_changed += gp_loop(GP_ITERATIONS);
        timers::set_mode(Timer::Timer2, 0);
    }
    irq::set_mask(irq::mask() & !(1 << source::TIMER2));
    let timer_irqs =
        unsafe { core::ptr::read_volatile(&raw const TIMER_IRQS) }.wrapping_sub(irqs_before);
    let timer_skips = interrupts::gte_skip_count().wrapping_sub(skips_mid);
    dispatch::detach(dispatch::Source::Timer2);
    let skips = interrupts::gte_skip_count().wrapping_sub(skips_before);

    let checks = [
        (counts.doubled == 0, "an RTPS ran twice"),
        (counts.missing == 0, "an RTPS was lost"),
        (counts.other == 0, "SXY0 held no marker"),
        (counts.ok == iterations, "not every iteration counted"),
        (interrupts::fault_count() == 0, "exceptions other than IRQs"),
        (
            hooks_ok,
            "the dispatcher refused a hook or kept a detached source",
        ),
        (
            order_bad == 0,
            "the dispatcher's order differs from its reference",
        ),
        (
            hook_a_runs.abs_diff(vblanks) <= 1,
            "hook A did not run once per VBlank",
        ),
        (
            hook_b_runs == hook_a_runs || hook_b_runs + 1 == hook_a_runs,
            "hook B did not follow hook A",
        ),
        (hook_order_errors == 0, "a hook ran out of order"),
        (
            timer_counts.doubled == 0,
            "an RTPS ran twice under timer IRQs",
        ),
        (
            timer_counts.missing == 0,
            "an RTPS was lost under timer IRQs",
        ),
        (
            timer_counts.other == 0,
            "SXY0 held no marker under timer IRQs",
        ),
        (
            timer_counts.ok == timer_iterations,
            "not every timer iteration counted",
        ),
        (timer_irqs > 500, "the dispatcher served too few timer IRQs"),
        (timer_skips >= 10, "too few timer IRQs landed on an RTPS"),
        (gp_changed == 0, "an interrupt handler left $28 changed"),
        (
            gp_hook_runs.abs_diff(vblanks) <= 1
                && unsafe { core::ptr::read_volatile(&raw const GP_TIMER_RUNS) } > 500,
            "the $28 handlers did not run",
        ),
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
        line("order_bad", order_bad),
        line("hook_a", hook_a_runs),
        line("timer_irqs", timer_irqs),
        line("timer_skips", timer_skips),
        line("timer_doubled", timer_counts.doubled),
        line("timer_missing", timer_counts.missing),
        line("gp_changed", gp_changed),
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
        gpu.wait_idle();
        interrupts::wait_vblank();
        fb.swap(&mut gpu);
    }
}
