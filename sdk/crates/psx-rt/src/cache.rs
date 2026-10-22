//! Instruction-cache invalidation, and the only code in the SDK that touches the cache
//! control register or COP0 Status.IsC.
//!
//! # The hazard this module exists for
//!
//! Invalidating the instruction cache means changing the cache control register
//! (`0xFFFE0130`) and isolating the cache (SR.IsC). On a console, doing either while a DMA
//! channel is moving data makes the CPU's next instruction fetch receive a word of the DMA's
//! traffic instead of the word from ROM or RAM: a GPU linked-list walk delivered a list word to
//! the instruction fetch and the program jumped into the weeds (Quake's flush with DMA2 live,
//! 2026-10-22: six of seven variants dead, the two that flushed with DMA idle or not at all
//! survived). The BIOS `A(44h) FlushCache` does the same writes and fails the same way. The
//! emulator cannot reproduce it, so the SDK makes it impossible to write down.
//!
//! # How the SDK enforces it
//!
//! 1. **Ownership.** The cache control register and SR.IsC are named in this file and nowhere
//!    else in the SDK (a host test scans every crate for them). There is no public constant, no
//!    raw accessor and no BIOS flush wrapper. `psx-hw` keeps the register's address as a hardware
//!    map entry for the emulator; guest code never uses it.
//! 2. **Proof token.** A flush is only reachable through a [`Quiesced`] token, which
//!    [`with_dma_quiesced`] hands to its closure after it has masked interrupts and waited
//!    (bounded, counted) for every DMA channel to go idle, re-checked inside the masked window.
//!    The token holds `&mut` of every DMA owner token ([`DmaOwners`]), so starting a DMA from
//!    the closure through any SDK driver does not compile. [`flush_instruction_cache`] and
//!    [`flush_instruction_cache_range`] are that API with nothing else in the closure.
//! 3. **Link time.** `cache-gate` (`tools/psoxide-hazard`) disassembles the final image and fails
//!    the build when code outside this module's routines writes `0xFFFE0130` or sets IsC.
//! 4. **Emulator.** A tripwire hard-fails a cache-control write that changes bits 13..16, 3 or 7
//!    while a DMA channel is busy.
//!
//! The wait cannot hang: every poll loop is bounded, and a bound that runs out is counted
//! (`psx_io::diag`, shown on the stop-panel) and the flush still happens, because stale
//! instructions are the worse failure and a channel wedged for that long is already a fault.
//!
//! # The flush itself
//!
//! The R3000A's instruction cache is 4 KiB, direct mapped, 256 lines of 16 bytes. psx-spx
//! ("Memory Control", FFFE0130h, and the COP0 status register's IsC bit 16) describes how
//! software reaches its tags: with the cache control register in tag test mode and SR.IsC set, a
//! word store to an address in `0x000..0x1000` writes the tag of the line that address maps to,
//! instead of memory. Storing zero marks the line invalid.
//!
//! The routines:
//!
//! - run from the KSEG1 (uncached) alias of their own code, so no instruction is fetched
//!   through the cache while it changes;
//! - clear SR before touching the cache control register, so no interrupt can run while the
//!   scratchpad is unmapped or the cache is isolated;
//! - select the flush setting, set SR.IsC alone, and zero the tag of every line;
//! - clear SR.IsC before writing the running setting back, since whether a store to the cache
//!   control register reaches the port while the cache is isolated isn't documented;
//! - put the caller's SR back and return.

use core::marker::PhantomData;
use psx_io::periph::{Cd, GpuDma, MdecDma, OrderingTableClearDma, SpuDma};

/// Polls of the DMA channels' control registers a wait makes before it gives up. A linked-list
/// walk of a heavy frame lasts a fraction of a display period; this is several periods.
const DMA_IDLE_SPINS: u32 = 1_000_000;

/// Times a flush lets interrupts back in and waits again because a VBlank kick started a walk
/// between the idle check and the mask. After that it waits inside the masked window.
const KICK_RETRIES: u32 = 4;

/// What the DMA gate saw on the way to a quiet bus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuiesceReport {
    /// A DMA channel was busy when the gate was entered.
    pub busy_at_entry: bool,
    /// Times a walk started between the idle check and the interrupt mask, so the gate backed
    /// out and waited again.
    pub retries: u32,
    /// Control-register polls that found a channel busy.
    pub spins: u32,
    /// A bound ran out: a channel was still busy when the gate gave up and went on.
    pub timed_out: bool,
}

/// The hardware the gate looks at; the host tests drive it with a script.
trait Bus {
    /// True while any DMA channel has its busy bit set.
    fn dma_busy(&mut self) -> bool;
    /// Mask interrupts; returns whether they were enabled.
    fn irq_off(&mut self) -> bool;
    /// Put the interrupt state back.
    fn irq_restore(&mut self, was_enabled: bool);
}

/// Poll until no channel is busy, at most `limit` busy polls. `true` when idle was seen.
fn spin_until_idle(bus: &mut impl Bus, limit: u32, spins: &mut u32) -> bool {
    let mut n = 0u32;
    while bus.dma_busy() {
        n += 1;
        *spins = spins.saturating_add(1);
        if n >= limit {
            return false;
        }
    }
    true
}

/// Run `run` with interrupts masked and every DMA channel idle.
///
/// Waits first with interrupts on (a walk takes up to a frame, and VBlank should keep being
/// served), then masks and checks again: the VBlank handler may have kicked a queued frame in
/// between. If it did, back out and wait again, a few times, then wait inside the window.
fn run_when_dma_idle<B: Bus, R>(
    bus: &mut B,
    limit: u32,
    run: impl FnOnce(QuiesceReport) -> R,
) -> (R, QuiesceReport) {
    let mut report = QuiesceReport {
        busy_at_entry: bus.dma_busy(),
        ..QuiesceReport::default()
    };
    let mut tries = 0u32;
    loop {
        let idle = spin_until_idle(bus, limit, &mut report.spins);
        let was = bus.irq_off();
        if !idle {
            // The bound ran out with interrupts on; do not wait twice as long again.
            report.timed_out = true;
            let r = run(report);
            bus.irq_restore(was);
            return (r, report);
        }
        if !bus.dma_busy() {
            let r = run(report);
            bus.irq_restore(was);
            return (r, report);
        }
        if tries >= KICK_RETRIES {
            report.timed_out = !spin_until_idle(bus, limit, &mut report.spins);
            let r = run(report);
            bus.irq_restore(was);
            return (r, report);
        }
        bus.irq_restore(was);
        tries += 1;
        report.retries += 1;
    }
}

/// The DMA owner tokens, borrowed for as long as the bus is held quiet.
///
/// Every SDK driver that starts a transfer takes one of these tokens by `&mut` (`psx_gpu::Gpu`
/// owns [`GpuDma`], `psx_spu::Spu` owns [`SpuDma`], and so on; see `psx_io::periph`). A
/// [`Quiesced`] holds all of them, so inside the closure of [`with_dma_quiesced`] none of those
/// drivers can be used.
pub struct DmaOwners<'a> {
    _gpu: &'a mut GpuDma,
    _otc: &'a mut OrderingTableClearDma,
    _mdec: &'a mut MdecDma,
    _spu: &'a mut SpuDma,
    _cd: &'a mut Cd,
}

impl<'a> DmaOwners<'a> {
    /// Borrow every DMA owner token. A driver that owns its token lends it through its own
    /// accessor (`Gpu::dma_mut`, `SectorReader::cd_mut`, ...); a program that kept its
    /// [`Peripherals`](crate::Peripherals) intact uses
    /// [`Peripherals::dma_owners`](crate::Peripherals::dma_owners).
    pub fn new(
        gpu: &'a mut GpuDma,
        otc: &'a mut OrderingTableClearDma,
        mdec: &'a mut MdecDma,
        spu: &'a mut SpuDma,
        cd: &'a mut Cd,
    ) -> Self {
        Self {
            _gpu: gpu,
            _otc: otc,
            _mdec: mdec,
            _spu: spu,
            _cd: cd,
        }
    }
}

/// Proof that interrupts are masked and no DMA channel is running, for as long as the borrow of
/// [`with_dma_quiesced`]'s closure lasts. It cannot be built any other way, and it owns the
/// right to change the cache.
pub struct Quiesced<'a> {
    _owners: DmaOwners<'a>,
    report: QuiesceReport,
    _not_send: PhantomData<*mut ()>,
}

impl Quiesced<'_> {
    /// What the gate saw on the way in.
    pub fn report(&self) -> QuiesceReport {
        self.report
    }

    /// Invalidate every line of the instruction cache.
    ///
    /// Call it after writing instructions to RAM (an exception vector, patched or loaded code)
    /// so the CPU fetches the new words.
    #[doc(alias = "FlushCache")]
    pub fn flush_icache(&mut self) {
        raw::flush_all();
    }

    /// Invalidate the instruction-cache lines that hold the bytes `[start, end)`.
    ///
    /// For a caller that rewrote a few words of code (a patched call site): the other lines, and
    /// the working set they hold, stay valid. A range of 4 KiB or more is the whole cache.
    pub fn flush_icache_range(&mut self, start: u32, end: u32) {
        if end.wrapping_sub(start) >= 0x1000 {
            raw::flush_all();
        } else {
            raw::flush_range(start, end);
        }
    }
}

/// Run `f` with interrupts masked and every DMA channel idle, and hand it the [`Quiesced`] token
/// that lets it change the instruction cache.
///
/// The gate waits for the channels with interrupts on, masks, checks again, and backs out and
/// waits if a queued frame was kicked meanwhile (see [`QuiesceReport`]). `owners` stays
/// borrowed until `f` returns, so the closure cannot start a transfer. Keep `f` short:
/// VBlank and every other interrupt wait until it returns.
///
/// ```
/// use psx_io::periph::{Cd, GpuDma, MdecDma, OrderingTableClearDma, SpuDma};
/// use psx_rt::cache::{with_dma_quiesced, DmaOwners};
///
/// // SAFETY: a doctest on the host; nothing else holds these tokens.
/// let (mut gpu, mut otc, mut mdec, mut spu, mut cd) = unsafe {
///     (GpuDma::steal(), OrderingTableClearDma::steal(), MdecDma::steal(), SpuDma::steal(), Cd::steal())
/// };
/// with_dma_quiesced(DmaOwners::new(&mut gpu, &mut otc, &mut mdec, &mut spu, &mut cd), |q| {
///     q.flush_icache();
/// });
/// ```
///
/// Starting a DMA inside the closure does not compile, because the token set is borrowed:
///
/// ```compile_fail,E0499
/// use psx_io::periph::{Cd, GpuDma, MdecDma, OrderingTableClearDma, SpuDma};
/// use psx_rt::cache::{with_dma_quiesced, DmaOwners};
///
/// // SAFETY: a doctest on the host; nothing else holds these tokens.
/// let (mut gpu, mut otc, mut mdec, mut spu, mut cd) = unsafe {
///     (GpuDma::steal(), OrderingTableClearDma::steal(), MdecDma::steal(), SpuDma::steal(), Cd::steal())
/// };
/// let mut table = [0u32; 16];
/// with_dma_quiesced(DmaOwners::new(&mut gpu, &mut otc, &mut mdec, &mut spu, &mut cd), |q| {
///     otc.clear_table(&mut table); // starts DMA 6 while the bus is held quiet
///     q.flush_icache();
/// });
/// ```
pub fn with_dma_quiesced<R>(owners: DmaOwners<'_>, f: impl FnOnce(&mut Quiesced<'_>) -> R) -> R {
    let mut bus = HwBus;
    let (r, report) = run_when_dma_idle(&mut bus, DMA_IDLE_SPINS, |report| {
        let mut quiesced = Quiesced {
            _owners: owners,
            report,
            _not_send: PhantomData,
        };
        diag_enter();
        f(&mut quiesced)
    });
    diag_exit(report);
    r
}

/// Invalidate every line of the instruction cache, once the DMA channels are idle.
///
/// [`with_dma_quiesced`] with a closure that only flushes, so no tokens are needed: the closure
/// starts no transfer, which is the whole of what the token set proves. Interrupts are off for
/// the whole time the cache is being changed.
#[inline(never)]
#[doc(alias = "FlushCache")]
pub fn flush_instruction_cache() {
    with_internal_owners(|q| q.flush_icache());
}

/// Invalidate the instruction-cache lines that hold the bytes `[start, end)`, once the DMA
/// channels are idle. A range of 4 KiB or more is the whole cache.
#[inline(never)]
pub fn flush_instruction_cache_range(start: u32, end: u32) {
    with_internal_owners(|q| q.flush_icache_range(start, end));
}

/// [`with_dma_quiesced`] for the SDK's own closures, which only flush.
fn with_internal_owners(f: impl FnOnce(&mut Quiesced<'_>)) {
    // SAFETY: the tokens are zero-sized and only borrowed by the `Quiesced` for the duration of
    // `f`, which is one of this module's flush calls and starts no transfer; nothing else runs
    // meanwhile (interrupts are masked and the program is single threaded).
    let (mut gpu, mut otc, mut mdec, mut spu, mut cd) = unsafe {
        (
            GpuDma::steal(),
            OrderingTableClearDma::steal(),
            MdecDma::steal(),
            SpuDma::steal(),
            Cd::steal(),
        )
    };
    with_dma_quiesced(
        DmaOwners::new(&mut gpu, &mut otc, &mut mdec, &mut spu, &mut cd),
        f,
    );
}

/// Renamed to [`flush_instruction_cache`].
#[deprecated(note = "renamed to `flush_instruction_cache`")]
#[inline(always)]
pub fn flush_i_cache() {
    flush_instruction_cache()
}

// ---------------------------------------------------------------- hardware

/// The value `psx_rt::diag` writes back to the cache control register when a fault is taken
/// inside a flush.
#[cfg(target_arch = "mips")]
#[allow(
    dead_code,
    reason = "only the stop-panel reads these two; the handler always uses the bit"
)]
pub(crate) const RUNNING_SETTING: u32 = psx_hw::memory::cache_control::RUNNING;

/// COP0 Status.IsC, which the exception handler clears first when a flush faults.
#[cfg(target_arch = "mips")]
#[allow(
    dead_code,
    reason = "only the stop-panel reads these two; the handler always uses the bit"
)]
pub(crate) const ISOLATE_CACHE_BIT: u32 = 1 << 16;

/// The cache control register's address, for the panel's read of it.
#[cfg(target_arch = "mips")]
#[allow(
    dead_code,
    reason = "only the stop-panel reads these two; the handler always uses the bit"
)]
pub(crate) const CONTROL_ADDRESS: u32 = psx_hw::memory::cache_control::ADDR;

#[cfg(target_arch = "mips")]
struct HwBus;

#[cfg(target_arch = "mips")]
impl Bus for HwBus {
    fn dma_busy(&mut self) -> bool {
        use psx_io::dma::{is_busy, Channel};
        is_busy(Channel::Gpu)
            || is_busy(Channel::Cd)
            || is_busy(Channel::Spu)
            || is_busy(Channel::MdecIn)
            || is_busy(Channel::MdecOut)
            || is_busy(Channel::OrderingTableClear)
            || is_busy(Channel::Expansion)
    }

    fn irq_off(&mut self) -> bool {
        // SAFETY: paired with `irq_restore` in `run_when_dma_idle`, in reverse order.
        unsafe { crate::critical_section::acquire() }.was_enabled()
    }

    fn irq_restore(&mut self, was_enabled: bool) {
        // SAFETY: `was_enabled` came from the matching `irq_off`.
        unsafe {
            crate::critical_section::release(crate::critical_section::RestoreState::from_enabled(
                was_enabled,
            ));
        }
    }
}

/// The host has no DMA and no cache: the gate is a lock and a no-op.
#[cfg(not(target_arch = "mips"))]
struct HwBus;

#[cfg(not(target_arch = "mips"))]
impl Bus for HwBus {
    fn dma_busy(&mut self) -> bool {
        false
    }

    fn irq_off(&mut self) -> bool {
        // SAFETY: paired with `irq_restore`.
        unsafe { crate::critical_section::acquire() }.was_enabled()
    }

    fn irq_restore(&mut self, was_enabled: bool) {
        // SAFETY: `was_enabled` came from the matching `irq_off`.
        unsafe {
            crate::critical_section::release(crate::critical_section::RestoreState::from_enabled(
                was_enabled,
            ));
        }
    }
}

/// Console-diag: record what the flush found, inside the window.
#[inline(always)]
fn diag_enter() {
    #[cfg(all(feature = "console-diag", target_arch = "mips"))]
    {
        use psx_io::dma::{control, Channel};
        let chcr = [
            control(Channel::MdecIn),
            control(Channel::MdecOut),
            control(Channel::Gpu),
            control(Channel::Cd),
            control(Channel::Spu),
            control(Channel::Expansion),
            control(Channel::OrderingTableClear),
        ];
        let sr: u32;
        // SAFETY: reads COP0 Status; the `nop` covers the MFC0 load delay.
        unsafe {
            core::arch::asm!("mfc0 {t}, $12", "nop", t = out(reg) sr, options(nostack, nomem));
        }
        // SAFETY: an aligned read of the cache control register, which has no side effect.
        let cc = unsafe { psx_io::read_u32(CONTROL_ADDRESS) };
        psx_io::diag::flush_enter(chcr, psx_io::gpu::status().bits(), sr, cc);
    }
}

/// Console-diag: count the wait.
#[inline(always)]
fn diag_exit(report: QuiesceReport) {
    psx_io::diag::flush_exit(
        report.busy_at_entry,
        report.retries,
        report.spins,
        report.timed_out,
    );
}

/// The routines that change the cache control register and SR.IsC. Private, and unsafe to call
/// outside the proof of a [`Quiesced`].
mod raw {
    #[cfg(target_arch = "mips")]
    use psx_hw::memory::cache_control;

    // O32: uses only $t0-$t6 (caller saved), never the stack, returns
    // through $ra. SR is COP0 register 12.
    #[cfg(target_arch = "mips")]
    core::arch::global_asm!(
        r#"
    .set noreorder
    .section .text.__psx_rt_flush_i_cache,"ax",@progbits
    .globl __psx_rt_flush_i_cache
    .type __psx_rt_flush_i_cache,@function
    .p2align 2
__psx_rt_flush_i_cache:
    lui   $t0, %hi(.Lpsx_rt_flush_uncached)
    addiu $t0, $t0, %lo(.Lpsx_rt_flush_uncached)
    lui   $t1, 0xA000
    or    $t0, $t0, $t1
    jr    $t0
    nop
.Lpsx_rt_flush_uncached:
    mfc0  $t2, $12
    lui   $t3, {port_hi}
    mtc0  $zero, $12
    lui   $t4, {flush_hi}
    ori   $t4, $t4, {flush_lo}
    sw    $t4, {port_lo}($t3)
    lui   $t4, 0x0001
    mtc0  $t4, $12
    nop
    nop
    addu  $t5, $zero, $zero
    li    $t6, 0x1000
.Lpsx_rt_flush_line:
    addiu $t5, $t5, 16
    bne   $t5, $t6, .Lpsx_rt_flush_line
    sw    $zero, -16($t5)
    mtc0  $zero, $12
    nop
    nop
    lui   $t4, {running_hi}
    ori   $t4, $t4, {running_lo}
    sw    $t4, {port_lo}($t3)
    mtc0  $t2, $12
    nop
    jr    $ra
    nop
    .size __psx_rt_flush_i_cache, . - __psx_rt_flush_i_cache
    .set reorder
    "#,
        port_hi = const cache_control::ADDR >> 16,
        port_lo = const cache_control::ADDR & 0xFFFF,
        flush_hi = const cache_control::FLUSH >> 16,
        flush_lo = const cache_control::FLUSH & 0xFFFF,
        running_hi = const cache_control::RUNNING >> 16,
        running_lo = const cache_control::RUNNING & 0xFFFF,
    );

    // Same routine over a byte range: invalidates only the lines that map the addresses
    // `[start, end)` (`$a0`, `$a1`; at most 4 KiB are visited, the cache's size). A caller that
    // changed a few words (one patched call site) pays for their lines instead of the whole cache,
    // and keeps the rest of the working set warm. Uses `$t0-$t7`, no stack.
    #[cfg(target_arch = "mips")]
    core::arch::global_asm!(
        r#"
    .set noreorder
    .section .text.__psx_rt_flush_i_cache_range,"ax",@progbits
    .globl __psx_rt_flush_i_cache_range
    .type __psx_rt_flush_i_cache_range,@function
    .p2align 2
__psx_rt_flush_i_cache_range:
    lui   $t0, %hi(.Lpsx_rt_flush_range_uncached)
    addiu $t0, $t0, %lo(.Lpsx_rt_flush_range_uncached)
    lui   $t1, 0xA000
    or    $t0, $t0, $t1
    jr    $t0
    nop
.Lpsx_rt_flush_range_uncached:
    mfc0  $t2, $12
    lui   $t3, {port_hi}
    mtc0  $zero, $12
    lui   $t4, {flush_hi}
    ori   $t4, $t4, {flush_lo}
    sw    $t4, {port_lo}($t3)
    lui   $t4, 0x0001
    mtc0  $t4, $12
    andi  $t5, $a0, 0x0FF0
    addiu $t6, $a1, -1
    srl   $t6, $t6, 4
    srl   $t7, $a0, 4
    subu  $t6, $t6, $t7
    addiu $t6, $t6, 1
.Lpsx_rt_flush_range_line:
    sw    $zero, 0($t5)
    addiu $t5, $t5, 16
    addiu $t6, $t6, -1
    bnez  $t6, .Lpsx_rt_flush_range_line
    andi  $t5, $t5, 0x0FFF
    mtc0  $zero, $12
    nop
    nop
    lui   $t4, {running_hi}
    ori   $t4, $t4, {running_lo}
    sw    $t4, {port_lo}($t3)
    mtc0  $t2, $12
    nop
    jr    $ra
    nop
    .size __psx_rt_flush_i_cache_range, . - __psx_rt_flush_i_cache_range
    .set reorder
    "#,
        port_hi = const cache_control::ADDR >> 16,
        port_lo = const cache_control::ADDR & 0xFFFF,
        flush_hi = const cache_control::FLUSH >> 16,
        flush_lo = const cache_control::FLUSH & 0xFFFF,
        running_hi = const cache_control::RUNNING >> 16,
        running_lo = const cache_control::RUNNING & 0xFFFF,
    );

    #[cfg(target_arch = "mips")]
    extern "C" {
        fn __psx_rt_flush_i_cache_range(start: u32, end: u32);
        fn __psx_rt_flush_i_cache();
    }

    /// Invalidate the whole instruction cache.
    ///
    /// Only [`super::Quiesced`] calls this: interrupts are masked and no DMA runs.
    #[cfg(target_arch = "mips")]
    #[inline(always)]
    pub(super) fn flush_all() {
        // SAFETY: the routine clobbers only O32 caller-saved registers ($t0-$t6), uses no
        // stack, returns through $ra, and leaves SR as it found it and the cache control register
        // in the running setting. Its only stores land on isolated cache tags or the cache control
        // port, never on RAM. The caller holds a `Quiesced`, so no DMA is moving data.
        unsafe { __psx_rt_flush_i_cache() }
    }

    /// Invalidate the lines of `[start, end)`, a range under 4 KiB.
    ///
    /// Only [`super::Quiesced`] calls this.
    #[cfg(target_arch = "mips")]
    #[inline(always)]
    pub(super) fn flush_range(start: u32, end: u32) {
        // SAFETY: as `flush_all`; it clobbers $t0-$t7.
        unsafe { __psx_rt_flush_i_cache_range(start, end) }
    }

    /// Host no-op, so shared code can call it unconditionally.
    #[cfg(not(target_arch = "mips"))]
    pub(super) fn flush_all() {}

    /// Host no-op.
    #[cfg(not(target_arch = "mips"))]
    pub(super) fn flush_range(_start: u32, _end: u32) {}
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    /// A scripted bus. `busy_polls` busy reads remain; each mask can start a new walk
    /// (`kicks_on_mask` times, `kick_len` busy reads each); `wedged` never goes idle.
    struct Script {
        busy_polls: u32,
        kicks_on_mask: u32,
        kick_len: u32,
        wedged: bool,
        enabled: bool,
        masked_polls: u32,
        unmasked_polls: u32,
        polls: u32,
    }

    impl Script {
        fn idle() -> Self {
            Self {
                busy_polls: 0,
                kicks_on_mask: 0,
                kick_len: 3,
                wedged: false,
                enabled: true,
                masked_polls: 0,
                unmasked_polls: 0,
                polls: 0,
            }
        }
    }

    impl Bus for Script {
        fn dma_busy(&mut self) -> bool {
            self.polls += 1;
            if self.enabled {
                self.unmasked_polls += 1;
            } else {
                self.masked_polls += 1;
            }
            if self.wedged {
                return true;
            }
            if self.busy_polls > 0 {
                self.busy_polls -= 1;
                return true;
            }
            false
        }

        fn irq_off(&mut self) -> bool {
            let was = self.enabled;
            self.enabled = false;
            if self.kicks_on_mask > 0 {
                self.kicks_on_mask -= 1;
                self.busy_polls = self.kick_len;
            }
            was
        }

        fn irq_restore(&mut self, was_enabled: bool) {
            self.enabled = was_enabled;
        }
    }

    /// The script behind a `RefCell`, so the closure the gate runs can look at the bus at the
    /// moment it is called.
    struct Shared<'a>(&'a core::cell::RefCell<Script>);

    impl Bus for Shared<'_> {
        fn dma_busy(&mut self) -> bool {
            self.0.borrow_mut().dma_busy()
        }
        fn irq_off(&mut self) -> bool {
            self.0.borrow_mut().irq_off()
        }
        fn irq_restore(&mut self, was_enabled: bool) {
            self.0.borrow_mut().irq_restore(was_enabled)
        }
    }

    /// Run the gate. When the closure runs, interrupts must be masked and, unless the channel is
    /// wedged, no walk may be running: the two conditions a flush needs.
    fn gate(bus: Script, limit: u32) -> (u32, QuiesceReport, Script) {
        let bus = core::cell::RefCell::new(bus);
        let runs = core::cell::Cell::new(0u32);
        let (_, report) = run_when_dma_idle(&mut Shared(&bus), limit, |r| {
            let b = bus.borrow();
            assert!(!b.enabled, "the flush must run with interrupts masked");
            assert!(
                r.timed_out || b.busy_polls == 0,
                "the flush must not run while a channel is busy"
            );
            runs.set(runs.get() + 1);
        });
        (runs.get(), report, bus.into_inner())
    }

    #[test]
    fn an_idle_bus_flushes_once_with_interrupts_masked() {
        let (runs, report, bus) = gate(Script::idle(), 100);
        assert_eq!(runs, 1);
        assert_eq!(report, QuiesceReport::default());
        assert!(bus.enabled, "interrupts are back on afterwards");
    }

    #[test]
    fn a_walk_is_waited_out_with_interrupts_on() {
        let mut bus = Script::idle();
        bus.busy_polls = 7;
        let (runs, report, bus) = gate(bus, 100);
        assert_eq!(runs, 1);
        assert!(report.busy_at_entry);
        assert_eq!(
            report.spins, 6,
            "the entry probe saw 1 of 7 busy reads, the wait the other 6"
        );
        assert!(!report.timed_out);
        assert_eq!(report.retries, 0);
        assert_eq!(bus.masked_polls, 1, "inside the window it only re-checks");
    }

    #[test]
    fn a_walk_kicked_between_the_check_and_the_mask_is_waited_out_again() {
        let mut bus = Script::idle();
        bus.kicks_on_mask = 1;
        let (runs, report, bus) = gate(bus, 100);
        assert_eq!(runs, 1);
        assert_eq!(report.retries, 1);
        assert!(!report.timed_out);
        assert_eq!(bus.busy_polls, 0);
        assert!(bus.enabled);
    }

    #[test]
    fn kicks_that_keep_coming_end_in_a_wait_inside_the_window() {
        let mut bus = Script::idle();
        bus.kicks_on_mask = 1000;
        let (runs, report, bus) = gate(bus, 100);
        assert_eq!(runs, 1);
        assert_eq!(report.retries, KICK_RETRIES);
        assert!(!report.timed_out, "the walk ended inside the window");
        assert_eq!(bus.busy_polls, 0);
        assert!(bus.masked_polls > 1);
    }

    #[test]
    fn a_wedged_channel_is_bounded_counted_and_the_flush_still_runs() {
        let mut bus = Script::idle();
        bus.wedged = true;
        let (runs, report, bus) = gate(bus, 50);
        assert_eq!(runs, 1);
        assert!(report.timed_out);
        assert!(report.spins <= 50);
        assert!(bus.polls <= 50 + 3, "no unbounded polling: {}", bus.polls);
        assert!(bus.enabled);
    }

    #[test]
    fn a_wedged_channel_behind_endless_kicks_is_still_bounded() {
        let mut bus = Script::idle();
        bus.kicks_on_mask = u32::MAX;
        bus.kick_len = u32::MAX; // each kick is a walk that never ends
        let (runs, report, bus) = gate(bus, 20);
        assert_eq!(runs, 1);
        assert!(report.timed_out);
        assert!(bus.polls < 20 * (KICK_RETRIES + 3), "polls: {}", bus.polls);
    }

    #[test]
    fn interrupt_state_is_restored_exactly() {
        let mut bus = Script::idle();
        bus.enabled = false;
        bus.busy_polls = 3;
        let (runs, _, bus) = gate(bus, 100);
        assert_eq!(runs, 1);
        assert!(
            !bus.enabled,
            "a caller that had interrupts off keeps them off"
        );
    }

    #[test]
    fn the_public_api_runs_on_the_host() {
        flush_instruction_cache();
        flush_instruction_cache_range(0x8001_0000, 0x8001_0010);
        flush_instruction_cache_range(0x8001_0000, 0x8001_2000);
    }

    /// The cache control register and SR.IsC are named in this file only (psx-hw keeps the
    /// register's address for the emulator). A new use anywhere else in the SDK fails here, and
    /// in the image at link time (`cache-gate`).
    #[test]
    fn nothing_else_in_the_sdk_names_the_cache_control_register_or_isc() {
        fn visit(dir: &std::path::Path, hits: &mut Vec<std::string::String>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    visit(&path, hits);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let is_this_file = path.ends_with("psx-rt/src/cache.rs");
                    for needle in ["cache_control", "FFFE0130", "FFFE_0130", "IsC"] {
                        if text.contains(needle) && !is_this_file {
                            hits.push(std::format!("{}: {needle}", path.display()));
                        }
                    }
                }
            }
        }
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut hits = Vec::new();
        visit(&crates, &mut hits);
        assert!(hits.is_empty(), "outside psx-rt/src/cache.rs: {hits:#?}");
    }
}
