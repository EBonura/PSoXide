//! DMA controller MMIO.
//!
//! The PS1 has 7 DMA channels (MDEC-in, MDEC-out, GPU, CD-ROM, SPU,
//! PIO, OTC). Each channel has three registers -- `MADR` (memory
//! address), `BCR` (block count), `CHCR` (control) -- at fixed
//! 16-byte strides starting at `0x1F80_1080`. Plus global `DPCR`
//! (priority/enable) at `0x1F80_10F0` and `DICR` (IRQ) at `0x1F80_10F4`.

/// Channel index 0..=6 in the order the DMA controller presents them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    /// RAM→MDEC: run-length coefficient data *into* the decoder. This is
    /// the direction the name refers to, not the direction of transfer to
    /// RAM -- channel 0 feeds the MDEC, channel 1 drains it.
    MdecIn = 0,
    /// MDEC→RAM: decoded macroblocks *out of* the decoder, 256 pixels
    /// (16x16) per macroblock at 15bpp.
    MdecOut = 1,
    /// RAM↔GPU.
    Gpu = 2,
    /// CD-ROM drive → RAM.
    Cdrom = 3,
    /// SPU ↔ RAM.
    Spu = 4,
    /// PIO (expansion port).
    Pio = 5,
    /// OTC (ordering-table clear).
    Otc = 6,
}

impl Channel {
    /// Base MMIO address of this channel's register block.
    #[inline(always)]
    pub const fn base(self) -> u32 {
        0x1F80_1080 + 0x10 * (self as u32)
    }

    /// Bit position in `DPCR` for this channel's enable flag.
    #[inline(always)]
    pub const fn dpcr_enable_bit(self) -> u32 {
        3 + 4 * (self as u32)
    }
}

/// Global DMA priority / enable register.
pub const DPCR: u32 = 0x1F80_10F0;
/// Global DMA interrupt / completion-flag register.
pub const DICR: u32 = 0x1F80_10F4;

// --- Per-channel offsets ---------------------------------------------------

const MADR_OFF: u32 = 0x0;
const BCR_OFF: u32 = 0x4;
const CHCR_OFF: u32 = 0x8;

// --- CHCR bits used by SDK callers -----------------------------------------

/// CHCR.0 -- direction: 0 = device→RAM, 1 = RAM→device.
pub const CHCR_TO_DEVICE: u32 = 1 << 0;
/// CHCR.1 -- step: 0 = +4 MADR, 1 = -4 MADR.
pub const CHCR_STEP_BACKWARD: u32 = 1 << 1;
/// CHCR.8 -- chopping enable (DMA yields for CPU periodically).
pub const CHCR_CHOPPING_ENABLE: u32 = 1 << 8;

/// CHCR.9..10 -- sync mode: manual (0), block (1), linked-list (2).
pub const CHCR_SYNC_MANUAL: u32 = 0 << 9;
/// Block-mode sync: BCR = block-count × block-size (in words).
pub const CHCR_SYNC_BLOCK: u32 = 1 << 9;
/// Linked-list sync: walks a chain of packet headers in RAM.
pub const CHCR_SYNC_LINKED: u32 = 2 << 9;

/// CHCR.24 -- start the transfer (busy while set).
pub const CHCR_START: u32 = 1 << 24;
/// CHCR.28 -- manual-mode trigger (self-clears when transfer begins).
pub const CHCR_TRIGGER: u32 = 1 << 28;

// --- Register access helpers ----------------------------------------------

/// `BCR` for manual or linked-list sync: a 16-bit word count (linked-list
/// mode ignores it, but silicon wants it written).
#[inline(always)]
pub const fn bcr_words(words: u16) -> u32 {
    words as u32
}

/// `BCR` for block sync: `block_count` blocks of `block_size` words.
///
/// A zero field means 0x1_0000 on silicon, so a zero count is a 65,536-block
/// transfer, not an empty one.
#[inline(always)]
pub const fn bcr_blocks(block_size: u16, block_count: u16) -> u32 {
    (block_size as u32) | ((block_count as u32) << 16)
}

/// The three per-channel register values that describe one transfer.
///
/// [`start`] stores them in the order silicon expects: `MADR`, `BCR`, then
/// `CHCR` (which starts the transfer when it carries [`CHCR_START`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// RAM address the channel reads from or writes to (`MADR`).
    pub madr: u32,
    /// Word or block count (`BCR`); see [`bcr_words`] and [`bcr_blocks`].
    pub bcr: u32,
    /// Control word (`CHCR`): direction, step, sync mode, start.
    pub chcr: u32,
}

/// Program channel `ch` with `transfer` and start it.
///
/// The same three stores every SDK driver makes: `MADR`, `BCR`, a
/// compiler-only barrier that publishes ordinary RAM stores made before the
/// call (it emits no instruction), then `CHCR`. The caller still enables the
/// channel in `DPCR` ([`enable_channel`]) and waits for completion
/// ([`wait_done`]).
///
/// # Safety
///
/// From the `CHCR` store until the channel goes idle ([`wait_done`] returns
/// `true`, or [`abort`] stops it), the DMA controller reads or writes RAM
/// with no regard for Rust's borrow rules. The caller must guarantee that
/// for that whole window:
///
/// - every word the transfer can **write** (a device-to-RAM channel, such
///   as MDEC-out, CD-ROM, GPU readback or OTC) lies in memory the caller
///   owns exclusively, with no live Rust reference to it;
/// - every word the transfer can **read** (a RAM-to-device channel, or each
///   node of a linked list and the nodes its tags link to) is live,
///   initialised and not written by anyone else;
/// - the extent is the one `bcr` and the sync mode describe, remembering
///   that a zero block count means 65,536 blocks;
/// - the channel is idle when this is called (silicon ignores a `CHCR`
///   write to a busy channel, so the old transfer would keep running).
#[doc(alias = "DMA kick")]
#[inline(always)]
pub unsafe fn start(ch: Channel, transfer: Transfer) {
    // SAFETY: the caller upholds this function's contract for the transfer
    // these three stores arm and start.
    unsafe {
        raw::set_madr(ch, transfer.madr);
        raw::set_bcr(ch, transfer.bcr);
    }
    publish_barrier();
    // SAFETY: as above.
    unsafe { raw::set_chcr(ch, transfer.chcr) };
}

/// Compiler-only barrier: ordinary RAM stores before it are emitted before
/// any MMIO store after it.
///
/// The pinned MIPS-I backend lowers even a single-thread compiler fence to
/// `SYNC`, which the R3000 lacks, so the target uses an empty `asm!` with its
/// default memory clobber. Do not add `nomem` or `readonly`: both would drop
/// the guarantee.
#[inline(always)]
fn publish_barrier() {
    #[cfg(target_arch = "mips")]
    // SAFETY: an empty asm block; it only constrains compiler ordering.
    unsafe {
        core::arch::asm!("", options(nostack, preserves_flags));
    }
    #[cfg(not(target_arch = "mips"))]
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Single-register writes for silicon probes and drivers that need a store
/// order other than [`start`]'s.
pub mod raw {
    use super::{Channel, BCR_OFF, CHCR_OFF, MADR_OFF};

    /// Write `MADR`.
    ///
    /// # Safety
    ///
    /// The value arms the next transfer on `ch`: the caller takes on
    /// [`super::start`]'s contract for whatever transfer later starts with it.
    #[inline(always)]
    pub unsafe fn set_madr(ch: Channel, addr: u32) {
        // SAFETY: a store to this channel's MADR; see the contract above.
        unsafe { crate::write32(ch.base() + MADR_OFF, addr) }
    }

    /// Write `BCR`.
    ///
    /// # Safety
    ///
    /// As [`set_madr`]: the count sets the extent of the next transfer.
    #[inline(always)]
    pub unsafe fn set_bcr(ch: Channel, value: u32) {
        // SAFETY: a store to this channel's BCR; see the contract above.
        unsafe { crate::write32(ch.base() + BCR_OFF, value) }
    }

    /// Write `CHCR`. With [`super::CHCR_START`] set this starts a transfer
    /// from whatever `MADR` and `BCR` hold.
    ///
    /// # Safety
    ///
    /// A value with `CHCR_START` set must satisfy [`super::start`]'s contract
    /// for the transfer it starts. A value without it (such as 0, an abort)
    /// starts nothing.
    #[inline(always)]
    pub unsafe fn set_chcr(ch: Channel, value: u32) {
        // SAFETY: a store to this channel's CHCR; see the contract above.
        unsafe { crate::write32(ch.base() + CHCR_OFF, value) }
    }
}

/// Write `MADR` (memory address that DMA will source from or drain to).
#[inline(always)]
pub fn set_madr(ch: Channel, addr: u32) {
    // SAFETY: none; see `raw::set_madr`.
    unsafe { raw::set_madr(ch, addr) }
}

/// Read `MADR`.
#[inline(always)]
pub fn madr(ch: Channel) -> u32 {
    unsafe { crate::read32(ch.base() + MADR_OFF) }
}

/// Write `BCR` in manual / linked-list mode: just a 16-bit word count.
/// For block-slice mode use [`set_bcr_block`].
#[inline(always)]
pub fn set_bcr_manual(ch: Channel, words: u16) {
    // SAFETY: none; see `raw::set_bcr`.
    unsafe { raw::set_bcr(ch, bcr_words(words)) }
}

/// Write `BCR` in block-slice mode:
/// `BS × BA = blocks of block_size words`.
#[inline(always)]
pub fn set_bcr_block(ch: Channel, block_size: u16, block_count: u16) {
    // SAFETY: none; see `raw::set_bcr`.
    unsafe { raw::set_bcr(ch, bcr_blocks(block_size, block_count)) }
}

/// Write `CHCR` (control). Starts the transfer if `CHCR_START` is set.
#[inline(always)]
pub fn set_chcr(ch: Channel, value: u32) {
    // SAFETY: none; see `raw::set_chcr`.
    unsafe { raw::set_chcr(ch, value) }
}

/// Read `CHCR`.
#[inline(always)]
pub fn chcr(ch: Channel) -> u32 {
    unsafe { crate::read32(ch.base() + CHCR_OFF) }
}

/// True while the channel is busy with an in-flight transfer.
#[inline(always)]
pub fn is_busy(ch: Channel) -> bool {
    chcr(ch) & CHCR_START != 0
}

/// Enable a channel in `DPCR` without disturbing the others.
pub fn enable_channel(ch: Channel) {
    let dpcr = unsafe { crate::read32(DPCR) };
    unsafe { crate::write32(DPCR, dpcr | (1 << ch.dpcr_enable_bit())) }
}

/// OTC-channel helper: clears `buf` as a reverse-linked chain the
/// GPU-DMA walker consumes. The hardware starts from the last word and
/// steps backward, writing a terminator at the first transfer and then
/// predecessor pointers.
///
/// Convenience wrapper: sets up MADR/BCR/CHCR and blocks until done.
/// Returns false if the channel wedged, or if `buf` is longer than the
/// 16-bit BCR word count can express.
#[doc(alias = "ClearOTagR")]
pub fn clear_ordering_table(buf: &mut [u32]) -> bool {
    let Ok(words) = u16::try_from(buf.len()) else {
        // Truncating to 16 bits would clear only the tail of the table
        // and leave the head pointing into words the DMA never touched.
        return false;
    };
    let Some(last) = buf.last_mut() else {
        return true;
    };
    let last_addr = last as *mut u32 as u32;
    // Abort whatever the channel was doing before arming it. On silicon a
    // transfer that never completes leaves START latched, and a write to
    // CHCR while the channel is still busy is ignored: one stuck kick
    // then poisons every later transfer on that channel, which is how a
    // single wedge turned into "the DMA moves nothing, forever" across
    // the CD reader, the ordering-table clear, and the boot uploads.
    abort(Channel::Otc);
    enable_channel(Channel::Otc);
    // OTC clear: direction backward (step -4), manual sync, trigger bit.
    // SAFETY: the channel was just aborted, so it is idle. The transfer
    // writes `words` words stepping back from `last_addr`, which is exactly
    // `buf`, borrowed exclusively until this function returns; the wait
    // below (or the abort on a wedge) ends the transfer before then.
    unsafe {
        start(
            Channel::Otc,
            Transfer {
                madr: last_addr,
                bcr: bcr_words(words),
                chcr: CHCR_STEP_BACKWARD | CHCR_SYNC_MANUAL | CHCR_START | CHCR_TRIGGER,
            },
        )
    };
    let done = wait_done(Channel::Otc, DEFAULT_DMA_SPINS);
    if !done {
        // Leave the controller usable for the next caller rather than
        // handing back a channel that will swallow its kick.
        abort(Channel::Otc);
    }
    done
}

/// Spin budget for one DMA completion wait. Comfortably longer than the
/// largest legitimate transfer (a full-screen VRAM upload) and short
/// enough that a wedged channel returns control inside a frame.
pub const DEFAULT_DMA_SPINS: u32 = 500_000;

/// Force `ch` out of any in-flight transfer by clearing CHCR. Silicon
/// treats dropping the START bit as an abort request; the channel is
/// safe to re-arm afterwards.
pub fn abort(ch: Channel) {
    // SAFETY: a CHCR of 0 has no START bit, so it starts nothing.
    unsafe { raw::set_chcr(ch, 0) };
}

/// Bounded completion wait. `false` means the channel was still busy
/// when the budget ran out, i.e. the transfer is wedged and the caller
/// must not assume its data landed.
pub fn wait_done(ch: Channel, spins: u32) -> bool {
    let mut waited = 0u32;
    while is_busy(ch) {
        if waited >= spins {
            return false;
        }
        waited += 1;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_bcr_packs_size_low_and_count_high() {
        assert_eq!(bcr_blocks(16, 3), 0x0003_0010);
        assert_eq!(bcr_blocks(0xFFFF, 0xFFFF), 0xFFFF_FFFF);
        assert_eq!(bcr_words(0x1234), 0x1234);
    }

    #[test]
    fn channel_blocks_sit_at_sixteen_byte_strides() {
        assert_eq!(Channel::MdecIn.base(), 0x1F80_1080);
        assert_eq!(Channel::Gpu.base(), 0x1F80_10A0);
        assert_eq!(Channel::Otc.base(), 0x1F80_10E0);
        assert_eq!(Channel::Otc.dpcr_enable_bit(), 27);
    }
}
