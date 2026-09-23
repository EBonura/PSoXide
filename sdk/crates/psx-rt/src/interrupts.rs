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

    # Step over the faulting instruction. With Cause.BD clear it is at EPC,
    # so resume at EPC + 4. With BD set it sat in the delay slot of the
    # branch at EPC, which has already run: resume where that branch sends
    # control (6: below). EPC + 4 would re-run the faulting instruction as
    # if the branch had fallen through. `fault_resume_pc` below is the same
    # decision in Rust, unit-tested on host.
4:
    mfc0  $27, $13
    mfc0  $26, $14
    nop
    bltz  $27, 6f
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

    # Fault in a delay slot: evaluate the branch at EPC. $26 = EPC. Save the
    # GPRs to __psx_rt_fault_regs (slot n = $n) so the branch's registers can
    # be read by index, use $8-$15 as scratch, restore those eight. $k0/$k1
    # slots are not written: this handler owns both, so no branch can test
    # them. A branch this cannot evaluate (a coprocessor BCzF/BCzT) halts.
6:
    .set  noat
    lui   $27, %hi(__psx_rt_fault_regs)
    addiu $27, $27, %lo(__psx_rt_fault_regs)
    sw    $0, 0($27)
    sw    $1, 4($27)
    sw    $2, 8($27)
    sw    $3, 12($27)
    sw    $4, 16($27)
    sw    $5, 20($27)
    sw    $6, 24($27)
    sw    $7, 28($27)
    sw    $8, 32($27)
    sw    $9, 36($27)
    sw    $10, 40($27)
    sw    $11, 44($27)
    sw    $12, 48($27)
    sw    $13, 52($27)
    sw    $14, 56($27)
    sw    $15, 60($27)
    sw    $16, 64($27)
    sw    $17, 68($27)
    sw    $18, 72($27)
    sw    $19, 76($27)
    sw    $20, 80($27)
    sw    $21, 84($27)
    sw    $22, 88($27)
    sw    $23, 92($27)
    sw    $24, 96($27)
    sw    $25, 100($27)
    sw    $28, 112($27)
    sw    $29, 116($27)
    sw    $30, 120($27)
    sw    $31, 124($27)
    .set  at

    # $8 = branch word, $11 = value of rs, $12 = value of rt,
    # $13 = EPC + 8 (not taken), $9 = EPC + 4 + (simm16 << 2) (taken),
    # $10 = major opcode.
    lw    $8, 0($26)
    nop
    srl   $11, $8, 19
    andi  $11, $11, 0x7c
    addu  $11, $11, $27
    lw    $11, 0($11)
    srl   $12, $8, 14
    andi  $12, $12, 0x7c
    addu  $12, $12, $27
    lw    $12, 0($12)
    addiu $13, $26, 8
    srl   $10, $8, 26
    sll   $9, $8, 16
    sra   $9, $9, 14
    addu  $9, $9, $26
    addiu $9, $9, 4

    beqz  $10, 10f
    addiu $14, $zero, 1
    beq   $10, $14, 11f
    addiu $14, $zero, 2
    beq   $10, $14, 12f
    addiu $14, $zero, 3
    beq   $10, $14, 12f
    addiu $14, $zero, 4
    beq   $10, $14, 14f
    addiu $14, $zero, 5
    beq   $10, $14, 15f
    addiu $14, $zero, 6
    beq   $10, $14, 16f
    addiu $14, $zero, 7
    beq   $10, $14, 17f
    nop
    b     3b
    nop

    # SPECIAL: jr (funct 8) and jalr (funct 9) go to rs.
10:
    andi  $14, $8, 0x3e
    xori  $14, $14, 0x08
    bnez  $14, 3b
    nop
    b     20f
    move  $13, $11

    # REGIMM: bltz/bltzal (rt bit 0 clear) are taken when rs < 0,
    # bgez/bgezal (set) when rs >= 0: taken when the sign bit differs from
    # rt bit 0.
11:
    srl   $14, $11, 31
    srl   $15, $8, 16
    andi  $15, $15, 1
    bne   $14, $15, 18f
    nop
    b     20f
    nop

    # j / jal: the 256 MB region of the delay slot, index << 2.
12:
    addiu $14, $26, 4
    lui   $15, 0xf000
    and   $14, $14, $15
    sll   $15, $8, 6
    srl   $15, $15, 4
    b     20f
    or    $13, $14, $15

14:
    beq   $11, $12, 18f
    nop
    b     20f
    nop
15:
    bne   $11, $12, 18f
    nop
    b     20f
    nop
16:
    blez  $11, 18f
    nop
    b     20f
    nop
17:
    bgtz  $11, 18f
    nop
    b     20f
    nop

18:
    move  $13, $9
20:
    move  $26, $13
    lw    $8, 32($27)
    lw    $9, 36($27)
    lw    $10, 40($27)
    lw    $11, 44($27)
    lw    $12, 48($27)
    lw    $13, 52($27)
    lw    $14, 56($27)
    lw    $15, 60($27)
    nop
    jr    $26
    .word 0x42000010
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
/// steps over the faulting instruction (in a delay slot, to wherever its
/// branch sends control: see [`fault_resume_pc`]) and counts it here, so a
/// bad access costs one wrong value instead of the whole program, and the
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

/// GPRs at the latest fault taken in a branch delay slot (slot n holds `$n`;
/// the `$k0`/`$k1` slots are never written). The handler saves them here to
/// evaluate the branch at EPC, see [`fault_resume_pc`].
#[no_mangle]
pub static mut __psx_rt_fault_regs: [u32; 32] = [0; 32];

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

/// COP0 Cause.BD: the exception was taken in the delay slot of the branch
/// at EPC.
pub const CAUSE_BD: u32 = 1 << 31;

/// Where the branch or jump `word` at `pc` sends control once its delay
/// slot has run: its target when taken, `pc + 8` when not. `None` when
/// `word` is not a branch this can evaluate (a coprocessor `BCzF`/`BCzT`,
/// or not a branch at all).
///
/// `regs[n]` is `$n` at the exception; `$zero` reads as zero whatever
/// `regs[0]` holds. The registers are read after the branch has run, which
/// gives the value it tested except in two cases the R3000 leaves
/// unpredictable or compilers never emit: a `bltzal`/`bgezal`/`jalr` whose
/// link register is also a source, and a branch that sits in the load delay
/// of a load into one of its sources (`tools/hazard_scan.py` flags those).
/// The REGIMM decode is the R3000's: `rt` bit 0 picks `bgez` over `bltz`,
/// every other `rt` bit only chooses whether to link.
pub const fn branch_resume_pc(pc: u32, word: u32, regs: &[u32; 32]) -> Option<u32> {
    let rs_index = ((word >> 21) & 31) as usize;
    let rt_index = ((word >> 16) & 31) as usize;
    let rs = if rs_index == 0 { 0 } else { regs[rs_index] };
    let rt = if rt_index == 0 { 0 } else { regs[rt_index] };
    let not_taken = pc.wrapping_add(8);
    let taken = pc
        .wrapping_add(4)
        .wrapping_add(((word as i16 as i32) << 2) as u32);
    let cond = match word >> 26 {
        // jr / jalr
        0x00 => {
            return if word & 0x3E == 0x08 { Some(rs) } else { None };
        }
        // bltz / bgez / bltzal / bgezal
        0x01 => ((rs as i32) < 0) != (rt_index & 1 != 0),
        // j / jal
        0x02 | 0x03 => {
            return Some((pc.wrapping_add(4) & 0xF000_0000) | ((word & 0x03FF_FFFF) << 2));
        }
        0x04 => rs == rt,
        0x05 => rs != rt,
        0x06 => (rs as i32) <= 0,
        0x07 => (rs as i32) > 0,
        _ => return None,
    };
    Some(if cond { taken } else { not_taken })
}

/// Where psx-rt's exception handler resumes after a fault (any exception
/// but an interrupt), or `None` where it halts. `badvaddr` is COP0
/// BadVAddr; `word_at_epc` and `regs` are only read with [`CAUSE_BD`] set.
///
/// BREAK (the shipping form of `panic=immediate-abort`), an instruction bus
/// error and a misaligned instruction fetch (AdEL with BadVAddr == EPC)
/// halt. Every other fault is stepped over and counted: the faulting
/// instruction is at EPC, so resume at `epc + 4`, unless Cause.BD says it
/// sat in the delay slot of the branch at EPC. That branch has already run,
/// so resume where it sends control ([`branch_resume_pc`]); `epc + 4` would
/// run the faulting instruction again as if the branch had fallen through.
/// A delay-slot fault behind a branch that cannot be evaluated halts. The
/// handler's assembly makes exactly this decision.
pub const fn fault_resume_pc(
    cause: u32,
    epc: u32,
    badvaddr: u32,
    word_at_epc: u32,
    regs: &[u32; 32],
) -> Option<u32> {
    match (cause >> 2) & 0x1F {
        // Break, IBE
        9 | 6 => return None,
        // AdEL on the fetch itself
        4 if badvaddr == epc => return None,
        _ => {}
    }
    if cause & CAUSE_BD == 0 {
        Some(epc.wrapping_add(4))
    } else {
        branch_resume_pc(epc, word_at_epc, regs)
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

    // Fault path. Cause values are ExcCode << 2, plus CAUSE_BD.
    const ADEL: u32 = 4 << 2;
    const ADES: u32 = 5 << 2;
    const IBE: u32 = 6 << 2;
    const DBE: u32 = 7 << 2;
    const BREAK: u32 = 9 << 2;
    const RI: u32 = 10 << 2;
    const CPU: u32 = 11 << 2;
    const OV: u32 = 12 << 2;

    /// A misaligned data address: never equal to an (aligned) EPC.
    const BAD_DATA: u32 = 0x8001_0001;
    const NOT_TAKEN: u32 = EPC + 8;

    /// `$8 = a`, `$9 = b`, `$31` = the link a jal at EPC wrote.
    fn regs(a: u32, b: u32) -> [u32; 32] {
        let mut regs = [0u32; 32];
        regs[8] = a;
        regs[9] = b;
        regs[31] = EPC + 8;
        regs
    }

    fn in_slot(cause: u32, branch: u32, regs: &[u32; 32]) -> Option<u32> {
        fault_resume_pc(cause | CAUSE_BD, EPC, BAD_DATA, branch, regs)
    }

    #[test]
    fn a_fault_outside_a_delay_slot_resumes_after_it() {
        // Word and registers are ignored with BD clear: even a branch word
        // at EPC (the faulting instruction can be a branch's target) steps
        // to EPC + 4.
        for cause in [ADEL, ADES, DBE, RI, CPU, OV] {
            for word in [0x8C82_0000, 0x1109_0004, RTPS] {
                assert_eq!(
                    fault_resume_pc(cause, EPC, BAD_DATA, word, &regs(1, 1)),
                    Some(EPC + 4),
                    "{cause:#x} {word:#010x}"
                );
            }
        }
    }

    #[test]
    fn fatal_faults_halt_in_or_out_of_a_delay_slot() {
        for bd in [0, CAUSE_BD] {
            let r = regs(1, 1);
            assert_eq!(fault_resume_pc(BREAK | bd, EPC, 0, 0x1109_0004, &r), None);
            assert_eq!(fault_resume_pc(IBE | bd, EPC, 0, 0x1109_0004, &r), None);
            assert_eq!(fault_resume_pc(ADEL | bd, EPC, EPC, 0x1109_0004, &r), None);
        }
    }

    #[test]
    fn a_delay_slot_fault_behind_a_taken_branch_resumes_at_its_target() {
        // (branch word, $8, $9, target)
        let cases = [
            (0x1109_0004, 7, 7, EPC + 4 + 16),          // beq $8, $9, +4
            (0x1509_FFFC, 7, 8, EPC + 4 - 16),          // bne $8, $9, -4
            (0x1000_0004, 0, 0, EPC + 4 + 16),          // b +4 (beq $0, $0)
            (0x0501_0008, 0, 0, EPC + 4 + 32),          // bgez $8, +8 ($8 = 0)
            (0x0501_0008, 5, 0, EPC + 4 + 32),          // bgez $8, +8 ($8 > 0)
            (0x0500_0008, -1i32 as u32, 0, EPC + 36),   // bltz $8, +8
            (0x0511_0008, 5, 0, EPC + 36),              // bgezal $8, +8
            (0x0510_0008, 0x8000_0000, 0, EPC + 36),    // bltzal $8, +8
            (0x1900_0008, 0, 0, EPC + 36),              // blez $8, +8 ($8 = 0)
            (0x1900_0008, -3i32 as u32, 0, EPC + 36),   // blez $8, +8 ($8 < 0)
            (0x1D00_0008, 1, 0, EPC + 36),              // bgtz $8, +8
            (0x0800_5000, 0, 0, 0x8001_4000),           // j 0x80014000
            (0x0C00_5000, 0, 0, 0x8001_4000),           // jal 0x80014000
            (0x0100_F809, 0x8002_0000, 0, 0x8002_0000), // jalr $8
        ];
        for (branch, a, b, target) in cases {
            for cause in [ADEL, ADES, DBE, RI, CPU, OV] {
                assert_eq!(
                    in_slot(cause, branch, &regs(a, b)),
                    Some(target),
                    "{cause:#x} {branch:#010x} $8={a:#x} $9={b:#x}"
                );
            }
        }
        // jr $ra: $31 as saved.
        let mut r = regs(0, 0);
        r[31] = 0x8003_0010;
        assert_eq!(in_slot(DBE, 0x03E0_0008, &r), Some(0x8003_0010));
    }

    #[test]
    fn a_delay_slot_fault_behind_a_branch_not_taken_resumes_after_the_slot() {
        let cases = [
            (0x1109_0004, 7, 8),            // beq $8, $9 with $8 != $9
            (0x1509_FFFC, 7, 7),            // bne $8, $9 with $8 == $9
            (0x0501_0008, -1i32 as u32, 0), // bgez $8 with $8 < 0
            (0x0500_0008, 0, 0),            // bltz $8 with $8 = 0
            (0x0511_0008, 0x8000_0000, 0),  // bgezal $8 with $8 < 0
            (0x0510_0008, 1, 0),            // bltzal $8 with $8 > 0
            (0x1900_0008, 1, 0),            // blez $8 with $8 > 0
            (0x1D00_0008, 0, 0),            // bgtz $8 with $8 = 0
            (0x1D00_0008, 0x8000_0000, 0),  // bgtz $8 with $8 < 0
        ];
        for (branch, a, b) in cases {
            assert_eq!(
                in_slot(DBE, branch, &regs(a, b)),
                Some(NOT_TAKEN),
                "{branch:#010x} $8={a:#x} $9={b:#x}"
            );
        }
    }

    #[test]
    fn zero_reads_as_zero_whatever_its_slot_holds() {
        let mut r = regs(0, 0);
        r[0] = 0xDEAD_BEEF;
        assert_eq!(in_slot(DBE, 0x1000_0004, &r), Some(EPC + 20)); // beq $0, $0
    }

    #[test]
    fn j_keeps_the_delay_slots_256mb_region() {
        // The region is that of EPC + 4, not EPC.
        assert_eq!(
            branch_resume_pc(0x8FFF_FFFC, 0x0800_0010, &[0; 32]),
            Some(0x9000_0040)
        );
        assert_eq!(
            branch_resume_pc(0x8001_0000, 0x0BFF_FFFF, &[0; 32]),
            Some(0x8FFF_FFFC)
        );
    }

    #[test]
    fn a_delay_slot_fault_behind_an_unevaluable_word_halts() {
        let words = [
            0x4900_0003, // bc2f
            0x4901_0003, // bc2t
            0x4100_0003, // bc0f
            0x0000_0000, // nop: BD with no branch at EPC
            0x8C82_0000, // lw
            0x0000_000C, // syscall
            RTPS,
        ];
        for word in words {
            assert_eq!(in_slot(DBE, word, &regs(0, 0)), None, "{word:#010x}");
        }
    }

    #[test]
    fn a_gte_command_in_a_delay_slot_interrupt_versus_fault() {
        // bne $8, $9, -4 taken, RTPS in its slot. An interrupt there
        // resumes at the branch (both run again, as documented above); only
        // a fault (Coprocessor Unusable with SR.CU2 clear, say) steps past
        // the RTPS, to where the branch goes.
        let bne = 0x1509_FFFC;
        assert_eq!(interrupt_resume_pc(EPC, bne), EPC);
        assert_eq!(in_slot(CPU, bne, &regs(1, 2)), Some(EPC + 4 - 16));
        assert_eq!(in_slot(CPU, bne, &regs(2, 2)), Some(NOT_TAKEN));
    }
}
