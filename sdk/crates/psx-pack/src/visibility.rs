//! Allocation-free Quake/GoldSrc visibility RLE.
//!
//! Nonzero bytes are literals; zero followed by a count advances over invisible
//! leaves. The caller selects strict container validation or GoldSrc's bounded
//! best-effort policy, and replacement or OR merging, without a second row buffer.

/// Decode exactly one row, rejecting truncation, zero runs and oversized runs.
/// Bytes already decoded remain in `output` on failure.
#[inline]
pub fn decode_strict(input: &[u8], offset: usize, output: &mut [u8]) -> bool {
    decode::<false, true>(input, offset, output)
}

/// OR one strict row into an existing row. Invisible runs leave it unchanged.
/// Bytes already merged remain in `output` on failure.
#[inline]
pub fn merge_strict(input: &[u8], offset: usize, output: &mut [u8]) -> bool {
    decode::<true, true>(input, offset, output)
}

/// Decode a bounded GoldSrc row, clipping oversized runs and leaving a
/// truncated tail invisible. Zero-length runs are consumed, not rejected.
#[inline]
pub fn decode_clamped(input: &[u8], offset: usize, output: &mut [u8]) {
    output.fill(0);
    let _ = decode::<false, false>(input, offset, output);
}

/// OR a bounded GoldSrc row into an existing row, clipping oversized runs and
/// preserving the destination after truncated input or invisible runs.
#[inline]
pub fn merge_clamped(input: &[u8], offset: usize, output: &mut [u8]) {
    let _ = decode::<true, false>(input, offset, output);
}

#[inline]
fn decode<const MERGE: bool, const STRICT: bool>(
    input: &[u8],
    offset: usize,
    output: &mut [u8],
) -> bool {
    let mut source = offset;
    let mut destination = 0usize;
    while destination < output.len() {
        let Some(&value) = input.get(source) else {
            return false;
        };
        source += 1;
        if value != 0 {
            if MERGE {
                output[destination] |= value;
            } else {
                output[destination] = value;
            }
            destination += 1;
            continue;
        }
        let Some(&run) = input.get(source) else {
            return false;
        };
        source += 1;
        let available = output.len() - destination;
        if STRICT && (run == 0 || run as usize > available) {
            return false;
        }
        let end = destination + (run as usize).min(available);
        if !MERGE {
            output[destination..end].fill(0);
        }
        destination = end;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_zero_runs_and_or_merging_preserve_rows() {
        let input = [0xaa, 0x11, 0, 2, 0x80];
        let mut row = [0xcc; 4];
        assert!(decode_strict(&input, 1, &mut row));
        assert_eq!(row, [0x11, 0, 0, 0x80]);
        row = [0x22; 4];
        assert!(merge_strict(&input, 1, &mut row));
        assert_eq!(row, [0x33, 0x22, 0x22, 0xa2]);
    }

    #[test]
    fn strict_failures_preserve_the_decoded_prefix_and_unwritten_tail() {
        for input in [&[0x11][..], &[0x11, 0], &[0x11, 0, 0], &[0x11, 0, 4]] {
            let mut row = [0xcc; 4];
            assert!(!decode_strict(input, 0, &mut row));
            assert_eq!(row, [0x11, 0xcc, 0xcc, 0xcc]);
            let mut merged = [0x22; 4];
            assert!(!merge_strict(input, 0, &mut merged));
            assert_eq!(merged, [0x33, 0x22, 0x22, 0x22]);
        }
        assert!(decode_strict(&[], usize::MAX, &mut []));
        assert!(!decode_strict(&[], usize::MAX, &mut [0]));
    }

    #[test]
    fn goldsrc_clips_runs_accepts_empty_runs_and_zeroes_truncated_tail() {
        for input in [&[0x11][..], &[0x11, 0], &[0x11, 0, 0], &[0x11, 0, 99]] {
            let mut row = [0xcc; 4];
            decode_clamped(input, 0, &mut row);
            assert_eq!(row, [0x11, 0, 0, 0]);
            let mut merged = [0x22; 4];
            merge_clamped(input, 0, &mut merged);
            assert_eq!(merged, [0x33, 0x22, 0x22, 0x22]);
        }
        let mut row = [0xcc; 2];
        decode_clamped(&[0, 0, 0, 0, 0x11, 0x80], 0, &mut row);
        assert_eq!(row, [0x11, 0x80]);
        decode_clamped(&[], usize::MAX, &mut row);
        assert_eq!(row, [0, 0]);
    }
}
