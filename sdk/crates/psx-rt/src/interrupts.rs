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

    # Apply one queued GP1 word exactly at the blank edge (deferred display
    # flip). Zero means no request; a display-start word is never zero.
    # Only apply once GPUSTAT bit 24 is set: the frame's work ends with
    # GP0(1Fh), which sets it when the GPU reaches it, i.e. when everything
    # before it has been drawn, and the game acknowledged the flag with
    # GP1(02h) when it kicked that work. Until then the word stays queued for
    # a later edge, so a flip never exposes a partial buffer and the CPU never
    # blocks on the GPU. Bit 28 is not a drawing-complete test: on silicon it
    # rises when the DMA has pushed the last packet, about one large
    # primitive before the drawing ends (hardware-tests v1.24 cases 219-226);
    # the v1.24 present probe flipped on bit 24 with 120 of 120 frames right.
    lui   $26, %hi(__psx_rt_pending_gp1)
    lw    $27, %lo(__psx_rt_pending_gp1)($26)
    nop
    beqz  $27, 2f
    nop
    lui   $26, 0x1f80
    lw    $26, 0x1814($26)
    nop
    srl   $26, $26, 24
    andi  $26, $26, 1
    beqz  $26, 2f
    nop
    lui   $26, %hi(__psx_rt_pending_gp1)
    sw    $zero, %lo(__psx_rt_pending_gp1)($26)
    lui   $26, 0x1f80
    sw    $27, 0x1814($26)

2:
    lui   $26, 0x1f80
    addiu $27, $zero, -2
    sw    $27, 0x1070($26)

1:
    mfc0  $27, $13
    nop
    andi  $27, $27, 0x007c
    beqz  $27, 2f
    nop

    # Preserve a compact unexpected-exception signature before deciding
    # whether this fault is recoverable. Immediate-abort panics compile to
    # BREAK, so Cause + EPC are the only reliable shipping diagnostics.
    lui   $26, %hi(__psx_rt_fault_cause)
    mfc0  $27, $13
    nop
    sw    $27, %lo(__psx_rt_fault_cause)($26)
    lui   $26, %hi(__psx_rt_fault_epc)
    mfc0  $27, $14
    nop
    sw    $27, %lo(__psx_rt_fault_epc)($26)

    lui   $26, %hi(__psx_rt_fault_count)
    lw    $27, %lo(__psx_rt_fault_count)($26)
    nop
    addiu $27, $27, 1
    sw    $27, %lo(__psx_rt_fault_count)($26)

    # Never advance EPC for fatal instruction-side faults. BREAK (ExcCode 9,
    # 0x24) marks panic=immediate-abort; IBE (ExcCode 6, 0x18) cannot become
    # executable by skipping one word. For AdEL (ExcCode 4, 0x10), halt only
    # when BadVAddr == EPC, which identifies a misaligned instruction fetch;
    # retain the historical skip-and-count policy for data-side AdEL.
    mfc0  $27, $13
    nop
    andi  $27, $27, 0x007c
    addiu $26, $zero, 0x0024
    beq   $27, $26, 3f
    nop
    addiu $26, $zero, 0x0018
    beq   $27, $26, 3f
    nop
    addiu $26, $zero, 0x0010
    bne   $27, $26, 4f
    nop
    mfc0  $26, $8
    mfc0  $27, $14
    nop
    beq   $26, $27, 3f
    nop

4:
    mfc0  $26, $14
    nop
    addiu $26, $26, 4
    jr    $26
    .word 0x42000010

    # Interrupt return: EPC, or EPC + 4 when the word at EPC is a GTE
    # command (top seven bits 0100101). An interrupt taken on a GTE command
    # lets the command run and still leaves EPC on it, so returning to EPC
    # runs it a second time (hardware-tests v1.24 on silicon: case 0xC9,
    # 38 of 38 such interrupts ran an RTPS twice; with this step, case 0xCB,
    # 61 skips, none doubled or lost). This is psx-spx's fix and the Sony
    # kernel's. With Cause.BD set EPC holds the branch, never a GTE command,
    # so a GTE command in a delay slot is not stepped over and does run twice:
    # psx-spx's answer is to keep GTE commands out of delay slots, and
    # tools/hazard_scan.py warns about any it finds. `interrupt_resume_pc`
    # below is the same decision in Rust, unit-tested on host.
2:
    mfc0  $26, $14
    nop
    lw    $27, 0($26)
    nop
    srl   $27, $27, 25
    xori  $27, $27, 0x0025
    bnez  $27, 5f
    nop
    lui   $27, %hi(__psx_rt_gte_skip_count)
    lw    $26, %lo(__psx_rt_gte_skip_count)($27)
    nop
    addiu $26, $26, 1
    sw    $26, %lo(__psx_rt_gte_skip_count)($27)
    mfc0  $26, $14
    nop
    addiu $26, $26, 4
5:
    jr    $26
    .word 0x42000010
3:
    b     3b
    nop
    .set reorder
    "#
);

/// Monotonic VBlank IRQ count.
#[no_mangle]
pub static mut __psx_rt_vblank_count: u32 = 0;

/// Count of exceptions that were NOT interrupts: bus errors, address
/// errors, reserved instructions.
///
/// The handler used to return straight to EPC for these, which
/// re-executes the faulting instruction and loops forever: a silent
/// freeze with no diagnostic, indistinguishable from a hung spin. It now
/// steps over the faulting instruction and counts it here, so a bad
/// access costs one wrong value instead of the whole program, and the
/// count says it happened. Fatal instruction-side faults are deliberately
/// different: BREAK is the shipping representation of
/// `panic=immediate-abort`, while IBE and instruction-side AdEL cannot be
/// repaired by skipping one word. The handler records Cause/EPC and halts for
/// all three instead of entering unreachable or non-executable code.
#[no_mangle]
pub static mut __psx_rt_fault_count: u32 = 0;

/// Interrupts the handler returned from at EPC + 4 because the word at EPC
/// was a GTE command (see [`interrupt_resume_pc`]).
#[no_mangle]
pub static mut __psx_rt_gte_skip_count: u32 = 0;

/// Raw COP0 Cause captured for the latest unexpected exception.
#[no_mangle]
pub static mut __psx_rt_fault_cause: u32 = 0;

/// COP0 EPC captured for the latest unexpected exception.
#[no_mangle]
pub static mut __psx_rt_fault_epc: u32 = 0;

/// One queued GP1 word the VBlank handler writes to the GPU at the first
/// blank edge on which GPUSTAT bit 24 is set, then clears. Zero = empty.
/// Written by [`queue_gp1_at_vblank`].
#[no_mangle]
pub static mut __psx_rt_pending_gp1: u32 = 0;

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

/// Unexpected exceptions survived so far. Non-zero means some access
/// faulted and was stepped over; the value it read or wrote is garbage.
#[inline]
pub fn fault_count() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_fault_count) }
}

/// Interrupts that landed on a GTE command, whose return the handler moved
/// to EPC + 4 so the command did not run twice.
#[inline]
pub fn gte_skip_count() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_gte_skip_count) }
}

/// True when `word` is a GTE command (a COP2 `cofun`: opcode 0x12 with bit
/// 25 set, so the top seven bits are `0100101`). psx-spx's test is
/// `(word & 0xFE00_0000) == 0x4A00_0000`. GTE register moves (`mfc2`,
/// `mtc2`, `cfc2`, `ctc2`), `lwc2`/`swc2` and the `bc2` branches are not
/// commands: an interrupt on them is taken before they run.
#[inline]
pub const fn is_gte_command(word: u32) -> bool {
    word >> 25 == 0x25
}

/// Where psx-rt's exception handler resumes after an interrupt: `epc`, or
/// `epc + 4` when `word_at_epc` is a GTE command.
///
/// On silicon an interrupt taken on a GTE command lets the command run and
/// still reports EPC at it, so resuming at EPC runs it twice
/// (hardware-tests v1.24, case 0xC9: 38 of 38; with this rule, case 0xCB:
/// 61 skips, none doubled or lost). The handler's assembly makes exactly
/// this decision.
///
/// With Cause.BD set, EPC is the branch whose delay slot was interrupted,
/// so `word_at_epc` is a branch and the result is `epc`: the branch and a
/// GTE command in its delay slot both run again. psx-spx documents that
/// the fix cannot cover delay slots; keep GTE commands out of them
/// (`tools/hazard_scan.py` warns about any in an image).
#[inline]
pub const fn interrupt_resume_pc(epc: u32, word_at_epc: u32) -> u32 {
    if is_gte_command(word_at_epc) {
        epc.wrapping_add(4)
    } else {
        epc
    }
}

/// Raw COP0 Cause captured for the latest unexpected exception.
#[inline]
pub fn fault_cause() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_fault_cause) }
}

/// COP0 EPC captured for the latest unexpected exception.
#[inline]
pub fn fault_epc() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_fault_epc) }
}

/// Current monotonic VBlank count.
#[inline]
pub fn vblank_count() -> u32 {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_vblank_count) }
}

/// Queue one GP1 word for the VBlank handler to apply at a blank edge
/// (deferred tear-free display flip). Overwrites any unapplied word.
///
/// The handler applies the word at the first VBlank edge on which GPUSTAT
/// bit 24 (the GPU's IRQ1 flag) is set, so the frame must signal its own
/// end:
///
/// 1. acknowledge the flag with GP1(02h) before kicking the frame's work
///    (`psx_gpu::arm_draw_done`), after the previous flip has landed;
/// 2. end that work with GP0(1Fh): as the last node of the DMA chain
///    (`psx_gpu::OrderingTable::end_with_draw_done`, or a one-word packet
///    in an ordered stream) or, after drawing through the ports,
///    `psx_gpu::signal_draw_done`;
/// 3. queue the display-start word here.
///
/// The flag stays set until the next acknowledge, so a word queued while
/// the GPU is idle after such a frame (a display enable, say) applies at
/// the next edge. A game that never sends GP0(1Fh) never flips: before this
/// handler tested bit 24 it tested GPUSTAT bit 28, which on silicon rises
/// about one large primitive before the drawing ends (hardware-tests v1.24
/// cases 219-226), so a flip could expose a frame one primitive short.
/// Keep interrupt source 1 (GPU) masked in `I_MASK`: this handler does not
/// acknowledge it.
#[cfg(target_arch = "mips")]
#[inline]
pub fn queue_gp1_at_vblank(word: u32) {
    unsafe { core::ptr::write_volatile(&raw mut __psx_rt_pending_gp1, word) }
}

/// Host no-op: no IRQ exists to consume the queue off-target.
#[cfg(not(target_arch = "mips"))]
#[inline]
pub fn queue_gp1_at_vblank(_word: u32) {}

/// True while a word queued by [`queue_gp1_at_vblank`] has not yet been
/// applied by the VBlank handler.
#[cfg(target_arch = "mips")]
#[inline]
pub fn gp1_queue_pending() -> bool {
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_pending_gp1) != 0 }
}

/// Empty the queue and hand back the word the handler has not applied, so
/// the caller can write it to GP1 itself. Returns 0 when nothing was queued.
///
/// The handler only applies its word at a blank edge on which GPUSTAT
/// bit 24 is set (see [`queue_gp1_at_vblank`]), so a long enough frame, or
/// one that never sends GP0(1Fh), leaves it queued indefinitely. A caller that has given up waiting must take the word
/// rather than leave it: the next [`queue_gp1_at_vblank`] overwrites the
/// slot, and a display start that never reaches the GPU desynchronises the
/// display side from the draw side for the rest of the session.
///
/// Racing the handler is harmless. If the IRQ lands between the read and
/// the clear, the handler applies the word and the caller writes the same
/// value again; GP1(05h) is idempotent.
#[cfg(target_arch = "mips")]
#[inline]
pub fn take_pending_gp1() -> u32 {
    unsafe {
        let word = core::ptr::read_volatile(&raw const __psx_rt_pending_gp1);
        if word != 0 {
            core::ptr::write_volatile(&raw mut __psx_rt_pending_gp1, 0);
        }
        word
    }
}

/// Host: nothing is ever queued, so nothing can be taken.
#[cfg(not(target_arch = "mips"))]
#[inline]
pub fn take_pending_gp1() -> u32 {
    0
}

/// Host no-op: nothing is ever pending off-target.
#[cfg(not(target_arch = "mips"))]
#[inline]
pub fn gp1_queue_pending() -> bool {
    false
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

/// True when the general exception vector jumps to psx-rt's handler, the
/// one that never touches `$sp` (so interrupts are safe on any stack).
#[cfg(target_arch = "mips")]
pub fn handler_installed() -> bool {
    let handler = __psx_rt_exception_handler as *const () as usize as u32;
    vector_word() == jump_word(handler)
}

/// The vector word of a game's exception handler declared with
/// [`declare_stack_safe_handler`]; zero when none is.
#[cfg(target_arch = "mips")]
static mut STACK_SAFE_HANDLER: u32 = 0;

/// Declare that the game's own exception handler at `handler` is safe to
/// take an interrupt on any stack, including a
/// [`ScratchpadStack`](crate::scratchpad::ScratchpadStack).
///
/// With the `scratchpad-stack-check` feature, a stack switch traps when
/// interrupts are on and the general exception vector jumps anywhere but
/// psx-rt's handler, because a handler that pushes onto the interrupted
/// stack would write below the scratchpad stack's frames. A game that
/// installs its own vector (hk-psx wraps psx-rt's handler to service CD
/// interrupts, then jumps to it) calls this once with the address it puts
/// in the vector; the check then accepts exactly that vector word, and
/// still traps any other handler, such as a BIOS vector restored behind the
/// game's back. Declaring does not install anything.
///
/// ```no_run
/// unsafe extern "C" fn game_exception_wrapper() {}
/// // SAFETY: game_exception_wrapper switches to its own stack before it
/// // stores anything and restores $sp before it hands back.
/// unsafe { psx_rt::interrupts::declare_stack_safe_handler(game_exception_wrapper) };
/// ```
///
/// # Safety
///
/// `handler`, and everything it calls or jumps to, must leave `$sp` and the
/// memory below it alone: it uses only `$k0`/`$k1` or saves state to memory
/// of its own and runs on a stack of its own, and it returns (or chains to
/// psx-rt's handler) with `$sp` unchanged.
#[cfg(target_arch = "mips")]
pub unsafe fn declare_stack_safe_handler(handler: unsafe extern "C" fn()) {
    // SAFETY: a plain store; nothing reads it from an interrupt.
    unsafe {
        core::ptr::write_volatile(
            &raw mut STACK_SAFE_HANDLER,
            jump_word(handler as *const () as usize as u32),
        )
    }
}

/// Declare a stack-safe exception handler. Host no-op: there are no
/// exception vectors off-target.
///
/// # Safety
///
/// As on the target.
#[cfg(not(target_arch = "mips"))]
pub unsafe fn declare_stack_safe_handler(_handler: unsafe extern "C" fn()) {}

/// True when the general exception vector jumps to psx-rt's handler or to
/// the one declared with [`declare_stack_safe_handler`].
#[cfg(target_arch = "mips")]
pub fn stack_safe_handler_installed() -> bool {
    // SAFETY: a plain read of a word only this module writes.
    let declared = unsafe { core::ptr::read_volatile(&raw const STACK_SAFE_HANDLER) };
    handler_installed() || (declared != 0 && vector_word() == declared)
}

/// The `j handler` word psx-rt writes into the vector.
#[cfg(target_arch = "mips")]
fn jump_word(handler: u32) -> u32 {
    0x0800_0000 | ((handler >> 2) & 0x03ff_ffff)
}

/// The first word of the general exception vector.
#[cfg(target_arch = "mips")]
fn vector_word() -> u32 {
    const EXCEPTION_VECTOR: *const u32 = 0x8000_0080 as *const u32;
    // SAFETY: a read of the kernel's vector word.
    unsafe { core::ptr::read_volatile(EXCEPTION_VECTOR) }
}

/// True when COP0 SR has interrupts enabled (IEc).
#[cfg(target_arch = "mips")]
pub fn cpu_interrupts_enabled() -> bool {
    let sr: u32;
    // The nop covers MFC0's load delay.
    unsafe { core::arch::asm!("mfc0 $8, $12", "nop", lateout("$8") sr, options(nomem, nostack)) };
    sr & 1 != 0
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const EPC: u32 = 0x8001_2340;

    /// GTE commands the SDK issues, as their encoded words.
    const RTPS: u32 = 0x4A18_0001;
    const RTPT: u32 = 0x4A28_0030;
    const NCLIP: u32 = 0x4B40_0006;
    const MVMVA: u32 = 0x4A48_6012;
    const AVSZ3: u32 = 0x4B58_002D;
    const GPF: u32 = 0x4B90_003D;

    #[test]
    fn an_interrupt_on_a_gte_command_resumes_after_it() {
        for word in [RTPS, RTPT, NCLIP, MVMVA, AVSZ3, GPF] {
            assert!(is_gte_command(word), "{word:#010x}");
            assert_eq!(interrupt_resume_pc(EPC, word), EPC + 4, "{word:#010x}");
        }
    }

    #[test]
    fn psx_spx_mask_and_seven_bit_test_agree() {
        // Every top byte, with the rest of the word set both ways.
        for top in 0u32..=0xFF {
            for low in [0, 0x00FF_FFFF] {
                let word = (top << 24) | low;
                assert_eq!(
                    is_gte_command(word),
                    word & 0xFE00_0000 == 0x4A00_0000,
                    "{word:#010x}"
                );
            }
        }
    }

    #[test]
    fn gte_register_moves_and_loads_resume_at_epc() {
        let not_commands = [
            0x480C_6000, // mfc2 $12, SXY0
            0x4889_6000, // mtc2 $9, SXY0
            0x4842_F800, // cfc2 $2, FLAG
            0x48C2_C000, // ctc2 $2, OFX
            0xC885_0000, // lwc2 $5, 0($4)
            0xE8AC_0000, // swc2 $12, 0($5)
            0x4900_0003, // bc2f
            0x4901_0003, // bc2t
            0x4080_6000, // mtc0 $0, SR
            0x0000_0000, // nop
            0x8C82_0000, // lw $2, 0($4)
            0x03E0_0008, // jr $ra
        ];
        for word in not_commands {
            assert!(!is_gte_command(word), "{word:#010x}");
            assert_eq!(interrupt_resume_pc(EPC, word), EPC, "{word:#010x}");
        }
    }

    #[test]
    fn a_gte_command_in_a_delay_slot_is_not_stepped_over() {
        // Cause.BD set: EPC names the branch, the RTPS sits at EPC + 4. The
        // handler reads the branch, so it resumes at EPC and the branch and
        // its RTPS both run again (psx-spx: the fix does not cover delay
        // slots). Stepping to EPC + 4 would drop the branch instead, and
        // EPC + 8 would drop the branch target; neither is safe, which is
        // why the answer is to keep GTE commands out of delay slots.
        let branches = [
            0x1509_FFF0, // bne $8, $9, back
            0x1000_0004, // b forward
            0x0C00_4000, // jal
            0x0800_4000, // j
            0x0100_F809, // jalr $8
            0x03E0_0008, // jr $ra
        ];
        for branch in branches {
            assert_eq!(interrupt_resume_pc(EPC, branch), EPC, "{branch:#010x}");
        }
    }

    #[test]
    fn the_resume_address_wraps_like_the_hardware_add() {
        assert_eq!(interrupt_resume_pc(0xFFFF_FFFC, RTPS), 0);
    }
}
