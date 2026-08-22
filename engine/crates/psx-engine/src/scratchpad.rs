// SPDX-License-Identifier: GPL-2.0-or-later
//! PS1 CPU scratchpad access.
//!
//! The R3000A exposes 1 KiB of CPU-local RAM at `0x1f80_0000`. It is
//! ordinary processor memory, not MMIO, and cannot be used as a DMA source or
//! destination. Reserve it for short-lived CPU working sets while the GPU is
//! reading packet data from main RAM.
//!
//! This module is the single address/alignment authority shared by PSoXide,
//! Quake-PSX and HL-PSX. Games still own their byte layouts and lifetimes: a
//! shared allocator would add bookkeeping to the console's hottest paths and
//! make otherwise valid overlapping phase reservations impossible.

/// Total hardware scratchpad capacity in bytes.
pub const SIZE: usize = 1024;

#[repr(C, align(16))]
struct AlignedScratchpad([u8; SIZE]);

// Give LLVM an absolute object instead of manufacturing an integer-derived
// pointer at every call site. The symbol occupies no EXE or main-RAM space.
#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    ".globl __psoxide_scratchpad",
    ".set __psoxide_scratchpad, 0x1f800000",
);

#[cfg(target_arch = "mips")]
unsafe extern "C" {
    static mut __psoxide_scratchpad: AlignedScratchpad;
}

// Native tests exercise the same pointer API and layouts without fabricating
// a PS1 address. This storage never ships in a MIPS executable.
#[cfg(not(target_arch = "mips"))]
static mut HOST_SCRATCHPAD: AlignedScratchpad = AlignedScratchpad([0; SIZE]);

/// Base address of the shared CPU scratchpad backing.
///
/// # Safety
///
/// The caller must coordinate all live ranges in the arena. The engine does
/// not impose an allocator because render phases deliberately reuse the same
/// bytes after earlier consumers have finished.
#[inline(always)]
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

/// Return a typed pointer at `byte_offset` within the scratchpad.
///
/// # Safety
///
/// The caller must reserve a suitably aligned, non-overlapping range large
/// enough for every value addressed through the returned pointer. No reference
/// into that range may outlive the operation which owns the reservation.
#[inline(always)]
pub unsafe fn ptr_at<T>(byte_offset: usize) -> *mut T {
    debug_assert!(byte_offset <= SIZE.saturating_sub(core::mem::size_of::<T>()));
    debug_assert!(byte_offset.is_multiple_of(core::mem::align_of::<T>()));
    unsafe { base_ptr().add(byte_offset).cast::<T>() }
}

/// Establish deterministic contents before the first scratchpad consumer.
///
/// # Safety
///
/// Call only while no scratchpad-backed working set is live.
#[inline]
pub unsafe fn clear() {
    let words = unsafe { ptr_at::<u32>(0) };
    let mut index = 0usize;
    while index < SIZE / core::mem::size_of::<u32>() {
        unsafe { words.add(index).write(0) };
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_regions_share_one_aligned_backing() {
        unsafe {
            clear();
            ptr_at::<u32>(0).write(0x7654_3210);
            ptr_at::<u16>(4).write(0xabcd);
            assert_eq!(ptr_at::<u32>(0).read(), 0x7654_3210);
            assert_eq!(ptr_at::<u16>(4).read(), 0xabcd);
            assert_eq!(base_ptr() as usize % 16, 0);
        }
    }
}
