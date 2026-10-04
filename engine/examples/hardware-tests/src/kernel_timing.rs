// SPDX-License-Identifier: GPL-2.0-or-later
//! CONSOLE TESTS: KERNEL TIMING. What the real BIOS's kernel costs, in
//! system-clock cycles from root counter 2.
//!
//! The SDK runtime replaces the BIOS exception vector with its own handler,
//! so on a normal run no kernel code executes at all. This case puts the
//! vector the BIOS left (snapshotted in `main` before the runtime touches it)
//! back for the measurements and then reinstalls the runtime's:
//!
//! - EnterCriticalSection and ExitCriticalSection, `SYSCALL` 1 and 2, timed
//!   one call at a time. The same timing around an empty call is the harness
//!   cost and is subtracted on screen. Interrupts are masked at the interrupt
//!   controller for this part, so no interrupt can land inside a sample.
//! - A VBlank interrupt round trip through the BIOS: a kernel event for
//!   VBlank (class F2000003h, spec 2) is opened and enabled, only VBlank is
//!   unmasked, and a loop reads the counter back to back. An interrupt shows
//!   as one read that is much later than the one before; that gap is the
//!   round trip, from the interrupted instruction to the same instruction
//!   again. The runtime's own handler gets the same measurement for
//!   reference. Both gaps include one loop iteration.
//!
//! The VBlank part is last and may hang a console whose kernel does not
//! acknowledge a VBlank nobody handles; the screen shows the earlier results
//! before it starts, so a hang still leaves them to read.
//!
//! The emulator's kernel is its own HLE one, so the same case run headless
//! measures that.

use crate::console_tests::{
    put_number, record, spread, text, Buttons, Screen, KERNEL_COUNT, KERNEL_RECORD,
};
use crate::TimingRecord;
use psx_font::FontAtlas;
use psx_io::{irq, timers};
use psx_rt::interrupts;

/// The general exception vector, and the four words the BIOS keeps there.
const VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
const BIOS_VECTOR: [u32; 4] = [0x3C1A_0000, 0x275A_0C80, 0x0340_0008, 0];

/// What `main` found at the vector before the runtime replaced it.
static mut SNAPSHOT: [u32; 4] = BIOS_VECTOR;
static mut SNAPSHOT_STANDARD: bool = false;

core::arch::global_asm!(
    ".set noreorder",
    ".section .text.hwtest_kernel",
    // The Psy-Q shape: function number in $a0, SYSCALL, return.
    ".globl __hwtest_enter_cs",
    "__hwtest_enter_cs:",
    "  li $4, 1",
    "  .word 0x0000000C",
    "  jr $31",
    "  nop",
    ".globl __hwtest_exit_cs",
    "__hwtest_exit_cs:",
    "  li $4, 2",
    "  .word 0x0000000C",
    "  jr $31",
    "  nop",
    // The harness: the same call and return with nothing in between.
    ".globl __hwtest_noop",
    "__hwtest_noop:",
    "  jr $31",
    "  move $2, $0",
    // B-table calls: table address in $t2, function number in $t1.
    ".globl __hwtest_open_event",
    "__hwtest_open_event:",
    "  li $10, 0xB0",
    "  jr $10",
    "  li $9, 0x08",
    ".globl __hwtest_close_event",
    "__hwtest_close_event:",
    "  li $10, 0xB0",
    "  jr $10",
    "  li $9, 0x09",
    ".globl __hwtest_test_event",
    "__hwtest_test_event:",
    "  li $10, 0xB0",
    "  jr $10",
    "  li $9, 0x0B",
    ".globl __hwtest_enable_event",
    "__hwtest_enable_event:",
    "  li $10, 0xB0",
    "  jr $10",
    "  li $9, 0x0C",
    ".set reorder",
);

extern "C" {
    fn __hwtest_enter_cs() -> u32;
    fn __hwtest_exit_cs() -> u32;
    fn __hwtest_noop() -> u32;
    fn __hwtest_open_event(class: u32, spec: u32, mode: u32, func: u32) -> u32;
    fn __hwtest_close_event(event: u32) -> u32;
    fn __hwtest_test_event(event: u32) -> u32;
    fn __hwtest_enable_event(event: u32) -> u32;
}

/// Record the vector the BIOS left. Call from `main`, before the runtime or
/// the engine can install a handler.
pub(crate) fn snapshot_vector() {
    // SAFETY: plain reads of kernel RAM and writes of two private statics, on
    // the one thread, before any interrupt handler of ours exists.
    unsafe {
        let mut words = [0u32; 4];
        for (i, word) in words.iter_mut().enumerate() {
            *word = core::ptr::read_volatile(VECTOR.add(i));
        }
        SNAPSHOT_STANDARD = words == BIOS_VECTOR;
        if SNAPSHOT_STANDARD {
            SNAPSHOT = words;
        }
    }
}

/// Make the BIOS's own vector the live one.
fn use_bios_vector() {
    // SAFETY: four aligned words of the exception vector, written with
    // interrupts masked at the controller and the cache flushed behind them.
    unsafe {
        let words = *core::ptr::addr_of!(SNAPSHOT);
        for (i, word) in words.iter().enumerate() {
            core::ptr::write_volatile(VECTOR.add(i), *word);
        }
    }
    psx_rt::cache::flush_instruction_cache();
}

/// Put the runtime's handler back (this also unmasks VBlank and enables CPU
/// interrupts, as at start-up).
fn use_runtime_vector() {
    interrupts::install_vblank_counter();
}

/// Cycles of one call, from root counter 2 (system clock, 16 bits).
#[inline(never)]
fn time_call(call: unsafe extern "C" fn() -> u32) -> u32 {
    let before = timers::counter(timers::Timer::Timer2);
    // SAFETY: each caller passes one of the stubs above with the vector it
    // needs in place.
    unsafe { call() };
    let after = timers::counter(timers::Timer::Timer2);
    after.wrapping_sub(before) as u32
}

fn start_counter() {
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
}

/// Samples of each call, after the warm-up that fills the I-cache.
const SAMPLES: usize = 16;
const WARMUP: usize = 4;

/// Min, median and max of a set of timings, in cycles.
#[derive(Copy, Clone, Default)]
pub(crate) struct Stat {
    pub(crate) count: u32,
    pub(crate) min: u32,
    pub(crate) med: u32,
    pub(crate) max: u32,
}

impl Stat {
    fn of(values: &mut [u32]) -> Self {
        let (min, med, max) = spread(values);
        Self {
            count: values.len() as u32,
            min,
            med,
            max,
        }
    }
}

#[derive(Copy, Clone, Default)]
pub(crate) struct Outcome {
    /// The vector `main` found was the standard BIOS one.
    pub(crate) standard_vector: bool,
    pub(crate) harness: Stat,
    pub(crate) enter: Stat,
    pub(crate) exit: Stat,
    /// VBlank round trip through the runtime's handler (reference).
    pub(crate) sdk_vblank: Stat,
    /// VBlank round trip through the BIOS; `count` 0 until it has run.
    pub(crate) bios_vblank: Stat,
    /// The kernel event for VBlank: its handle, or `None` if it would not open.
    pub(crate) event: Option<u32>,
    /// Gaps after which the event had been delivered.
    pub(crate) events_seen: u32,
    /// The BIOS VBlank loop ran to its end.
    pub(crate) bios_done: bool,
}

/// EnterCriticalSection, ExitCriticalSection and the harness, with the BIOS
/// vector live and no interrupt source unmasked.
#[inline(never)]
pub(crate) fn measure_critical_sections(out: &mut Outcome) {
    let mask = irq::mask();
    irq::set_mask(0);
    use_bios_vector();
    start_counter();
    let mut harness = [0u32; SAMPLES];
    let mut enter = [0u32; SAMPLES];
    let mut exit = [0u32; SAMPLES];
    for round in 0..WARMUP + SAMPLES {
        let h = time_call(__hwtest_noop);
        let e = time_call(__hwtest_enter_cs);
        let x = time_call(__hwtest_exit_cs);
        if let Some(i) = round.checked_sub(WARMUP) {
            harness[i] = h;
            enter[i] = e;
            exit[i] = x;
        }
    }
    use_runtime_vector();
    irq::set_mask(mask | 1);
    out.harness = Stat::of(&mut harness);
    out.enter = Stat::of(&mut enter);
    out.exit = Stat::of(&mut exit);
}

/// Gaps collected per VBlank measurement, and the iteration cap that ends a
/// loop that sees fewer (about a second at 14 cycles an iteration).
const GAPS: usize = 24;
const ITERATIONS: u32 = 2_500_000;

/// Read root counter 2 back to back until `GAPS` reads came much later than
/// the one before (more than `threshold` cycles), or the cap. `on_gap` runs
/// after each, outside the timing. Returns the gaps found.
fn collect_gaps(threshold: u32, mut on_gap: impl FnMut()) -> ([u32; GAPS], usize) {
    let mut gaps = [0u32; GAPS];
    let mut found = 0usize;
    let mut last = timers::counter(timers::Timer::Timer2);
    for _ in 0..ITERATIONS {
        let now = timers::counter(timers::Timer::Timer2);
        let step = now.wrapping_sub(last) as u32;
        if step > threshold {
            gaps[found] = step;
            found += 1;
            on_gap();
            if found == GAPS {
                break;
            }
            last = timers::counter(timers::Timer::Timer2);
        } else {
            last = now;
        }
    }
    (gaps, found)
}

/// A VBlank round trip through the runtime's own handler, for reference.
#[inline(never)]
pub(crate) fn measure_runtime_vblank(out: &mut Outcome) {
    start_counter();
    interrupts::wait_vblank();
    let (mut gaps, found) = collect_gaps(40, || {});
    out.sdk_vblank = Stat::of(&mut gaps[..found]);
}

/// A VBlank round trip through the BIOS's handler, with a kernel event for
/// VBlank open so the BIOS delivers it. May hang a kernel that does not
/// acknowledge the interrupt.
#[inline(never)]
pub(crate) fn measure_bios_vblank(out: &mut Outcome) {
    // SAFETY: BIOS B-table calls through the stubs above, with the BIOS's own
    // kernel RAM intact (the program never touches the first 64 KiB).
    let event = unsafe { __hwtest_open_event(0xF200_0003, 0x0002, 0x2000, 0) };
    let event = (event != u32::MAX && event != 0).then_some(event);
    if let Some(event) = event {
        // SAFETY: as above, on the handle just opened.
        unsafe { __hwtest_enable_event(event) };
    }
    out.event = event;
    let mask = irq::mask();
    irq::set_mask(0);
    use_bios_vector();
    irq::acknowledge(1);
    start_counter();
    // Interrupts on again (ExitCriticalSection sets IEc), then VBlank only.
    // SAFETY: the BIOS vector is live, so SYSCALL 2 is answered.
    unsafe { __hwtest_exit_cs() };
    irq::set_mask(1);
    let mut seen = 0u32;
    let (mut gaps, found) = collect_gaps(400, || {
        // SAFETY: the stub, on the handle opened above.
        if event.is_some_and(|e| unsafe { __hwtest_test_event(e) } == 1) {
            seen += 1;
        }
    });
    irq::set_mask(0);
    use_runtime_vector();
    irq::set_mask(mask | 1);
    if let Some(event) = event {
        // SAFETY: closing the handle opened above.
        unsafe { __hwtest_close_event(event) };
    }
    out.events_seen = seen;
    out.bios_vblank = Stat::of(&mut gaps[..found]);
    out.bios_done = true;
}

/// The outcome as timing-block records, from `0x2C0`. Cycles throughout.
/// 0x2C0 EnterCriticalSection and 0x2C1 ExitCriticalSection per call
/// (min/median/max, harness included); 0x2C2 the empty call and its two
/// counter reads; 0x2C3 BIOS VBlank round trip; 0x2C4 BIOS gaps found, events
/// delivered, flags (bit 0 standard vector, bit 1 event opened, bit 2 BIOS
/// loop finished, bits 8-15 gaps found under the runtime handler); 0x2C5
/// runtime VBlank round trip.
pub(crate) fn records(out: &Outcome) -> [TimingRecord; KERNEL_COUNT] {
    let stat = |offset: u16, s: &Stat| record(KERNEL_RECORD + offset, s.min, s.med, s.max);
    let flags = out.standard_vector as u32
        | (out.event.is_some() as u32) << 1
        | (out.bios_done as u32) << 2
        | out.sdk_vblank.count << 8;
    [
        stat(0, &out.enter),
        stat(1, &out.exit),
        stat(2, &out.harness),
        stat(3, &out.bios_vblank),
        record(
            KERNEL_RECORD + 4,
            out.bios_vblank.count,
            out.events_seen,
            flags,
        ),
        stat(5, &out.sdk_vblank),
    ]
}

/// Set `standard_vector` from what `main` found.
pub(crate) fn start() -> Outcome {
    Outcome {
        // SAFETY: a read of a private static written once at start-up.
        standard_vector: unsafe { SNAPSHOT_STANDARD },
        ..Outcome::default()
    }
}

const LABEL: (u8, u8, u8) = (150, 170, 200);
const VALUE: (u8, u8, u8) = (236, 240, 248);
const NOTE: (u8, u8, u8) = (255, 216, 96);
const COLUMNS: [i16; 4] = [120, 168, 216, 264];

fn row(font: &FontAtlas, y: i16, label: &str, stat: &Stat, net: Option<u32>) {
    text(font, 8, y, label, LABEL);
    if stat.count == 0 {
        text(font, COLUMNS[0], y, "-", LABEL);
        return;
    }
    for (x, value) in COLUMNS.iter().zip([stat.min, stat.med, stat.max]) {
        put_number(font, *x, y, value, VALUE);
    }
    if let Some(net) = net {
        put_number(font, 276, y, net, NOTE);
    }
}

/// The result screen. `status` is the line at the foot.
#[inline(never)]
pub(crate) fn draw(font: &FontAtlas, out: &Outcome, status: &str) {
    text(font, 8, 8, "KERNEL TIMING (CYCLES)", VALUE);
    text(
        font,
        8,
        20,
        if out.standard_vector {
            "VECTOR: BIOS SNAPSHOT"
        } else {
            "VECTOR: FALLBACK WORDS"
        },
        LABEL,
    );
    text(font, COLUMNS[0], 36, "MIN", LABEL);
    text(font, COLUMNS[1], 36, "MED", LABEL);
    text(font, COLUMNS[2], 36, "MAX", LABEL);
    text(font, 276, 36, "NET", LABEL);
    let net = |s: &Stat| s.med.saturating_sub(out.harness.med);
    row(font, 50, "EMPTY CALL", &out.harness, None);
    row(font, 62, "ENTER CS", &out.enter, Some(net(&out.enter)));
    row(font, 74, "EXIT CS", &out.exit, Some(net(&out.exit)));
    if out.enter.count != 0 {
        text(font, 8, 86, "ENTER+EXIT NET", LABEL);
        put_number(font, 276, 86, net(&out.enter) + net(&out.exit), NOTE);
    }
    row(font, 106, "VBLANK SDK", &out.sdk_vblank, None);
    row(font, 118, "VBLANK BIOS", &out.bios_vblank, None);
    if out.bios_vblank.count != 0 {
        text(font, 8, 134, "BIOS GAPS", LABEL);
        put_number(font, 120, 134, out.bios_vblank.count, VALUE);
        text(font, 8, 146, "EVENT READY", LABEL);
        put_number(font, 120, 146, out.events_seen, VALUE);
        text(
            font,
            168,
            146,
            if out.event.is_some() {
                "OPENED"
            } else {
                "NOT OPENED"
            },
            LABEL,
        );
    }
    text(font, 8, 218, status, NOTE);
}

/// Frames the earlier results stay up before the BIOS VBlank part starts.
const WARNING_FRAMES: u32 = 180;

/// Show the screen for `frames` frames (or until the exit button when
/// `frames` is zero).
#[inline(never)]
fn hold(screen: &mut Screen, font: &FontAtlas, out: &Outcome, status: &str, frames: u32) {
    let mut buttons = Buttons::new();
    let mut shown = 0u32;
    while frames == 0 || shown < frames {
        if buttons.poll().exit() {
            break;
        }
        screen.clear((6, 8, 18));
        draw(font, out, status);
        screen.present();
        shown += 1;
    }
}

/// Run the whole case and leave the result on screen until CROSS.
#[inline(never)]
pub(crate) fn run(screen: &mut Screen, font: &FontAtlas) -> Outcome {
    let mut out = start();
    measure_critical_sections(&mut out);
    measure_runtime_vblank(&mut out);
    hold(
        screen,
        font,
        &out,
        "BIOS VBLANK NEXT (MAY HANG)",
        WARNING_FRAMES,
    );
    measure_bios_vblank(&mut out);
    // Interrupt handling put the runtime's clock back to zero; the caller
    // realigns the engine after the case.
    hold(screen, font, &out, "CROSS: BACK", 0);
    out
}
