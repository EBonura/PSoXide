//! Instruction-cache invalidation, done directly on the hardware.
//!
//! The runtime does this itself rather than through the BIOS `A(44h)
//! FlushCache` service, so its boot path makes no BIOS call.
//!
//! The R3000A's instruction cache is 4 KiB, direct mapped, 256 lines of
//! 16 bytes. psx-spx ("Memory Control", FFFE0130h, and the COP0 status
//! register's IsC bit 16) describes how software reaches its tags: with
//! the cache control register in tag test mode and SR.IsC set, a word
//! store to an address in `0x000..0x1000` writes the tag of the line that
//! address maps to, instead of memory. Storing zero marks the line
//! invalid.
//!
//! The routine below:
//!
//! - runs from the KSEG1 (uncached) alias of its own code, so no
//!   instruction is fetched through the cache while it changes;
//! - clears SR before touching the cache control register, so no
//!   interrupt can run while the scratchpad is unmapped or the cache is
//!   isolated;
//! - selects `cache_control::FLUSH`, sets SR.IsC alone, and zeroes the
//!   tag of every line;
//! - clears SR.IsC before writing `cache_control::RUNNING` back, since
//!   whether a store to FFFE0130h reaches the port while the cache is
//!   isolated isn't documented;
//! - puts the caller's SR back and returns.

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

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_flush_i_cache();
}

/// Invalidate every line of the instruction cache.
///
/// Call it after writing instructions to RAM (an exception vector,
/// patched or loaded code) so the CPU fetches the new words. Interrupts
/// are off for the whole time the cache is being changed.
#[cfg(target_arch = "mips")]
#[inline(always)]
#[doc(alias = "FlushCache")]
pub fn flush_instruction_cache() {
    // SAFETY: the routine above clobbers only O32 caller-saved registers
    // ($t0-$t6), uses no stack, returns through $ra, and leaves SR and the
    // cache control register as it found them (SR) or in the running
    // setting (cache control). Its only stores land on isolated cache
    // tags or the cache control port, never on RAM.
    unsafe { __psx_rt_flush_i_cache() }
}

/// Host no-op, so shared code can call it unconditionally.
#[cfg(not(target_arch = "mips"))]
#[doc(alias = "FlushCache")]
pub fn flush_instruction_cache() {}

/// Renamed to [`flush_instruction_cache`].
#[deprecated(note = "renamed to `flush_instruction_cache`")]
#[inline(always)]
pub fn flush_i_cache() {
    flush_instruction_cache()
}
