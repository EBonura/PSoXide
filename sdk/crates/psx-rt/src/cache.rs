//! Instruction-cache control, straight to the hardware.
//!
//! This replaces the BIOS `A(44h) FlushCache` service so the runtime
//! makes no BIOS call on its boot path. The sequence is the documented
//! R3000/PS1 i-cache invalidation recipe (nocash PSX-SPX, "Cache
//! Control" register `0xFFFE_0130`):
//!
//! 1. jump to the routine's KSEG1 alias so instruction fetches bypass
//!    the cache being invalidated,
//! 2. switch the cache-control port to tag-test mode with the i-cache
//!    enabled (`0x804`),
//! 3. isolate the cache via COP0 SR bit 16 (IsC) with interrupts
//!    disabled, so stores hit cache tags instead of memory,
//! 4. store once per 16-byte line across the 4 KiB cache, clearing
//!    every line's tag,
//! 5. drop isolation first (SR = 0, interrupts still off), THEN restore
//!    the normal cache-control value (`0x1E988`, the state a booted
//!    console runs with -- the same constant the BIOS restores), then
//!    the caller's SR. The order matches the BIOS routine exactly: with
//!    IsC still set, whether a store reaches the CPU-internal
//!    cache-control port or is swallowed by the isolated cache is
//!    undocumented, and a swallowed restore would leave the console in
//!    tag-test mode with the scratchpad unmapped.
//!
//! The emulator models isolated tag-test stores and the cache-control
//! port, so this path is exercised identically in emulation and on
//! silicon.

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .section .text.psx_rt_cache
    .globl __psx_rt_flush_i_cache
__psx_rt_flush_i_cache:
    la    $8, .Lpsx_rt_flush_body
    lui   $9, 0x2000
    or    $8, $8, $9
    jr    $8
    nop

.Lpsx_rt_flush_body:
    mfc0  $10, $12
    nop
    lui   $8, 0xfffe
    ori   $9, $zero, 0x0804
    sw    $9, 0x0130($8)
    lui   $9, 0x0001
    mtc0  $9, $12
    nop
    nop
    or    $9, $zero, $zero
    ori   $11, $zero, 0x1000
.Lpsx_rt_flush_line:
    sw    $zero, 0($9)
    addiu $9, $9, 0x0010
    bne   $9, $11, .Lpsx_rt_flush_line
    nop
    mtc0  $zero, $12
    nop
    nop
    lui   $9, 0x0001
    ori   $9, $9, 0xe988
    sw    $9, 0x0130($8)
    mtc0  $10, $12
    nop
    nop
    jr    $31
    nop
    .set reorder
    "#
);

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_flush_i_cache();
}

/// Invalidate the entire instruction cache.
///
/// Must be called after writing instructions to memory (patching the
/// exception vector, self-modifying code, dynamic loading). Interrupts
/// are disabled internally for the duration of the isolated sequence.
#[cfg(target_arch = "mips")]
#[inline(always)]
pub fn flush_i_cache() {
    unsafe { __psx_rt_flush_i_cache() }
}

/// Host no-op so shared code can call unconditionally.
#[cfg(not(target_arch = "mips"))]
pub fn flush_i_cache() {}
