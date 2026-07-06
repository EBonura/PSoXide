// SPDX-License-Identifier: GPL-2.0-or-later
//! A small, self-contained LZSS codec for save payloads (feature `compress`).
//!
//! Chosen over LZ4/zlib because it is tiny, `no_std`, allocation-free, and
//! decodes with only the output buffer as its history window -- so a card read
//! can stream compressed frames straight into the caller's buffer with no
//! scratch. Typical structured save data (sparse tables, repeated records)
//! compresses well; already-dense data falls back to being stored verbatim by
//! [`crate::Card::write_compressed`].
//!
//! ## Bitstream
//!
//! Groups of up to 8 tokens, each group prefixed by a flag byte (bit `i`,
//! LSB-first, describes token `i`): `1` = literal (one byte follows), `0` =
//! match (two bytes follow). A match encodes a 12-bit distance (1..=4096) and a
//! 4-bit length (3..=18) as `((dist-1) << 4) | (len-3)`, big-endian.

const WINDOW: usize = 4096;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = MIN_MATCH + 15; // 18

/// Compress `src` into `dst`. Returns the compressed length, or `None` if `dst`
/// is too small to hold the result.
///
/// The match search is a bounded linear scan of the window -- simple and
/// allocation-free. Save writes are deliberate, infrequent actions, so the
/// O(n·window) cost is acceptable; swap in a hash-chain finder if a project
/// compresses large buffers on a hot path.
pub fn compress(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut out = Emitter { dst, pos: 0 };
    let mut i = 0;
    while i < src.len() {
        // Open a group: reserve the flag byte, then fill up to 8 tokens.
        let flag_pos = out.reserve()?;
        let mut flag = 0u8;
        let mut bit = 0;
        while bit < 8 && i < src.len() {
            let (dist, len) = longest_match(src, i);
            if len >= MIN_MATCH {
                let token = (((dist - 1) as u16) << 4) | (len - MIN_MATCH) as u16;
                out.push((token >> 8) as u8)?;
                out.push(token as u8)?;
                i += len;
            } else {
                flag |= 1 << bit;
                out.push(src[i])?;
                i += 1;
            }
            bit += 1;
        }
        out.set(flag_pos, flag);
    }
    Some(out.pos)
}

/// Decompress a full compressed slice into `dst`. Returns the number of bytes
/// produced (should equal the original length). Convenience wrapper over
/// [`decompress_from`].
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Option<usize> {
    let mut i = 0;
    decompress_from(
        || {
            let b = src.get(i).copied();
            if b.is_some() {
                i += 1;
            }
            b
        },
        dst,
    )
}

/// Streaming decompression: pull compressed bytes from `next` and write the
/// decoded output into `dst`, stopping once `dst` is full. Returns the number
/// of bytes produced, or `None` on a malformed stream (a back-reference before
/// the start of output, or input exhausted early).
///
/// Only `dst` is used as history, so no separate window buffer is needed.
pub fn decompress_from<F: FnMut() -> Option<u8>>(mut next: F, dst: &mut [u8]) -> Option<usize> {
    let mut pos = 0;
    while pos < dst.len() {
        let flag = next()?;
        let mut bit = 0;
        while bit < 8 && pos < dst.len() {
            if flag & (1 << bit) != 0 {
                dst[pos] = next()?;
                pos += 1;
            } else {
                let b0 = next()? as u16;
                let b1 = next()? as u16;
                let token = (b0 << 8) | b1;
                let dist = ((token >> 4) + 1) as usize;
                let len = (token & 0xF) as usize + MIN_MATCH;
                if dist > pos {
                    return None; // reference before start of output
                }
                for _ in 0..len {
                    if pos >= dst.len() {
                        break;
                    }
                    dst[pos] = dst[pos - dist];
                    pos += 1;
                }
            }
            bit += 1;
        }
    }
    Some(pos)
}

/// Longest window match for the data at `src[at..]`. Returns `(distance, len)`;
/// `len < MIN_MATCH` means "no useful match".
fn longest_match(src: &[u8], at: usize) -> (usize, usize) {
    let start = at.saturating_sub(WINDOW);
    let max_len = MAX_MATCH.min(src.len() - at);
    let mut best_len = 0;
    let mut best_dist = 0;
    let mut j = start;
    while j < at {
        let mut l = 0;
        while l < max_len && src[j + l] == src[at + l] {
            l += 1;
        }
        if l > best_len {
            best_len = l;
            best_dist = at - j;
            if l == max_len {
                break;
            }
        }
        j += 1;
    }
    (best_dist, best_len)
}

/// Bounds-checked forward writer into a caller buffer.
struct Emitter<'a> {
    dst: &'a mut [u8],
    pos: usize,
}

impl Emitter<'_> {
    fn push(&mut self, b: u8) -> Option<()> {
        *self.dst.get_mut(self.pos)? = b;
        self.pos += 1;
        Some(())
    }
    /// Reserve one byte (a flag slot) and return its index.
    fn reserve(&mut self) -> Option<usize> {
        let p = self.pos;
        self.push(0)?;
        Some(p)
    }
    fn set(&mut self, at: usize, b: u8) {
        self.dst[at] = b;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8]) {
        let mut comp = [0u8; 4096];
        let clen = compress(data, &mut comp).expect("fits");
        let mut out = [0u8; 2048];
        let n = decompress(&comp[..clen], &mut out[..data.len()]).expect("decode");
        assert_eq!(n, data.len());
        assert_eq!(&out[..data.len()], data);
    }

    #[test]
    fn empty() {
        roundtrip(b"");
    }

    #[test]
    fn incompressible_and_runs() {
        roundtrip(b"A");
        roundtrip(b"AAAAAAAAAAAAAAAAAAAAAAAA");
        roundtrip(b"the quick brown fox the quick brown fox the quick brown fox");
        let mut seq = [0u8; 500];
        for (i, b) in seq.iter_mut().enumerate() {
            *b = (i * 7 + (i / 13)) as u8;
        }
        roundtrip(&seq);
    }

    #[test]
    fn sparse_like_a_save() {
        // Mostly zeros with a few records -- the memory-card common case.
        let mut data = [0u8; 1024];
        data[0] = b'S';
        data[1] = b'C';
        for k in 0..8 {
            data[64 * k + 3] = k as u8;
            data[64 * k + 4] = 0xFF;
        }
        roundtrip(&data);
        let mut comp = [0u8; 2048];
        let clen = compress(&data, &mut comp).unwrap();
        assert!(clen < data.len() / 2, "sparse data should shrink a lot");
    }

    #[test]
    fn dst_too_small_is_none() {
        let mut tiny = [0u8; 2];
        assert!(compress(b"hello world this will not fit", &mut tiny).is_none());
    }
}
