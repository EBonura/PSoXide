//! LZ compression of far-panel tiles.
//!
//! A tile is one atlas slot of [`TILE_BYTES`] bytes (63 rows of 64 4-bit texels and the
//! palette row). The disc holds it as an LZ4 block: a token whose high nibble is the literal
//! count and low nibble the match length minus four (15 in either nibble is continued by
//! bytes that add up until one is below 255), the literals, then a 16-bit little-endian
//! offset back into the output. The last sequence has literals only. A tile that does not
//! shrink is stored as it is, so a block of [`TILE_BYTES`] bytes is a plain copy.
//!
//! The guest decodes with [`decode_tile`] into the upload's staging row buffer; the cook
//! encodes with [`encode_tile`].

/// Bytes of a decoded tile.
pub const TILE_BYTES: usize = 2048;

/// Why a block did not decode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TileLzError {
    /// The block ended inside a sequence.
    Truncated,
    /// A match reached before the start of the output.
    BadOffset,
    /// The block would write past the end of the tile.
    Overrun,
    /// The block ended before the tile was full.
    Short,
}

/// Copy `n` bytes from `src` to `dst`, four at a time where it can (the R3000 has `lwl`/`lwr` and
/// `swl`/`swr`, so an unaligned word costs a few cycles; a byte loop costs six a byte, and a
/// call to `memcpy` more than that for the short runs a tile is made of).
///
/// With `dst` after `src` the copy may overlap, as a match does, provided `dst - src >= 4`: each
/// word then reads only bytes that were already written.
///
/// # Safety
/// `src` must be valid for `n` reads and `dst` for `n` writes.
#[inline(always)]
unsafe fn copy_forward(dst: *mut u8, src: *const u8, n: usize) {
    let mut i = 0;
    while i + 4 <= n {
        // SAFETY: `i + 4 <= n`, within both ranges (the caller's contract).
        unsafe {
            dst.add(i)
                .cast::<u32>()
                .write_unaligned(src.add(i).cast::<u32>().read_unaligned());
        }
        i += 4;
    }
    while i < n {
        // SAFETY: `i < n`.
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
}

/// Write `n` copies of `byte` at `dst`, four at a time.
///
/// # Safety
/// `dst` must be valid for `n` writes.
#[inline(always)]
unsafe fn fill(dst: *mut u8, byte: u8, n: usize) {
    let word = u32::from(byte) * 0x0101_0101;
    let mut i = 0;
    while i + 4 <= n {
        // SAFETY: `i + 4 <= n`.
        unsafe { dst.add(i).cast::<u32>().write_unaligned(word) };
        i += 4;
    }
    while i < n {
        // SAFETY: `i < n`.
        unsafe { *dst.add(i) = byte };
        i += 1;
    }
}

/// Decode the block `src` into `out`.
///
/// A block of exactly [`TILE_BYTES`] bytes is a stored tile and is copied.
pub fn decode_tile(src: &[u8], out: &mut [u8; TILE_BYTES]) -> Result<(), TileLzError> {
    if src.len() == TILE_BYTES {
        out.copy_from_slice(src);
        return Ok(());
    }
    let (mut i, mut o) = (0usize, 0usize);
    let (input, output) = (src.as_ptr(), out.as_mut_ptr());
    loop {
        let token = *src.get(i).ok_or(TileLzError::Truncated)?;
        i += 1;
        let mut literals = usize::from(token >> 4);
        if literals == 15 {
            loop {
                let b = *src.get(i).ok_or(TileLzError::Truncated)?;
                i += 1;
                literals += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        if literals > src.len() - i {
            return Err(TileLzError::Truncated);
        }
        if literals > TILE_BYTES - o {
            return Err(TileLzError::Overrun);
        }
        // SAFETY: the checks above leave `literals` bytes to read at `i` and to write at `o`.
        unsafe { copy_forward(output.add(o), input.add(i), literals) };
        i += literals;
        o += literals;
        if i == src.len() {
            break;
        }
        let offset = match src.get(i..i + 2) {
            Some(&[lo, hi]) => usize::from(lo) | usize::from(hi) << 8,
            _ => return Err(TileLzError::Truncated),
        };
        i += 2;
        let mut length = usize::from(token & 15) + 4;
        if token & 15 == 15 {
            loop {
                let b = *src.get(i).ok_or(TileLzError::Truncated)?;
                i += 1;
                length += usize::from(b);
                if b != 255 {
                    break;
                }
            }
        }
        if offset == 0 || offset > o {
            return Err(TileLzError::BadOffset);
        }
        if length > TILE_BYTES - o {
            return Err(TileLzError::Overrun);
        }
        // SAFETY: `offset <= o` puts the source inside what was written, and `length` bytes
        // fit after `o`. An offset of four or more is read through `copy_forward`, which
        // handles the overlap; a run of one byte is a fill; two and three go a byte at a time.
        unsafe {
            let at = output.add(o);
            if offset >= 4 {
                copy_forward(at, at.sub(offset), length);
            } else if offset == 1 {
                fill(at, *at.sub(1), length);
            } else {
                for k in 0..length {
                    *at.add(k) = *at.add(k).sub(offset);
                }
            }
        }
        o += length;
    }
    if o == TILE_BYTES {
        Ok(())
    } else {
        Err(TileLzError::Short)
    }
}

pub use encode::encode_tile;

/// The host-side encoder; the guest never calls it, so the linker drops it.
mod encode {
    use super::TILE_BYTES;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Shortest match worth a sequence.
    const MIN_MATCH: usize = 4;
    /// Candidate offsets kept per position when parsing.
    const CANDIDATES: usize = 16;
    /// Longest offset (16 bits).
    const WINDOW: usize = 65535;

    /// Bytes the length `n` adds past its nibble.
    fn extra(n: usize) -> usize {
        if n >= 15 {
            1 + (n - 15) / 255
        } else {
            0
        }
    }

    /// The longest matches ending the search at `pos`: `(length, offset)` for the offsets that
    /// match longest, best first.
    fn matches(data: &[u8], pos: usize) -> Vec<(usize, usize)> {
        let mut found: Vec<(usize, usize)> = Vec::new();
        let start = pos.saturating_sub(WINDOW);
        for from in (start..pos).rev() {
            if data[from] != data[pos] {
                continue;
            }
            let mut length = 0;
            while pos + length < data.len() && data[from + length] == data[pos + length] {
                length += 1;
            }
            if length >= MIN_MATCH {
                found.push((length, pos - from));
            }
        }
        // Longest first; among equals the nearest offset (it was found first).
        found.sort_by(|a, b| b.0.cmp(&a.0));
        found.truncate(CANDIDATES);
        found
    }

    /// Encode a tile as the smallest block this parser finds, or the tile itself when no
    /// block is smaller.
    ///
    /// # Panics
    /// If `tile` is not [`TILE_BYTES`] long.
    pub fn encode_tile(tile: &[u8]) -> Vec<u8> {
        assert_eq!(tile.len(), TILE_BYTES, "a tile is {TILE_BYTES} bytes");
        let n = tile.len();
        // best[p]: fewest bytes to encode tile[..p] as sequences whose last one ends a match at
        // p, counting a literal as one byte and a sequence's token and offset as three.
        // `via[p]`: the sequence that gets there, as (start of the literals, match length,
        // offset).
        let inf = usize::MAX / 2;
        let mut best = vec![inf; n + 1];
        let mut via: Vec<(usize, usize, usize)> = vec![(0, 0, 0); n + 1];
        best[0] = 0;
        for p in 0..n {
            if best[p] == inf {
                continue;
            }
            for (length, offset) in matches(tile, p) {
                // Every shorter length is as good a candidate at its own end.
                for l in MIN_MATCH..=length {
                    let cost = best[p] + 3 + extra(l - 4);
                    if cost < best[p + l] {
                        best[p + l] = cost;
                        via[p + l] = (p, l, offset);
                    }
                }
            }
            // Literals carry on from here: a boundary at p + 1 costs one more byte; modelled as
            // a literal "match" of length 1.
            let cost = best[p] + 1;
            if cost < best[p + 1] {
                best[p + 1] = cost;
                via[p + 1] = (p, 0, 0);
            }
        }
        // Walk back from the end collecting the matches (end, length, offset); the bytes
        // between two are literals.
        let mut seqs: Vec<(usize, usize, usize)> = Vec::new();
        let mut p = n;
        while p > 0 {
            let (from, l, offset) = via[p];
            if l != 0 {
                seqs.push((p, l, offset));
            }
            p = from;
        }
        seqs.reverse();
        let mut out = Vec::with_capacity(n);
        let mut cursor = 0usize;
        for &(end, l, offset) in &seqs {
            let start = end - l;
            let literals = start - cursor;
            let token_l = literals.min(15) as u8;
            let token_m = (l - 4).min(15) as u8;
            out.push(token_l << 4 | token_m);
            if literals >= 15 {
                let mut rest = literals - 15;
                while rest >= 255 {
                    out.push(255);
                    rest -= 255;
                }
                out.push(rest as u8);
            }
            out.extend_from_slice(&tile[cursor..start]);
            out.push(offset as u8);
            out.push((offset >> 8) as u8);
            if l - 4 >= 15 {
                let mut rest = l - 4 - 15;
                while rest >= 255 {
                    out.push(255);
                    rest -= 255;
                }
                out.push(rest as u8);
            }
            cursor = end;
        }
        let literals = n - cursor;
        out.push((literals.min(15) as u8) << 4);
        if literals >= 15 {
            let mut rest = literals - 15;
            while rest >= 255 {
                out.push(255);
                rest -= 255;
            }
            out.push(rest as u8);
        }
        out.extend_from_slice(&tile[cursor..]);
        if out.len() >= TILE_BYTES {
            tile.to_vec()
        } else {
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn roundtrip(tile: &[u8]) -> usize {
        let block = encode_tile(tile);
        let mut out = [0u8; TILE_BYTES];
        decode_tile(&block, &mut out).expect("decodes");
        assert_eq!(&out[..], tile);
        block.len()
    }

    fn noise(seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..TILE_BYTES)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn an_empty_tile_shrinks_to_a_few_bytes() {
        assert!(roundtrip(&[0u8; TILE_BYTES]) < 24);
    }

    #[test]
    fn a_tile_that_does_not_shrink_is_stored_whole() {
        assert_eq!(roundtrip(&noise(7)), TILE_BYTES);
    }

    #[test]
    fn patterns_and_runs_round_trip() {
        let mut tile = std::vec![0u8; TILE_BYTES];
        for (k, byte) in tile.iter_mut().enumerate() {
            *byte = if k % 97 < 40 { 0x11 } else { (k / 32) as u8 };
        }
        assert!(roundtrip(&tile) < TILE_BYTES / 2);
        let mut halves = noise(3);
        halves[1024..].copy_from_slice(&noise(3)[..1024]);
        assert!(roundtrip(&halves) < 1100);
    }

    #[test]
    fn short_offsets_repeat_bytes_as_they_are_written() {
        // A hand-made block: literal "ab", then a match of 10 at offset 2, then fill the rest
        // with zero bytes through an offset-1 match.
        let mut block = Vec::new();
        block.extend_from_slice(&[0x26, b'a', b'b', 2, 0]);
        // 2 + 10 = 12 bytes so far; one literal, then offset 1 for the remaining 2035.
        let rest = TILE_BYTES - 12 - 1;
        block.push(0x1f);
        block.push(b'z');
        block.extend_from_slice(&[1, 0]);
        let mut ext = rest - 4 - 15;
        while ext >= 255 {
            block.push(255);
            ext -= 255;
        }
        block.push(ext as u8);
        block.push(0x00);
        let mut out = [0u8; TILE_BYTES];
        decode_tile(&block, &mut out).expect("decodes");
        assert_eq!(&out[..12], b"ababababababab"[..12].as_ref());
        assert_eq!(out[12], b'z');
        assert!(out[13..].iter().all(|&b| b == b'z'));
    }

    #[test]
    fn corrupt_blocks_are_refused_without_panicking() {
        let good = encode_tile(&[5u8; TILE_BYTES]);
        let mut out = [0u8; TILE_BYTES];
        for cut in 0..good.len() {
            let _ = decode_tile(&good[..cut], &mut out);
        }
        assert_eq!(decode_tile(&[], &mut out), Err(TileLzError::Truncated));
        // Offset past the start.
        assert_eq!(
            decode_tile(&[0x10, 7, 9, 0, 0x00], &mut out),
            Err(TileLzError::BadOffset)
        );
        // Ends early.
        assert_eq!(decode_tile(&[0x10, 7], &mut out), Err(TileLzError::Short));
        for seed in 0..64 {
            let junk = noise(seed);
            let _ = decode_tile(&junk[..300 + usize::from(seed as u8)], &mut out);
        }
    }

    #[test]
    fn tiles_of_every_texture_round_trip() {
        // Runs of zeros, repeated rows, short repeats at every distance, and noise, mixed.
        let mut x = 12345u32;
        let mut next = move || {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            x >> 16
        };
        for round in 0..200 {
            let mut tile = std::vec![0u8; TILE_BYTES];
            let mut at = 0;
            while at < TILE_BYTES {
                let len = (next() as usize % 90 + 1).min(TILE_BYTES - at);
                match next() % 5 {
                    0 => {}
                    1 => tile[at..at + len].fill((next() & 15) as u8 * 0x11),
                    2 => {
                        for k in 0..len {
                            tile[at + k] = (next() & 0xff) as u8;
                        }
                    }
                    3 if at >= 32 => {
                        let back = [1usize, 2, 3, 4, 5, 7, 32][next() as usize % 7].min(at);
                        for k in 0..len {
                            tile[at + k] = tile[at + k - back];
                        }
                    }
                    _ => {
                        for k in 0..len {
                            tile[at + k] = if k % 7 < 3 { 0 } else { (next() & 3) as u8 };
                        }
                    }
                }
                at += len;
            }
            let block = encode_tile(&tile);
            let mut out = [0u8; TILE_BYTES];
            decode_tile(&block, &mut out).unwrap_or_else(|e| panic!("round {round}: {e:?}"));
            assert_eq!(&out[..], &tile[..], "round {round}");
        }
    }
}
