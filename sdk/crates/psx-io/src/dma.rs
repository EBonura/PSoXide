//! DMA controller.
//!
//! The PS1 has 7 DMA channels (MDEC in, MDEC out, GPU, CD-ROM, SPU,
//! expansion, ordering-table clear). Each channel has an address, a size and
//! a control register at fixed 16-byte strides, plus a global enable and a
//! global interrupt register. The addresses and bit layouts live in
//! [`psx_hw::dma`].

use psx_hw::dma as reg;

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
    #[doc(alias = "CDROM")]
    Cd = 3,
    /// SPU ↔ RAM.
    Spu = 4,
    /// Expansion port.
    #[doc(alias = "PIO")]
    Expansion = 5,
    /// Ordering-table clear: writes a reverse-linked empty ordering table.
    #[doc(alias = "OTC")]
    OrderingTableClear = 6,
}

#[allow(non_upper_case_globals)]
impl Channel {
    /// Renamed to [`Channel::Cd`].
    #[deprecated(note = "renamed to `Channel::Cd`")]
    pub const Cdrom: Channel = Channel::Cd;
    /// Renamed to [`Channel::Expansion`].
    #[deprecated(note = "renamed to `Channel::Expansion`")]
    pub const Pio: Channel = Channel::Expansion;
    /// Renamed to [`Channel::OrderingTableClear`].
    #[deprecated(note = "renamed to `Channel::OrderingTableClear`")]
    pub const Otc: Channel = Channel::OrderingTableClear;
}

impl Channel {
    /// Address of this channel's register block.
    #[inline(always)]
    pub const fn register_base(self) -> u32 {
        reg::CHANNEL_BASE + reg::CHANNEL_STRIDE * (self as u32)
    }

    /// Bit position of this channel's enable flag in the global enable
    /// register.
    #[doc(alias = "DPCR")]
    #[inline(always)]
    pub const fn enable_bit(self) -> u32 {
        3 + 4 * (self as u32)
    }

    /// Renamed to [`Channel::register_base`].
    #[deprecated(note = "renamed to `register_base`")]
    #[inline(always)]
    pub const fn base(self) -> u32 {
        self.register_base()
    }

    /// Renamed to [`Channel::enable_bit`].
    #[deprecated(note = "renamed to `enable_bit`")]
    #[inline(always)]
    pub const fn dpcr_enable_bit(self) -> u32 {
        self.enable_bit()
    }
}

/// Size word for manual or linked-list sync: a 16-bit word count
/// (linked-list mode ignores it, but silicon wants it written).
#[doc(alias = "BCR")]
#[inline(always)]
pub const fn size_words(words: u16) -> u32 {
    words as u32
}

/// Size word for block sync: `block_count` blocks of `block_size` words.
///
/// A zero field means 0x1_0000 on silicon, so a zero count is a 65,536-block
/// transfer, not an empty one.
#[doc(alias = "BCR")]
#[inline(always)]
pub const fn size_blocks(block_size: u16, block_count: u16) -> u32 {
    (block_size as u32) | ((block_count as u32) << 16)
}

/// Renamed to [`size_words`].
#[deprecated(note = "renamed to `size_words`")]
#[inline(always)]
pub const fn bcr_words(words: u16) -> u32 {
    size_words(words)
}

/// Renamed to [`size_blocks`].
#[deprecated(note = "renamed to `size_blocks`")]
#[inline(always)]
pub const fn bcr_blocks(block_size: u16, block_count: u16) -> u32 {
    size_blocks(block_size, block_count)
}

/// The three per-channel register values that describe one transfer.
///
/// [`start`] stores them in the order silicon expects: address, size, then
/// control (which starts the transfer when it carries `CHCR_START`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transfer {
    /// RAM address the channel reads from or writes to.
    #[doc(alias = "MADR")]
    pub address: u32,
    /// Word or block count; see [`size_words`] and [`size_blocks`].
    #[doc(alias = "BCR")]
    pub size: u32,
    /// Control word ([`psx_hw::dma`]'s `CHCR_*` bits): direction, step, sync
    /// mode, start.
    #[doc(alias = "CHCR")]
    pub control: u32,
}

/// Program channel `ch` with `transfer` and start it.
///
/// The same three stores every SDK driver makes: address, size, a
/// compiler-only barrier that publishes ordinary RAM stores made before the
/// call (it emits no instruction), then control. The caller still enables
/// the channel ([`enable_channel`]) and waits for completion ([`wait_done`]).
///
/// # Safety
///
/// From the control store until the channel goes idle ([`wait_done`]
/// returns `true`, or [`abort`] stops it), the DMA controller reads or writes
/// RAM with no regard for Rust's borrow rules. The caller must guarantee that
/// for that whole window:
///
/// - every word the transfer can **write** (a device-to-RAM channel, such
///   as MDEC-out, CD-ROM, GPU readback or the ordering-table clear) lies in
///   memory the caller owns exclusively, with no live Rust reference to it;
/// - every word the transfer can **read** (a RAM-to-device channel, or each
///   node of a linked list and the nodes its tags link to) is live,
///   initialised and not written by anyone else;
/// - the extent is the one `size` and the sync mode describe, remembering
///   that a zero block count means 65,536 blocks;
/// - the channel is idle when this is called (silicon ignores a control
///   write to a busy channel, so the old transfer would keep running).
#[inline(always)]
pub unsafe fn start(ch: Channel, transfer: Transfer) {
    // SAFETY: the caller upholds this function's contract for the transfer
    // these three stores arm and start.
    unsafe {
        raw::set_address(ch, transfer.address);
        raw::set_size(ch, transfer.size);
    }
    compiler_barrier();
    // SAFETY: as above.
    unsafe { raw::set_control(ch, transfer.control) };
}

/// Compiler-only barrier: ordinary RAM accesses are not moved across it in
/// either direction, so stores before it land before an MMIO store after it
/// and accesses after it follow an MMIO read before it.
///
/// The pinned MIPS-I backend lowers even a single-thread compiler fence to
/// `SYNC`, which the R3000 lacks, so the target uses an empty `asm!` with its
/// default memory clobber. Do not add `nomem` or `readonly`: both would drop
/// the guarantee.
#[inline(always)]
pub fn compiler_barrier() {
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
    use super::{reg, Channel};

    /// Write the channel's RAM address register.
    ///
    /// # Safety
    ///
    /// The value arms the next transfer on `ch`: the caller takes on
    /// [`super::start`]'s contract for whatever transfer later starts with it.
    ///
    /// With the `present-queue` feature, a store to the GPU channel first
    /// runs an armed direct-access guard (`gpu::arm_direct_access_guard`),
    /// so it never re-arms the channel under a queued walk. Every GPU DMA
    /// start, [`super::start`] included, goes through here.
    #[doc(alias = "MADR")]
    #[inline(always)]
    pub unsafe fn set_address(ch: Channel, addr: u32) {
        #[cfg(feature = "present-queue")]
        if matches!(ch, Channel::Gpu) {
            crate::gpu::run_direct_access_guard();
        }
        // SAFETY: a store to this channel's address register; see the contract above.
        unsafe { crate::write_u32(ch.register_base() + reg::MADR, addr) }
    }

    /// Write the channel's size register.
    ///
    /// # Safety
    ///
    /// As [`set_address`]: the count sets the extent of the next transfer.
    #[doc(alias = "BCR")]
    #[inline(always)]
    pub unsafe fn set_size(ch: Channel, value: u32) {
        // SAFETY: a store to this channel's size register; see the contract above.
        unsafe { crate::write_u32(ch.register_base() + reg::BCR, value) }
    }

    /// Write the channel's control register. With `CHCR_START` set this
    /// starts a transfer from whatever the address and size registers hold.
    ///
    /// # Safety
    ///
    /// A value with `CHCR_START` set must satisfy [`super::start`]'s contract
    /// for the transfer it starts. A value without it (such as 0, an abort)
    /// starts nothing.
    #[doc(alias = "CHCR")]
    #[inline(always)]
    pub unsafe fn set_control(ch: Channel, value: u32) {
        // SAFETY: a store to this channel's control register; see the contract above.
        unsafe { crate::write_u32(ch.register_base() + reg::CHCR, value) }
    }

    /// Renamed to [`set_address`].
    ///
    /// # Safety
    /// See [`set_address`].
    #[deprecated(note = "renamed to `set_address`")]
    #[inline(always)]
    pub unsafe fn set_madr(ch: Channel, addr: u32) {
        // SAFETY: same contract as the renamed function.
        unsafe { set_address(ch, addr) }
    }

    /// Renamed to [`set_size`].
    ///
    /// # Safety
    /// See [`set_size`].
    #[deprecated(note = "renamed to `set_size`")]
    #[inline(always)]
    pub unsafe fn set_bcr(ch: Channel, value: u32) {
        // SAFETY: same contract as the renamed function.
        unsafe { set_size(ch, value) }
    }

    /// Renamed to [`set_control`].
    ///
    /// # Safety
    /// See [`set_control`].
    #[deprecated(note = "renamed to `set_control`")]
    #[inline(always)]
    pub unsafe fn set_chcr(ch: Channel, value: u32) {
        // SAFETY: same contract as the renamed function.
        unsafe { set_control(ch, value) }
    }
}

/// Write the channel's RAM address register.
///
/// # Safety
///
/// The value arms or starts a transfer on `ch`; see [`start`]'s contract.
#[deprecated(
    note = "a safe address store lets safe code aim DMA anywhere in RAM; use the unsafe `dma::start`, or `dma::raw::set_address` for probes"
)]
#[inline(always)]
pub unsafe fn set_madr(ch: Channel, addr: u32) {
    // SAFETY: forwarded contract.
    unsafe { raw::set_address(ch, addr) }
}

/// Read the channel's RAM address register.
#[doc(alias = "MADR")]
#[inline(always)]
pub fn address(ch: Channel) -> u32 {
    // SAFETY: a side-effect-free read of this channel's address register.
    unsafe { crate::read_u32(ch.register_base() + reg::MADR) }
}

/// Renamed to [`address`].
#[deprecated(note = "renamed to `address`")]
#[inline(always)]
pub fn madr(ch: Channel) -> u32 {
    address(ch)
}

/// Write the size register in manual / linked-list mode: just a 16-bit word
/// count.
///
/// # Safety
///
/// The value arms or starts a transfer on `ch`; see [`start`]'s contract.
#[deprecated(
    note = "use the unsafe `dma::start` with `dma::size_words`, or `dma::raw::set_size` for probes"
)]
#[inline(always)]
pub unsafe fn set_bcr_manual(ch: Channel, words: u16) {
    // SAFETY: forwarded contract.
    unsafe { raw::set_size(ch, size_words(words)) }
}

/// Write the size register in block mode: `block_count` blocks of
/// `block_size` words.
///
/// # Safety
///
/// The value arms or starts a transfer on `ch`; see [`start`]'s contract.
#[deprecated(
    note = "use the unsafe `dma::start` with `dma::size_blocks`, or `dma::raw::set_size` for probes"
)]
#[inline(always)]
pub unsafe fn set_bcr_block(ch: Channel, block_size: u16, block_count: u16) {
    // SAFETY: forwarded contract.
    unsafe { raw::set_size(ch, size_blocks(block_size, block_count)) }
}

/// Write the control register. Starts the transfer if `CHCR_START` is set.
///
/// # Safety
///
/// The value arms or starts a transfer on `ch`; see [`start`]'s contract.
#[deprecated(
    note = "a safe control store starts DMA from safe code; use the unsafe `dma::start`, `dma::abort` to stop a channel, or `dma::raw::set_control` for probes"
)]
#[inline(always)]
pub unsafe fn set_chcr(ch: Channel, value: u32) {
    // SAFETY: forwarded contract.
    unsafe { raw::set_control(ch, value) }
}

/// Read the channel's control register.
#[doc(alias = "CHCR")]
#[inline(always)]
pub fn control(ch: Channel) -> u32 {
    // SAFETY: a side-effect-free read of this channel's control register.
    unsafe { crate::read_u32(ch.register_base() + reg::CHCR) }
}

/// Renamed to [`control`].
#[deprecated(note = "renamed to `control`")]
#[inline(always)]
pub fn chcr(ch: Channel) -> u32 {
    control(ch)
}

/// True while the channel is busy with an in-flight transfer.
#[inline(always)]
pub fn is_busy(ch: Channel) -> bool {
    control(ch) & reg::CHCR_START != 0
}

/// Enable a channel without disturbing the others.
pub fn enable_channel(ch: Channel) {
    // SAFETY: a side-effect-free read of the global enable register.
    let enabled = unsafe { crate::read_u32(reg::DPCR) };
    // SAFETY: sets one channel's enable bit and keeps the rest; enabling a
    // channel starts no transfer.
    unsafe { crate::write_u32(reg::DPCR, enabled | (1 << ch.enable_bit())) }
}

/// Ordering-table clear: writes `buf` as a reverse-linked chain the
/// GPU-DMA walker consumes. The hardware starts from the last word and
/// steps backward, writing a terminator at the first transfer and then
/// predecessor pointers.
///
/// Convenience wrapper: programs the channel and blocks until done.
/// Returns false if the channel wedged, or if `buf` is longer than the
/// 16-bit word count can express.
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
    // the control register while the channel is still busy is ignored: one
    // stuck kick then poisons every later transfer on that channel, which is
    // how a single wedge turned into "the DMA moves nothing, forever" across
    // the CD reader, the ordering-table clear, and the boot uploads.
    abort(Channel::OrderingTableClear);
    enable_channel(Channel::OrderingTableClear);
    // Ordering-table clear: direction backward (step -4), manual sync,
    // trigger bit.
    // SAFETY: the channel was just aborted, so it is idle. The transfer
    // writes `words` words stepping back from `last_addr`, which is exactly
    // `buf`, borrowed exclusively until this function returns. It cannot
    // outlive that borrow, and this does not rest on what an abort does:
    // DMA6 has only CHCR bits 24, 28 and 30 writable, so it always runs in
    // burst mode (SyncMode 0) without chopping, and psx-spx ("DMA Channels",
    // "CPU Operation during DMA") documents that any RAM or I/O read stalls
    // the CPU until such a transfer is finished, allowing only I/O reads
    // within 3 cycles of the CHCR write. The wait below reads CHCR until it
    // reads idle or its budget ends, so by the time it returns the transfer
    // has finished or, wedged with START latched, never ran.
    unsafe {
        start(
            Channel::OrderingTableClear,
            Transfer {
                address: last_addr,
                size: size_words(words),
                control: reg::CHCR_STEP_BACKWARD
                    | reg::CHCR_SYNC_MANUAL
                    | reg::CHCR_START
                    | reg::CHCR_TRIGGER,
            },
        )
    };
    // A wedge is aborted, which leaves the controller usable for the next
    // caller rather than handing back a channel that will swallow its kick.
    wait_or_abort(Channel::OrderingTableClear, DEFAULT_SPINS)
}

/// Spin budget for one DMA completion wait. Comfortably longer than the
/// largest legitimate transfer (a full-screen VRAM upload) and short
/// enough that a wedged channel returns control inside a frame.
pub const DEFAULT_SPINS: u32 = 500_000;

/// Renamed to [`DEFAULT_SPINS`].
#[deprecated(note = "renamed to `DEFAULT_SPINS`")]
pub const DEFAULT_DMA_SPINS: u32 = DEFAULT_SPINS;

/// Clear `ch`'s control register, dropping its START bit, so the channel can
/// be re-armed: on silicon a CHCR write to a channel whose START is still
/// latched is ignored, which is how one wedged kick used to poison every
/// later transfer on the channel.
///
/// psx-spx documents only that START clears when a transfer completes, not
/// that clearing it stops one in progress. Code whose memory safety needs a
/// transfer to have stopped must not rely on this call for it; see
/// [`clear_ordering_table`] for a bound that does not.
pub fn abort(ch: Channel) {
    // SAFETY: a control word of 0 has no START bit, so it starts nothing.
    unsafe { raw::set_control(ch, 0) };
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

/// End a transfer started with [`start`]: [`wait_done`]; if the budget runs
/// out, [`abort`] the channel and wait again, within the same budget, for it
/// to read idle; then a compiler barrier so the caller's next RAM access to
/// the buffer is not moved before the completion read (the barrier emits no
/// instruction). `false` means the data did not all land.
///
/// The second wait is there because clearing START is not documented to stop
/// a running transfer (see [`abort`]). What it leaves assumed: a channel that
/// still reads busy after both budgets is wedged and moving no data, as the
/// CD channel's console wedges were (START latched, MADR frozen). Burst-mode
/// channels without chopping need no assumption; see
/// [`clear_ordering_table`].
pub fn wait_or_abort(ch: Channel, spins: u32) -> bool {
    let done = wait_done(ch, spins);
    if !done {
        abort(ch);
        let _ = wait_done(ch, spins);
    }
    compiler_barrier();
    done
}

/// Moved to [`psx_hw::dma::DPCR`].
#[deprecated(note = "moved to `psx_hw::dma::DPCR`")]
pub const DPCR: u32 = reg::DPCR;
/// Moved to [`psx_hw::dma::DICR`].
#[deprecated(note = "moved to `psx_hw::dma::DICR`")]
pub const DICR: u32 = reg::DICR;
/// Moved to [`psx_hw::dma::CHCR_TO_DEVICE`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_TO_DEVICE`")]
pub const CHCR_TO_DEVICE: u32 = reg::CHCR_TO_DEVICE;
/// Moved to [`psx_hw::dma::CHCR_STEP_BACKWARD`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_STEP_BACKWARD`")]
pub const CHCR_STEP_BACKWARD: u32 = reg::CHCR_STEP_BACKWARD;
/// Moved to [`psx_hw::dma::CHCR_CHOPPING_ENABLE`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_CHOPPING_ENABLE`")]
pub const CHCR_CHOPPING_ENABLE: u32 = reg::CHCR_CHOPPING_ENABLE;
/// Moved to [`psx_hw::dma::CHCR_SYNC_MANUAL`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_SYNC_MANUAL`")]
pub const CHCR_SYNC_MANUAL: u32 = reg::CHCR_SYNC_MANUAL;
/// Moved to [`psx_hw::dma::CHCR_SYNC_BLOCK`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_SYNC_BLOCK`")]
pub const CHCR_SYNC_BLOCK: u32 = reg::CHCR_SYNC_BLOCK;
/// Moved to [`psx_hw::dma::CHCR_SYNC_LINKED`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_SYNC_LINKED`")]
pub const CHCR_SYNC_LINKED: u32 = reg::CHCR_SYNC_LINKED;
/// Moved to [`psx_hw::dma::CHCR_START`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_START`")]
pub const CHCR_START: u32 = reg::CHCR_START;
/// Moved to [`psx_hw::dma::CHCR_TRIGGER`].
#[deprecated(note = "moved to `psx_hw::dma::CHCR_TRIGGER`")]
pub const CHCR_TRIGGER: u32 = reg::CHCR_TRIGGER;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_packs_size_low_and_count_high() {
        assert_eq!(size_blocks(16, 3), 0x0003_0010);
        assert_eq!(size_blocks(0xFFFF, 0xFFFF), 0xFFFF_FFFF);
        assert_eq!(size_words(0x1234), 0x1234);
    }

    #[test]
    fn channel_blocks_sit_at_sixteen_byte_strides() {
        assert_eq!(Channel::MdecIn.register_base(), 0x1F80_1080);
        assert_eq!(Channel::Gpu.register_base(), 0x1F80_10A0);
        assert_eq!(Channel::OrderingTableClear.register_base(), 0x1F80_10E0);
        assert_eq!(Channel::OrderingTableClear.enable_bit(), 27);
    }
}
