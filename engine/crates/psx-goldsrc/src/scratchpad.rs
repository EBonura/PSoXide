//! PS1 CPU scratchpad access.
//!
//! Both ports use this one module; each passes its own canary end to
//! `call_on_projection_stack` and `call_on_stack`.
//!
//! The R3000A exposes 1 KiB of CPU-local memory at `0x1f80_0000` through
//! KUSEG/KSEG0. It is ordinary fast RAM, not MMIO, and cannot participate in
//! DMA. Keep GPU packets and streamed payloads in main RAM; this module is for
//! small CPU-only working sets that are hottest while DMA owns the main bus.

/// Total hardware scratchpad capacity.
pub use psx_engine::scratchpad::SIZE;

#[cfg(feature = "main-ram-render-scratch")]
#[repr(C, align(16))]
struct AlignedScratchpad([u8; SIZE]);

// Host builds and the explicit A/B baseline use an identically sized/aligned
// main-RAM block. That keeps all callers and data layouts identical: only the
// physical memory backing changes.
#[cfg(feature = "main-ram-render-scratch")]
static mut MAIN_RAM_SCRATCHPAD: AlignedScratchpad = AlignedScratchpad([0; SIZE]);

// `a0=context`, `a1=entry`, `a2=new stack top`, `a3=save area`. Preserve the
// old stack, return address, and s0 in the arena below the guarded stack floor;
// s0 is callee-saved, so it remains a reliable way back after the Rust entry
// function has used every caller-saved register.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".section .text.__psx_goldsrc_call_on_stack,\"ax\",@progbits",
    ".globl __psx_goldsrc_call_on_stack",
    ".type __psx_goldsrc_call_on_stack,@function",
    ".set noreorder",
    ".ent __psx_goldsrc_call_on_stack",
    "__psx_goldsrc_call_on_stack:",
    "sw $sp, 0($7)",
    "sw $ra, 4($7)",
    "sw $16, 8($7)",
    "move $16, $7",
    "move $sp, $6",
    "jalr $5",
    "nop",
    "lw $ra, 4($16)",
    "lw $sp, 0($16)",
    "lw $16, 8($16)",
    "jr $ra",
    "nop",
    ".end __psx_goldsrc_call_on_stack",
);

#[cfg(target_arch = "mips")]
unsafe extern "C" {
    fn __psx_goldsrc_call_on_stack(
        context: *mut u8,
        entry: unsafe extern "C" fn(*mut u8),
        stack_top: *mut u8,
        save_area: *mut u32,
    );
}

const PROJECTION_SAVE_OFFSET: usize = 128;
const PROJECTION_GUARD_OFFSET: usize = PROJECTION_SAVE_OFFSET + 3 * core::mem::size_of::<u32>();
// The canary's end is the port's own audited stack budget (`GUARD_END` of
// `call_on_projection_stack`): the stack-guard proof of the linked chain is what keeps the
// stack off it, and the canary only has to catch what static analysis cannot see.
const PROJECTION_GUARD: u32 = 0x5a17_c0de;
// Both A/B builds retain the same runtime branch and both projection paths.
// Distinct non-zero values keep this byte in `.data` in either build; the
// volatile read prevents the selector from folding either path away.
static mut PROJECTION_STACK_MODE: u8 = if cfg!(feature = "main-ram-projection-stack") {
    2
} else {
    1
};

#[inline(always)]
unsafe fn base() -> *mut u8 {
    #[cfg(not(feature = "main-ram-render-scratch"))]
    {
        unsafe { psx_engine::scratchpad::base_ptr() }
    }

    #[cfg(feature = "main-ram-render-scratch")]
    {
        core::ptr::addr_of_mut!(MAIN_RAM_SCRATCHPAD).cast::<u8>()
    }
}

/// Execute one context entry on the recyclable projection stack.
///
/// Bytes 0..128 remain owned by persistent viewmodel bucket heads. The save
/// record occupies 128..140 and a canary extends from byte 140 to `GUARD_END`;
/// the stack grows down from byte 1024. Returning `false` means the stack used
/// more than its audited `1024 - GUARD_END` byte guarded budget, but did not reach the bucket
/// heads or the trampoline's save record.
///
/// # Safety
/// The caller must ensure the face-group bitsets at bytes 128..840 are dead,
/// `entry` follows the C ABI, and `context` remains valid until it returns.
pub unsafe fn call_on_projection_stack<const GUARD_END: usize>(
    context: *mut u8,
    entry: unsafe extern "C" fn(*mut u8),
) -> bool {
    const { assert!(GUARD_END > PROJECTION_GUARD_OFFSET && GUARD_END < SIZE) };
    let guard_words = (GUARD_END - PROJECTION_GUARD_OFFSET) / core::mem::size_of::<u32>();
    // The A/B control retains the same non-inlined entry function but lets it
    // use the ordinary CPU stack. It needs no second main-RAM arena and makes
    // the scratchpad candidate earn back its trampoline and guard overhead.
    let mode = unsafe { core::ptr::addr_of!(PROJECTION_STACK_MODE).read_volatile() };
    if mode == 2 {
        unsafe { entry(context) };
        return true;
    }

    let arena = unsafe { base() };
    let guard = unsafe { arena.add(PROJECTION_GUARD_OFFSET).cast::<u32>() };
    let mut i = 0usize;
    while i < guard_words {
        unsafe { guard.add(i).write(PROJECTION_GUARD ^ i as u32) };
        i += 1;
    }

    #[cfg(target_arch = "mips")]
    unsafe {
        __psx_goldsrc_call_on_stack(
            context,
            entry,
            arena.add(SIZE),
            arena.add(PROJECTION_SAVE_OFFSET).cast::<u32>(),
        );
    }

    #[cfg(not(target_arch = "mips"))]
    unsafe {
        entry(context);
    }

    i = 0;
    while i < guard_words {
        if unsafe { guard.add(i).read() } != PROJECTION_GUARD ^ i as u32 {
            return false;
        }
        i += 1;
    }
    true
}

/// Run `entry(context)` with the stack pointer at scratchpad byte
/// `stack_top`, the trampoline's 12-byte save record at `save`.
///
/// # Safety
/// `entry` follows the C ABI and its deepest call chain fits below
/// `stack_top` without reaching `save + 12`; nothing else lives in that range
/// until it returns; the CPU is not already on a scratchpad stack.
#[inline(always)]
pub unsafe fn call_on_stack(
    context: *mut u8,
    entry: unsafe extern "C" fn(*mut u8),
    stack_top: usize,
    save: usize,
) {
    #[cfg(target_arch = "mips")]
    unsafe {
        let arena = base();
        __psx_goldsrc_call_on_stack(
            context,
            entry,
            arena.add(stack_top),
            arena.add(save).cast::<u32>(),
        );
    }
    #[cfg(not(target_arch = "mips"))]
    unsafe {
        let _ = (stack_top, save);
        entry(context);
    }
}

/// Return a typed pointer at `byte_offset` within the scratchpad arena.
///
/// # Safety
/// The caller must reserve a suitably aligned, non-overlapping range large
/// enough for every element it will address through the returned pointer.
#[inline(always)]
pub unsafe fn ptr_at<T>(byte_offset: usize) -> *mut T {
    debug_assert!(byte_offset <= SIZE.saturating_sub(core::mem::size_of::<T>()));
    debug_assert!(byte_offset % core::mem::align_of::<T>() == 0);

    unsafe { base().add(byte_offset).cast::<T>() }
}

/// Establish deterministic contents before the first consumer starts.
///
/// # Safety
/// Call only when no scratchpad-backed working set is live.
pub unsafe fn clear() {
    let words = unsafe { ptr_at::<u32>(0) };
    let mut i = 0usize;
    while i < SIZE / core::mem::size_of::<u32>() {
        unsafe { words.add(i).write(0) };
        i += 1;
    }
}
