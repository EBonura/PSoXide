// SPDX-License-Identifier: GPL-2.0-or-later
//! Run-length data: what [`crate::bitstream::decode_frame`] produces and the
//! MDEC decodes, and the lengths the MDEC can be handed.
//!
//! The decode command carries the run-length length as a 16-bit word count,
//! and DMA0 feeds it in 32-word blocks, so a length the MDEC can take is a
//! non-zero multiple of 32 words no larger than [`MAX_WORDS`]. A zero block
//! count would not mean "nothing": the DMA controller reads it as 65,536
//! blocks.

/// DMA block size the MDEC channels use, in words.
pub const BLOCK_WORDS: usize = 32;

/// Longest run-length input one decode command can announce: the largest
/// multiple of [`BLOCK_WORDS`] that fits the command's 16-bit word count.
pub const MAX_WORDS: usize = 0xFFFF / BLOCK_WORDS * BLOCK_WORDS;

/// A run-length length the MDEC cannot be handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RleLengthError {
    /// No words: the DMA block count would be 0, which means 65,536 blocks.
    Empty,
    /// Not a whole number of 32-word DMA blocks, so the decode command's word
    /// count and the DMA length would disagree.
    NotWholeBlocks,
    /// More than [`MAX_WORDS`]: the decode command's 16-bit word count would
    /// wrap.
    TooLong,
    /// More words than the buffer holds.
    PastBuffer,
}

impl core::fmt::Display for RleLengthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Empty => "no run-length words",
            Self::NotWholeBlocks => "run-length words not a multiple of 32",
            Self::TooLong => "run-length words exceed the decode command's 16-bit count",
            Self::PastBuffer => "run-length words past the end of the buffer",
        })
    }
}

/// DMA0 block count for `words` run-length words, or why the MDEC cannot be
/// handed that many.
pub const fn dma_block_count(words: usize) -> Result<u16, RleLengthError> {
    if words == 0 {
        return Err(RleLengthError::Empty);
    }
    if !words.is_multiple_of(BLOCK_WORDS) {
        return Err(RleLengthError::NotWholeBlocks);
    }
    if words > MAX_WORDS {
        return Err(RleLengthError::TooLong);
    }
    // At most MAX_WORDS / 32 = 2047, so the cast is exact.
    Ok((words / BLOCK_WORDS) as u16)
}

/// Owned run-length storage of `WORDS` words: written as halfwords by
/// [`crate::bitstream::decode_frame`], read as words by the MDEC.
///
/// One value owns the memory and hands out one view at a time, so the
/// halfword writer and the word reader can never alias the way two slices
/// cast from one buffer do.
#[derive(Clone, Debug)]
#[repr(transparent)]
pub struct RleBuffer<const WORDS: usize>([u32; WORDS]);

impl<const WORDS: usize> RleBuffer<WORDS> {
    /// A zeroed buffer; `const`, so it can be a `static`.
    pub const fn new() -> Self {
        Self([0; WORDS])
    }

    /// The buffer as `2 * WORDS` halfwords, for [`crate::bitstream::decode_frame`].
    pub fn as_halfwords_mut(&mut self) -> &mut [u16] {
        // SAFETY: `[u32; WORDS]` is `4 * WORDS` initialised bytes aligned
        // for `u16`, every bit pattern is a valid `u16`, and the exclusive
        // borrow of `self` keeps any other view out while this one lives.
        unsafe { core::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast::<u16>(), WORDS * 2) }
    }

    /// The buffer as words, for the MDEC decode (`mdec::decode`, guest only).
    pub const fn as_words(&self) -> &[u32] {
        &self.0
    }
}

impl<const WORDS: usize> Default for RleBuffer<WORDS> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_counts_cover_every_boundary() {
        assert_eq!(dma_block_count(0), Err(RleLengthError::Empty));
        assert_eq!(dma_block_count(8), Err(RleLengthError::NotWholeBlocks));
        assert_eq!(dma_block_count(31), Err(RleLengthError::NotWholeBlocks));
        assert_eq!(dma_block_count(32), Ok(1));
        assert_eq!(dma_block_count(33), Err(RleLengthError::NotWholeBlocks));
        assert_eq!(dma_block_count(MAX_WORDS), Ok(2047));
        assert_eq!(MAX_WORDS, 0xFFE0);
        assert_eq!(
            dma_block_count(MAX_WORDS + BLOCK_WORDS),
            Err(RleLengthError::TooLong)
        );
        // 32 * 65,536 words: the old `as u16` cast made this 0 blocks.
        assert_eq!(dma_block_count(32 << 16), Err(RleLengthError::TooLong));
    }

    /// Halfwords written through one view are the little-endian halves of
    /// the words the other view reads (Miri checks the two never alias).
    #[test]
    fn buffer_views_share_storage_without_aliasing() {
        let mut buffer = RleBuffer::<4>::new();
        let halfwords = buffer.as_halfwords_mut();
        assert_eq!(halfwords.len(), 8);
        halfwords[0] = 0x1111;
        halfwords[1] = 0x2222;
        halfwords[7] = 0xFE00;
        let expected = if cfg!(target_endian = "little") {
            [0x2222_1111, 0, 0, 0xFE00_0000]
        } else {
            [0x1111_2222, 0, 0, 0x0000_FE00]
        };
        assert_eq!(buffer.as_words(), &expected);
        buffer.as_halfwords_mut()[2] = 0xAAAA;
        assert_eq!(buffer.as_words()[1] & 0xFFFF, 0xAAAA);
    }
}
