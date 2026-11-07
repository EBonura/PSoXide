//! Device owners for probes that drive hardware without owning it.
//!
//! A probe pokes a device whatever the program holds: the CD drive while a
//! stream is mid-sector, DMA channel 6 with no ordering table around, the
//! controller port under a memory card. The SDK's drivers take their owner
//! token so a game cannot reach one device from two places; a probe takes the
//! token with `steal`, as the free forwarders it replaces did on every call.

use psx_io::periph::{Cd, ControllerPort, OrderingTableClearDma};

/// The CD drive, for one command or register step.
#[inline(always)]
pub fn cd() -> Cd {
    // SAFETY: a token is a logic guard, not a memory-safety one (see
    // `psx_io::periph`); a probe takes the drive whenever it likes.
    unsafe { Cd::steal() }
}

/// Clear an ordering table over DMA channel 6.
#[inline(always)]
pub fn clear_ordering_table(buf: &mut [u32]) -> bool {
    // SAFETY: as for [`cd`].
    unsafe { OrderingTableClearDma::steal() }.clear_table(buf)
}

/// The memory-card diagnostic on the controller port.
#[inline(always)]
pub fn memcard_diagnostic() -> hello_memcard_recovery::Diagnostic {
    // SAFETY: as for [`cd`].
    hello_memcard_recovery::Diagnostic::new_on(unsafe { ControllerPort::steal() })
}
