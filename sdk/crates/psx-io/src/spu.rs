//! SPU register addresses, moved to [`psx_hw::spu`].
//!
//! The SPU driver lives in `psx-spu`.

/// Moved to [`psx_hw::spu::BASE`].
#[deprecated(note = "moved to `psx_hw::spu::BASE`")]
pub const SPU_BASE: u32 = psx_hw::spu::BASE;
/// Moved to [`psx_hw::spu::SPUCNT`].
#[deprecated(note = "moved to `psx_hw::spu::SPUCNT`")]
pub const SPUCNT: u32 = psx_hw::spu::SPUCNT;
/// Moved to [`psx_hw::spu::SPUSTAT`].
#[deprecated(note = "moved to `psx_hw::spu::SPUSTAT`")]
pub const SPUSTAT: u32 = psx_hw::spu::SPUSTAT;
/// Moved to [`psx_hw::spu::TRANSFER_ADDR`].
#[deprecated(note = "moved to `psx_hw::spu::TRANSFER_ADDR`")]
pub const TRANSFER_ADDR: u32 = psx_hw::spu::TRANSFER_ADDR;
/// Moved to [`psx_hw::spu::TRANSFER_DATA`].
#[deprecated(note = "moved to `psx_hw::spu::TRANSFER_DATA`")]
pub const TRANSFER_DATA: u32 = psx_hw::spu::TRANSFER_DATA;
/// Moved to [`psx_hw::spu::TRANSFER_CTRL`].
#[deprecated(note = "moved to `psx_hw::spu::TRANSFER_CTRL`")]
pub const TRANSFER_CTRL: u32 = psx_hw::spu::TRANSFER_CTRL;
