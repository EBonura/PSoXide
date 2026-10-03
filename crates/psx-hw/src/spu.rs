//! SPU register addresses.
//!
//! Reference: nocash PSX-SPX "Sound Processing Unit (SPU)".

/// Base of the SPU register bank. Voice, volume and reverb registers sit at
/// fixed offsets from it.
pub const BASE: u32 = 0x1F80_1C00;
/// Control register.
pub const SPUCNT: u32 = 0x1F80_1DAA;
/// Status register.
pub const SPUSTAT: u32 = 0x1F80_1DAE;
/// Sound-RAM transfer address register (`address / 8`). Latches the SPU-RAM
/// cursor for FIFO and DMA uploads and downloads.
pub const TRANSFER_ADDR: u32 = 0x1F80_1DA6;
/// Sound-RAM transfer FIFO (manual-write port). Writes land in SPU RAM only
/// while the control register's transfer mode is manual write (bits 5..4 =
/// 01).
pub const TRANSFER_DATA: u32 = 0x1F80_1DA8;
/// Sound-RAM transfer control register (transfer type; 0x0004 is the normal
/// byte order every game uses).
pub const TRANSFER_CTRL: u32 = 0x1F80_1DAC;
