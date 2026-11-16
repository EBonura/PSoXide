//! PS1 CPU scratchpad access for the GoldSrc ports: `psx_rt::scratchpad`, plus the projection
//! stack both ports run their model projection on.
//!
//! The R3000A exposes 1 KiB of CPU-local memory at `0x1f80_0000` through
//! KUSEG/KSEG0. It is ordinary fast RAM, not MMIO, and cannot participate in
//! DMA. Keep GPU packets and streamed payloads in main RAM; this module is for
//! small CPU-only working sets that are hottest while DMA owns the main bus.

pub use psx_rt::scratchpad::{clear, ptr_at, ScratchpadStack, SIZE};

/// Start of the projection stack's canary. Bytes 0..128 stay owned by the persistent
/// viewmodel bucket heads; 128..140 held the old trampoline's save record and are left unused
/// so both ports keep their offsets.
const PROJECTION_GUARD_OFFSET: usize = 140;
const PROJECTION_GUARD: u32 = 0x5a17_c0de;

/// Execute one context entry on the recyclable projection stack.
///
/// The stack is `ScratchpadStack<GUARD_END, SIZE>`: `stack-guard` proves the linked chain fits
/// its budget after every link. A canary extends from byte 140 to `GUARD_END`, below the
/// stack, and catches what static analysis cannot see. Returning `false` means the canary was
/// written over; nothing below byte 140 is reached before that.
///
/// # Safety
/// The caller must ensure the face-group bitsets at bytes 128..840 are dead,
/// `entry` follows the C ABI, and `context` remains valid until it returns.
#[inline(always)]
pub unsafe fn call_on_projection_stack<const GUARD_END: usize>(
    context: *mut u8,
    entry: unsafe extern "C" fn(*mut u8),
) -> bool {
    const { assert!(GUARD_END > PROJECTION_GUARD_OFFSET && GUARD_END < SIZE) };
    let guard_words = (GUARD_END - PROJECTION_GUARD_OFFSET) / core::mem::size_of::<u32>();
    // SAFETY: bytes 140..GUARD_END are this call's canary; the caller guarantees nothing else
    // lives there.
    let guard = unsafe { ptr_at::<u32>(PROJECTION_GUARD_OFFSET) };
    let mut i = 0usize;
    while i < guard_words {
        // SAFETY: `i` stays inside the canary words reserved above.
        unsafe { guard.add(i).write(PROJECTION_GUARD ^ i as u32) };
        i += 1;
    }
    // SAFETY: the caller guarantees GUARD_END..SIZE is free and `entry`/`context` are valid;
    // `stack-guard` proves the linked call tree fits the stack's budget.
    unsafe { ScratchpadStack::<GUARD_END, SIZE>::run(|| entry(context)) };
    i = 0;
    while i < guard_words {
        // SAFETY: as above.
        if unsafe { guard.add(i).read() } != PROJECTION_GUARD ^ i as u32 {
            return false;
        }
        i += 1;
    }
    true
}
