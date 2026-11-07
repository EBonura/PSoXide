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
//! # Whose kernel it is
//!
//! Nothing here assumes a BIOS. Sony versions put a stub at the vector that
//! jumps to their own handler (0x0C80 on the versions measured), OpenBIOS puts
//! one that jumps to its handler at 0x27D0, and a program started by another
//! program may find something else altogether (a `j` into that program's own
//! code, which has no SYSCALL handler). The case therefore records the four
//! words it found and decodes them: only a `lui`/`addiu` (or `ori`)/`jr`/`nop`
//! stub whose target lies in the kernel's first 64 KiB of RAM counts as a
//! kernel vector. That stub is what is put back for the measurements, whatever
//! its target, so the first SYSCALL reaches the kernel that booted the program.
//! Any other vector means there is no kernel to measure, and the BIOS parts are
//! skipped (records zero, flag bit 3 clear), never attempted. The result flags
//! say which happened, and carry the stub's target.
//!
//! The emulator's kernel is its own HLE one, so the same case run headless
//! measures that.

use crate::console_tests::{bit, record, spread, KERNEL_COUNT, KERNEL_RECORD};
use crate::TimingRecord;
use psx_font::FontAtlas;
use psx_io::{irq, timers};
use psx_rt::interrupts;

/// The general exception vector: four words.
const VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
/// The stub Sony versions keep there, for the "standard" flag only. It decides nothing.
const SONY_VECTOR: [u32; 4] = [0x3C1A_0000, 0x275A_0C80, 0x0340_0008, 0];

/// What `main` found at the vector before the runtime replaced it, whatever it was.
static mut SNAPSHOT: [u32; 4] = [0; 4];
/// The stub's target when the snapshot decodes as a kernel vector.
static mut SNAPSHOT_TARGET: Option<u32> = None;

/// Where a kernel vector stub jumps, or `None` when the words are not a stub into the
/// kernel's own RAM.
///
/// The stub is `lui $k0, hi; addiu $k0, $k0, lo` (or `ori`), `jr $k0`, `nop`, with the target in
/// the first 64 KiB of RAM through any of its mirrors. Anything else (a `j` into a program, zeros,
/// a ROM address) is not something a SYSCALL may be sent through.
pub(crate) const fn kernel_stub_target(words: [u32; 4]) -> Option<u32> {
    const LUI_K0: u32 = 0x3C1A_0000;
    const ADDIU_K0: u32 = 0x275A_0000;
    const ORI_K0: u32 = 0x375A_0000;
    const JR_K0: u32 = 0x0340_0008;
    let high = (words[0] & 0xFFFF) << 16;
    let low = words[1] & 0xFFFF;
    let target = if words[0] & 0xFFFF_0000 != LUI_K0 {
        return None;
    } else if words[1] & 0xFFFF_0000 == ADDIU_K0 {
        // The immediate is sign-extended.
        high.wrapping_add(low as u16 as i16 as i32 as u32)
    } else if words[1] & 0xFFFF_0000 == ORI_K0 {
        high | low
    } else {
        return None;
    };
    if words[2] != JR_K0 || words[3] != 0 {
        return None;
    }
    if target & 0x1FFF_FFFF >= 0x1_0000 || target & 0x1FFF_FFFF < 0x80 {
        return None;
    }
    Some(target)
}

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

/// Record the vector the BIOS left, whatever it is. Call from `main`, before the runtime or
/// the engine can install a handler.
pub(crate) fn snapshot_vector() {
    // SAFETY: plain reads of kernel RAM and writes of two private statics, on
    // the one thread, before any interrupt handler of ours exists.
    unsafe {
        let mut words = [0u32; 4];
        for (i, word) in words.iter_mut().enumerate() {
            *word = core::ptr::read_volatile(VECTOR.add(i));
        }
        SNAPSHOT = words;
        SNAPSHOT_TARGET = kernel_stub_target(words);
    }
}

/// The kernel vector's target, when the snapshot is one.
fn kernel_target() -> Option<u32> {
    // SAFETY: a read of a private static written once at start-up.
    unsafe { SNAPSHOT_TARGET }
}

/// Make the BIOS's own vector the live one. Only called when [`kernel_target`] is `Some`.
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
    /// The vector `main` found was Sony's stub (jump to 0x0C80).
    pub(crate) standard_vector: bool,
    /// The vector `main` found was a kernel stub of any BIOS (see the module docs), with its
    /// target; `None` means no SYSCALL was sent through it.
    pub(crate) kernel_target: Option<u32>,
    /// The SYSCALL part ran.
    pub(crate) critical_done: bool,
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
    if out.kernel_target.is_none() {
        return;
    }
    out.critical_done = true;
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
    if out.kernel_target.is_none() {
        return;
    }
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
/// delivered, flags (bit 0 Sony's standard vector, bit 1 event opened, bit 2 BIOS
/// loop finished, bit 3 SYSCALL part ran, bit 4 the vector was a kernel stub,
/// bits 8-15 gaps found under the runtime handler, bits 16-31 the low half of the
/// stub's target, 0x0C80 for Sony and 0x27D0 for OpenBIOS); 0x2C5
/// runtime VBlank round trip. A BIOS part that did not run has all-zero timings.
pub(crate) fn records(out: &Outcome) -> [TimingRecord; KERNEL_COUNT] {
    let stat = |offset: u16, s: &Stat| record(KERNEL_RECORD + offset, s.min, s.med, s.max);
    let flags = bit(out.standard_vector, 0)
        | bit(out.event.is_some(), 1)
        | bit(out.bios_done, 2)
        | bit(out.critical_done, 3)
        | bit(out.kernel_target.is_some(), 4)
        | out.sdk_vblank.count << 8
        | (out.kernel_target.unwrap_or(0) & 0xFFFF) << 16;
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

/// Set the vector fields from what `main` found.
pub(crate) fn start() -> Outcome {
    Outcome {
        // SAFETY: a read of a private static written once at start-up.
        standard_vector: unsafe { *core::ptr::addr_of!(SNAPSHOT) } == SONY_VECTOR,
        kernel_target: kernel_target(),
        ..Outcome::default()
    }
}

/// Run the whole case. Both BIOS parts are skipped when the vector found at start-up is not a
/// kernel stub, and when `skip_bios` (the run was started to avoid risky steps): a SYSCALL goes
/// to a kernel handler, and the VBlank round trip can hang a kernel which does not acknowledge
/// a VBlank nobody handles.
/// There is no picture of its own: the run's progress screen names the step.
#[inline(never)]
pub(crate) fn run_headless(
    _gpu: &mut psx_gpu::Gpu,
    _font: &FontAtlas,
    skip_bios: bool,
) -> [TimingRecord; KERNEL_COUNT] {
    let mut out = start();
    if skip_bios {
        out.kernel_target = None;
    }
    measure_critical_sections(&mut out);
    measure_runtime_vblank(&mut out);
    measure_bios_vblank(&mut out);
    records(&out)
}
