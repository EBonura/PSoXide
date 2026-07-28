// SPDX-License-Identifier: GPL-2.0-or-later
//! Where this program's data sits on the disc it was booted from.
//!
//! A program that owns its disc knows exactly where everything is: pack LBAs
//! are cooked in, CD-DA tracks are numbered from 2. Put several programs on
//! one disc and those absolutes stop being absolute. Rather than re-cook every
//! program for every disc it might ship on, a multi-program loader hands over
//! two numbers at boot and the SDK's disc entry points apply them.
//!
//! Both default to zero, so a program booted the ordinary way -- BIOS reads
//! `SYSTEM.CNF`, runs the one EXE -- behaves exactly as it did before this
//! module existed. Nothing needs a separate build to work on both.
//!
//! The handover is by register, not by an agreed memory address: there is no
//! address the SDK can prove a game will leave alone (hl-psx already uses the
//! scratchpad, and a headerless PSX-EXE gives no way to bound `.bss`).
//! [`psx-rt`'s `_start`](../../psx_rt/fn._start.html) takes the loader's
//! arguments and calls [`install`] before anything else runs.

/// Marks `$a0` as a loader handover rather than boot-time register garbage.
/// ASCII `PSXD`.
pub const HANDOFF_MAGIC: u32 = 0x5053_5844;

// Forced into `.data` rather than left in `.bss`. A chain-loader blob is
// linked as a bare `.text` image with no `.bss` to zero, but it still reads
// the disc through this SDK; leaving these in `.bss` would have it consult
// uninitialised memory. Guest-only: the host builds these crates too, and
// mach-o spells section names differently.
#[cfg_attr(target_arch = "mips", link_section = ".data")]
static mut LBA_OFFSET: u32 = 0;
#[cfg_attr(target_arch = "mips", link_section = ".data")]
static mut CDDA_TRACK_BASE: u8 = 0;

/// Record the loader's handover, if `magic` says there is one.
///
/// A normal BIOS boot leaves whatever it likes in the argument registers, so
/// the magic is what separates "chain-loaded, here are your bases" from
/// "nobody told us anything". Calling this with a wrong magic is a no-op.
///
/// # Safety
/// Must be called once, before any other thread of control could observe the
/// bases -- in practice the first thing `_start` does after zeroing `.bss`.
pub unsafe fn install(magic: u32, lba_offset: u32, cdda_track_base: u32) {
    if magic != HANDOFF_MAGIC {
        return;
    }
    unsafe {
        LBA_OFFSET = lba_offset;
        CDDA_TRACK_BASE = cdda_track_base as u8;
    }
}

/// Sectors between the disc's LBA 0 and this program's.
#[inline(always)]
pub fn lba_offset() -> u32 {
    // SAFETY: written once by `install` before anything else runs; read-only
    // afterwards, and the guest is single-threaded.
    unsafe { LBA_OFFSET }
}

/// CD-DA tracks belonging to programs ahead of this one on the disc.
#[inline(always)]
pub fn cdda_track_base() -> u8 {
    // SAFETY: as `lba_offset`.
    unsafe { CDDA_TRACK_BASE }
}

/// Translate a disc-relative LBA into a physical one.
#[inline(always)]
pub fn shift_lba(lba: u32) -> u32 {
    lba.wrapping_add(lba_offset())
}

/// Translate a program-relative CD-DA track number into a physical one.
#[inline(always)]
pub fn shift_track(track: u8) -> u8 {
    track.wrapping_add(cdda_track_base())
}
