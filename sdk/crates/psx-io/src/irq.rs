//! Interrupt controller: pending and enabled sources.
//!
//! The register addresses and source bit positions live in
//! [`psx_hw::irq`].

use psx_hw::irq as reg;

/// Pending interrupt sources, one bit per [`psx_hw::irq::source`] position.
#[doc(alias = "I_STAT")]
#[inline(always)]
pub fn pending() -> u32 {
    // SAFETY: I_STAT (`psx_hw::irq::I_STAT`) is the interrupt controller's aligned 32-bit pending register on
    // every PS1; reading it has no side effects.
    unsafe { crate::read_u32(reg::I_STAT) }
}

/// Enabled interrupt sources (the mask register).
#[doc(alias = "I_MASK")]
#[inline(always)]
pub fn mask() -> u32 {
    // SAFETY: I_MASK (`psx_hw::irq::I_MASK`) is the interrupt controller's aligned 32-bit mask register on
    // every PS1; reading it has no side effects.
    unsafe { crate::read_u32(reg::I_MASK) }
}

/// Acknowledge pending bits by writing `!(bits)` -- the hardware
/// AND-accumulates, so any bit left 1 in the written value is
/// preserved, any bit that was 0 is cleared.
#[inline(always)]
pub fn acknowledge(bits: u32) {
    // SAFETY: an aligned 32-bit write to I_STAT (`psx_hw::irq::I_STAT`). The hardware ANDs the value in, so
    // it can only clear pending bits.
    unsafe { crate::write_u32(reg::I_STAT, !bits) }
}

/// Set the mask register (who can interrupt the CPU).
#[inline(always)]
pub fn set_mask(bits: u32) {
    // SAFETY: an aligned 32-bit write to I_MASK (`psx_hw::irq::I_MASK`). It only selects which sources raise
    // the CPU interrupt line and touches no memory.
    unsafe { crate::write_u32(reg::I_MASK, bits) }
}

/// Clear the CPU's interrupt-enable bit (COP0 `SR.IEc`) and report whether it
/// was set, so [`restore_cpu_interrupts`] can put it back. Sections nest.
///
/// This gates every interrupt at the CPU, whatever [`mask`] says. Use
/// [`without_interrupts`] unless a guard cannot be a closure.
///
/// # Safety
///
/// Pair every call with one [`restore_cpu_interrupts`] of its result, in
/// reverse order. Interrupts enabled early would let a handler run inside a
/// section whose code assumes it cannot.
#[inline(always)]
pub unsafe fn disable_cpu_interrupts() -> bool {
    #[cfg(target_arch = "mips")]
    {
        let sr: u32;
        // Read SR, clear IEc, write it back. The nop after MFC0 covers its
        // load delay. The two nops after MTC0 let the write settle before the
        // section's first instruction. An interrupt taken between the read and
        // the write returns through RFE with IEc as it was, and psx-rt's
        // handler leaves the rest of SR alone, so the write-back loses nothing.
        // SAFETY: COP0 SR read-modify-write of the interrupt-enable bit only.
        unsafe {
            core::arch::asm!(
                "mfc0 $8, $12",
                "nop",
                // Clear bit 0 (IEc) with a shift pair: no mask register.
                "srl $9, $8, 1",
                "sll $9, $9, 1",
                "mtc0 $9, $12",
                "nop",
                "nop",
                out("$8") sr,
                out("$9") _,
                options(nostack),
            );
        }
        sr & 1 != 0
    }
    // The host has no interrupts to mask.
    #[cfg(not(target_arch = "mips"))]
    false
}

/// Put back the interrupt-enable bit [`disable_cpu_interrupts`] found.
///
/// # Safety
///
/// `was_enabled` must be the result of the matching
/// [`disable_cpu_interrupts`], and every section entered after it must
/// already have ended.
#[inline(always)]
pub unsafe fn restore_cpu_interrupts(was_enabled: bool) {
    #[cfg(target_arch = "mips")]
    if was_enabled {
        // SAFETY: re-enables interrupts only when the matching disable found
        // them enabled.
        unsafe {
            core::arch::asm!(
                "mfc0 $8, $12",
                "nop",
                "ori $8, $8, 1",
                "mtc0 $8, $12",
                out("$8") _,
                options(nostack),
            );
        }
    }
    #[cfg(not(target_arch = "mips"))]
    let _ = was_enabled;
}

/// Run `f` with CPU interrupts masked, then restore the previous state.
///
/// For a read-modify-write of a register an interrupt handler also writes
/// (the DMA enable register, `DPCR`). Keep `f` short: VBlank waits until it
/// returns.
#[inline(always)]
pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: restored below, after `f` returns. A panic in `f` halts the
    // console, so the section is never left half-open with code running.
    let was_enabled = unsafe { disable_cpu_interrupts() };
    let result = f();
    // SAFETY: pairs the disable above; nested sections inside `f` have ended.
    unsafe { restore_cpu_interrupts(was_enabled) };
    result
}

/// Renamed to [`acknowledge`].
#[deprecated(note = "renamed to `acknowledge`")]
#[inline(always)]
pub fn ack(bits: u32) {
    acknowledge(bits)
}

/// Moved to [`psx_hw::irq::source`].
pub mod source {
    use psx_hw::irq::source as bit;

    /// Moved to [`psx_hw::irq::source::CDROM`].
    #[deprecated(note = "moved to `psx_hw::irq::source::CDROM`")]
    pub const CDROM: u32 = bit::CDROM;
    /// Moved to [`psx_hw::irq::source::DMA`].
    #[deprecated(note = "moved to `psx_hw::irq::source::DMA`")]
    pub const DMA: u32 = bit::DMA;
    /// Moved to [`psx_hw::irq::source::SPU`].
    #[deprecated(note = "moved to `psx_hw::irq::source::SPU`")]
    pub const SPU: u32 = bit::SPU;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_section_runs_its_closure_once_and_returns_its_value() {
        let mut runs = 0;
        let value = without_interrupts(|| {
            runs += 1;
            without_interrupts(|| 7)
        });
        assert_eq!((runs, value), (1, 7));
    }
}
