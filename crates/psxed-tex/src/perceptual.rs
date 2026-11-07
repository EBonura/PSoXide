// SPDX-License-Identifier: GPL-2.0-or-later
//! Oklab, computed with bit-reproducible arithmetic.
//!
//! The quantiser measures colour error perceptually, which matters most for
//! exactly the art the PS1 is full of: large near-black masses where sRGB
//! Euclidean distance badly under-weights the differences a viewer can see.
//!
//! Cooked `.psxt` files are committed artifacts and two fixture generators
//! assert their output byte-for-byte, so the transform has to give the same
//! answer on every machine that runs the cooker. `f64::cbrt` and `f64::powf`
//! do not: they are libm calls whose last ulp is free to differ between
//! platforms, and a single flipped ulp can move a colour across a cluster
//! boundary and change the palette. So the two transcendentals here are
//! computed with Newton iterations built only from `+ - * /`, which IEEE 754
//! requires to be correctly rounded, with a fixed iteration cap rather than a
//! tolerance-based exit. Same input, same bits, anywhere.

use std::sync::OnceLock;

/// Divide the exponent by three in place. The classic double-precision cube
/// root seed: good to a few percent, and integer arithmetic on the bit pattern
/// is exact on every target.
const CBRT_SEED_BIAS: u64 = 0x2A9F_7625_3119_D328;

/// Cube root of a non-negative `a`, via `y <- (2y + a / y²) / 3`.
///
/// Called three times per colour, so it gets the bit-pattern seed; Newton then
/// converges quadratically and six steps take a few-percent seed well past
/// `f64` precision.
fn cbrt(a: f64) -> f64 {
    if a <= 0.0 {
        return 0.0;
    }
    let mut y = f64::from_bits(a.to_bits() / 3 + CBRT_SEED_BIAS);
    for _ in 0..6 {
        y = (2.0 * y + a / (y * y)) / 3.0;
    }
    y
}

/// Fifth root of a non-negative `a`, via `y <- (4y + a / y⁴) / 5`.
///
/// Only feeds the 256-entry sRGB table, built once per process, so it can
/// afford to start from 1.0 and iterate until the step stops moving.
fn fifth_root(a: f64) -> f64 {
    if a <= 0.0 {
        return 0.0;
    }
    let mut y = 1.0f64;
    for _ in 0..64 {
        let y2 = y * y;
        let next = (4.0 * y + a / (y2 * y2)) / 5.0;
        if next == y {
            break;
        }
        y = next;
    }
    y
}

/// The sRGB decode curve's `x^2.4`, split as `x² · (x²)^(1/5)` so it needs
/// only the fifth root above instead of a general `powf`.
fn pow_2_4(x: f64) -> f64 {
    let squared = x * x;
    squared * fifth_root(squared)
}

/// 8-bit sRGB channel -> linear light. 256 entries, built once.
fn linear_table() -> &'static [f64; 256] {
    static TABLE: OnceLock<[f64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0.0f64; 256];
        for (value, slot) in table.iter_mut().enumerate() {
            let c = value as f64 / 255.0;
            *slot = if c <= 0.04045 {
                c / 12.92
            } else {
                pow_2_4((c + 0.055) / 1.055)
            };
        }
        table
    })
}

/// 8-bit sRGB -> Oklab. `L` runs 0..1, `a`/`b` are roughly ±0.4.
pub fn oklab(rgb: [u8; 3]) -> [f64; 3] {
    let table = linear_table();
    let r = table[rgb[0] as usize];
    let g = table[rgb[1] as usize];
    let b = table[rgb[2] as usize];

    let l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b;
    let m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b;
    let s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b;
    let (l, m, s) = (cbrt(l), cbrt(m), cbrt(s));

    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

/// Squared Oklab distance. The quantiser only ever compares distances, so it
/// never needs the square root.
pub fn distance_squared(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    let dl = a[0] - b[0];
    let da = a[1] - b[1];
    let db = a[2] - b[2];
    dl * dl + da * da + db * db
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Newton roots have to agree with the platform libm to well inside
    /// the precision the quantiser cares about; they exist for reproducibility,
    /// not because we want different numbers.
    #[test]
    fn newton_roots_track_libm() {
        for step in 0..=1000 {
            let x = step as f64 / 1000.0;
            assert!((cbrt(x) - x.cbrt()).abs() < 1e-12, "cbrt({x})");
            assert!(
                (fifth_root(x) - x.powf(0.2)).abs() < 1e-12,
                "fifth_root({x})"
            );
            assert!((pow_2_4(x) - x.powf(2.4)).abs() < 1e-12, "pow_2_4({x})");
        }
    }

    #[test]
    fn oklab_reference_points() {
        // Reference values from the Oklab definition: white is L=1 on the
        // achromatic axis, black is the origin, and greys stay achromatic.
        let white = oklab([255, 255, 255]);
        assert!((white[0] - 1.0).abs() < 1e-6, "{white:?}");
        assert!(white[1].abs() < 1e-6 && white[2].abs() < 1e-6, "{white:?}");

        assert_eq!(oklab([0, 0, 0]), [0.0, 0.0, 0.0]);

        let grey = oklab([128, 128, 128]);
        assert!(grey[1].abs() < 1e-6 && grey[2].abs() < 1e-6, "{grey:?}");
        assert!(grey[0] > 0.0 && grey[0] < 1.0, "{grey:?}");
    }

    /// Oklab's whole reason for being here: near black, one 8-bit step is a
    /// far bigger perceptual move than one step near white, and sRGB
    /// Euclidean distance cannot see that.
    #[test]
    fn dark_steps_separate_further_than_bright_steps() {
        let dark = distance_squared(&oklab([0, 0, 0]), &oklab([8, 8, 8]));
        let bright = distance_squared(&oklab([240, 240, 240]), &oklab([248, 248, 248]));
        assert!(dark > bright * 10.0, "dark {dark} vs bright {bright}");
    }
}
