// SPDX-License-Identifier: GPL-2.0-or-later
//! Console gates for three performance levers the emulator cannot vouch for.
//!
//! Each lever measured well headless and each rests on something only silicon
//! can answer. All three run near the end of the conformance battery (cases
//! `0xC8`-`0xD2`), so both RUN ALL TESTS and FULL CHARACTERISATION carry them,
//! and RESUME FROM TEST at index 200 runs these, the list-busy cases after them
//! (`list_busy_probes.rs`) and the timing scan.
//!
//! * **GTE vs IRQ** (`0xC8`-`0xCB`). psx-rt's exception handler returns to
//!   EPC. psx-spx documents that an interrupt taken on a GTE command lets the
//!   command execute and still leaves EPC pointing at it, so returning there
//!   runs it twice; its advice is to step EPC over a GTE command. The emulator
//!   never takes an interrupt in front of one (`should_take_interrupt`), so it
//!   cannot show the hazard either way. A loop of RTPS whose double execution
//!   is visible (the SXY FIFO shifts twice) runs while Timer 2 interrupts at
//!   four coprime periods, once under a handler that returns to EPC and once
//!   under the psx-spx fix. One burn says whether the hazard is real and
//!   whether the fix removes it without losing a command.
//! * **Present queue** (`0xCC`-`0xCF`). Quake's prototype (quake-psx
//!   a29ca4d) flips and kicks the next frame's DMA chain from the VBlank
//!   handler, on the first edge where GPUSTAT bit 28 and channel 2 are idle.
//!   The same handler runs here for 120 frames of uneven GPU load, and checks
//!   at every flip that the frame it exposes has executed its final GP0(1Fh):
//!   if bit 28 reports idle while the GPU is still drawing, a flip shows a
//!   partial frame, which is what a tear is.
//! * **Scratchpad stack** (`0xD0`-`0xD2`, timing `0x137`-`0x13A`). The SDK's
//!   `ScratchpadStack` (PSoXide 3e939cd54) runs a call with `$sp` in the
//!   scratchpad. The editor pins an SDK from before it, so the 12-instruction
//!   trampoline is vendored below. hello-spstack's workload runs on both
//!   stacks under Timer 2 and VBlank interrupts, GPU and SPU DMA, a CD read
//!   stream and pad polling, and the checksums must agree. The timing records
//!   price one `level2` call on each stack, idle and during a linked-list DMA.

use core::hint::black_box;
use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

use psx_io::gpu as gpu_io;
use psx_io::{cdrom, dma, irq, timers};
use psx_pack::cd::{SectorReader, SECTOR_WORDS};

use crate::{gpu_read_word_at, seed_gte_state, spin, IrqGuard, TestResult};

// ---------------------------------------------------------------------------
// Exception-vector ownership
// ---------------------------------------------------------------------------

const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
const J_OPCODE: u32 = 0x0800_0000;
const SR_IE: u32 = 1 << 0;
const SR_IM2: u32 = 1 << 10;

/// The two vector words a probe replaced, put back by [`Vector::restore`].
struct Vector {
    saved: [u32; 2],
}

impl Vector {
    fn install(handler: unsafe extern "C" fn()) -> Self {
        let guard = IrqGuard::mask();
        let saved = unsafe {
            [
                read_volatile(EXCEPTION_VECTOR),
                read_volatile(EXCEPTION_VECTOR.add(1)),
            ]
        };
        let target = handler as *const () as u32;
        unsafe {
            write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((target >> 2) & 0x03FF_FFFF));
            write_volatile(EXCEPTION_VECTOR.add(1), 0);
        }
        psx_rt::cache::flush_i_cache();
        drop(guard);
        Self { saved }
    }

    fn restore(self) {
        let guard = IrqGuard::mask();
        unsafe {
            write_volatile(EXCEPTION_VECTOR, self.saved[0]);
            write_volatile(EXCEPTION_VECTOR.add(1), self.saved[1]);
        }
        psx_rt::cache::flush_i_cache();
        drop(guard);
    }
}

fn status_register() -> u32 {
    let sr: u32;
    unsafe { core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") sr) };
    sr
}

fn set_status_register(sr: u32) {
    unsafe {
        core::arch::asm!(
            "mtc0 $8, $12",
            "nop",
            "nop",
            in("$8") sr,
            options(nostack, nomem)
        )
    };
}

/// psx-rt applies a queued display word at the next VBlank. The probes' own
/// handlers do not, so apply any word still waiting before taking the vector.
fn apply_pending_display_word() {
    let word = psx_rt::interrupts::take_pending_gp1();
    if word != 0 {
        gpu_io::write_gp1(word);
    }
}

// ---------------------------------------------------------------------------
// The handler GTE and scratchpad probes share
// ---------------------------------------------------------------------------

/// Interrupts the handler took.
#[no_mangle]
static mut HWTEST_LEVER_IRQS: u32 = 0;
/// Interrupts whose EPC held a GTE command word.
#[no_mangle]
static mut HWTEST_LEVER_GTE_HITS: u32 = 0;
/// Interrupts taken while `$sp` pointed into the scratchpad.
#[no_mangle]
static mut HWTEST_LEVER_ON_SPAD: u32 = 0;
/// Non-zero: return to EPC + 4 when EPC holds a GTE command (psx-spx's fix).
#[no_mangle]
static mut HWTEST_LEVER_SKIP_GTE: u32 = 0;

// Acknowledges every pending, enabled source, keeps psx-rt's VBlank count
// running, counts, and returns to EPC, or EPC + 4 over a GTE command when
// HWTEST_LEVER_SKIP_GTE is set. The test is psx-spx's: the word at EPC with
// its top seven bits equal to 0100101 (a COP2 command; `(op >> 24) & 0xFE ==
// 0x4A`). With Cause.BD set EPC holds the branch, which is never a GTE
// command, so a delay-slot command is not skipped; the probe keeps its GTE
// commands out of delay slots. Only $k0/$k1, never $sp. Exceptions other than
// interrupts go to psx-rt's handler, which records and survives them.
core::arch::global_asm!(
    r#"
    .set noreorder
    .section .text.hwtest_lever_handler,"ax",@progbits
    .globl __hwtest_lever_handler
__hwtest_lever_handler:
    mfc0  $26, $13
    nop
    andi  $26, $26, 0x007c
    bnez  $26, 8f
    nop
    lui   $26, 0x1f80
    lw    $27, 0x1070($26)
    lw    $26, 0x1074($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, 0x0001
    beqz  $27, 1f
    nop
    lui   $26, %hi(__psx_rt_vblank_count)
    lw    $27, %lo(__psx_rt_vblank_count)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(__psx_rt_vblank_count)($26)
1:
    lui   $26, 0x1f80
    lw    $27, 0x1070($26)
    lw    $26, 0x1074($26)
    nop
    and   $27, $27, $26
    nor   $27, $27, $zero
    lui   $26, 0x1f80
    sw    $27, 0x1070($26)
    lui   $26, %hi(HWTEST_LEVER_IRQS)
    lw    $27, %lo(HWTEST_LEVER_IRQS)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(HWTEST_LEVER_IRQS)($26)
    lui   $26, 0x1f80
    subu  $26, $sp, $26
    sltiu $26, $26, 0x0401
    beqz  $26, 2f
    nop
    lui   $26, %hi(HWTEST_LEVER_ON_SPAD)
    lw    $27, %lo(HWTEST_LEVER_ON_SPAD)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(HWTEST_LEVER_ON_SPAD)($26)
2:
    mfc0  $26, $14
    nop
    lw    $27, 0($26)
    nop
    srl   $27, $27, 25
    xori  $27, $27, 0x0025
    bnez  $27, 3f
    nop
    lui   $26, %hi(HWTEST_LEVER_GTE_HITS)
    lw    $27, %lo(HWTEST_LEVER_GTE_HITS)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(HWTEST_LEVER_GTE_HITS)($26)
    lui   $26, %hi(HWTEST_LEVER_SKIP_GTE)
    lw    $27, %lo(HWTEST_LEVER_SKIP_GTE)($26)
    nop
    beqz  $27, 3f
    nop
    mfc0  $26, $14
    nop
    addiu $26, $26, 4
    jr    $26
    .word 0x42000010
3:
    mfc0  $26, $14
    nop
    jr    $26
    .word 0x42000010
8:
    j     __psx_rt_exception_handler
    nop
    .set reorder
    "#
);

unsafe extern "C" {
    fn __hwtest_lever_handler();
    fn __hwtest_present_handler();
    fn __hwtest_call_on_stack(arg: *mut u8, entry: unsafe extern "C" fn(*mut u8), sp: u32);
}

fn lever_counter(counter: *const u32) -> u32 {
    unsafe { read_volatile(counter) }
}

fn reset_lever_counters(skip_gte: bool) {
    unsafe {
        write_volatile(addr_of_mut!(HWTEST_LEVER_IRQS), 0);
        write_volatile(addr_of_mut!(HWTEST_LEVER_GTE_HITS), 0);
        write_volatile(addr_of_mut!(HWTEST_LEVER_ON_SPAD), 0);
        write_volatile(addr_of_mut!(HWTEST_LEVER_SKIP_GTE), skip_gte as u32);
    }
}

const TIMER_RESET_AT_TARGET: u16 = 1 << 3;
const TIMER_IRQ_ON_TARGET: u16 = 1 << 4;
const TIMER_IRQ_REPEAT: u16 = 1 << 6;

/// Timer 2 interrupting every `period` system clocks, on top of VBlank, with
/// the CPU accepting interrupts. Returns what to hand [`stop_timer_irqs`].
fn start_timer_irqs(period: u16) -> (u32, u32) {
    let old_mask = irq::mask();
    let old_sr = status_register();
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_target(timers::Timer::Timer2, period);
    timers::set_counter(timers::Timer::Timer2, 0);
    timers::set_mode(
        timers::Timer::Timer2,
        TIMER_RESET_AT_TARGET | TIMER_IRQ_ON_TARGET | TIMER_IRQ_REPEAT,
    );
    irq::ack(1 << irq::source::TIMER2);
    irq::set_mask(old_mask | (1 << irq::source::TIMER2) | (1 << irq::source::VBLANK));
    set_status_register(old_sr | SR_IE | SR_IM2);
    (old_mask, old_sr)
}

fn stop_timer_irqs((old_mask, old_sr): (u32, u32)) {
    let guard = IrqGuard::mask();
    timers::set_mode(timers::Timer::Timer2, 0);
    irq::set_mask(old_mask);
    irq::ack(1 << irq::source::TIMER2);
    drop(guard);
    set_status_register(old_sr);
}

// ---------------------------------------------------------------------------
// GTE vs IRQ
// ---------------------------------------------------------------------------

/// Written to SXY0/SXY1/SXY2 before each RTPS. Their 0x5A5A high halves lie
/// outside the -0x400..0x3FF range RTPS saturates to, so no projection equals
/// one. After one RTPS, SXY0 holds B; after two it holds C; after none, A.
const SXY_A: u32 = 0x5A5A_1111;
const SXY_B: u32 = 0x5A5A_2222;
const SXY_C: u32 = 0x5A5A_3333;
/// Timer 2 periods in system clocks: primes, so the interrupt walks every
/// phase of the loop instead of locking to one instruction.
const GTE_PERIODS: [u16; 4] = [331, 457, 613, 797];
const GTE_ITERATIONS: u32 = 8192;

#[derive(Clone, Copy, Default)]
struct GteCounts {
    ok: u32,
    doubled: u32,
    missing: u32,
    other: u32,
    irqs: u32,
    hits: u32,
}

impl GteCounts {
    fn corrupted(&self) -> u32 {
        self.doubled + self.missing + self.other
    }

    /// Doubled results low, lost or unrecognisable ones high.
    fn corruption_word(&self) -> u32 {
        self.doubled.min(0xFFFF) | ((self.missing + self.other).min(0xFFFF) << 16)
    }
}

/// `iterations` x (reload SXY0-2, RTPS, wait it out, classify SXY0).
///
/// The RTPS is preceded by ordinary instructions and never sits in a delay
/// slot. Twenty nops cover its 15-cycle latency before the read, so a stale
/// read cannot pass for a missing command.
fn gte_loop(iterations: u32, counts: &mut GteCounts) {
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
            ".word 0x4A080001", // RTPS
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

fn gte_irq_run(skip_gte: bool) -> GteCounts {
    apply_pending_display_word();
    seed_gte_state();
    reset_lever_counters(skip_gte);
    let vector = Vector::install(__hwtest_lever_handler);
    let mut counts = GteCounts::default();
    for period in GTE_PERIODS {
        let saved = start_timer_irqs(period);
        gte_loop(GTE_ITERATIONS, &mut counts);
        stop_timer_irqs(saved);
    }
    vector.restore();
    counts.irqs = lever_counter(addr_of!(HWTEST_LEVER_IRQS));
    counts.hits = lever_counter(addr_of!(HWTEST_LEVER_GTE_HITS));
    seed_gte_state();
    counts
}

/// One run feeds both cases of a pair: the exposure case runs it, the verdict
/// case takes the stored result (or runs its own when reached alone).
static mut GTE_PLAIN: Option<GteCounts> = None;
static mut GTE_SKIP: Option<GteCounts> = None;

fn gte_counts(skip_gte: bool, take: bool) -> GteCounts {
    let slot = unsafe {
        if skip_gte {
            &mut *addr_of_mut!(GTE_SKIP)
        } else {
            &mut *addr_of_mut!(GTE_PLAIN)
        }
    };
    if take {
        if let Some(counts) = slot.take() {
            return counts;
        }
        return gte_irq_run(skip_gte);
    }
    let counts = gte_irq_run(skip_gte);
    *slot = Some(counts);
    counts
}

/// `0xC8`: how often an interrupt landed on a GTE command, returning to EPC.
pub(crate) fn test_gte_irq_plain_exposure() -> TestResult {
    let counts = gte_counts(false, false);
    TestResult::info(counts.irqs, counts.hits, "irqs/gte")
}

/// `0xC9`: returning to EPC must not repeat or lose an RTPS. Expected is the
/// number of interrupts that landed on one, so a failure record carries the
/// rate: observed doubled (low half) over expected.
pub(crate) fn test_gte_irq_plain_intact() -> TestResult {
    let counts = gte_counts(false, true);
    if counts.corrupted() == 0 {
        TestResult::pass(counts.hits, 0, "rtps intact")
    } else {
        TestResult::fail(counts.hits, counts.corruption_word(), "rtps twice")
    }
}

/// `0xCA`: the same loop under psx-spx's fix; observed is the number of
/// commands the handler stepped over.
pub(crate) fn test_gte_irq_skip_exposure() -> TestResult {
    let counts = gte_counts(true, false);
    TestResult::info(counts.irqs, counts.hits, "irqs/skip")
}

/// `0xCB`: with the fix, no RTPS may run twice or be lost. A skip that lands
/// on a command which had NOT run shows up as missing (high half).
pub(crate) fn test_gte_irq_skip_intact() -> TestResult {
    let counts = gte_counts(true, true);
    if counts.corrupted() == 0 {
        TestResult::pass(counts.hits, 0, "fix intact")
    } else {
        TestResult::fail(counts.hits, counts.corruption_word(), "fix broke")
    }
}

// ---------------------------------------------------------------------------
// Present queue
// ---------------------------------------------------------------------------

/// Chain head the handler kicks at the next idle edge; 0 = empty.
#[no_mangle]
static mut HWTEST_PQ_HEAD: u32 = 0;
/// GP1(05h) word applied just before the kick; 0 = no flip.
#[no_mangle]
static mut HWTEST_PQ_DISPLAY: u32 = 0;
#[no_mangle]
static mut HWTEST_PQ_KICKS: u32 = 0;
#[no_mangle]
static mut HWTEST_PQ_SKIPPED: u32 = 0;
/// VBlank edges the handler saw.
#[no_mangle]
static mut HWTEST_PQ_EDGES: u32 = 0;
/// GPUSTAT as the handler read it after deciding to flip.
#[no_mangle]
static mut HWTEST_PQ_STAT_AT_KICK: u32 = 0;
/// Timer 1 (HBlanks since VBlank) at the same moment.
#[no_mangle]
static mut HWTEST_PQ_LINE_AT_KICK: u32 = 0;

// quake-psx a29ca4d's present handler with its IRQ-EPC probe left out, plus
// three instrumentation steps marked below. Like the prototype it runs ahead
// of psx-rt's handler, which counts the VBlank, acknowledges it and returns.
core::arch::global_asm!(
    r#"
    .set noreorder
    .section .text.hwtest_present_handler,"ax",@progbits
    .globl __hwtest_present_handler
__hwtest_present_handler:
    lui   $26, 0x1f80
    lw    $27, 0x1070($26)
    lw    $26, 0x1074($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, 0x0001
    beqz  $27, 9f
    nop
    # instrumentation: count the edge
    lui   $26, %hi(HWTEST_PQ_EDGES)
    lw    $27, %lo(HWTEST_PQ_EDGES)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(HWTEST_PQ_EDGES)($26)
    lui   $26, %hi(HWTEST_PQ_HEAD)
    lw    $27, %lo(HWTEST_PQ_HEAD)($26)
    nop
    beqz  $27, 9f
    nop
    lui   $26, 0x1f80
    lw    $27, 0x1814($26)
    nop
    srl   $27, $27, 28
    andi  $27, $27, 1
    beqz  $27, 8f
    nop
    lw    $27, 0x10a8($26)
    nop
    srl   $27, $27, 24
    andi  $27, $27, 1
    bnez  $27, 8f
    nop
    # instrumentation: GPUSTAT and Timer 1 at the flip decision
    lw    $27, 0x1814($26)
    lui   $26, %hi(HWTEST_PQ_STAT_AT_KICK)
    sw    $27, %lo(HWTEST_PQ_STAT_AT_KICK)($26)
    lui   $26, 0x1f80
    lw    $27, 0x1110($26)
    lui   $26, %hi(HWTEST_PQ_LINE_AT_KICK)
    sw    $27, %lo(HWTEST_PQ_LINE_AT_KICK)($26)
    lui   $26, 0x1f80
    # instrumentation: GP1(02h), so this chain's GP0(1Fh) raises the flag anew
    lui   $27, 0x0200
    sw    $27, 0x1814($26)
    lui   $27, %hi(HWTEST_PQ_DISPLAY)
    lw    $27, %lo(HWTEST_PQ_DISPLAY)($27)
    nop
    beqz  $27, 7f
    nop
    sw    $27, 0x1814($26)
7:
    lui   $27, 0x0400
    ori   $27, $27, 0x0002
    sw    $27, 0x1814($26)
    lw    $27, 0x10f0($26)
    nop
    ori   $27, $27, 0x0800
    sw    $27, 0x10f0($26)
    lui   $27, %hi(HWTEST_PQ_HEAD)
    lw    $27, %lo(HWTEST_PQ_HEAD)($27)
    nop
    sw    $27, 0x10a0($26)
    sw    $zero, 0x10a4($26)
    lui   $27, 0x0100
    ori   $27, $27, 0x0401
    sw    $27, 0x10a8($26)
    lui   $26, %hi(HWTEST_PQ_HEAD)
    sw    $zero, %lo(HWTEST_PQ_HEAD)($26)
    lui   $26, %hi(HWTEST_PQ_KICKS)
    lw    $27, %lo(HWTEST_PQ_KICKS)($26)
    nop
    addiu $27, $27, 1
    b     9f
    sw    $27, %lo(HWTEST_PQ_KICKS)($26)
8:
    lui   $26, %hi(HWTEST_PQ_SKIPPED)
    lw    $27, %lo(HWTEST_PQ_SKIPPED)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(HWTEST_PQ_SKIPPED)($26)
9:
    j     __psx_rt_exception_handler
    nop
    .set reorder
    "#
);

const PQ_FRAMES: u32 = 120;
const PQ_ARENA_WORDS: usize = 192;
/// A frame that waits this many VBlanks for its kick counts as a timeout.
const PQ_WAIT_VBLANKS: u32 = 60;
/// Timer 1: HBlank clock, reset to zero at VBlank (sync mode 1).
const TIMER1_LINES_SINCE_VBLANK: u16 = 0x0103;
/// Marker the chain paints in a corner of its buffer, read back at the end.
const PQ_MARKER_X: u32 = 304;

static mut PQ_ARENAS: [[u32; PQ_ARENA_WORDS]; 2] = [[0; PQ_ARENA_WORDS]; 2];

const fn pq_buffer_y(frame: u32) -> u32 {
    (frame & 1) * 240
}

/// A 24-bit fill colour whose 15-bit form is exact, different per frame.
const fn pq_marker_colour(frame: u32) -> u32 {
    let r = (frame * 40) & 0xF8;
    let g = (frame * 24 + 64) & 0xF8;
    let b = 0x80;
    r | (g << 8) | (b << 16)
}

const fn rgb24_to_15(colour: u32) -> u32 {
    ((colour & 0xF8) >> 3) | ((colour >> 8 & 0xF8) << 2) | ((colour >> 16 & 0xF8) << 7)
}

/// Frame `frame`'s chain: draw environment and clear for its buffer, a
/// varying number of screen-sized Gouraud triangles so that some frames
/// outlast a VBlank, a bar that moves 16 pixels a frame (a torn flip breaks
/// it), the read-back marker, and GP0(1Fh) as the completion flag.
fn pq_build_frame(frame: u32) -> u32 {
    let arena = unsafe { addr_of_mut!(PQ_ARENAS[(frame & 1) as usize]) as *mut u32 };
    let mut at = 0usize;
    let mut emit = |words: &[u32], last: bool| {
        let next = if last {
            0x00FF_FFFF
        } else {
            unsafe { arena.add(at + 1 + words.len()) as u32 & 0x00FF_FFFF }
        };
        unsafe {
            write_volatile(arena.add(at), ((words.len() as u32) << 24) | next);
            for (offset, word) in words.iter().enumerate() {
                write_volatile(arena.add(at + 1 + offset), *word);
            }
        }
        at += 1 + words.len();
    };
    let y = pq_buffer_y(frame);
    let xy = |x: u32, y: u32| (y << 16) | x;
    emit(
        &[
            0xE300_0000 | (y << 10),
            0xE400_0000 | 319 | ((y + 239) << 10),
            0xE500_0000 | (y << 11),
        ],
        false,
    );
    emit(&[0x0200_0000 | 0x0018_0808, xy(0, y), xy(320, 240)], false);
    let triangles = (frame * 7) % 20;
    for t in 0..triangles {
        let shade = (t * 12 + frame * 3) & 0x7F;
        let (a, b, c) = if t & 1 == 0 {
            (xy(0, 0), xy(319, 0), xy(0, 239))
        } else {
            (xy(319, 239), xy(0, 239), xy(319, 0))
        };
        emit(
            &[0x3000_0000 | shade, a, shade << 8, b, shade << 16, c],
            false,
        );
    }
    emit(
        &[
            0x0200_0000 | 0x00F0_F0F0,
            xy((frame * 16) % 304, y),
            xy(16, 240),
        ],
        false,
    );
    emit(
        &[
            0x0200_0000 | pq_marker_colour(frame),
            xy(PQ_MARKER_X, y),
            xy(16, 16),
        ],
        false,
    );
    emit(&[0x1F00_0000], true);
    debug_assert!(at <= PQ_ARENA_WORDS);
    arena as u32
}

#[derive(Clone, Copy, Default)]
struct PresentCounts {
    published: u32,
    kicks: u32,
    timeouts: u32,
    marker_bad: u32,
    /// Flips whose previous chain had executed its GP0(1Fh), checked.
    checked: u32,
    /// Flips made while the previous chain had NOT reached its GP0(1Fh).
    early: u32,
    /// Timer 1 at the flips, lines since its VBlank reset.
    max_line: u32,
    min_line: u32,
    lines_per_frame: u32,
    skipped: u32,
    edges: u32,
}

fn pq_slot_full() -> bool {
    unsafe { read_volatile(addr_of!(HWTEST_PQ_HEAD)) != 0 }
}

fn present_queue_run() -> PresentCounts {
    apply_pending_display_word();
    let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
    let old_timer1 = timers::mode(timers::Timer::Timer1);
    timers::set_mode(timers::Timer::Timer1, TIMER1_LINES_SINCE_VBLANK);
    unsafe {
        for counter in [
            addr_of_mut!(HWTEST_PQ_HEAD),
            addr_of_mut!(HWTEST_PQ_DISPLAY),
            addr_of_mut!(HWTEST_PQ_KICKS),
            addr_of_mut!(HWTEST_PQ_SKIPPED),
            addr_of_mut!(HWTEST_PQ_EDGES),
            addr_of_mut!(HWTEST_PQ_STAT_AT_KICK),
            addr_of_mut!(HWTEST_PQ_LINE_AT_KICK),
        ] {
            write_volatile(counter, 0);
        }
    }
    // Nothing of ours may be walking when the handler takes over channel 2.
    if !dma::wait_done(dma::Channel::Gpu, dma::DEFAULT_DMA_SPINS) {
        dma::abort(dma::Channel::Gpu);
    }
    gpu_io::write_gp1(0x0200_0000);
    let old_mask = irq::mask();
    irq::set_mask(old_mask | (1 << irq::source::VBLANK));
    let old_sr = status_register();
    let vector = Vector::install(__hwtest_present_handler);
    set_status_register(old_sr | SR_IE | SR_IM2);

    let mut counts = PresentCounts {
        min_line: u32::MAX,
        ..PresentCounts::default()
    };
    'frames: for frame in 0..PQ_FRAMES {
        let start = psx_rt::interrupts::vblank_count();
        let mut spins = 0u32;
        while pq_slot_full() {
            let line = timers::counter(timers::Timer::Timer1) as u32;
            counts.lines_per_frame = counts.lines_per_frame.max(line);
            spins += 1;
            if psx_rt::interrupts::vblank_count().wrapping_sub(start) > PQ_WAIT_VBLANKS
                || spins > 20_000_000
            {
                counts.timeouts += 1;
                break 'frames;
            }
        }
        // The handler has just kicked frame - 1. From frame 1 on that kick
        // flipped to frame - 2's buffer, which is only safe if frame - 2's
        // chain had reached its GP0(1Fh).
        if frame >= 2 {
            let stat = unsafe { read_volatile(addr_of!(HWTEST_PQ_STAT_AT_KICK)) };
            let line = unsafe { read_volatile(addr_of!(HWTEST_PQ_LINE_AT_KICK)) } & 0xFFFF;
            counts.checked += 1;
            if stat & (1 << 24) == 0 {
                counts.early += 1;
            }
            counts.max_line = counts.max_line.max(line);
            counts.min_line = counts.min_line.min(line);
        }
        let head = pq_build_frame(frame);
        let display = if frame == 0 {
            0
        } else {
            0x0500_0000 | (pq_buffer_y(frame - 1) << 10)
        };
        unsafe {
            core::arch::asm!("", options(nostack, preserves_flags));
            write_volatile(addr_of_mut!(HWTEST_PQ_DISPLAY), display);
            write_volatile(addr_of_mut!(HWTEST_PQ_HEAD), head);
        }
        counts.published += 1;
    }

    // Let the last published frame go out and finish.
    let start = psx_rt::interrupts::vblank_count();
    let mut spins = 0u32;
    while pq_slot_full()
        && psx_rt::interrupts::vblank_count().wrapping_sub(start) <= PQ_WAIT_VBLANKS
        && spins < 20_000_000
    {
        spins += 1;
    }
    let guard = IrqGuard::mask();
    if pq_slot_full() {
        counts.timeouts += 1;
        unsafe { write_volatile(addr_of_mut!(HWTEST_PQ_HEAD), 0) };
    } else if counts.published == PQ_FRAMES {
        // The last kick flipped to the second-to-last frame's buffer.
        let stat = unsafe { read_volatile(addr_of!(HWTEST_PQ_STAT_AT_KICK)) };
        let line = unsafe { read_volatile(addr_of!(HWTEST_PQ_LINE_AT_KICK)) } & 0xFFFF;
        counts.checked += 1;
        if stat & (1 << 24) == 0 {
            counts.early += 1;
        }
        counts.max_line = counts.max_line.max(line);
        counts.min_line = counts.min_line.min(line);
    }
    vector.restore();
    drop(guard);
    set_status_register(old_sr);
    let last_done = dma::wait_done(dma::Channel::Gpu, dma::DEFAULT_DMA_SPINS);
    if !last_done {
        dma::abort(dma::Channel::Gpu);
        gpu_io::write_gp1(0x0100_0000);
        counts.timeouts += 1;
    }
    let mut polls = 0u32;
    while gpu_io::gpustat().bits() & (1 << 24) == 0 && polls < 1_000_000 {
        polls += 1;
    }
    if polls == 1_000_000 {
        counts.timeouts += 1;
    }
    counts.kicks = unsafe { read_volatile(addr_of!(HWTEST_PQ_KICKS)) };
    counts.skipped = unsafe { read_volatile(addr_of!(HWTEST_PQ_SKIPPED)) };
    counts.edges = unsafe { read_volatile(addr_of!(HWTEST_PQ_EDGES)) };

    gpu_io::write_gp1(0x0200_0000);
    irq::ack(1 << irq::source::GPU);
    gpu_io::write_gp1(0x0400_0000 | old_direction);
    irq::set_mask(old_mask);
    timers::set_mode(timers::Timer::Timer1, old_timer1 & 0x03FF);

    // Both buffers must hold the markers of the last two frames drawn.
    if counts.published == PQ_FRAMES {
        for frame in [PQ_FRAMES - 2, PQ_FRAMES - 1] {
            let pixel = gpu_read_word_at(PQ_MARKER_X as u16, pq_buffer_y(frame) as u16) & 0x7FFF;
            if pixel != rgb24_to_15(pq_marker_colour(frame)) {
                counts.marker_bad += 1;
            }
        }
    } else {
        counts.marker_bad = 2;
    }
    psx_gpu::set_draw_area(0, 0, 1023, 511);
    psx_gpu::set_draw_offset(0, 0);
    counts
}

static mut PQ_RESULT: Option<PresentCounts> = None;

/// The first present case runs the queue; the other three read its result.
fn present_counts(first: bool) -> PresentCounts {
    let slot = unsafe { &mut *addr_of_mut!(PQ_RESULT) };
    if first || slot.is_none() {
        *slot = Some(present_queue_run());
    }
    slot.unwrap_or_default()
}

/// `0xCC`: every published frame was kicked by the handler and drawn.
pub(crate) fn test_present_queue_frames() -> TestResult {
    let c = present_counts(true);
    let observed =
        c.kicks.min(0xFFFF) | (c.timeouts.min(0xFF) << 16) | (c.marker_bad.min(0xFF) << 24);
    if c.kicks == PQ_FRAMES && c.timeouts == 0 && c.marker_bad == 0 {
        TestResult::pass(PQ_FRAMES, observed, "all drawn")
    } else {
        TestResult::fail(PQ_FRAMES, observed, "lost frame")
    }
}

/// `0xCD`: bit 28 plus an idle channel must mean the last chain finished, or
/// a flip exposes a partial frame.
pub(crate) fn test_present_queue_bit28() -> TestResult {
    let c = present_counts(false);
    if c.early == 0 {
        TestResult::pass(c.checked, 0, "bit28 idle")
    } else {
        TestResult::fail(c.checked, c.early, "bit28 early")
    }
}

/// `0xCE`: where the beam was at the flips. Timer 1 counts HBlanks and resets
/// at VBlank (sync mode 1); observed is the latest flip line (bits 0-15) and
/// the earliest (16-31), expected the largest value the main loop saw (lines
/// per frame). A measurement, not a verdict: where the reset sits relative to
/// the VBlank IRQ differs between console models (the emulator's SCPH-9902
/// profile puts it 29 lines away), and the emulator reads Timer 1 without catching it
/// up (emulator-accuracy-from-silicon.md, "Timers"), so its value here is not
/// a beam position. On silicon a spread of a few lines means every flip hit
/// the same beam position, the VBlank edge; a torn flip is also visible on the
/// CRT as a break in the moving bar.
pub(crate) fn test_present_queue_flip_line() -> TestResult {
    let c = present_counts(false);
    let earliest = if c.checked == 0 { 0 } else { c.min_line };
    TestResult::info(
        c.lines_per_frame,
        c.max_line.min(0xFFFF) | (earliest.min(0xFFFF) << 16),
        "flip lines",
    )
}

/// `0xCF`: VBlank edges that found the GPU or channel 2 busy and waited.
pub(crate) fn test_present_queue_skipped() -> TestResult {
    let c = present_counts(false);
    TestResult::info(c.edges, c.skipped, "busy edges")
}

// ---------------------------------------------------------------------------
// Scratchpad stack
// ---------------------------------------------------------------------------

// void __hwtest_call_on_stack(void *arg, void (*entry)(void *), void *sp)
//
// psx-rt's __psx_rt_call_on_stack (PSoXide 3e939cd54) without the panic
// bookkeeping: $sp is saved in $s0, which the entry preserves, and set in the
// call's delay slot. An interrupt in that delay slot re-executes the jalr and
// the move, which are idempotent.
core::arch::global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.hwtest_spstack,"ax",@progbits
    .globl __hwtest_call_on_stack
__hwtest_call_on_stack:
    addiu $sp, $sp, -24
    sw    $ra, 20($sp)
    sw    $s0, 16($sp)
    move  $s0, $sp
    move  $t9, $a1
    jalr  $t9
    move  $sp, $a2
    move  $sp, $s0
    lw    $ra, 20($sp)
    lw    $s0, 16($sp)
    jr    $ra
    addiu $sp, $sp, 24
    .set macro
    .set reorder
    "#
);

const SCRATCHPAD: u32 = 0x1F80_0000;
/// hello-spstack's layout: a 256-byte table the workload reads, then the
/// stack in bytes 256..1024.
const TABLE_WORDS: usize = 64;
const STACK_START: u32 = 256;
const STACK_END: u32 = 1024;
const STACK_TOP: u32 = SCRATCHPAD + STACK_END - 16;
const STACK_PAINT: u32 = 0x5C4A_7C4D;
const SP_ROUNDS: u32 = 32;
/// Timer 2 period while the workload runs: an interrupt every ~45 us.
const SP_IRQ_PERIOD: u16 = 1531;
/// The scratchpad run must take at least this many interrupts from inside.
const SP_MIN_IRQS: u32 = 64;
const CDTEST_LBA: u32 = 524;

static mut SCRATCHPAD_SAVE: [u32; 256] = [0; 256];

fn save_scratchpad() {
    let save = addr_of_mut!(SCRATCHPAD_SAVE) as *mut u32;
    for i in 0..256 {
        unsafe {
            write_volatile(
                save.add(i),
                read_volatile((SCRATCHPAD as *const u32).add(i)),
            )
        };
    }
}

fn restore_scratchpad() {
    let save = addr_of!(SCRATCHPAD_SAVE) as *const u32;
    for i in 0..256 {
        unsafe { write_volatile((SCRATCHPAD as *mut u32).add(i), read_volatile(save.add(i))) };
    }
}

fn table() -> *mut u32 {
    SCRATCHPAD as *mut u32
}

fn table_word(i: usize) -> u32 {
    (i as u32).wrapping_mul(0x9E37_79B9) ^ 0x0BAD_F00D
}

fn fill_table() {
    for i in 0..TABLE_WORDS {
        unsafe { write_volatile(table().add(i), table_word(i)) };
    }
}

fn table_intact() -> bool {
    (0..TABLE_WORDS).all(|i| unsafe { read_volatile(table().add(i)) } == table_word(i))
}

// hello-spstack's workload, unchanged: three levels of calls, each holding an
// array in its frame, reading the table kept in the scratchpad.
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

#[repr(C)]
struct Call {
    seed: u32,
    result: u32,
    elapsed: u32,
}

unsafe extern "C" fn level1_entry(arg: *mut u8) {
    let call = unsafe { &mut *(arg as *mut Call) };
    call.result = level1(call.seed);
}

/// The timed body both stacks share: Timer 2 around one `level2` call.
unsafe extern "C" fn timed_level2_entry(arg: *mut u8) {
    let call = unsafe { &mut *(arg as *mut Call) };
    let seed = call.seed;
    timers::set_mode(timers::Timer::Timer2, 0);
    timers::set_counter(timers::Timer::Timer2, 0);
    let result = level2(seed);
    let elapsed = timers::counter(timers::Timer::Timer2);
    call.result = result;
    call.elapsed = elapsed as u32;
}

fn run_entry(entry: unsafe extern "C" fn(*mut u8), call: &mut Call, on_scratchpad: bool) {
    let arg = (call as *mut Call).cast::<u8>();
    if on_scratchpad {
        unsafe { __hwtest_call_on_stack(arg, entry, STACK_TOP) };
    } else {
        unsafe { entry(arg) };
    }
}

fn stack_pointer() -> u32 {
    let sp: u32;
    unsafe { core::arch::asm!("move $8, $sp", lateout("$8") sp, options(nomem, nostack)) };
    sp
}

fn round_seed(round: u32) -> u32 {
    round.wrapping_mul(0x9E37_79B9) ^ 0x1234_5678
}

/// Words the background GPU list walks: empty packets, 10 clocks a node.
const LIST_NODES: usize = 2048;
static mut LEVER_LIST: [u32; LIST_NODES] = [0; LIST_NODES];
const LIST_KICK: u32 = dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_LINKED | dma::CHCR_START;

fn build_list() -> u32 {
    let list = addr_of_mut!(LEVER_LIST) as *mut u32;
    for index in 0..LIST_NODES {
        let next = if index + 1 == LIST_NODES {
            0x00FF_FFFF
        } else {
            unsafe { list.add(index + 1) as u32 & 0x00FF_FFFF }
        };
        unsafe { write_volatile(list.add(index), next) };
    }
    list as u32
}

fn kick_list(head: u32) {
    dma::set_madr(dma::Channel::Gpu, head);
    dma::set_bcr_manual(dma::Channel::Gpu, 0);
    dma::set_chcr(dma::Channel::Gpu, LIST_KICK);
}

const SPUCNT: u32 = 0x1F80_1DAA;
const SPUSTAT: u32 = 0x1F80_1DAE;
const SPU_TRANSFER_ADDR: u32 = 0x1F80_1DA6;
const SPU_TRANSFER_CTRL: u32 = 0x1F80_1DAC;
/// Sound RAM byte offset the background upload rewrites (untouched by the
/// suite's own samples, which sit low).
const SPU_SCRATCH_ADDR: u32 = 0x6_0000;
/// 64 blocks of 16 words: 4 KiB a kick, taken from the GPU list's words.
const SPU_BLOCKS: u16 = 64;

fn spu_mode(mode: u16) -> bool {
    unsafe { psx_io::write16(SPUCNT, mode) };
    let mut polls = 0;
    while unsafe { psx_io::read16(SPUSTAT) } & 0x3F != mode & 0x3F {
        polls += 1;
        if polls > 100_000 {
            return false;
        }
    }
    true
}

/// Everything else a game has going while the workload runs.
struct Activity {
    head: u32,
    old_direction: u32,
    spu_enabled: bool,
    spucnt: u16,
    reader: SectorReader,
    cd_streaming: bool,
    gpu_kicks: u32,
    spu_kicks: u32,
    cd_sectors: u32,
    pad_polls: u32,
}

static mut CD_SINK: [u32; SECTOR_WORDS] = [0; SECTOR_WORDS];

impl Activity {
    fn start() -> Self {
        let head = build_list();
        let old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
        if !dma::wait_done(dma::Channel::Gpu, dma::DEFAULT_DMA_SPINS) {
            dma::abort(dma::Channel::Gpu);
        }
        gpu_io::write_gp1(0x0400_0002);
        dma::enable_channel(dma::Channel::Gpu);
        dma::enable_channel(dma::Channel::Spu);
        let spucnt = unsafe { psx_io::read16(SPUCNT) } & !0x0030;
        let spu_enabled = spucnt & 0x8000 != 0;
        // The SDK's reader, as games use it: PIO pops of a ReadN stream.
        // Channel 3 is deliberately not used: on the project console chopping
        // CD DMA can latch busy for good, which would take the timing scan's
        // CD records with it, and no DMA channel can address the scratchpad.
        let mut reader = SectorReader::new();
        let cd_streaming = unsafe { reader.prepare() && reader.start_read(CDTEST_LBA) };
        Self {
            head,
            old_direction,
            spu_enabled,
            spucnt,
            reader,
            cd_streaming,
            gpu_kicks: 0,
            spu_kicks: 0,
            cd_sectors: 0,
            pad_polls: 0,
        }
    }

    /// Between rounds, on the RAM stack: re-arm whatever has finished.
    fn service(&mut self) {
        if !dma::is_busy(dma::Channel::Gpu) {
            kick_list(self.head);
            self.gpu_kicks += 1;
        }
        if self.spu_enabled && !dma::is_busy(dma::Channel::Spu) {
            let ready = spu_mode(self.spucnt) && {
                unsafe {
                    psx_io::write16(SPU_TRANSFER_CTRL, 0x0004);
                    psx_io::write16(SPU_TRANSFER_ADDR, (SPU_SCRATCH_ADDR / 8) as u16);
                }
                spu_mode(self.spucnt | 0x0020)
            };
            if ready {
                dma::set_madr(dma::Channel::Spu, self.head);
                dma::set_bcr_block(dma::Channel::Spu, 16, SPU_BLOCKS);
                dma::set_chcr(
                    dma::Channel::Spu,
                    dma::CHCR_TO_DEVICE | dma::CHCR_SYNC_BLOCK | dma::CHCR_START,
                );
                self.spu_kicks += 1;
            } else {
                self.spu_enabled = false;
            }
        }
        if self.cd_streaming && matches!(cdrom::poll_data_sector(), Ok(true)) {
            let sink = unsafe { &mut *addr_of_mut!(CD_SINK) };
            if unsafe { self.reader.read_sector(sink) } {
                self.cd_sectors += 1;
            } else {
                self.cd_streaming = false;
            }
        }
        let _ = psx_pad::poll_port1();
        self.pad_polls += 1;
    }

    fn stop(mut self) -> Self {
        if !dma::wait_done(dma::Channel::Gpu, dma::DEFAULT_DMA_SPINS) {
            dma::abort(dma::Channel::Gpu);
        }
        if !dma::wait_done(dma::Channel::Spu, dma::DEFAULT_DMA_SPINS) {
            dma::abort(dma::Channel::Spu);
        }
        if self.spu_kicks != 0 {
            let _ = spu_mode(self.spucnt);
        }
        gpu_io::write_gp1(0x0400_0000 | self.old_direction);
        unsafe { self.reader.stop() };
        self.cd_streaming = false;
        self
    }
}

#[derive(Clone, Copy, Default)]
struct SpOutcome {
    ram_sum: u32,
    spad_sum: u32,
    mismatched_rounds: u32,
    irqs_on_spad: u32,
    stack_bytes: u32,
    /// bit 0 table changed, bit 1 region bottom word changed, bit 2 `$sp`
    /// not restored, bit 3 the caller's RAM frame changed, bit 4 an
    /// interrupt saw the scratchpad during the RAM run.
    flags: u32,
    rounds: u32,
    gpu_kicks: u32,
    spu_kicks: u32,
    cd_sectors: u32,
    pad_polls: u32,
}

/// `ROUNDS` x (service the background, a phase delay, one `level1`).
fn sp_rounds(
    activity: &mut Activity,
    on_scratchpad: bool,
    results: &mut [u32; SP_ROUNDS as usize],
) {
    for round in 0..SP_ROUNDS {
        activity.service();
        // Shift where in the workload the VBlank lands, round to round.
        spin((round * 1543) % 8192);
        let mut call = Call {
            seed: round_seed(round),
            result: 0,
            elapsed: 0,
        };
        run_entry(level1_entry, &mut call, on_scratchpad);
        results[round as usize] = call.result;
    }
}

fn spstack_run() -> SpOutcome {
    apply_pending_display_word();
    save_scratchpad();
    fill_table();
    let ram_frame = black_box([0xA5A5_1111u32, 0x2222, 0x3333, 0x4444]);
    let sp_before = stack_pointer();

    let mut activity = Activity::start();
    // After the reader's prepare(), which rewrites I_MASK to VBlank only.
    reset_lever_counters(true);
    let vector = Vector::install(__hwtest_lever_handler);
    let saved = start_timer_irqs(SP_IRQ_PERIOD);

    let mut ram = [0u32; SP_ROUNDS as usize];
    sp_rounds(&mut activity, false, &mut ram);
    let ram_run_on_spad = lever_counter(addr_of!(HWTEST_LEVER_ON_SPAD));

    for offset in (STACK_START..STACK_END).step_by(4) {
        unsafe { write_volatile((SCRATCHPAD + offset) as *mut u32, STACK_PAINT) };
    }
    let mut spad = [0u32; SP_ROUNDS as usize];
    sp_rounds(&mut activity, true, &mut spad);
    let irqs_on_spad = lever_counter(addr_of!(HWTEST_LEVER_ON_SPAD)).wrapping_sub(ram_run_on_spad);

    stop_timer_irqs(saved);
    vector.restore();
    let activity = activity.stop();

    let mut lowest = STACK_END;
    for offset in (STACK_START..STACK_END).step_by(4) {
        if unsafe { read_volatile((SCRATCHPAD + offset) as *const u32) } != STACK_PAINT {
            lowest = offset;
            break;
        }
    }
    let mut outcome = SpOutcome {
        rounds: SP_ROUNDS,
        irqs_on_spad,
        stack_bytes: STACK_END - lowest,
        gpu_kicks: activity.gpu_kicks,
        spu_kicks: activity.spu_kicks,
        cd_sectors: activity.cd_sectors,
        pad_polls: activity.pad_polls,
        ..SpOutcome::default()
    };
    for round in 0..SP_ROUNDS as usize {
        outcome.ram_sum = outcome.ram_sum.rotate_left(1) ^ ram[round];
        outcome.spad_sum = outcome.spad_sum.rotate_left(1) ^ spad[round];
        if ram[round] != spad[round] {
            outcome.mismatched_rounds += 1;
        }
    }
    if !table_intact() {
        outcome.flags |= 1;
    }
    if lowest == STACK_START {
        outcome.flags |= 2;
    }
    if stack_pointer() != sp_before {
        outcome.flags |= 4;
    }
    if black_box(ram_frame) != [0xA5A5_1111, 0x2222, 0x3333, 0x4444] {
        outcome.flags |= 8;
    }
    if ram_run_on_spad != 0 {
        outcome.flags |= 16;
    }
    restore_scratchpad();
    outcome
}

static mut SP_RESULT: Option<SpOutcome> = None;

fn sp_outcome(first: bool) -> SpOutcome {
    let slot = unsafe { &mut *addr_of_mut!(SP_RESULT) };
    if first || slot.is_none() {
        *slot = Some(spstack_run());
    }
    slot.unwrap_or_default()
}

/// `0xD0`: the same rounds on the scratchpad stack give the same checksum as
/// on the RAM stack, with interrupts and DMA going on.
pub(crate) fn test_spstack_checksum() -> TestResult {
    let o = sp_outcome(true);
    if o.ram_sum == o.spad_sum && o.mismatched_rounds == 0 {
        TestResult::pass(o.ram_sum, o.spad_sum, "same sums")
    } else {
        TestResult::fail(o.ram_sum, o.spad_sum, "sums differ")
    }
}

/// `0xD1`: interrupts really landed on the scratchpad stack, and nothing
/// around it moved. Observed: interrupts on the stack (bits 0-14), deepest
/// stack use in bytes (15-26), failure flags (27-31, see [`SpOutcome`]).
pub(crate) fn test_spstack_integrity() -> TestResult {
    let o = sp_outcome(false);
    let observed =
        o.irqs_on_spad.min(0x7FFF) | (o.stack_bytes.min(0xFFF) << 15) | ((o.flags & 0x1F) << 27);
    if o.flags == 0 && o.irqs_on_spad >= SP_MIN_IRQS {
        TestResult::pass(SP_MIN_IRQS, observed, "spad intact")
    } else {
        TestResult::fail(SP_MIN_IRQS, observed, "spad broken")
    }
}

/// `0xD2`: what ran alongside: GPU list kicks (bits 0-7), SPU upload kicks
/// (8-15), CD sectors read (16-23), pad polls (24-31), each saturating.
pub(crate) fn test_spstack_activity() -> TestResult {
    let o = sp_outcome(false);
    let observed = o.gpu_kicks.min(0xFF)
        | (o.spu_kicks.min(0xFF) << 8)
        | (o.cd_sectors.min(0xFF) << 16)
        | (o.pad_polls.min(0xFF) << 24);
    TestResult::info(o.rounds * 2, observed, "gpu/spu/cd/pad")
}

// Timing records 0x137-0x13A, run from perf_probes' LEVERS table with
// interrupts masked. One `level2` call (16 `level3` calls) per sample.

fn timed_level2(on_scratchpad: bool, during_dma: bool) -> u16 {
    save_scratchpad();
    fill_table();
    let mut old_direction = 0;
    if during_dma {
        let head = build_list();
        old_direction = (gpu_io::gpustat().bits() >> 29) & 3;
        gpu_io::write_gp1(0x0400_0002);
        dma::enable_channel(dma::Channel::Gpu);
        kick_list(head);
    }
    let mut call = Call {
        seed: 0x1234_5678,
        result: 0,
        elapsed: 0,
    };
    run_entry(timed_level2_entry, &mut call, on_scratchpad);
    if during_dma {
        if !dma::wait_done(dma::Channel::Gpu, dma::DEFAULT_DMA_SPINS) {
            dma::abort(dma::Channel::Gpu);
        }
        gpu_io::write_gp1(0x0400_0000 | old_direction);
    }
    restore_scratchpad();
    call.elapsed as u16
}

pub(crate) fn level2_ram_stack(_: u32, _: u32) -> u16 {
    timed_level2(false, false)
}

pub(crate) fn level2_scratchpad_stack(_: u32, _: u32) -> u16 {
    timed_level2(true, false)
}

pub(crate) fn level2_ram_stack_during_dma(_: u32, _: u32) -> u16 {
    timed_level2(false, true)
}

pub(crate) fn level2_scratchpad_stack_during_dma(_: u32, _: u32) -> u16 {
    timed_level2(true, true)
}
