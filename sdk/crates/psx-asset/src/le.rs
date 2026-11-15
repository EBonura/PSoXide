// SPDX-License-Identifier: GPL-2.0-or-later
//! Bounds-checked little-endian field readers for cooked blobs.
//!
//! Every cooked format is little-endian and byte-packed, so a field may sit
//! at any offset. These are the readers the parsers in this crate use; game
//! code that reads its own cooked headers uses the same ones instead of
//! writing `from_le_bytes([b[o], b[o + 1]])` again.
//!
//! Each reader panics if `offset + width` is past `bytes.len()` (the indexing
//! panic), like the slice index it replaces. Callers that need a recoverable
//! failure check the length first.

/// Little-endian `u16` at `offset`.
///
/// ```
/// assert_eq!(psx_asset::le::read_u16(&[0, 0x34, 0x12], 1), 0x1234);
/// ```
#[inline]
#[must_use]
pub fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

/// Little-endian `u32` at `offset`.
///
/// ```
/// assert_eq!(psx_asset::le::read_u32(&[0x78, 0x56, 0x34, 0x12], 0), 0x1234_5678);
/// ```
#[inline]
#[must_use]
pub fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

/// Little-endian `i16` at `offset`.
///
/// ```
/// assert_eq!(psx_asset::le::read_i16(&[0xfe, 0xff], 0), -2);
/// ```
#[inline]
#[must_use]
pub fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_match_from_le_bytes_at_every_offset() {
        let data: [u8; 32] = core::array::from_fn(|i| (i as u32 * 53 + 7) as u8);
        for o in 0..data.len() - 3 {
            assert_eq!(
                read_u16(&data, o),
                u16::from_le_bytes(data[o..o + 2].try_into().unwrap())
            );
            assert_eq!(
                read_i16(&data, o),
                i16::from_le_bytes(data[o..o + 2].try_into().unwrap())
            );
            assert_eq!(
                read_u32(&data, o),
                u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
            );
        }
    }

    #[test]
    #[should_panic]
    fn read_past_the_end_panics() {
        let _ = read_u32(&[1, 2, 3], 0);
    }
}
