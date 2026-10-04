// SPDX-License-Identifier: GPL-2.0-or-later
//! The MDEC's IDCT matrix, generated from the DCT basis.
//!
//! MDEC command 3 takes 64 signed 16-bit coefficients, sent as 32 words
//! with the even-numbered coefficient in each word's low halfword
//! (psx-spx, "MDEC Decompression", command 3). Coefficient `8k + n` is
//! the 8-point DCT-II basis function of frequency `k` sampled at `n`,
//! scaled to 1.15 fixed point and truncated toward zero:
//!
//! ```text
//! M[0][n] = 32768 / sqrt(2)                      (= 16384 * sqrt(2))
//! M[k][n] = 32768 * cos((2n + 1) * k * pi / 16)   for k = 1..7
//! ```
//!
//! Everything below runs at compile time, in integers: pi from Machin's
//! formula, cosines from their Taylor series in 2.62 fixed point, and the
//! DC term from an integer square root. Nothing here exists at run time
//! except the finished table.

/// One in the 2.62 fixed point used for the intermediate math.
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const ONE: i128 = 1 << 62;

/// Coefficient scale: the matrix is in 1.15 fixed point.
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const SCALE: i128 = 1 << 15;

/// arctan(1/x) in 2.62 fixed point, from its alternating series.
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const fn atan_inv(x: i128) -> i128 {
    let x2 = x * x;
    let mut power = ONE / x; // 1 / x^(2i+1)
    let mut sum = power;
    let mut i = 1;
    while power != 0 {
        power /= x2;
        let term = power / (2 * i + 1);
        if i % 2 == 1 {
            sum -= term;
        } else {
            sum += term;
        }
        i += 1;
    }
    sum
}

/// pi in 2.62 fixed point (Machin: pi = 16 atan(1/5) - 4 atan(1/239)).
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const PI: i128 = 16 * atan_inv(5) - 4 * atan_inv(239);

/// cos(a * pi / 16) in 2.62 fixed point, for `a` in 0..=8 (the first
/// quadrant), from the Taylor series around zero.
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const fn cos_sixteenths(a: i128) -> i128 {
    let theta = PI * a / 16;
    let mut term = ONE;
    let mut sum = ONE;
    let mut i = 1;
    while term != 0 {
        // term(i) = -term(i-1) * theta^2 / ((2i - 1) * 2i)
        term = -(term * theta / ONE) * theta / ONE / ((2 * i - 1) * (2 * i));
        sum += term;
        i += 1;
    }
    sum
}

/// trunc(32768 * cos(m * pi / 16)) for any `m`, folding the angle into
/// the first quadrant and carrying the sign separately.
const fn basis(m: usize) -> i16 {
    // psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
    let m = (m % 32) as i128;
    let (a, negative) = match m {
        0..=8 => (m, false),
        9..=16 => (16 - m, true),
        17..=24 => (m - 16, true),
        _ => (32 - m, false),
    };
    let mut c = cos_sixteenths(a);
    if c < 0 {
        // Only a rounding sliver at a = 8, where the cosine is zero.
        c = 0;
    }
    let magnitude = (SCALE * c / ONE) as i16;
    if negative {
        -magnitude
    } else {
        magnitude
    }
}

/// floor(sqrt(n)), by bisection.
// psx-numeric-allow-next-line: const-evaluated; only the finished i16 table exists at run time
const fn isqrt(n: u64) -> u64 {
    let (mut lo, mut hi) = (0u64, n + 1);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if mid * mid <= n {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The 64 coefficients, frequency-major: `MATRIX[8 * k + n]`.
pub const MATRIX: [i16; 64] = {
    // 32768 / sqrt(2) = sqrt(2^29).
    let dc = isqrt(1 << 29) as i16;
    let mut m = [0i16; 64];
    let mut n = 0;
    while n < 8 {
        m[n] = dc;
        let mut k = 1;
        while k < 8 {
            m[8 * k + n] = basis((2 * n + 1) * k);
            k += 1;
        }
        n += 1;
    }
    m
};

/// [`MATRIX`] packed for the MDEC: word `w` holds coefficient `2w` in its
/// low halfword and `2w + 1` in its high halfword.
pub const MATRIX_WORDS: [u32; 32] = {
    let mut words = [0u32; 32];
    let mut w = 0;
    while w < 32 {
        let lo = MATRIX[2 * w] as u16 as u32;
        let hi = MATRIX[2 * w + 1] as u16 as u32;
        words[w] = lo | (hi << 16);
        w += 1;
    }
    words
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The table the driver uploaded before this module generated it,
    /// captured by running the previous code. The generated one must
    /// match it word for word.
    const PREVIOUS: [u32; 32] = [
        0x5A82_5A82,
        0x5A82_5A82,
        0x5A82_5A82,
        0x5A82_5A82,
        0x6A6D_7D8A,
        0x18F8_471C,
        0xB8E4_E708,
        0x8276_9593,
        0x30FB_7641,
        0x89BF_CF05,
        0xCF05_89BF,
        0x7641_30FB,
        0xE708_6A6D,
        0xB8E4_8276,
        0x7D8A_471C,
        0x9593_18F8,
        0xA57E_5A82,
        0x5A82_A57E,
        0xA57E_5A82,
        0x5A82_A57E,
        0x8276_471C,
        0x6A6D_18F8,
        0xE708_9593,
        0xB8E4_7D8A,
        0x89BF_30FB,
        0xCF05_7641,
        0x7641_CF05,
        0x30FB_89BF,
        0xB8E4_18F8,
        0x8276_6A6D,
        0x9593_7D8A,
        0xE708_471C,
    ];

    #[test]
    fn generated_matrix_matches_the_previous_table() {
        assert_eq!(MATRIX_WORDS, PREVIOUS);
    }

    #[test]
    fn basis_rows_are_orthogonal() {
        // Distinct DCT-II rows are orthogonal. Truncating to 1.15 moves
        // each of the 8 products by less than 2 * 32768, against a row
        // norm near 2^32.
        for a in 0..8 {
            for b in 0..a {
                let dot: i64 = (0..8)
                    .map(|n| i64::from(MATRIX[8 * a + n]) * i64::from(MATRIX[8 * b + n]))
                    .sum();
                assert!(dot.abs() < 1 << 20, "rows {a} and {b}: {dot}");
            }
        }
    }
}
