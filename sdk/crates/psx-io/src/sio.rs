//! SIO0 register addresses, moved to [`psx_hw::sio::sio0`].
//!
//! The controller and memory-card protocols live in `psx-pad` and `psx-mc`.

/// Moved to [`psx_hw::sio::sio0::DATA`].
#[deprecated(note = "moved to `psx_hw::sio::sio0::DATA`")]
pub const DATA: u32 = psx_hw::sio::sio0::DATA;
/// Moved to [`psx_hw::sio::sio0::STAT`].
#[deprecated(note = "moved to `psx_hw::sio::sio0::STAT`")]
pub const STAT: u32 = psx_hw::sio::sio0::STAT;
/// Moved to [`psx_hw::sio::sio0::MODE`].
#[deprecated(note = "moved to `psx_hw::sio::sio0::MODE`")]
pub const MODE: u32 = psx_hw::sio::sio0::MODE;
/// Moved to [`psx_hw::sio::sio0::CTRL`].
#[deprecated(note = "moved to `psx_hw::sio::sio0::CTRL`")]
pub const CTRL: u32 = psx_hw::sio::sio0::CTRL;
/// Moved to [`psx_hw::sio::sio0::BAUD`].
#[deprecated(note = "moved to `psx_hw::sio::sio0::BAUD`")]
pub const BAUD: u32 = psx_hw::sio::sio0::BAUD;
