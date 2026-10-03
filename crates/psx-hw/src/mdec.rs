//! MDEC (motion decoder) registers, command words and status bits.
//!
//! Reference: nocash PSX-SPX "Macroblock Decoder (MDEC)".

/// Command and parameter register (write); decoded data (read).
pub const MDEC0: u32 = 0x1F80_1820;
/// Control register (write); status register (read).
pub const MDEC1: u32 = 0x1F80_1824;

/// Decode command, 15bpp output (bits 28..27 = 3).
pub const DECODE_15BPP: u32 = 0x3800_0000;
/// Decode command, 24bpp output (bits 28..27 = 2).
pub const DECODE_24BPP: u32 = 0x3000_0000;
/// Decode command flag: set bit 15 on every 15bpp pixel (mask /
/// semi-transparency bit).
pub const DECODE_STP: u32 = 0x0200_0000;
/// Command 2 for luma and chroma: 32 parameter words follow.
pub const COMMAND_SET_QUANT: u32 = 0x4000_0001;
/// Command 3: 32 parameter words follow.
pub const COMMAND_SET_SCALE: u32 = 0x6000_0000;

/// Status: data-out FIFO empty.
pub const STATUS_OUT_EMPTY: u32 = 1 << 31;
/// Status: data-in FIFO full.
pub const STATUS_IN_FULL: u32 = 1 << 30;
/// Status: a command is receiving or processing parameters.
pub const STATUS_BUSY: u32 = 1 << 29;
/// Status: data-in request (DMA0 enabled and the MDEC wants data).
pub const STATUS_IN_REQUEST: u32 = 1 << 28;
/// Status: data-out request.
pub const STATUS_OUT_REQUEST: u32 = 1 << 27;

/// Control: abort everything and reset. Tables survive it.
pub const CONTROL_RESET: u32 = 0x8000_0000;
/// Control: enable the DMA0 and DMA1 requests.
pub const CONTROL_ENABLE_DMA: u32 = 0x6000_0000;
