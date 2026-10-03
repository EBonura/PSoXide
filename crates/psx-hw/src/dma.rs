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
pub const CHCR_SYNC_BLOCK: u32 = 1 << 9;
/// CHCR.9..10 sync mode 2: linked list, walks a chain of packet headers.
pub const CHCR_SYNC_LINKED: u32 = 2 << 9;
/// CHCR.24: start the transfer (busy while set).
pub const CHCR_START: u32 = 1 << 24;
/// CHCR.28: manual-mode trigger (self-clears when the transfer begins).
pub const CHCR_TRIGGER: u32 = 1 << 28;
