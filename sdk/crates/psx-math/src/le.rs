// SPDX-License-Identifier: GPL-2.0-or-later
//! Little-endian field readers for cooked blobs and on-disc records.
//!
//! Every cooked format is little-endian and byte-packed, so a field may sit
//! at any offset. These are the readers every parser in the SDK uses (this
//! crate has no dependencies, so `psx-io`, `psx-pack`, `psx-mc`, `psx-seq` and
//! `psx-settings` can reach it; `psx_asset::le` re-exports it); game code that
//! reads its own cooked headers uses the same ones instead of writing
//! `from_le_bytes([b[o], b[o + 1]])` again. Three forms of each width:
//!
//! - `read_*`: panics when the field is past the end, like the slice index it replaces.
//! - `get_*`: `None` when the field is past the end, for parsers that reject bad input.
//! - `read_*_unchecked`: no test, for offsets that are in range by construction.
//!
//! Shapes decide inlining (LLVM prices a caller on the IR it sees), so a few named variants carry
//! the shape a private reader had: `read_*_inline` (the checked form, always inlined, for a header
//! decoder after its own length test), `read_*_ptr` (the unchecked form from a base pointer, for
//! parsers that walk a blob by pointer), and `get_*` is the slice-range form `bytes.get(o..o+n)`.
//! Each of these converted a reader that had been private (psx-pack, psx-mc, psx-fmv, psx-asset
//! hmd8) at zero or negative object size.
//!
//! Each checked reader panics if `offset + width` is past `bytes.len()`, like
//! the slice index it replaces: one length test, then one unaligned load (the
//! R3000's `lwl`/`lwr` pair for a word), where four indexed byte reads cost
//! four tests and four loads. Callers that need a recoverable failure check
//! the length first. The `_unchecked` readers are the same load without the
//! test, for offsets that are in range by construction.
//!
//! The unchecked readers are indexed byte loads joined with `from_le_bytes`, not a
//! `read_unaligned`. The R3000 backend merges four consecutive byte loads into one
//! `lwl`/`lwr` pair either way, so the machine code of one read is the same, but LLVM
//! prices an accessor on the IR it sees before that merge: a one-instruction
//! `read_unaligned` makes every accessor that reads fields look about ten instructions
//! cheaper per field, so the inliner and unroller treat it differently.
//! In Counter-Strike's `map.rs`, adopting the `read_unaligned` form took 3,216 B of free
//! RAM (accessors such as `Map::entity`, `nav_node`, `leaf` and `prop` went from
//! outlined to inlined at every call site, 2 to 11 sites each, and `mark_leaf_faces`
//! grew from 680 to 3,248 B); the same adoption with the byte form gave 2,752 B back.
//! Keep this shape unless the same comparison on a ship link says otherwise.
//!
//! The checked readers (`read_*` and `read_*_inline`) are the other way round, on purpose: a
//! length test and one `read_unaligned`. Their callers are load-time parsers, often in
//! `#[optimize(size)]` functions, where the inliner prices the callee by its IR: the byte form
//! (four loads, shifts and ors) is too dear to inline there, so each of Ocarina's header
//! parsers (`Skm::new`, `Prm::new`, `Col::parse`, `TitleCam::new`) took a call per field and
//! grew by about 1,400 B; with the one-load form they are the size they were before the shape
//! change. A hot accessor that must not be inlined everywhere reads through the unchecked byte
//! form after its own test.

/// Little-endian `u16` at `offset`.
///
/// # Panics
///
/// If `offset + 2` is past `bytes.len()`.
///
/// ```
/// assert_eq!(psx_math::le::read_u16(&[0, 0x34, 0x12], 1), 0x1234);
/// ```
#[inline]
#[must_use]
pub fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    assert!(bytes.len() >= 2 && offset <= bytes.len() - 2);
    // SAFETY: the two bytes at `offset` are inside `bytes`, checked above.
    u16::from_le(unsafe { bytes.as_ptr().add(offset).cast::<u16>().read_unaligned() })
}

/// Little-endian `u32` at `offset`.
///
/// # Panics
///
/// If `offset + 4` is past `bytes.len()`.
///
/// ```
/// assert_eq!(psx_math::le::read_u32(&[0x78, 0x56, 0x34, 0x12], 0), 0x1234_5678);
/// ```
#[inline]
#[must_use]
pub fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    assert!(bytes.len() >= 4 && offset <= bytes.len() - 4);
    // SAFETY: the four bytes at `offset` are inside `bytes`, checked above.
    u32::from_le(unsafe { bytes.as_ptr().add(offset).cast::<u32>().read_unaligned() })
}

/// Little-endian `u16` at `offset`, or `None` if `offset + 2` is past `bytes.len()`.
///
/// ```
/// assert_eq!(psx_math::le::get_u16(&[0, 0x34, 0x12], 1), Some(0x1234));
/// assert_eq!(psx_math::le::get_u16(&[0, 0x34], 1), None);
/// ```
#[inline]
#[must_use]
pub fn get_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let b = bytes.get(offset..offset.wrapping_add(2))?;
    Some(u16::from_le_bytes([b[0], b[1]]))
}

/// Little-endian `u32` at `offset`, or `None` if `offset + 4` is past `bytes.len()`.
///
/// ```
/// assert_eq!(psx_math::le::get_u32(&[0x78, 0x56, 0x34, 0x12], 0), Some(0x1234_5678));
/// assert_eq!(psx_math::le::get_u32(&[0x78, 0x56, 0x34], 0), None);
/// ```
#[inline]
#[must_use]
pub fn get_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset.wrapping_add(4))?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Little-endian `i16` at `offset`, or `None` if `offset + 2` is past `bytes.len()`.
#[inline]
#[must_use]
pub fn get_i16(bytes: &[u8], offset: usize) -> Option<i16> {
    get_u16(bytes, offset).map(|v| v as i16)
}

/// Little-endian `i32` at `offset`, or `None` if `offset + 4` is past `bytes.len()`.
#[inline]
#[must_use]
pub fn get_i32(bytes: &[u8], offset: usize) -> Option<i32> {
    get_u32(bytes, offset).map(|v| v as i32)
}

/// Little-endian `i16` at `offset`.
///
/// # Panics
///
/// If `offset + 2` is past `bytes.len()`.
///
/// ```
/// assert_eq!(psx_math::le::read_i16(&[0xfe, 0xff], 0), -2);
/// ```
#[inline]
#[must_use]
pub fn read_i16(bytes: &[u8], offset: usize) -> i16 {
    read_u16(bytes, offset) as i16
}

/// Little-endian `i32` at `offset`.
///
/// ```
/// assert_eq!(psx_math::le::read_i32(&[0xfe, 0xff, 0xff, 0xff], 0), -2);
/// ```
#[inline]
#[must_use]
pub fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    read_u32(bytes, offset) as i32
}

/// [`read_u16`] without the length test.
///
/// # Safety
///
/// `offset + 2` must not be past `bytes.len()`.
#[inline(always)]
#[must_use]
pub unsafe fn read_u16_unchecked(bytes: &[u8], offset: usize) -> u16 {
    // SAFETY: the caller keeps the two bytes inside `bytes`.
    unsafe {
        u16::from_le_bytes([
            *bytes.get_unchecked(offset),
            *bytes.get_unchecked(offset + 1),
        ])
    }
}

/// [`read_u32`] without the length test.
///
/// # Safety
///
/// `offset + 4` must not be past `bytes.len()`.
#[inline(always)]
#[must_use]
pub unsafe fn read_u32_unchecked(bytes: &[u8], offset: usize) -> u32 {
    // SAFETY: the caller keeps the four bytes inside `bytes`.
    unsafe {
        u32::from_le_bytes([
            *bytes.get_unchecked(offset),
            *bytes.get_unchecked(offset + 1),
            *bytes.get_unchecked(offset + 2),
            *bytes.get_unchecked(offset + 3),
        ])
    }
}

/// [`read_i16`] without the length test.
///
/// # Safety
///
/// `offset + 2` must not be past `bytes.len()`.
#[inline(always)]
#[must_use]
pub unsafe fn read_i16_unchecked(bytes: &[u8], offset: usize) -> i16 {
    // SAFETY: the caller keeps the two bytes inside `bytes`.
    unsafe { read_u16_unchecked(bytes, offset) as i16 }
}

/// [`read_i32`] without the length test.
///
/// # Safety
///
/// `offset + 4` must not be past `bytes.len()`.
#[inline(always)]
#[must_use]
pub unsafe fn read_i32_unchecked(bytes: &[u8], offset: usize) -> i32 {
    // SAFETY: the caller keeps the four bytes inside `bytes`.
    unsafe { read_u32_unchecked(bytes, offset) as i32 }
}

/// [`read_u16`] always inlined: for a header decoder that reads several fields of a slice whose
/// length it has just checked (or is about to), where the call must cost nothing and the
/// bounds tests must fold together. Same value and the same panic as `read_u16`.
///
/// # Panics
///
/// If `offset + 2` is past `bytes.len()`.
#[inline(always)]
#[must_use]
pub fn read_u16_inline(bytes: &[u8], offset: usize) -> u16 {
    assert!(bytes.len() >= 2 && offset <= bytes.len() - 2);
    // SAFETY: the two bytes at `offset` are inside `bytes`, checked above.
    u16::from_le(unsafe { bytes.as_ptr().add(offset).cast::<u16>().read_unaligned() })
}

/// [`read_u32`] always inlined; see [`read_u16_inline`].
///
/// # Panics
///
/// If `offset + 4` is past `bytes.len()`.
#[inline(always)]
#[must_use]
pub fn read_u32_inline(bytes: &[u8], offset: usize) -> u32 {
    assert!(bytes.len() >= 4 && offset <= bytes.len() - 4);
    // SAFETY: the four bytes at `offset` are inside `bytes`, checked above.
    u32::from_le(unsafe { bytes.as_ptr().add(offset).cast::<u32>().read_unaligned() })
}

/// [`get_u16`] in the shape a hand-written Option reader has: the slice-range `get`, the array
/// conversion, `from_le_bytes`, always inlined. Same value as `get_u16`. Use it where `get_u16`
/// costs bytes: in `#[optimize(size)]` header parsers the byte-indexed body of `get_u16` is
/// priced too dear to inline, and this one-load form is not (measured on the three Option readers
/// Ocarina had: `get_*` was 40 B larger over the three sites, this form never larger than the
/// private reader it replaces).
#[inline(always)]
#[must_use]
pub fn get_u16_inline(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?,
    ))
}

/// [`get_u32`] in the shape of [`get_u16_inline`].
#[inline(always)]
#[must_use]
pub fn get_u32_inline(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// [`get_i16`] in the shape of [`get_u16_inline`].
#[inline(always)]
#[must_use]
pub fn get_i16_inline(bytes: &[u8], offset: usize) -> Option<i16> {
    get_u16_inline(bytes, offset).map(|v| v as i16)
}

/// [`get_i32`] in the shape of [`get_u16_inline`].
#[inline(always)]
#[must_use]
pub fn get_i32_inline(bytes: &[u8], offset: usize) -> Option<i32> {
    get_u32_inline(bytes, offset).map(|v| v as i32)
}

/// [`read_u16_unchecked`] from a base pointer: the form a parser that walks a blob by pointer
/// uses (`hmd8`, `hma1`, `tracks`). The offset is added once and the next byte is a constant
/// step from it, which is a different (smaller) shape for LLVM to price than
/// `get_unchecked(offset + 1)`.
///
/// # Safety
///
/// `base + offset .. base + offset + 2` must be readable.
#[inline(always)]
#[must_use]
pub unsafe fn read_u16_ptr(base: *const u8, offset: usize) -> u16 {
    // SAFETY: the caller keeps the two bytes readable.
    unsafe {
        let p = base.add(offset);
        u16::from_le_bytes([p.read(), p.add(1).read()])
    }
}

/// [`read_u32_unchecked`] from a base pointer; see [`read_u16_ptr`].
///
/// # Safety
///
/// `base + offset .. base + offset + 4` must be readable.
#[inline(always)]
#[must_use]
pub unsafe fn read_u32_ptr(base: *const u8, offset: usize) -> u32 {
    // SAFETY: the caller keeps the four bytes readable.
    unsafe {
        let p = base.add(offset);
        u32::from_le_bytes([p.read(), p.add(1).read(), p.add(2).read(), p.add(3).read()])
    }
}

/// Little-endian `i16` at `offset` through one halfword load, for sections whose format keeps
/// the field on an even address.
///
/// # Safety
///
/// `offset + 2` must not be past `bytes.len()` and `bytes.as_ptr().add(offset)` must be 2-byte
/// aligned.
#[inline(always)]
#[must_use]
pub unsafe fn read_i16_aligned_unchecked(bytes: &[u8], offset: usize) -> i16 {
    // SAFETY: the caller keeps the pointer inside `bytes`.
    let at = unsafe { bytes.as_ptr().add(offset) };
    debug_assert_eq!((at as usize) & 1, 0);
    // SAFETY: two readable bytes at an even address, as the caller guarantees.
    i16::from_le(unsafe { at.cast::<i16>().read() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The byte-indexed readers every game wrote, as oracles.
    fn game_u32(d: &[u8], o: usize) -> u32 {
        u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
    }

    #[test]
    fn the_inline_option_readers_equal_the_get_forms_at_every_offset() {
        let data: [u8; 9] = core::array::from_fn(|i| (i as u32 * 91 + 13) as u8);
        for len in 0..=data.len() {
            let d = &data[..len];
            for o in (0..=len + 2).chain([usize::MAX - 3, usize::MAX - 1, usize::MAX]) {
                assert_eq!(get_u16_inline(d, o), get_u16(d, o));
                assert_eq!(get_u32_inline(d, o), get_u32(d, o));
                assert_eq!(get_i16_inline(d, o), get_i16(d, o));
                assert_eq!(get_i32_inline(d, o), get_i32(d, o));
            }
        }
        assert_eq!(
            get_u32_inline(&[0x78, 0x56, 0x34, 0x12], 0),
            Some(0x1234_5678)
        );
        assert_eq!(get_i16_inline(&[0xfe, 0xff], 0), Some(-2));
    }

    #[test]
    fn readers_match_from_le_bytes_at_every_offset_for_every_pattern() {
        // 256 four-byte patterns spread over every alignment.
        for pattern in 0..=255u32 {
            let word = pattern.wrapping_mul(0x0101_0101) ^ (pattern << 11);
            for lead in 0..4 {
                let mut data = [0xa5u8; 12];
                data[lead..lead + 4].copy_from_slice(&word.to_le_bytes());
                assert_eq!(read_u32(&data, lead), word);
                assert_eq!(read_i32(&data, lead), word as i32);
                assert_eq!(read_u16(&data, lead), word as u16);
                assert_eq!(read_i16(&data, lead), word as u16 as i16);
                assert_eq!(read_u32(&data, lead), game_u32(&data, lead));
                // SAFETY: lead + 4 <= 12.
                unsafe {
                    assert_eq!(read_u32_unchecked(&data, lead), word);
                    assert_eq!(read_u16_unchecked(&data, lead), word as u16);
                }
            }
        }
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
            assert_eq!(read_u32(&data, o), game_u32(&data, o));
        }
    }

    #[test]
    fn inline_and_pointer_forms_equal_the_checked_readers() {
        let data: [u8; 24] = core::array::from_fn(|i| (i as u32 * 77 + 5) as u8);
        for o in 0..=data.len() - 4 {
            assert_eq!(read_u32_inline(&data, o), read_u32(&data, o));
            assert_eq!(read_u16_inline(&data, o), read_u16(&data, o));
            // SAFETY: o + 4 <= data.len().
            unsafe {
                assert_eq!(read_u32_ptr(data.as_ptr(), o), read_u32(&data, o));
                assert_eq!(read_u16_ptr(data.as_ptr(), o), read_u16(&data, o));
            }
        }
    }

    #[test]
    #[should_panic]
    fn inline_form_panics_past_the_end() {
        let _ = read_u32_inline(&[1, 2, 3], 0);
    }

    #[test]
    fn get_matches_read_in_range_and_is_none_past_the_end() {
        let data: [u8; 16] = core::array::from_fn(|i| (i as u32 * 91 + 3) as u8);
        for o in 0..=data.len() {
            assert_eq!(
                get_u32(&data, o),
                (o + 4 <= data.len()).then(|| read_u32(&data, o))
            );
            assert_eq!(
                get_u16(&data, o),
                (o + 2 <= data.len()).then(|| read_u16(&data, o))
            );
            assert_eq!(get_i32(&data, o), get_u32(&data, o).map(|v| v as i32));
            assert_eq!(get_i16(&data, o), get_u16(&data, o).map(|v| v as i16));
        }
        assert_eq!(get_u32(&data, usize::MAX), None);
        assert_eq!(get_u16(&data, usize::MAX), None);
        assert_eq!(get_u32(&[], 0), None);
        assert_eq!(get_u16(&[1], 0), None);
    }

    #[test]
    #[should_panic]
    fn read_past_the_end_panics() {
        let _ = read_u32(&[1, 2, 3], 0);
    }

    #[test]
    #[should_panic]
    fn read_one_byte_short_panics() {
        let _ = read_u32(&[1, 2, 3, 4, 5], 2);
    }

    #[test]
    #[should_panic]
    fn offset_overflow_panics() {
        let _ = read_u16(&[1, 2], usize::MAX);
    }
}
