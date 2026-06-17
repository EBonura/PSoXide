//! SPU MMIO base.

/// Base address of the Sound Processing Unit's MMIO bank.
/// Voice / volume / reverb registers are stamped at fixed
/// offsets from this address; see the SPUCNT / SPUSTAT
/// constants below for the two non-voice registers the
/// emulator surfaces today.
pub const SPU_BASE: u32 = 0x1F80_1C00;
/// SPU control register (`SPUCNT`).
pub const SPUCNT: u32 = 0x1F80_1DAA;
/// SPU status register (`SPUSTAT`).
pub const SPUSTAT: u32 = 0x1F80_1DAE;
/// Sound-RAM data-transfer address register (`addr / 8`). Latches the
/// SPU-RAM cursor for FIFO / DMA uploads and downloads.
pub const TRANSFER_ADDR: u32 = 0x1F80_1DA6;
/// Sound-RAM data-transfer FIFO (manual-write port). Writes land in SPU
/// RAM only while SPUCNT's transfer mode is Manual-Write (bits 5..4 = 01).
pub const TRANSFER_DATA: u32 = 0x1F80_1DA8;
/// Sound-RAM data-transfer control register (transfer type; 0x0004 = the
/// normal byte order every game uses).
pub const TRANSFER_CTRL: u32 = 0x1F80_1DAC;
