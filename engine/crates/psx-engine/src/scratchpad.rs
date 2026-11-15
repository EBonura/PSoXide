// SPDX-License-Identifier: GPL-2.0-or-later
//! PS1 CPU scratchpad access: psx-rt's one module, re-exported.
//!
//! The R3000A exposes 1 KiB of CPU-local RAM at `0x1f80_0000`. It is ordinary processor memory, not
//! MMIO, and cannot be used as a DMA source or destination. Reserve it for short-lived CPU working
//! sets while the GPU is reading packet data from main RAM.
//!
//! `psx_rt::scratchpad` is the single address, alignment and size authority (base pointer, typed
//! offsets, clearing, named regions with compile-time overlap checks, and running a
//! scratchpad-free phase with its stack there). Games still own their byte layouts and lifetimes: a
//! shared allocator would add bookkeeping to the console's hottest paths and make otherwise valid
//! overlapping phase reservations impossible. Every guest that runs a [`ScratchpadStack`] must pass
//! `stack-guard` on its linked image; the editor guest build runs it after the hazard patch.
pub use psx_rt::scratchpad::{
    assert_disjoint, base_ptr, clear, ptr_at, Region, ScratchpadStack, SIZE,
};

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
