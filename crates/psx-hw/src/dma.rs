//! DMA controller registers and channel-control bits.
//!
//! Seven channels (MDEC in, MDEC out, GPU, CD-ROM, SPU, PIO, OTC), each with
//! three registers at a 16-byte stride from [`CHANNEL_BASE`], plus the global
//! [`DPCR`] and [`DICR`].
//!
//! Reference: nocash PSX-SPX "DMA Channels".

/// Register block of channel 0; channel `n` is at `CHANNEL_BASE + n *
/// CHANNEL_STRIDE`.
pub const CHANNEL_BASE: u32 = 0x1F80_1080;
/// Distance between two channels' register blocks.
pub const CHANNEL_STRIDE: u32 = 0x10;
/// Offset of the memory-address register in a channel block.
#[doc(alias = "D_MADR")]
pub const MADR: u32 = 0x0;
/// Offset of the block-control (size) register in a channel block.
#[doc(alias = "D_BCR")]
pub const BCR: u32 = 0x4;
/// Offset of the channel-control register in a channel block.
#[doc(alias = "D_CHCR")]
pub const CHCR: u32 = 0x8;

/// Global priority / enable register.
pub const DPCR: u32 = 0x1F80_10F0;
/// Global interrupt / completion-flag register.
pub const DICR: u32 = 0x1F80_10F4;

/// CHCR.0: direction, 0 = device to RAM, 1 = RAM to device.
pub const CHCR_TO_DEVICE: u32 = 1 << 0;
/// CHCR.1: step, 0 = +4 per word, 1 = -4 per word.
pub const CHCR_STEP_BACKWARD: u32 = 1 << 1;
/// CHCR.8: chopping enable (the DMA yields to the CPU periodically).
pub const CHCR_CHOPPING_ENABLE: u32 = 1 << 8;
/// CHCR.9..10 sync mode 0: manual, one burst of BCR words.
pub const CHCR_SYNC_MANUAL: u32 = 0 << 9;
/// CHCR.9..10 sync mode 1: block, BCR = block count x block size (words).
///
/// On silicon the controller counts BCR's block field down as blocks finish,
/// so a finished transfer leaves it at 0, and a block kick with count 0 runs
/// 65,536 blocks. Write BCR again before every kick; a second kick that
/// reuses the old value runs away (console-tested on the SPU and GPU
/// channels, hardware-tests v2.1).
pub const CHCR_SYNC_BLOCK: u32 = 1 << 9;
/// CHCR.9..10 sync mode 2: linked list, walks a chain of packet headers.
pub const CHCR_SYNC_LINKED: u32 = 2 << 9;
/// CHCR.24: start the transfer (busy while set).
pub const CHCR_START: u32 = 1 << 24;
/// CHCR.28: manual-mode trigger (self-clears when the transfer begins).
pub const CHCR_TRIGGER: u32 = 1 << 28;

/// Channel numbers, in the order the controller presents them.
pub mod channel {
    /// RAM to MDEC.
    pub const MDEC_IN: u32 = 0;
    /// MDEC to RAM.
    pub const MDEC_OUT: u32 = 1;
    /// RAM and GPU.
    pub const GPU: u32 = 2;
    /// CD-ROM to RAM.
    pub const CD: u32 = 3;
    /// RAM and SPU.
    pub const SPU: u32 = 4;
    /// Expansion port.
    pub const PIO: u32 = 5;
    /// Ordering-table clear.
    pub const OTC: u32 = 6;
}

/// [`DPCR`] bit that enables channel `channel`: bit 3 of its four-bit
/// field (the other three bits are the channel's priority).
#[inline(always)]
pub const fn dpcr_enable(channel: u32) -> u32 {
    1 << (3 + 4 * channel)
}

/// The tag word that heads every node of a linked-list walk (GPU ordering
/// tables and packet chains): the node's payload length in words in the top
/// byte and the next node's physical address in the low 24 bits.
///
/// Reference: nocash PSX-SPX "DMA Channels", linked-list mode.
pub mod linked_list {
    /// The usual terminator: the walk ends at a node whose next pointer has
    /// bit 23 set, and this value has it.
    pub const END: u32 = 0x00FF_FFFF;
    /// Mask of the next-node address in a tag word.
    pub const ADDRESS_MASK: u32 = 0x00FF_FFFF;
    /// Position of the payload length in a tag word.
    pub const LENGTH_SHIFT: u32 = 24;

    /// Tag word for a node of `words` payload words whose successor is at
    /// physical address `next` (or [`END`]).
    #[inline(always)]
    pub const fn tag(words: u32, next: u32) -> u32 {
        (words << LENGTH_SHIFT) | (next & ADDRESS_MASK)
    }
}

/// CHCR for a linked-list walk into a device: RAM to device, linked-list
/// sync, started (the GPU's ordering-table DMA).
pub const CHCR_LINKED_LIST_TO_DEVICE: u32 = CHCR_TO_DEVICE | CHCR_SYNC_LINKED | CHCR_START;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpcr_enable_bits_follow_psx_spx() {
        // DPCR bits 3, 7, 11, 15, 19, 23, 27 for channels 0..=6.
        assert_eq!(dpcr_enable(channel::MDEC_IN), 0x0000_0008);
        assert_eq!(dpcr_enable(channel::GPU), 0x0000_0800);
        assert_eq!(dpcr_enable(channel::OTC), 0x0800_0000);
    }

    #[test]
    fn linked_list_tags_pack_length_and_address() {
        assert_eq!(linked_list::tag(3, 0x0012_3456), 0x0312_3456);
        assert_eq!(linked_list::tag(1, linked_list::END), 0x01FF_FFFF);
        // An address above 24 bits cannot spill into the length byte.
        assert_eq!(linked_list::tag(0, 0x8012_3456), 0x0012_3456);
    }

    #[test]
    fn channel_blocks_and_linked_list_control_word() {
        assert_eq!(CHANNEL_BASE + CHANNEL_STRIDE * channel::GPU, 0x1F80_10A0);
        assert_eq!(CHANNEL_BASE + CHANNEL_STRIDE * channel::OTC, 0x1F80_10E0);
        assert_eq!(CHCR_LINKED_LIST_TO_DEVICE, 0x0100_0401);
    }
}
