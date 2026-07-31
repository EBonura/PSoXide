//! Minimal interrupt support used by engine-level clocks.
//!
//! The first consumer is a monotonic VBlank counter. We install a
//! tiny exception-vector trampoline that handles VBlank IRQs itself,
//! increments a volatile counter, acknowledges the IRQ, then returns
//! with `rfe`. The handler deliberately uses only the MIPS kernel
//! registers `$k0/$k1`, so it does not need a stack frame.

#[cfg(target_arch = "mips")]
use psx_io::irq;

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .section .text.psx_rt_exception
    .globl __psx_rt_exception_handler
__psx_rt_exception_handler:
    lui   $26, 0x1f80
    lw    $27, 0x1070($26)
    lw    $26, 0x1074($26)
    nop
    and   $27, $27, $26
    andi  $27, $27, 0x0001
    beqz  $27, 1f
    nop

    lui   $26, %hi(__psx_rt_vblank_count)
    lw    $27, %lo(__psx_rt_vblank_count)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(__psx_rt_vblank_count)($26)

    lui   $26, 0x1f80
    addiu $27, $zero, -2
    sw    $27, 0x1070($26)

1:
    mfc0  $26, $14
    nop
    jr    $26
    .word 0x42000010
    .set reorder
    "#
);

/// Monotonic VBlank IRQ count.
#[no_mangle]
pub static mut __psx_rt_vblank_count: u32 = 0;

/// Set once [`install_vblank_counter`] has run, so [`wait_vblank`] can
/// install lazily without resetting a counter the game is already using.
#[cfg(target_arch = "mips")]
static mut INSTALLED: bool = false;

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_exception_handler();
}

/// Install and enable the VBlank counter interrupt path.
///
/// This writes a branch into the MIPS general exception vector,
/// enables the VBlank source in `I_MASK`, and sets the COP0 interrupt
/// enable bits used by the R3000A. The operation is idempotent for
/// the current runtime: reinstalling simply resets the software
/// counter and refreshes the vector.
#[cfg(target_arch = "mips")]
pub fn install_vblank_counter() {
    const EXCEPTION_VECTOR: *mut u32 = 0x8000_0080 as *mut u32;
    const J_OPCODE: u32 = 0x0800_0000;

    unsafe {
        let handler = __psx_rt_exception_handler as *const () as usize as u32;
        core::ptr::write_volatile(EXCEPTION_VECTOR, J_OPCODE | ((handler >> 2) & 0x03ff_ffff));
        core::ptr::write_volatile(EXCEPTION_VECTOR.add(1), 0);
        crate::cache::flush_i_cache();

        core::ptr::write_volatile(&raw mut __psx_rt_vblank_count, 0);
        irq::ack(1 << irq::source::VBLANK);
        // This handler services VBlank only. After a BIOS disc boot, do not
        // preserve CD-ROM/DMA/etc. bits the BIOS may have left enabled.
        irq::set_mask(1 << irq::source::VBLANK);
        enable_cpu_interrupts();
        core::ptr::write_volatile(&raw mut INSTALLED, true);
    }
}

/// Install and enable the VBlank counter interrupt path.
#[cfg(not(target_arch = "mips"))]
pub fn install_vblank_counter() {}

/// Current monotonic VBlank count.
#[inline]
pub fn vblank_count() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_vblank_count) }
}

/// Block until the next VBlank IRQ.
///
/// This is the display-sync primitive: a frame that finishes early sleeps
/// until the blank, and a slow frame snaps to the next one, so presentation
/// quantizes to whole display periods. (`psx_gpu::vsync()` cannot do this:
/// it reconfigures Timer 1 on every call, and a mode write resets the
/// counter, so it busy-waits a fixed 242 HBlanks from the call site
/// instead of syncing to the display.)
///
/// Installs the VBlank counter on first use if the game has not already
/// called [`install_vblank_counter`].
#[cfg(target_arch = "mips")]
pub fn wait_vblank() {
    unsafe {
        if !core::ptr::read_volatile(&raw const INSTALLED) {
            install_vblank_counter();
        }
    }
    let v = vblank_count();
    while vblank_count() == v {}
}

/// Block until the next VBlank IRQ. Host no-op: the counter never
/// advances off-target, so waiting would hang.
#[cfg(not(target_arch = "mips"))]
pub fn wait_vblank() {}

#[cfg(target_arch = "mips")]
unsafe fn enable_cpu_interrupts() {
    const STATUS_IE: u32 = 1 << 0;
    const STATUS_IM2: u32 = 1 << 10;
    const STATUS_CU2: u32 = 1 << 30;

    let mut sr: u32;
    // MFC0 has a one-instruction load-delay hazard on the R3000: without the
    // nop the asm block hands back the STALE $8, and whatever garbage it held
    // gets OR'd into SR (seen in the wild as BEV set -> exceptions vectoring
    // into ROM -> pc walking off the end of the BIOS).
    unsafe { core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") sr) };
    sr |= STATUS_IE | STATUS_IM2 | STATUS_CU2;
    unsafe { core::arch::asm!("mtc0 $8, $12", in("$8") sr) };
}
