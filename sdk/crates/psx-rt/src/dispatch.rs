// SPDX-License-Identifier: GPL-2.0-or-later
//! The interrupt dispatcher: how a driver or a game gets code run from an interrupt
//! without writing the exception vector.
//!
//! psx-rt's exception handler is the only handler in the vector. After its own
//! VBlank work (counter, present-queue kick, queued GP1 word, VBlank
//! acknowledge) and for any other interrupt, it checks one word: is something
//! registered for what is pending? If not, the interrupt costs a load and a
//! branch. If so, a short assembly routine saves the interrupted context once on
//! a private stack, calls the handlers in a fixed order and restores.
//!
//! * [`on_vblank`] adds a per-VBlank hook. Hooks run in registration order,
//!   before any source handler.
//! * [`attach`] gives one handler to one other source; [`detach`] takes it away.
//!   Source handlers run in the order of the sources' `I_STAT` bits (CD-ROM,
//!   DMA, root counters 1 and 2).
//!
//! # What a handler may do
//!
//! It runs with CPU interrupts off, on the dispatcher stack ([`STACK_BYTES`],
//! of which the saved context uses the top 112), after every register the C ABI
//! lets a callee change was saved. It must not block, panic, use the GTE (the
//! interrupted code's GTE registers are not saved) or touch `$26` and `$27`. It
//! acknowledges its own source in `I_STAT` (the dispatcher acknowledges
//! nothing; VBlank was acknowledged by psx-rt before any hook runs). It must
//! finish well inside a frame: nothing else runs, and no other interrupt is
//! taken, until it returns.
//!
//! Registration is a store into the table and a bit in one mask. The table
//! word is stored before the bit is set, so the exception handler never
//! calls a slot that is not filled; the bit's read-modify-write runs with CPU
//! interrupts off, because a handler may register or unregister too
//! (psx-cdstream's detaches from its own interrupt). Which handlers one
//! dispatch calls is decided from the bits as they were when it started: a
//! handler that detaches another source does not stop that source's handler
//! being called in the same dispatch if it was already due.

use crate::interrupts::__psx_rt_irq;

/// Bytes of the private stack every handler runs on, context included.
///
/// The saved context takes the top 112, which leaves 400 for the handler. The
/// deepest handler known used 248 (psx-cdstream's transport in a Half-Life
/// build, emulator); the transport's deepest path in `hello-cdstream` is 168
/// bytes and `drain`'s 32, by a static walk of the linked image (the
/// `stack-guard` tool's). The old per-wrapper stacks were 768 and the OoT music
/// pump's 1,024. One stack serves all of them because handlers never nest;
/// [`stack_unused_bytes`] tells a run how much it used, and a new handler should
/// be walked with `stack-guard --root <its symbol> --budget 400`.
pub const STACK_BYTES: usize = 512;

/// Per-VBlank hook slots.
pub const VBLANK_SLOTS: usize = 4;

/// What a registered function is: a plain function, called with CPU
/// interrupts off on the dispatcher stack (see the module docs).
pub type Handler = fn();

/// The interrupt sources other than VBlank that a handler can own: the ones a
/// program registers for today (the controller port and root counter 0 are
/// served by polling and have no slot).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
    /// The CD-ROM controller.
    Cdrom,
    /// DMA completion.
    Dma,
    /// Root counter 1.
    Timer1,
    /// Root counter 2.
    Timer2,
}

impl Source {
    /// The source's position in `I_STAT` and `I_MASK`.
    #[must_use]
    pub const fn bit(self) -> u32 {
        match self {
            Source::Cdrom => 2,
            Source::Dma => 3,
            Source::Timer1 => 5,
            Source::Timer2 => 6,
        }
    }
}

/// A registration was refused: every VBlank slot is in use.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Full;

/// Handler addresses, laid out the way the assembly walks them: the hooks and a
/// zero that ends them, then one word per `I_STAT` bit from 2 to 6.
#[repr(C)]
struct Table {
    hooks: [u32; VBLANK_SLOTS + 1],
    slots: [u32; SLOTS],
}

/// `I_STAT` bits 2 (CD-ROM) to 6 (root counter 2).
const SLOTS: usize = 5;

#[cfg(any(target_arch = "mips", test))]
const SLOTS_OFFSET: usize = (VBLANK_SLOTS + 1) * 4;

impl Table {
    const fn new() -> Self {
        Table { hooks: [0; VBLANK_SLOTS + 1], slots: [0; SLOTS] }
    }

    /// The handlers one dispatch calls, in order, for `pending` `I_STAT & I_MASK`
    /// bits with `registered` bits registered. The assembly does exactly this.
    fn walk(&self, pending: u32, registered: u32, mut call: impl FnMut(u32)) {
        let due = pending & registered;
        if due & 1 != 0 {
            for &hook in self.hooks.iter().take_while(|&&hook| hook != 0) {
                call(hook);
            }
        }
        for (i, &handler) in self.slots.iter().enumerate() {
            if due >> (i + 2) & 1 != 0 {
                call(handler);
            }
        }
    }

    /// The first free hook slot, if any.
    fn free_hook(&self) -> Option<usize> {
        self.hooks[..VBLANK_SLOTS].iter().position(|&hook| hook == 0)
    }
}

struct Shared(core::cell::UnsafeCell<Table>);

// SAFETY: written only by foreground code (single word stores, see the module docs)
// and read by the exception handler, which cannot run meanwhile; one CPU.
unsafe impl Sync for Shared {}

static TABLE: Shared = Shared(core::cell::UnsafeCell::new(Table::new()));

#[cfg(target_arch = "mips")]
extern "C" {
    fn __psx_rt_dispatch_tramp();
}

/// Point the exception handler at the dispatcher.
#[inline(always)]
fn link() {
    #[cfg(target_arch = "mips")]
    // SAFETY: an aligned word store the exception handler reads; one CPU.
    unsafe {
        core::ptr::write_volatile(
            &raw mut __psx_rt_irq.vector,
            __psx_rt_dispatch_tramp as *const () as usize as u32,
        );
    }
}

/// Set the `set` bits and clear the `clear` bits of the registered mask.
///
/// With CPU interrupts off: a handler may register or unregister too
/// (psx-cdstream's closes its source, and so detaches, from its own interrupt),
/// and one taken between this read and this write would be undone by it. On
/// the console it is `__psx_rt_dispatch_update` below, 15 instructions shared by
/// every registration call (`without_interrupts` around the same three lines
/// compiled to 24).
#[inline(always)]
fn update_registered(set: u32, clear: u32) {
    #[cfg(target_arch = "mips")]
    {
        extern "C" {
            fn __psx_rt_dispatch_update(set: u32, clear: u32);
        }
        // SAFETY: the routine below changes the registered word and COP0 SR's IEc
        // (restored before it returns), and only registers a C callee may change.
        unsafe { __psx_rt_dispatch_update(set, clear) }
    }
    #[cfg(not(target_arch = "mips"))]
    // SAFETY: host tests only; a volatile aligned read-modify-write of a word.
    unsafe {
        let registered = &raw mut __psx_rt_irq.registered;
        core::ptr::write_volatile(registered, core::ptr::read_volatile(registered) & !clear | set);
    }
}

/// Run `handler` once per VBlank, after psx-rt's own VBlank work, in the order
/// hooks were registered. [`VBLANK_SLOTS`] hooks fit and they stay for the life
/// of the program; `Err(Full)` is a boot-time bug. Call it from foreground code,
/// not from a handler.
///
/// # Errors
///
/// [`Full`] when all [`VBLANK_SLOTS`] are in use.
#[inline(never)]
pub fn on_vblank(handler: Handler) -> Result<(), Full> {
    // SAFETY: the table is written by foreground code only; the exception handler's
    // reads see either the old zero or the new address, both fine.
    unsafe {
        let table = TABLE.0.get();
        let slot = (*table).free_hook().ok_or(Full)?;
        core::ptr::write_volatile((&raw mut (*table).hooks).cast::<u32>().add(slot), handler as usize as u32);
    }
    link();
    update_registered(1, 0);
    // The VBlank path's own word, set last: the bit above is already there when
    // the handler first takes it.
    #[cfg(target_arch = "mips")]
    // SAFETY: an aligned word store the exception handler reads; one CPU.
    unsafe {
        core::ptr::write_volatile(
            &raw mut __psx_rt_irq.vblank_vector,
            __psx_rt_dispatch_tramp as *const () as usize as u32,
        );
    }
    Ok(())
}

/// Give `source` its handler, replacing any other. It does not enable the
/// source in `I_MASK`: the owner does that when its hardware is ready.
#[inline(never)]
pub fn attach(source: Source, handler: Handler) {
    let bit = source.bit();
    // SAFETY: an aligned word store into a slot the exception handler reads only
    // while the bit below is set; one CPU.
    unsafe {
        core::ptr::write_volatile(
            (&raw mut (*TABLE.0.get()).slots).cast::<u32>().add(bit as usize - 2),
            handler as usize as u32,
        );
    }
    link();
    update_registered(1 << bit, 0);
}

/// Take `source`'s handler away. The source's `I_MASK` bit is the owner's to clear.
#[inline(never)]
pub fn detach(source: Source) {
    update_registered(0, 1 << source.bit());
}

/// The `I_STAT` bits something is registered for (VBlank is bit 0).
#[must_use]
pub fn registered_sources() -> u32 {
    // SAFETY: a volatile aligned read of a word foreground code writes.
    unsafe { core::ptr::read_volatile(&raw const __psx_rt_irq.registered) }
}

/// The handlers a dispatch of `pending` would call right now, in order: the
/// pure reference the assembly follows, for a test to compare against
/// [`dispatch_for_test`].
pub fn plan(pending: u32, call: impl FnMut(u32)) {
    // SAFETY: foreground reads of words foreground code writes.
    unsafe { (*TABLE.0.get()).walk(pending, registered_sources(), call) }
}

/// The address a handler is recorded under, to match [`plan`]'s output.
#[must_use]
pub fn address_of(handler: Handler) -> u32 {
    handler as usize as u32
}

/// The dispatcher stack.
#[repr(C, align(16))]
struct Stack([u8; STACK_BYTES]);

#[allow(non_upper_case_globals)]
#[cfg_attr(target_arch = "mips", no_mangle)]
static mut __psx_rt_dispatch_stack: Stack = Stack([0; STACK_BYTES]);

const STACK_FILL: u8 = 0xa5;

/// Paint the dispatcher stack so [`stack_unused_bytes`] can measure it. A
/// measuring program calls this once, before its handlers run; nothing else
/// does, so a normal program does not link it.
pub fn paint_stack() {
    psx_io::irq::without_interrupts(|| {
        // SAFETY: plain volatile writes to a static this module owns, with no
        // handler running on it.
        unsafe {
            let stack = (&raw mut __psx_rt_dispatch_stack).cast::<u8>();
            for i in 0..STACK_BYTES {
                stack.add(i).write_volatile(STACK_FILL);
            }
        }
    });
}

/// Bytes of the dispatcher stack no handler has touched since [`paint_stack`].
#[must_use]
pub fn stack_unused_bytes() -> usize {
    let mut unused = 0;
    // SAFETY: volatile reads of a static this module owns.
    unsafe {
        let stack = (&raw const __psx_rt_dispatch_stack).cast::<u8>();
        while unused < STACK_BYTES && stack.add(unused).read_volatile() == STACK_FILL {
            unused += 1;
        }
    }
    unused
}

/// Run one dispatch for `pending` from foreground code, through the same
/// assembly an interrupt uses.
///
/// # Safety
///
/// CPU interrupts must be off, and `pending` must only have bits something is
/// registered for.
#[cfg(target_arch = "mips")]
pub unsafe fn dispatch_for_test(pending: u32) {
    extern "C" {
        fn __psx_rt_dispatch_test(pending: u32);
    }
    // SAFETY: the caller's contract; the routine returns to its caller with the
    // caller-saved registers restored.
    unsafe { __psx_rt_dispatch_test(pending) };
}

#[cfg(target_arch = "mips")]
core::arch::global_asm!(
    r#"
    .set noreorder
    .set noat
    .section .text.psx_rt_dispatch
    # Entered by `j` from the exception handler with $26 = the address to resume
    # at. Saves what the C ABI lets a callee change, switches to the dispatcher
    # stack, runs the due VBlank hooks and then the due source handlers in
    # I_STAT bit order, restores and jumps to $26. $26 and $27 are the kernel's
    # and no handler touches them: $27 holds the interrupted stack pointer.
    # Frame (112 bytes): 0..15 the callee's argument area, 16 $16, 20 $17,
    # 24 $18, 32.. $1..$15, 92 $24, 96 $25, 100 $31, 104 hi, 108 lo.
    .globl __psx_rt_dispatch_tramp
__psx_rt_dispatch_tramp:
    move  $27, $29
    lui   $29, %hi({stack}+{frame_at})
    addiu $29, $29, %lo({stack}+{frame_at})
    sw    $1, 32($29)
    sw    $2, 36($29)
    sw    $3, 40($29)
    sw    $4, 44($29)
    sw    $5, 48($29)
    sw    $6, 52($29)
    sw    $7, 56($29)
    sw    $8, 60($29)
    sw    $9, 64($29)
    sw    $10, 68($29)
    sw    $11, 72($29)
    sw    $12, 76($29)
    sw    $13, 80($29)
    sw    $14, 84($29)
    sw    $15, 88($29)
    sw    $24, 92($29)
    sw    $25, 96($29)
    sw    $31, 100($29)
    mfhi  $1
    sw    $1, 104($29)
    mflo  $1
    sw    $1, 108($29)
    sw    $16, 16($29)
    sw    $17, 20($29)
    sw    $18, 24($29)
    lui   $2, %hi({irq})
    lw    $17, %lo({irq})($2)
    lw    $2, %lo({irq}+4)($2)
    lui   $16, %hi({table})
    addiu $16, $16, %lo({table})
    and   $17, $17, $2
    andi  $2, $17, 1
    beqz  $2, 2f
    move  $18, $16
1:
    lw    $25, 0($18)
    nop
    beqz  $25, 2f
    nop
    jalr  $25
    addiu $18, $18, 4
    b     1b
    nop
2:
    srl   $17, $17, 2
    beqz  $17, 5f
    addiu $18, $16, {slots_off}
3:
    andi  $2, $17, 1
    beqz  $2, 4f
    lw    $25, 0($18)
    nop
    jalr  $25
    nop
4:
    srl   $17, $17, 1
    bnez  $17, 3b
    addiu $18, $18, 4
5:
    lw    $1, 104($29)
    nop
    mthi  $1
    lw    $1, 108($29)
    nop
    mtlo  $1
    lw    $1, 32($29)
    lw    $2, 36($29)
    lw    $3, 40($29)
    lw    $4, 44($29)
    lw    $5, 48($29)
    lw    $6, 52($29)
    lw    $7, 56($29)
    lw    $8, 60($29)
    lw    $9, 64($29)
    lw    $10, 68($29)
    lw    $11, 72($29)
    lw    $12, 76($29)
    lw    $13, 80($29)
    lw    $14, 84($29)
    lw    $15, 88($29)
    lw    $24, 92($29)
    lw    $25, 96($29)
    lw    $31, 100($29)
    lw    $16, 16($29)
    lw    $17, 20($29)
    lw    $18, 24($29)
    jr    $26
    move  $29, $27

    # __psx_rt_dispatch_update(set, clear): registered = registered & !clear | set,
    # with IEc cleared around the read-modify-write so no handler's own
    # registration lands between the load and the store. SR goes back whole
    # (nothing else writes it meanwhile: interrupts are off and psx-rt's handler
    # leaves SR alone). The two instructions after the first mtc0 let the write
    # take effect before the load.
    .section .text.psx_rt_dispatch_update
    .globl __psx_rt_dispatch_update
__psx_rt_dispatch_update:
    mfc0  $8, $12
    lui   $2, %hi({irq})
    addiu $9, $zero, -2
    and   $9, $8, $9
    mtc0  $9, $12
    nor   $5, $5, $zero
    nop
    lw    $3, %lo({irq}+4)($2)
    nop
    and   $3, $3, $5
    or    $3, $3, $4
    sw    $3, %lo({irq}+4)($2)
    mtc0  $8, $12
    jr    $31
    nop

    # __psx_rt_dispatch_test(pending): store `pending` as the exception handler
    # would have and run the dispatcher, returning to the caller. In its own
    # section, so only a program that calls it links it.
    .section .text.psx_rt_dispatch_test
    .globl __psx_rt_dispatch_test
__psx_rt_dispatch_test:
    lui   $2, %hi({irq})
    sw    $4, %lo({irq})($2)
    move  $26, $31
    j     __psx_rt_dispatch_tramp
    nop
    .set at
    .set reorder
    "#,
    stack = sym __psx_rt_dispatch_stack,
    table = sym TABLE,
    irq = sym __psx_rt_irq,
    frame_at = const STACK_BYTES - 112,
    slots_off = const SLOTS_OFFSET,
);

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn order(table: &Table, pending: u32, registered: u32) -> Vec<u32> {
        let mut called = Vec::new();
        table.walk(pending, registered, |handler| called.push(handler));
        called
    }

    #[test]
    fn hooks_run_in_slot_order_and_only_when_vblank_is_pending_and_registered() {
        let mut table = Table::new();
        table.hooks[..2].copy_from_slice(&[0xb, 0xa]);
        assert_eq!(order(&table, 0, 1), vec![]);
        assert_eq!(order(&table, 1, 0), vec![]);
        assert_eq!(order(&table, 1, 1), vec![0xb, 0xa]);
    }

    #[test]
    fn a_full_hook_array_ends_at_the_zero_after_it() {
        let mut table = Table::new();
        table.hooks[..VBLANK_SLOTS].copy_from_slice(&[1, 2, 3, 4]);
        table.slots[0] = 9;
        assert_eq!(order(&table, 1, 1), vec![1, 2, 3, 4]);
        assert_eq!(table.free_hook(), None);
        table.hooks[1] = 0;
        assert_eq!(table.free_hook(), Some(1));
    }

    #[test]
    fn sources_run_in_i_stat_bit_order_after_the_hooks() {
        let mut table = Table::new();
        table.hooks[0] = 0x10;
        let sources = [Source::Timer2, Source::Timer1, Source::Dma, Source::Cdrom];
        let mut registered = 1;
        for (n, source) in sources.into_iter().enumerate() {
            table.slots[source.bit() as usize - 2] = 0x20 + n as u32;
            registered |= 1 << source.bit();
        }
        assert_eq!(
            order(&table, registered, registered),
            vec![0x10, 0x23, 0x22, 0x21, 0x20]
        );
    }

    #[test]
    fn a_pending_source_nothing_registered_for_is_not_called() {
        let mut table = Table::new();
        table.slots[0] = 7; // CD-ROM
        table.slots[3] = 8; // root counter 1, but not registered
        let registered = 1 << Source::Cdrom.bit();
        assert_eq!(order(&table, (1 << 2) | (1 << 5) | (1 << 7), registered), vec![7]);
    }

    #[test]
    fn every_source_has_a_slot() {
        for source in [Source::Cdrom, Source::Dma, Source::Timer1, Source::Timer2] {
            assert!((2..2 + SLOTS as u32).contains(&source.bit()));
        }
        assert_eq!(core::mem::size_of::<Table>(), SLOTS_OFFSET + 4 * SLOTS);
    }
}
