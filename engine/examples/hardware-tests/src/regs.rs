// SPDX-License-Identifier: GPL-2.0-or-later
//! Memory-controller and CPU-local registers psx-io has no module for.

/// SPU bus delay/size, `0x1F801014`. The BIOS boot value `0x200931E1` leaves
/// bits 24-27 (the DMA timing override) zero, and in that mode silicon
/// corrupts the first FIFO halfword of each SPU-to-RAM DMA block. Setting
/// `0x0200_0000` makes readback faithful (precision values 036-038).
pub(crate) const SPU_DELAY: u32 = 0x1F80_1014;
/// `0x1F801060`. Bit 7: "delay on simultaneous CODE+DATA fetch from RAM".
pub(crate) const RAM_SIZE: u32 = 0x1F80_1060;
/// `0xFFFE0130`, CPU-local. Bits 12-17 are only "supposedly" documented.
pub(crate) const CACHE_CONTROL: u32 = 0xFFFE_0130;
