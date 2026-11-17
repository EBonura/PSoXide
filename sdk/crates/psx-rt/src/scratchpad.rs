//! The CPU scratchpad, and running code with its stack there.
//!
//! The R3000A has no data cache. Its 1 KiB scratchpad ([`BASE`]) is
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
//!   interrupted stack, and then says so with
//!   [`declare_stack_safe_handler`](crate::interrupts::declare_stack_safe_handler)
//!   (hk-psx's CD wrapper saves to its own context, runs on its own stack
//!   and chains to psx-rt's handler). The `scratchpad-stack-check` feature
//!   traps a call made with interrupts enabled and any other handler in
//!   the vector.
//! * Stack depth. The whole call tree of `f` must fit in the region minus
//!   [`STACK_OVERHEAD`] ([`ScratchpadStack::BUDGET`]). `stack-guard`
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
//!
//! # The two forms
//!
//! [`ScratchpadStack::call`] runs a known function, [`StackEntry::enter`],
//! with one context pointer: the switch is ten instructions and the entry
//! function is a jump straight into `enter`, so it costs nothing a
//! hand-written switch would not. [`ScratchpadStack::run`] takes any closure
//! and goes through `call`: a closure that returns nothing and captures at
//! most one word travels in the context register, any other keeps itself and
//! its result in the caller's frame.

use core::marker::PhantomData;
use core::mem::{ManuallyDrop, MaybeUninit};

/// Address of the first scratchpad byte (KUSEG/KSEG0; there is no KSEG1
/// alias).
pub const BASE: usize = psx_hw::memory::scratchpad::BASE as usize;

/// Scratchpad capacity in bytes.
pub const SIZE: usize = psx_hw::memory::scratchpad::SIZE;

/// Bytes of a stack region a call tree cannot use: the 16-byte o32
/// argument home area above the first frame, and the canary word at the
/// bottom of the region (reserved whether or not the check is enabled, so
/// the budget does not depend on a feature).
pub const STACK_OVERHEAD: usize = 20;

/// The value the `scratchpad-stack-check` feature keeps in a stack region's
/// bottom word.
pub const CANARY: u32 = 0x5c4a_7c4d;

#[repr(C, align(16))]
struct AlignedScratchpad([u8; SIZE]);

// Give LLVM an absolute object instead of manufacturing an integer-derived pointer at every call
// site. The symbol occupies no EXE or main-RAM space.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".globl __psoxide_scratchpad",
    ".set __psoxide_scratchpad, {base}",
    base = const psx_hw::memory::scratchpad::BASE,
);

#[cfg(target_arch = "mips")]
unsafe extern "C" {
    static mut __psoxide_scratchpad: AlignedScratchpad;
}

// Native tests exercise the same pointer API and layouts without fabricating a PS1 address. This
// storage never ships in a MIPS executable.
#[cfg(not(target_arch = "mips"))]
static mut HOST_SCRATCHPAD: AlignedScratchpad = AlignedScratchpad([0; SIZE]);

/// Base address of the scratchpad as a byte pointer, 16-byte aligned (on the host, a private
/// block of the same size and alignment).
///
/// # Safety
///
/// The caller must coordinate all live ranges in the scratchpad. There is no allocator because
/// render phases deliberately reuse the same bytes after earlier consumers have finished; name
/// each range as a [`Region`] and check the live sets with [`assert_disjoint`].
#[inline(always)]
#[must_use]
pub unsafe fn base_ptr() -> *mut u8 {
    #[cfg(target_arch = "mips")]
    {
        core::ptr::addr_of_mut!(__psoxide_scratchpad).cast::<u8>()
    }

    #[cfg(not(target_arch = "mips"))]
    {
        core::ptr::addr_of_mut!(HOST_SCRATCHPAD).cast::<u8>()
    }
}

/// A typed pointer at `byte_offset` within the scratchpad.
///
/// # Safety
///
/// The caller must reserve a suitably aligned, non-overlapping range large enough for every value
/// addressed through the returned pointer. No reference into that range may outlive the operation
/// which owns the reservation.
#[inline(always)]
#[must_use]
pub unsafe fn ptr_at<T>(byte_offset: usize) -> *mut T {
    debug_assert!(byte_offset <= SIZE.saturating_sub(core::mem::size_of::<T>()));
    debug_assert!(byte_offset.is_multiple_of(core::mem::align_of::<T>()));
    // SAFETY: forwarded contract; the offset stays inside the scratchpad (checked in debug builds).
    unsafe { base_ptr().add(byte_offset).cast::<T>() }
}

/// Zero the whole scratchpad, so the first consumer starts from deterministic contents.
///
/// # Safety
///
/// Call only while no scratchpad-backed working set is live.
#[inline]
pub unsafe fn clear() {
    // SAFETY: forwarded contract; word 0 is in range and 4-aligned (the base is 16-aligned).
    let words = unsafe { ptr_at::<u32>(0) };
    let mut index = 0usize;
    while index < SIZE / core::mem::size_of::<u32>() {
        // SAFETY: `index` stays below the scratchpad's word count.
        unsafe { words.add(index).write(0) };
        index += 1;
    }
}

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

/// A function that runs as the root of a scratchpad stack call, given one
/// context pointer ([`ScratchpadStack::call`]).
///
/// `enter` is the first real frame on the scratchpad stack: the switch reaches
/// it through a two-instruction jump with no frame of its own, so a known
/// function costs no more than the switch. Implement it on a type that names
/// the call:
///
/// ```
/// use psx_rt::scratchpad::{ScratchpadStack, StackEntry};
///
/// struct Walk;
/// impl StackEntry for Walk {
///     unsafe extern "C" fn enter(context: *mut u8) {
///         // SAFETY: the caller passes a live `u32`.
///         unsafe { *context.cast::<u32>() += 1 };
///     }
/// }
///
/// let mut count = 0u32;
/// // SAFETY: on the host `call` only calls `enter`.
/// unsafe { ScratchpadStack::<128, 1024>::call::<Walk>((&raw mut count).cast()) };
/// assert_eq!(count, 1);
/// ```
pub trait StackEntry {
    /// The call. `context` is the pointer given to [`ScratchpadStack::call`].
    ///
    /// # Safety
    ///
    /// The contract of [`ScratchpadStack::call`], plus whatever `context`
    /// must point to for this entry.
    unsafe extern "C" fn enter(context: *mut u8);
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

    /// Bytes of frames the call tree may use; `stack-guard` fails
    /// the build when the linked tree needs more.
    pub const BUDGET: usize = Self::REGION.len() - STACK_OVERHEAD;

    /// Run `E::enter(context)` with `$sp` in this region.
    ///
    /// The lean form: a 4-instruction `$sp` test, then the 10-instruction
    /// switch, which jumps through [`stack_entry`](Self::stack_entry) straight
    /// into `enter`. Already on a scratchpad stack, it calls `enter` in place.
    /// On the host there is no scratchpad and it only calls `enter`.
    ///
    /// # Safety
    ///
    /// As [`run`](Self::run), with `enter`'s call tree in place of `f`'s,
    /// and `context` valid for `enter`.
    #[inline(always)]
    pub unsafe fn call<E: StackEntry>(context: *mut u8) {
        // Force the layout checks for every instantiation.
        let region = Self::REGION;
        #[cfg(feature = "scratchpad-window")]
        let in_window = crate::scratchpad_window::is_active();
        #[cfg(not(feature = "scratchpad-window"))]
        let in_window = false;
        if is_on_scratchpad_stack() || in_window {
            // SAFETY: forwarded contract; a nested call runs on the stack it is on.
            unsafe { E::enter(context) };
            return;
        }
        #[cfg(feature = "scratchpad-stack-check")]
        check::before(region);
        // SAFETY: the stack top is 8-byte aligned and inside the scratchpad
        // (REGION's checks); the caller upholds the rest.
        unsafe { call_on_stack(context, Self::stack_entry::<E>, BASE + region.end() - 16) };
        #[cfg(feature = "scratchpad-stack-check")]
        check::after(region);
    }

    /// Run `f` with `$sp` in this region and return its result.
    ///
    /// The general form, on [`call`](Self::call). A closure that returns
    /// nothing and captures at most one word travels in the context register
    /// itself; any other closure and its result live in the caller's frame.
    /// Already on a scratchpad stack, it calls `f` in place. On the host
    /// there is no scratchpad and it only calls `f`.
    ///
    /// # Safety
    ///
    /// * No scratchpad data another piece of code relies on may live in
    ///   [`Self::REGION`] while `f` runs, and `f` must not put any there.
    ///   Record the regions that are live around the call with
    ///   [`assert_disjoint`].
    /// * The linked call tree of `f` must fit in [`Self::BUDGET`]. Run
    ///   `stack-guard` on every linked image that calls this.
    /// * Any exception handler installed while interrupts are enabled must
    ///   leave `$sp` and the memory below it alone (psx-rt's does; declare
    ///   another with
    ///   [`declare_stack_safe_handler`](crate::interrupts::declare_stack_safe_handler)).
    #[inline(always)]
    pub unsafe fn run<R, F: FnOnce() -> R>(f: F) -> R {
        if InRegister::<F, R>::FITS {
            let mut word = core::ptr::null_mut::<u8>();
            // SAFETY: FITS: `F` is no larger and no more aligned than the word.
            unsafe { (&raw mut word).cast::<F>().write(f) };
            // SAFETY: forwarded contract; `enter` takes the closure back out of `word`.
            unsafe { Self::call::<InRegister<F, R>>(word) };
            // SAFETY: `R` is zero-sized, so the value `enter` wrote is moved
            // out of the dangling pointer it was written through.
            return unsafe { core::ptr::NonNull::<R>::dangling().as_ptr().read() };
        }
        let mut frame = Frame::<F, R> {
            f: ManuallyDrop::new(f),
            result: MaybeUninit::uninit(),
        };
        // SAFETY: forwarded contract; `enter` reads `frame` exactly once and
        // writes the result before returning.
        unsafe { Self::call::<Frame<F, R>>((&raw mut frame).cast::<u8>()) };
        // SAFETY: `enter` initialised it, and it cannot return early: a
        // panic aborts.
        unsafe { frame.result.assume_init() }
    }

    /// First function on the scratchpad stack: a jump to `E::enter`, with no
    /// frame. `stack-guard` finds every monomorphised copy by this name and
    /// reads `START`/`END` from the symbol, so do not rename it without
    /// updating the tool.
    #[cfg(target_arch = "mips")]
    #[unsafe(naked)]
    unsafe extern "C" fn stack_entry<E: StackEntry>(_context: *mut u8) {
        core::arch::naked_asm!(
            ".set noreorder",
            "j {enter}",
            "nop",
            ".set reorder",
            enter = sym E::enter,
        )
    }

    #[cfg(not(target_arch = "mips"))]
    unsafe extern "C" fn stack_entry<E: StackEntry>(context: *mut u8) {
        // SAFETY: forwarded from `call`.
        unsafe { E::enter(context) }
    }
}

/// The closure and its result, kept in the caller's RAM frame.
struct Frame<F, R> {
    f: ManuallyDrop<F>,
    result: MaybeUninit<R>,
}

impl<R, F: FnOnce() -> R> StackEntry for Frame<F, R> {
    unsafe extern "C" fn enter(context: *mut u8) {
        // SAFETY: `run` passes its own live `Frame<F, R>`.
        let frame = unsafe { &mut *context.cast::<Frame<F, R>>() };
        // SAFETY: taken exactly once; `run` never drops `frame.f` itself.
        let f = unsafe { ManuallyDrop::take(&mut frame.f) };
        frame.result.write(f());
    }
}

/// A closure that returns nothing and fits in the context word itself.
struct InRegister<F, R>(PhantomData<(F, R)>);

impl<F, R> InRegister<F, R> {
    const FITS: bool = core::mem::size_of::<R>() == 0
        && core::mem::size_of::<F>() <= core::mem::size_of::<*mut u8>()
        && core::mem::align_of::<F>() <= core::mem::align_of::<*mut u8>();
}

impl<R, F: FnOnce() -> R> StackEntry for InRegister<F, R> {
    unsafe extern "C" fn enter(context: *mut u8) {
        // SAFETY: `run` wrote an `F` into this word (FITS) and never uses it again.
        let f = unsafe { (&raw const context).cast::<F>().read() };
        let result = f();
        // SAFETY: `R` is zero-sized; `run` moves it back out of the same dangling pointer.
        unsafe { core::ptr::NonNull::<R>::dangling().as_ptr().write(result) };
    }
}

/// True while `$sp` points into the scratchpad. Always false on the host.
#[inline(always)]
pub fn is_on_scratchpad_stack() -> bool {
    #[cfg(target_arch = "mips")]
    {
        crate::stack_pointer().wrapping_sub(BASE) <= SIZE
    }
    #[cfg(not(target_arch = "mips"))]
    {
        false
    }
}

// void __psx_rt_call_on_stack(void *context, void (*entry)(void *), void *sp)
//
// Saves the caller's $sp and $ra in __psx_rt_scratchpad_return_sp (the RAM
// $sp is also what the panic handler returns to), sets $sp in the call's delay
// slot, calls entry(context), then restores both from the record and clears its
// $sp word in the return's delay slot. No frame and no callee-saved register:
// one switch is in progress at a time (a nested call runs in place), and the
// entry's call tree hands $sp back unchanged. An IRQ in the jalr's delay slot
// re-executes the jalr and the move, which are idempotent. Ten instructions.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set nomacro
    .section .text.psx_rt_scratchpad,"ax",@progbits
    .globl __psx_rt_call_on_stack
    .type __psx_rt_call_on_stack,@function
__psx_rt_call_on_stack:
    lui   $t0, %hi(__psx_rt_scratchpad_return_sp)
    sw    $sp, %lo(__psx_rt_scratchpad_return_sp)($t0)
    sw    $ra, %lo(__psx_rt_scratchpad_return_sp+4)($t0)
    jalr  $a1
    move  $sp, $a2
    lui   $t0, %hi(__psx_rt_scratchpad_return_sp)
    lw    $ra, %lo(__psx_rt_scratchpad_return_sp+4)($t0)
    lw    $sp, %lo(__psx_rt_scratchpad_return_sp)($t0)
    jr    $ra
    sw    $zero, %lo(__psx_rt_scratchpad_return_sp)($t0)
    .size __psx_rt_call_on_stack, .-__psx_rt_call_on_stack

    # Its own section: the panic handler keeps this one in every guest, and
    # only guests that use a ScratchpadStack should carry the switch above
    # (stack-guard asks for a link map when it finds it).
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

/// The switch's save record: the caller's RAM `$sp` (zero when no switch is
/// in progress) and its `$ra`. 8-aligned, so `%hi` of both words is the same.
#[cfg(target_arch = "mips")]
#[repr(C, align(8))]
pub(crate) struct SwitchRecord([usize; 2]);

/// The record of the outermost [`ScratchpadStack::call`] in progress. Its
/// first word is also set by a scratchpad window's level-1 switch.
#[cfg(target_arch = "mips")]
#[no_mangle]
pub(crate) static mut __psx_rt_scratchpad_return_sp: SwitchRecord = SwitchRecord([0; 2]);

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_call_on_stack(context: *mut u8, entry: unsafe extern "C" fn(*mut u8), sp: usize);
    fn __psx_rt_jump_on_stack(arg: *const u8, sp: usize, entry: extern "C" fn(*const u8) -> !)
        -> !;
}

#[cfg(target_arch = "mips")]
#[inline(always)]
unsafe fn call_on_stack(context: *mut u8, entry: unsafe extern "C" fn(*mut u8), sp: usize) {
    // SAFETY: the caller (`ScratchpadStack::call`) passes the entry that matches `context` and an
    // 8-byte-aligned stack top inside the scratchpad. The trampoline keeps $ra and $sp in its
    // record, switches $sp, calls `entry(context)`, and restores both, so the O32 contract holds
    // across it.
    unsafe { __psx_rt_call_on_stack(context, entry, sp) }
}

#[cfg(not(target_arch = "mips"))]
#[inline(always)]
unsafe fn call_on_stack(context: *mut u8, entry: unsafe extern "C" fn(*mut u8), _sp: usize) {
    // SAFETY: the caller passes the entry that matches `context`, as on the target; the host has
    // no scratchpad, so `entry` simply runs on the current stack.
    unsafe { entry(context) }
}

/// If a panic starts on a scratchpad stack, continue `report(arg)` on the
/// RAM stack the outermost [`ScratchpadStack::call`] left, and never
/// return. Returns when the panic is already on the RAM stack.
#[cfg(target_arch = "mips")]
#[inline(always)]
pub(crate) fn leave_for_panic(arg: *const u8, report: extern "C" fn(*const u8) -> !) {
    if !is_on_scratchpad_stack() {
        return;
    }
    // SAFETY: a plain read; the value is either zero or a RAM stack
    // pointer below every live RAM frame.
    let sp = unsafe { core::ptr::read_volatile(&raw const __psx_rt_scratchpad_return_sp.0[0]) };
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
        if crate::interrupts::are_interrupts_enabled()
            && !crate::interrupts::is_stack_safe_handler_installed()
        {
            panic!(
                "scratchpad stack: interrupts are on and the exception vector is neither psx-rt's \
                 handler nor one declared with interrupts::declare_stack_safe_handler"
            );
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

    #[test]
    fn typed_regions_share_one_aligned_backing() {
        // SAFETY: the test owns the host scratchpad block for its duration (tests of this
        // module that touch it take no other reference into it).
        unsafe {
            clear();
            ptr_at::<u32>(0).write(0x7654_3210);
            ptr_at::<u16>(4).write(0xabcd);
            assert_eq!(ptr_at::<u32>(0).read(), 0x7654_3210);
            assert_eq!(ptr_at::<u16>(4).read(), 0xabcd);
            assert_eq!(base_ptr() as usize % 16, 0);
        }
    }

    const HEADS: Region = Region::new(0, 128);
    const PLANES: Region = Region::new(128, 224);
    const BATCH: Region = Region::new(224, 1024);
    type Stack = ScratchpadStack<128, 1024>;
    const _: () = assert_disjoint(&[HEADS, PLANES, BATCH]);
    const _: () = assert_disjoint(&[HEADS, Stack::REGION]);

    #[test]
    fn regions_report_their_bytes() {
        assert_eq!(BATCH.len(), 800);
        assert_eq!(BATCH.addr(), BASE + 0xe0);
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
        // SAFETY: on the host `run` only calls the closure, so none of its scratchpad, stack-budget
        // or exception-handler obligations apply.
        let (len, words) = unsafe {
            Stack::run(|| {
                touched += 1;
                (text.len(), [7u32; 9])
            })
        };
        assert_eq!((touched, len, words), (1, 5, [7; 9]));
        assert!(!is_on_scratchpad_stack());
    }

    #[test]
    fn a_unit_closure_of_one_word_travels_in_the_context_register() {
        let mut count = 0u32;
        let by_ref = || count += 3;
        // SAFETY: as above, the host only calls the closure.
        unsafe { Stack::run(by_ref) };
        assert_eq!(count, 3);
        assert!(!InRegister::<[usize; 2], ()>::FITS);
        assert!(!InRegister::<u8, u32>::FITS);
        assert!(InRegister::<u32, ()>::FITS);
        assert!(InRegister::<(), ()>::FITS);
        let text = std::string::String::from("dropped once");
        // SAFETY: as above. The captured `String` is moved in and dropped by the closure.
        unsafe { Stack::run(move || drop(text)) };
    }

    struct AddTwo;
    impl StackEntry for AddTwo {
        unsafe extern "C" fn enter(context: *mut u8) {
            // SAFETY: the test passes a live `u32`.
            unsafe { *context.cast::<u32>() += 2 };
        }
    }

    #[test]
    fn call_runs_the_entry_with_its_context() {
        let mut value = 40u32;
        // SAFETY: as above, the host only calls the entry.
        unsafe { Stack::call::<AddTwo>((&raw mut value).cast()) };
        assert_eq!(value, 42);
    }
}
