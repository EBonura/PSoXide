// SPDX-License-Identifier: GPL-2.0-or-later
//! Residency lock: a debug mode that proves nothing loads mid-level.
//!
//! A game calls [`seal`] when its level is ready and [`unseal`] when it starts
//! the next load. While sealed, any pack load (every `cd::` loader in this
//! crate, and the GoldSrc chunk streamer, which calls [`check`]) is a fatal
//! error that names the chunk it wanted: the key is kept in
//! [`culprit`] and handed to the hook set with [`set_violation_hook`]
//! (a game's fatal screen), or, with no hook, the program stops on a panic.
//!
//! Only with the `residency-lock` cargo feature. Without it every function
//! here is an empty inline and the program is byte-identical: a debug build
//! turns it on, a shipping build does not.

/// The key [`check`] reports for a read of the pack table itself
/// ([`crate::cd::read_index`], a table scan before its chunk is known).
pub const TABLE_KEY: u32 = u32::MAX;

#[cfg(feature = "residency-lock")]
mod state {
    pub(crate) use cell::{culprit, hook, sealed, set_culprit, set_hook, set_sealed};
    // Guest: process-wide words. Host tests: per test thread, so the one test
    // that seals cannot fail the others running beside it.
    #[cfg(not(test))]
    mod cell {
        // No atomics on the R3000; one CPU, and no interrupt handler loads a chunk, so plain
        // words read and written in place are enough.
        static mut SEALED: bool = false;
        static mut CULPRIT: usize = usize::MAX;
        static mut HOOK: usize = 0;
        pub(crate) fn sealed() -> bool {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of!(SEALED).read_volatile() }
        }
        pub(crate) fn set_sealed(v: bool) {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of_mut!(SEALED).write_volatile(v) }
        }
        pub(crate) fn culprit() -> usize {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of!(CULPRIT).read_volatile() }
        }
        pub(crate) fn set_culprit(v: usize) {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of_mut!(CULPRIT).write_volatile(v) }
        }
        pub(crate) fn hook() -> usize {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of!(HOOK).read_volatile() }
        }
        pub(crate) fn set_hook(v: usize) {
            // SAFETY: one aligned word of this module's static, on the one thread.
            unsafe { core::ptr::addr_of_mut!(HOOK).write_volatile(v) }
        }
    }
    #[cfg(test)]
    mod cell {
        extern crate std;
        use core::cell::Cell;
        std::thread_local! {
            static SEALED: Cell<bool> = const { Cell::new(false) };
            static CULPRIT: Cell<usize> = const { Cell::new(usize::MAX) };
            static HOOK: Cell<usize> = const { Cell::new(0) };
        }
        pub(crate) fn sealed() -> bool {
            SEALED.get()
        }
        pub(crate) fn set_sealed(v: bool) {
            SEALED.set(v)
        }
        pub(crate) fn culprit() -> usize {
            CULPRIT.get()
        }
        pub(crate) fn set_culprit(v: usize) {
            CULPRIT.set(v)
        }
        pub(crate) fn hook() -> usize {
            HOOK.get()
        }
        pub(crate) fn set_hook(v: usize) {
            HOOK.set(v)
        }
    }
}

/// The level is ready: from now on any pack load is fatal.
#[inline(always)]
pub fn seal() {
    #[cfg(feature = "residency-lock")]
    state::set_sealed(true);
}

/// A load is about to start (level change, death reload): loads are allowed
/// again until the next [`seal`].
#[inline(always)]
pub fn unseal() {
    #[cfg(feature = "residency-lock")]
    state::set_sealed(false);
}

/// Whether the lock is sealed now (always `false` without the feature).
#[inline(always)]
#[must_use]
pub fn is_sealed() -> bool {
    #[cfg(feature = "residency-lock")]
    let sealed = state::sealed();
    #[cfg(not(feature = "residency-lock"))]
    let sealed = false;
    sealed
}

/// The key of the load that broke the seal, if one did.
#[inline(always)]
#[must_use]
pub fn culprit() -> Option<u32> {
    #[cfg(feature = "residency-lock")]
    let key = state::culprit();
    #[cfg(not(feature = "residency-lock"))]
    let key = usize::MAX;
    (key != usize::MAX).then_some(key as u32)
}

/// What runs on a violation, given the culprit's key; it must not return
/// (a game's fatal screen). Without one the violation panics.
#[inline(always)]
pub fn set_violation_hook(hook: Option<fn(u32) -> !>) {
    #[cfg(feature = "residency-lock")]
    state::set_hook(hook.map_or(0, |f| f as usize));
    #[cfg(not(feature = "residency-lock"))]
    let _ = hook;
}

/// Called by every loader before it touches the drive, with the chunk id it
/// loads ([`TABLE_KEY`] for a table read). Fatal while sealed.
#[inline(always)]
pub fn check(key: u32) {
    #[cfg(feature = "residency-lock")]
    if state::sealed() {
        violation(key);
    }
    #[cfg(not(feature = "residency-lock"))]
    let _ = key;
}

#[cfg(feature = "residency-lock")]
#[inline(never)]
#[cold]
fn violation(key: u32) -> ! {
    state::set_culprit(key as usize);
    let hook = state::hook();
    if hook != 0 {
        // SAFETY: the only non-zero value ever stored is a `fn(u32) -> !`
        // from `set_violation_hook`, as its address.
        let hook: fn(u32) -> ! = unsafe { core::mem::transmute::<usize, fn(u32) -> !>(hook) };
        hook(key);
    }
    panic!("residency lock: chunk {key:#x} loaded after the level was sealed");
}

#[cfg(all(test, feature = "residency-lock"))]
mod tests {
    use super::*;
    use crate::cd::test_drive::ImageDrive;
    use crate::cd::{scan_entry, SECTOR_WORDS, WORLD_PACK_DEFAULT_LBA};
    extern crate std;

    // One test: the lock is process-wide state.
    #[test]
    fn a_load_after_the_seal_is_fatal_and_names_its_chunk() {
        let pack = psx_iso::build_world_pack(&[(7, &[1u8; 10][..]), (9, &[2u8; 10][..])]);
        let mut drive = ImageDrive::new(pack, WORLD_PACK_DEFAULT_LBA);
        let mut scratch = [0u32; SECTOR_WORDS];
        assert!(scan_entry(&mut drive, WORLD_PACK_DEFAULT_LBA, 7, &mut scratch).is_some());
        seal();
        assert!(is_sealed());
        let reads = drive.runs.len();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan_entry(&mut drive, WORLD_PACK_DEFAULT_LBA, 9, &mut scratch)
        }));
        assert!(caught.is_err(), "a sealed load must not return");
        assert_eq!(culprit(), Some(9));
        assert_eq!(drive.runs.len(), reads, "the drive was not touched");
        unseal();
        assert!(scan_entry(&mut drive, WORLD_PACK_DEFAULT_LBA, 9, &mut scratch).is_some());
    }
}
