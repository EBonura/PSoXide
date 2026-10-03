// SPDX-License-Identifier: GPL-2.0-or-later
//! Renamed to [`crate::bitstream`].
//!
//! Every item here forwards to its new path and is deprecated.

use crate::bitstream;

/// Moved to [`bitstream::HEADER_BYTES`].
#[deprecated(note = "moved to `psx_fmv::bitstream::HEADER_BYTES`")]
pub const HEADER_BYTES: usize = bitstream::HEADER_BYTES;
/// Moved to [`bitstream::END_OF_BLOCK`].
#[deprecated(note = "moved to `psx_fmv::bitstream::END_OF_BLOCK`")]
pub const END_OF_BLOCK: u16 = bitstream::END_OF_BLOCK;

/// Moved to [`bitstream::DecodeError`].
#[deprecated(note = "moved to `psx_fmv::bitstream::DecodeError`")]
pub type BsError = bitstream::DecodeError;
/// Moved to [`bitstream::Header`].
#[deprecated(note = "moved to `psx_fmv::bitstream::Header`")]
pub type Header = bitstream::Header;

/// Moved to [`bitstream::decode_frame`].
#[deprecated(note = "moved to `psx_fmv::bitstream::decode_frame`")]
#[inline(always)]
pub fn decode_frame(
    frame: &[u8],
    out: &mut [u16],
    max_macroblocks: u32,
    pump_every: u32,
    pump: &mut impl FnMut(),
) -> Result<usize, bitstream::DecodeError> {
    bitstream::decode_frame(frame, out, max_macroblocks, pump_every, pump)
}
