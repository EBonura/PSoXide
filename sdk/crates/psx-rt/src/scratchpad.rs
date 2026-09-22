//! The CPU scratchpad, and running code with its stack there.
//!
//! The R3000A has no data cache. Its 1 KiB scratchpad at `0x1F80_0000` is
//! the only data memory that loads from in one cycle; a main-RAM load stalls
//! for about six. Code whose frames spill a lot (long loops with more live
//! values than callee-saved registers, explicit work stacks in a frame) pays
//! that stall on every stack reload. [`ScratchpadStack::run`] moves `$sp`
//! into a scratchpad region for one call, so the same instructions run with
//! their frames in fast memory. Nothing about the code changes, only where
//! its frames live, so results are identical by construction. On Cortex,
//! running face selection, collision traces and the model face walker this
//! way cut gameplay work 3.9% in the emulator (2026-09-22). No console has
//! run it yet.
//!
//! # Sharing the scratchpad
//!
//! Games and the engine already keep working sets in the scratchpad (clip
//! planes, batch workspaces, bucket heads), usually in phases that reuse the
//! same bytes. There is deliberately no allocator. Instead each user names
//! its bytes as a const [`Region`], and every set of regions that is live at
//! the same time is checked once at compile time with [`assert_disjoint`]:
//!
//! ```
//! use psx_rt::scratchpad::{assert_disjoint, Region, ScratchpadStack};
//!
//! // Persistent across the whole frame.
//! const BUCKET_HEADS: Region = Region::new(0, 128);
//! // Face pass only: clip planes, then the batch workspace.
//! const CLIP_PLANES: Region = Region::new(128, 224);
//! const BATCH: Region = Region::new(224, 1024);
//! // Selection runs before the face pass claims its bytes.
//! type SelectionStack = ScratchpadStack<128, 1024>;
//!
//! const _: () = assert_disjoint(&[BUCKET_HEADS, CLIP_PLANES, BATCH]);
//! const _: () = assert_disjoint(&[BUCKET_HEADS, SelectionStack::REGION]);
//! ```
//!
//! A stack region may overlap a phase's region only when that phase is not
//! live around the call, and the second `assert_disjoint` is the written
//! record of which regions are.
//!
//! # What makes it safe
//!
//! * Interrupts. psx-rt's exception handler (installed by
//!   `interrupts::install_vblank_counter`) uses only `$k0`/`$k1` and
//!   never touches `$sp`, so an IRQ taken on a scratchpad stack writes
//!   nothing there and returns to the same `$sp`. A game that runs with a
//!   different handler installed must check that it does not push onto the
//!   interrupted stack. The `scratchpad-stack-check` feature traps a call
//!   made with interrupts enabled and another handler in the vector.
//! * Stack depth. The whole call tree of `f` must fit in the region minus
//!   [`STACK_OVERHEAD`] ([`ScratchpadStack::BUDGET`]). `tools/stack_guard.py`
//!   proves it from the linked image after every build: it finds each
//!   monomorphised [`ScratchpadStack`] entry, sums frame sizes down the static
//!   call graph, and fails on overflow, recursion, calls through a register
//!   and BIOS calls, none of which it can bound. The `scratchpad-stack-check`
//!   feature adds a canary word at the bottom of the region, checked after
//!   every call.
//! * Panics. SDK guests build with `panic = "abort"`, so nothing unwinds
//!   through the switch. With `panic_immediate_abort` a panic is a BREAK the
//!   exception handler halts on. Otherwise psx-rt's panic handler moves `$sp`
//!   back to the RAM stack before it prints, so reporting never runs on (or
//!   overflows) the scratchpad.
//! * Nesting. A call made while already on a scratchpad stack runs `f` in
//!   place, and the guard counts it as part of the outer call tree.

use core::mem::{ManuallyDrop, MaybeUninit};

/// Address of the first scratchpad byte (KUSEG/KSEG0; there is no KSEG1
/// alias).
pub const BASE: usize = 0x1F80_0000;

/// Scratchpad capacity in bytes.
pub const SIZE: usize = 1024;

/// Bytes of a stack region a call tree cannot use: the 16-byte o32
/// argument home area above the first frame, and the canary word at the
/// bottom of the region (reserved whether or not the check is enabled, so
/// the budget does not depend on a feature).
pub const STACK_OVERHEAD: usize = 20;

/// The value the `scratchpad-stack-check` feature keeps in a stack region's
/// bottom word.
pub const CANARY: u32 = 0x5C4A_7C4D;

/// A byte range `start..end` of the scratchpad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    start: usize,
    end: usize,
}

impl Region {
    /// The bytes `start..end`. Fails const evaluation (a compile error in a
    /// `const`) when the range is empty or runs past [`SIZE`].
    pub const fn new(start: usize, end: usize) -> Self {
        assert!(start < end, "scratchpad region is empty");
        assert!(
            end <= SIZE,
            "scratchpad region runs past the 1 KiB scratchpad"
        );
        Self { start, end }
    }

    /// First byte offset.
    pub const fn start(self) -> usize {
        self.start
    }

    /// One past the last byte offset.
    pub const fn end(self) -> usize {
        self.end
    }

    /// Length in bytes.
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Always false: a region cannot be empty.
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Absolute address of the first byte.
    pub const fn addr(self) -> usize {
        BASE + self.start
    }

    /// True when the two ranges share a byte.
    pub const fn overlaps(self, other: Region) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Fail const evaluation when any two of `regions` overlap.
///
/// Pass every region that is live at one time, and call it in a `const _`
/// item so an overlap is a compile error rather than a corrupted frame.
///
/// ```compile_fail
/// use psx_rt::scratchpad::{assert_disjoint, Region};
/// const _: () = assert_disjoint(&[Region::new(0, 64), Region::new(32, 96)]);
/// ```
pub const fn assert_disjoint(regions: &[Region]) {
    let mut i = 0;
    while i < regions.len() {
        let mut j = i + 1;
        while j < regions.len() {
            if regions[i].overlaps(regions[j]) {
                panic!("scratchpad regions that are live together overlap");
            }
            j += 1;
        }
        i += 1;
    }
}

/// A call stack in scratchpad bytes `START..END`.
///
/// The stack grows down from `END - 16`; the bottom word at `START` is the
/// canary. See the [module docs](self) for the rules.
///
/// A layout the stack cannot use is a compile error where it is run:
///
/// ```compile_fail
/// use psx_rt::scratchpad::ScratchpadStack;
/// // END is not 8-byte aligned.
/// const _: usize = ScratchpadStack::<0, 1020>::BUDGET;
/// ```
pub struct ScratchpadStack<const START: usize, const END: usize>;

impl<const START: usize, const END: usize> ScratchpadStack<START, END> {
    /// The bytes this stack owns while a call runs on it. Evaluating it
    /// checks the layout: in bounds, `START` word aligned, `END` 8-byte
    /// aligned (the o32 stack alignment) and room for more than the
    /// overhead.
    pub const REGION: Region = {
        let region = Region::new(START, END);
        assert!(
            START.is_multiple_of(4),
            "scratchpad stack START must be word aligned"
        );
        assert!(
            END.is_multiple_of(8),
            "scratchpad stack END must be 8-byte aligned"
        );
        assert!(
            region.len() > STACK_OVERHEAD,
            "scratchpad stack region is smaller than its overhead"
        );
        region
    };

    /// Bytes of frames the call tree may use; `tools/stack_guard.py` fails
    /// the build when the linked tree needs more.
    pub const BUDGET: usize = Self::REGION.len() - STACK_OVERHEAD;

    /// Run `f` with `$sp` in this region and return its result.
    ///
    /// The switch is a 16-instruction trampoline after a 4-instruction `$sp`
    /// test. Already on a scratchpad stack, it calls `f` in place. On the host there is no scratchpad and
    /// it only calls `f`.
    ///
    /// # Safety
    ///
    /// * No scratchpad data another piece of code relies on may live in
    ///   [`Self::REGION`] while `f` runs, and `f` must not put any there.
    ///   Record the regions that are live around the call with
    ///   [`assert_disjoint`].
    /// * The linked call tree of `f` must fit in [`Self::BUDGET`]. Run
    ///   `tools/stack_guard.py` on every linked image that calls this.
    /// * Any exception handler installed while interrupts are enabled must
    ///   leave `$sp` and the memory below it alone (psx-rt's does).
    #[inline(always)]
    pub unsafe fn run<R, F: FnOnce() -> R>(f: F) -> R {
        // Force the layout checks for every instantiation.
        let region = Self::REGION;
        if on_scratchpad_stack() {
            return f();
        }
        let mut frame = Frame::<F, R> {
            f: ManuallyDrop::new(f),
            result: MaybeUninit::uninit(),
        };
        #[cfg(feature = "scratchpad-stack-check")]
        check::before(region);
        // SAFETY: the stack top is 8-byte aligned and inside the scratchpad
        // (REGION's checks); the entry reads `frame` exactly once and writes
        // the result before returning. The caller upholds the rest.
        unsafe {
            call_on_stack(
                (&raw mut frame).cast::<u8>(),
                Self::stack_entry::<R, F>,
                BASE + region.end() - 16,
            );
        }
        #[cfg(feature = "scratchpad-stack-check")]
        check::after(region);
        // SAFETY: stack_entry initialised it, and it cannot return early:
        // a panic aborts.
        unsafe { frame.result.assume_init() }
    }

    /// First function on the scratchpad stack. `tools/stack_guard.py` finds
    /// every monomorphised copy by this name and reads `START`/`END` from
    /// the symbol, so do not rename it without updating the tool.
    unsafe extern "C" fn stack_entry<R, F: FnOnce() -> R>(frame: *mut u8) {
        // SAFETY: `run` passes its own live `Frame<F, R>`.
        let frame = unsafe { &mut *frame.cast::<Frame<F, R>>() };
        // SAFETY: taken exactly once; `run` never drops `frame.f` itself.
        let f = unsafe { ManuallyDrop::take(&mut frame.f) };
        frame.result.write(f());
    }
}

/// The closure and its result, kept in the caller's RAM frame.
struct Frame<F, R> {
    f: ManuallyDrop<F>,
    result: MaybeUninit<R>,
}

/// True while `$sp` points into the scratchpad. Always false on the host.
#[inline(always)]
pub fn on_scratchpad_stack() -> bool {
    #[cfg(target_arch = "mips")]
    {
        crate::stack_pointer().wrapping_sub(BASE) <= SIZE
    }
    #[cfg(not(target_arch = "mips"))]
    {
        false
    }
}

// void __psx_rt_call_on_stack(void *frame, void (*entry)(void *), void *sp)
//
// Saves the caller's $sp in $s0 (callee-saved, so the entry preserves it)
// and in __psx_rt_scratchpad_return_sp (for the panic handler), sets $sp in
// the call's delay slot, calls entry(frame), then restores. Its own 24-byte
// frame stays on the caller's stack. An IRQ in the delay slot re-executes
// the jalr and the move, which are idempotent.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.psx_rt_scratchpad,"ax",@progbits
    .globl __psx_rt_call_on_stack
    .type __psx_rt_call_on_stack,@function
__psx_rt_call_on_stack:
    addiu $sp, $sp, -24
    sw    $ra, 20($sp)
    sw    $s0, 16($sp)
    move  $s0, $sp
    lui   $t0, %hi(__psx_rt_scratchpad_return_sp)
    sw    $sp, %lo(__psx_rt_scratchpad_return_sp)($t0)
    move  $t9, $a1
    jalr  $t9
    move  $sp, $a2
    lui   $t0, %hi(__psx_rt_scratchpad_return_sp)
    sw    $zero, %lo(__psx_rt_scratchpad_return_sp)($t0)
    move  $sp, $s0
    lw    $ra, 20($sp)
    lw    $s0, 16($sp)
    jr    $ra
    addiu $sp, $sp, 24
    .size __psx_rt_call_on_stack, .-__psx_rt_call_on_stack

    # Its own section: the panic handler keeps this one in every guest, and
    # only guests that use a ScratchpadStack should carry the switch above
    # (tools/stack_guard.py asks for a link map when it finds it).
    .section .text.psx_rt_scratchpad_panic,"ax",@progbits
    .globl __psx_rt_jump_on_stack
    .type __psx_rt_jump_on_stack,@function
__psx_rt_jump_on_stack:
    jr    $a2
    move  $sp, $a1
    .size __psx_rt_jump_on_stack, .-__psx_rt_jump_on_stack
    .set macro
    .set reorder
    "#
);

/// The RAM `$sp` below the frame of the outermost [`ScratchpadStack::run`]
/// in progress; zero when none is.
#[cfg(target_arch = "mips")]
#[no_mangle]
static mut __psx_rt_scratchpad_return_sp: usize = 0;

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_call_on_stack(frame: *mut u8, entry: unsafe extern "C" fn(*mut u8), sp: usize);
    fn __psx_rt_jump_on_stack(arg: *const u8, sp: usize, entry: extern "C" fn(*const u8) -> !)
        -> !;
}

#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn call_on_stack(frame: *mut u8, entry: unsafe extern "C" fn(*mut u8), sp: usize) {
    unsafe { __psx_rt_call_on_stack(frame, entry, sp) }
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
unsafe fn call_on_stack(frame: *mut u8, entry: unsafe extern "C" fn(*mut u8), _sp: usize) {
    unsafe { entry(frame) }
}

/// If a panic starts on a scratchpad stack, continue `report(arg)` on the
/// RAM stack the outermost [`ScratchpadStack::run`] left, and never
/// return. Returns when the panic is already on the RAM stack.
#[cfg(target_arch = "mips")]
#[inline(always)]
pub(crate) fn leave_for_panic(arg: *const u8, report: extern "C" fn(*const u8) -> !) {
    if !on_scratchpad_stack() {
        return;
    }
    // SAFETY: a plain read; the value is either zero or a RAM stack
    // pointer below every live RAM frame.
    let sp = unsafe { core::ptr::read_volatile(&raw const __psx_rt_scratchpad_return_sp) };
    if sp != 0 {
        // SAFETY: nothing on the scratchpad stack is used again.
        unsafe { __psx_rt_jump_on_stack(arg, sp, report) }
    }
}

#[cfg(feature = "scratchpad-stack-check")]
mod check {
    use super::Region;

    #[cfg(target_arch = "mips")]
    fn canary(region: Region) -> *mut u32 {
        (super::BASE + region.start()) as *mut u32
    }

    #[cfg(target_arch = "mips")]
    pub(super) fn before(region: Region) {
        if crate::interrupts::cpu_interrupts_enabled() && !crate::interrupts::handler_installed() {
            panic!("scratchpad stack: interrupts are on and psx-rt's exception handler is not installed");
        }
        // SAFETY: the word is inside the scratchpad and owned by the call.
        unsafe { canary(region).write_volatile(super::CANARY) }
    }

    #[cfg(target_arch = "mips")]
    pub(super) fn after(region: Region) {
        // SAFETY: as in `before`.
        if unsafe { canary(region).read_volatile() } != super::CANARY {
            panic!("scratchpad stack overran its region");
        }
    }

    #[cfg(not(target_arch = "mips"))]
    pub(super) fn before(_region: Region) {}

    #[cfg(not(target_arch = "mips"))]
    pub(super) fn after(_region: Region) {}
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    const HEADS: Region = Region::new(0, 128);
    const PLANES: Region = Region::new(128, 224);
    const BATCH: Region = Region::new(224, 1024);
    type Stack = ScratchpadStack<128, 1024>;
    const _: () = assert_disjoint(&[HEADS, PLANES, BATCH]);
    const _: () = assert_disjoint(&[HEADS, Stack::REGION]);

    #[test]
    fn regions_report_their_bytes() {
        assert_eq!(BATCH.len(), 800);
        assert_eq!(BATCH.addr(), 0x1F80_00E0);
        assert!(PLANES.overlaps(Stack::REGION));
        assert!(!HEADS.overlaps(PLANES));
        assert!(Region::new(0, 8).overlaps(Region::new(4, 12)));
        assert!(!Region::new(0, 8).overlaps(Region::new(8, 12)));
    }

    #[test]
    fn budget_leaves_the_home_area_and_canary() {
        assert_eq!(Stack::BUDGET, 896 - STACK_OVERHEAD);
        assert_eq!(ScratchpadStack::<0, 1024>::BUDGET, 1004);
    }

    #[test]
    #[should_panic(expected = "overlap")]
    fn overlapping_live_regions_are_rejected() {
        assert_disjoint(core::hint::black_box(&[HEADS, BATCH, Region::new(96, 160)]));
    }

    #[test]
    #[should_panic(expected = "past")]
    fn regions_stay_inside_the_scratchpad() {
        Region::new(core::hint::black_box(512), 1028);
    }

    #[test]
    #[should_panic(expected = "empty")]
    fn empty_regions_are_rejected() {
        Region::new(core::hint::black_box(64), 64);
    }

    #[test]
    fn run_returns_the_closure_result_and_moves_captures() {
        let mut touched = 0u32;
        let text = std::string::String::from("moved");
        let (len, words) = unsafe {
            Stack::run(|| {
                touched += 1;
                (text.len(), [7u32; 9])
            })
        };
        assert_eq!((touched, len, words), (1, 5, [7; 9]));
        assert!(!on_scratchpad_stack());
    }
}
