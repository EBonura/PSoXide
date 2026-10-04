//! Critical sections: run code with CPU interrupts masked.
//!
//! On the PS1 a critical section clears COP0 SR.IEc for its duration and
//! puts the previous value back afterwards, so sections nest. psx-rt's own
//! interrupt handler is assembly that touches no Rust state, so the SDK does
//! not need one; games that install a Rust IRQ handler do, to share data
//! with it.
//!
//! The shape follows the `critical-section` crate: [`acquire`] and
//! [`release`] are the raw pair, [`with`] the scoped form, and [`Mutex`]
//! hands out a shared reference only while a [`CriticalSection`] token is
//! alive. With psx-rt's `critical-section` feature the same pair is also
//! registered as that crate's implementation, so `critical_section::with`
//! in any dependency masks interrupts here too.
//!
//! Host builds (tests, tools) serialise through a spin lock instead, so the
//! same code stays sound under a multi-threaded test runner.

use core::cell::UnsafeCell;
use core::marker::PhantomData;

/// What [`release`] needs to put back: whether interrupts were enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "pass it to release()"]
pub struct RestoreState(bool);

impl RestoreState {
    /// Whether the section this closes found interrupts enabled.
    #[inline(always)]
    pub fn was_enabled(self) -> bool {
        self.0
    }

    /// Rebuild a state saved as a `bool` (the `critical-section` crate's
    /// `restore-state-bool` representation).
    ///
    /// Passing it to [`release`] carries the same contract as the original.
    #[inline(always)]
    pub fn from_enabled(enabled: bool) -> Self {
        Self(enabled)
    }
}

/// Proof that the current code runs inside a critical section.
///
/// Only [`with`] (or `unsafe` [`CriticalSection::new`]) makes one; the
/// lifetime keeps it from escaping the section.
#[derive(Clone, Copy, Debug)]
pub struct CriticalSection<'cs> {
    _scope: PhantomData<&'cs ()>,
}

impl CriticalSection<'_> {
    /// Assert that a critical section is active.
    ///
    /// # Safety
    ///
    /// Interrupts must stay masked (or, on the host, the section lock held)
    /// for as long as the returned token, or anything borrowed through it,
    /// is alive.
    #[inline(always)]
    pub unsafe fn new() -> Self {
        Self {
            _scope: PhantomData,
        }
    }
}

/// Enter a critical section, returning the state to restore on exit.
///
/// # Safety
///
/// Each call must be paired with exactly one [`release`] of its result, in
/// reverse order of acquisition (sections nest like brackets).
#[inline(always)]
pub unsafe fn acquire() -> RestoreState {
    imp::acquire()
}

/// Leave a critical section entered by [`acquire`].
///
/// # Safety
///
/// `state` must come from the matching [`acquire`], and every section
/// entered after it must already have been released.
#[inline(always)]
pub unsafe fn release(state: RestoreState) {
    // SAFETY: forwarded contract.
    unsafe { imp::release(state) }
}

/// Run `f` with interrupts masked, then restore the previous state.
///
/// Keep `f` short: VBlank and every other interrupt wait until it returns.
#[inline(always)]
pub fn with<R>(f: impl FnOnce(CriticalSection<'_>) -> R) -> R {
    // SAFETY: released below, after `f` and the token it got are done. A
    // panic in `f` halts the console (and aborts the host test), so the
    // section is never left half-open with code still running.
    let state = unsafe { acquire() };
    // SAFETY: the section is active until `release` below.
    let result = f(unsafe { CriticalSection::new() });
    // SAFETY: pairs the acquire above; nested sections inside `f` have
    // already released.
    unsafe { release(state) };
    result
}

/// Data shared with interrupt code, reachable only inside a critical
/// section.
///
/// For mutation, wrap the value in a [`core::cell::Cell`] or
/// [`core::cell::RefCell`].
pub struct Mutex<T> {
    inner: UnsafeCell<T>,
}

// SAFETY: the only access is `borrow`, which needs a `CriticalSection`, so
// no two contexts reach `inner` at once. `T: Send` because the value may be
// touched from interrupt context as well as the main program.
unsafe impl<T: Send> Sync for Mutex<T> {}

impl<T> Mutex<T> {
    /// Wrap `value`.
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
        }
    }

    /// Borrow the value for as long as the critical section lasts.
    #[inline(always)]
    pub fn borrow<'cs>(&'cs self, _cs: CriticalSection<'cs>) -> &'cs T {
        // SAFETY: interrupts are masked while `_cs` lives, and every other
        // access also goes through a critical section.
        unsafe { &*self.inner.get() }
    }

    /// Mutable access through exclusive ownership, no section needed.
    #[inline(always)]
    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    /// Unwrap the value.
    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

#[cfg(target_arch = "mips")]
mod imp {
    use super::RestoreState;

    // The COP0 SR read-modify-write lives in psx-io, which needs it for its
    // own DPCR update; this is the same pair behind the critical-section API.
    #[inline(always)]
    pub(super) fn acquire() -> RestoreState {
        // SAFETY: `release` pairs it, per `super::acquire`'s contract.
        RestoreState(unsafe { psx_io::irq::disable_cpu_interrupts() })
    }

    #[inline(always)]
    pub(super) unsafe fn release(state: RestoreState) {
        // SAFETY: `state` came from the matching `acquire`.
        unsafe { psx_io::irq::restore_cpu_interrupts(state.0) }
    }
}

#[cfg(not(target_arch = "mips"))]
mod imp {
    extern crate std;

    use super::RestoreState;
    use core::cell::Cell;
    use core::sync::atomic::{AtomicBool, Ordering};

    // One global lock; a per-thread depth lets nested sections on the
    // owning thread pass while other threads wait.
    static LOCKED: AtomicBool = AtomicBool::new(false);
    std::thread_local! {
        static DEPTH: Cell<u32> = const { Cell::new(0) };
    }

    pub(super) fn acquire() -> RestoreState {
        if DEPTH.get() > 0 {
            DEPTH.set(DEPTH.get() + 1);
            return RestoreState(false);
        }
        while LOCKED
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        DEPTH.set(1);
        RestoreState(true)
    }

    pub(super) unsafe fn release(state: RestoreState) {
        DEPTH.set(DEPTH.get() - 1);
        if state.0 {
            LOCKED.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn sections_nest_and_mutex_borrows_inside_one() {
        static COUNT: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));
        with(|cs| {
            COUNT.borrow(cs).set(COUNT.borrow(cs).get() + 1);
            with(|inner| COUNT.borrow(inner).set(COUNT.borrow(inner).get() + 1));
        });
        assert_eq!(with(|cs| COUNT.borrow(cs).get()), 2);
    }
}
