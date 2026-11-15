// SPDX-License-Identifier: GPL-2.0-or-later
//! Integer easing and interpolation: no floats, no 64-bit maths.
//!
//! Time and the results are fixed point, `_q8` with 256 as 1.0 and `_q12`
//! with 4096 as 1.0. Easing curves clamp their input to `0..=ONE` and return
//! `0..=ONE`, so `lerp_q8(a, b, ease_out_quart_q8(t))` is a complete tween.
//!
//! These are the curves the launcher, NitroXide and WipEout each wrote for
//! themselves. They are not interchangeable, so each keeps its own name and
//! unit; the rounding of every one is that of the copy it replaces (shifts,
//! not rounded divisions).

/// 1.0 in Q8.
pub const ONE_Q8: i32 = 256;
/// 1.0 in Q12.
pub const ONE_Q12: i32 = 4096;

/// `v` limited to `lo..=hi`; `i32::clamp` is not `const`.
const fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// `a + (b - a) * t`, `t` in Q8 (not clamped, so `t > 256` extrapolates).
/// The product is a plain `i32` one: `|b - a| * |t|` must fit.
#[must_use]
pub const fn lerp_q8(a: i32, b: i32, t: i32) -> i32 {
    a + (((b - a) * t) >> 8)
}

/// Quartic ease-out, `1 - (1 - t)^4`, Q8: fast start, long settle.
#[must_use]
pub const fn ease_out_quart_q8(t: i32) -> i32 {
    let u = ONE_Q8 - clamp(t, 0, ONE_Q8);
    let u2 = (u * u) >> 8;
    ONE_Q8 - ((u2 * u2) >> 8)
}

/// Cubic ease-in-out, Q8: slow at both ends, fastest in the middle.
#[must_use]
pub const fn ease_in_out_cubic_q8(t: i32) -> i32 {
    let t = clamp(t, 0, ONE_Q8);
    if t < 128 {
        (4 * t * t * t) >> 16
    } else {
        let u = ONE_Q8 - t;
        ONE_Q8 - ((4 * u * u * u) >> 16)
    }
}

/// Quadratic ease-in, `t^2`, Q8.
#[must_use]
pub const fn ease_in_quad_q8(t: i32) -> i32 {
    let t = clamp(t, 0, ONE_Q8);
    (t * t) >> 8
}

/// Smoothstep, `3t^2 - 2t^3`, Q8.
#[must_use]
pub const fn smoothstep_q8(t: i32) -> i32 {
    let t = clamp(t, 0, ONE_Q8);
    ((3 * t * t) >> 8) - (((2 * t * t) >> 8) * t) / 256
}

/// Cubic ease-out, `1 - (1 - t)^3`, Q12: fast at the start, settling at the end.
#[must_use]
pub const fn ease_out_cubic_q12(t: i32) -> i32 {
    let u = ONE_Q12 - clamp(t, 0, ONE_Q12);
    let u2 = (u * u) >> 12;
    ONE_Q12 - ((u2 * u) >> 12)
}

/// Cubic ease-in, `t^3`, Q12: slow at the start, fast at the end.
#[must_use]
pub const fn ease_in_cubic_q12(t: i32) -> i32 {
    let t = clamp(t, 0, ONE_Q12);
    let t2 = (t * t) >> 12;
    (t2 * t) >> 12
}

/// Progress of `t` through the window `[from, to)` as Q8, clamped to `0..=256`.
/// `to` must be greater than `from`.
#[must_use]
pub const fn span_q8(t: i32, from: i32, to: i32) -> i32 {
    clamp(((t - from) * ONE_Q8) / (to - from), 0, ONE_Q8)
}

/// Move `x` toward `target` by `1 / 2^shift` of the gap, landing exactly once
/// it is within `2^shift`.
#[must_use]
pub const fn follow(x: i32, target: i32, shift: u32) -> i32 {
    let gap = target - x;
    if gap.abs() < (1 << shift) {
        target
    } else {
        x + (gap >> shift)
    }
}

/// Piecewise-linear lookup through `knots` of `(x, y)`, sorted by `x`, `x`
/// strictly increasing; clamps to the first and last `y` outside them.
///
/// # Panics
/// If `knots` is empty.
#[must_use]
pub fn knots_q8(x: i32, knots: &[(i32, i32)]) -> i32 {
    if x <= knots[0].0 {
        return knots[0].1;
    }
    for pair in knots.windows(2) {
        if x <= pair[1].0 {
            let t = ((x - pair[0].0) * ONE_Q8) / (pair[1].0 - pair[0].0);
            return lerp_q8(pair[0].1, pair[1].1, t);
        }
    }
    knots[knots.len() - 1].1
}

#[cfg(test)]
mod tests {
    use super::*;

    // The games' copies, kept as the oracle each function must equal.

    // launcher/src/ease.rs
    fn launcher_out(t: i32) -> i32 {
        let u = 256 - t.clamp(0, 256);
        let u2 = (u * u) >> 8;
        256 - ((u2 * u2) >> 8)
    }
    fn launcher_in_out(t: i32) -> i32 {
        let t = t.clamp(0, 256);
        if t < 128 {
            (4 * t * t * t) >> 16
        } else {
            let u = 256 - t;
            256 - ((4 * u * u * u) >> 16)
        }
    }
    fn launcher_quad_in(t: i32) -> i32 {
        let t = t.clamp(0, 256);
        (t * t) >> 8
    }
    fn launcher_lerp(a: i32, b: i32, t: i32) -> i32 {
        a + (((b - a) * t) >> 8)
    }
    fn launcher_span(t: i32, from: i32, to: i32) -> i32 {
        (((t - from) * 256) / (to - from)).clamp(0, 256)
    }
    fn launcher_follow(x: i32, target: i32, shift: u32) -> i32 {
        let d = target - x;
        if d.abs() < (1 << shift) {
            target
        } else {
            x + (d >> shift)
        }
    }
    fn launcher_knots(x: i32, k: &[(i32, i32)]) -> i32 {
        if x <= k[0].0 {
            return k[0].1;
        }
        for w in k.windows(2) {
            if x <= w[1].0 {
                let t = ((x - w[0].0) * 256) / (w[1].0 - w[0].0);
                return launcher_lerp(w[0].1, w[1].1, t);
            }
        }
        k[k.len() - 1].1
    }
    // nitroxide game/src/fx.rs
    fn nitro_ease_out(t: i32) -> i32 {
        let u = 4096 - t.clamp(0, 4096);
        let u2 = (u * u) >> 12;
        4096 - ((u2 * u) >> 12)
    }
    fn nitro_ease_in(t: i32) -> i32 {
        let t = t.clamp(0, 4096);
        let t2 = (t * t) >> 12;
        (t2 * t) >> 12
    }
    // nitroxide sim/src/replay.rs
    fn nitro_lerp(a: i32, b: i32, k: i32) -> i32 {
        a + (((b - a) * k) >> 8)
    }
    fn nitro_smooth(t: i32) -> i32 {
        let t = t.clamp(0, 256);
        ((3 * t * t) >> 8) - (((2 * t * t) >> 8) * t) / 256
    }
    // nitroxide sim/src/victory.rs
    fn nitro_victory_ease_out(t: i32, len: i32) -> i32 {
        let u = (4096 - (t.clamp(0, len) * 4096 / len)).max(0);
        4096 - (((u * u) >> 12) * u) / 4096
    }

    #[test]
    fn q8_curves_equal_the_launcher_copies() {
        for t in -300..=600 {
            assert_eq!(ease_out_quart_q8(t), launcher_out(t), "{t}");
            assert_eq!(ease_in_out_cubic_q8(t), launcher_in_out(t), "{t}");
            assert_eq!(ease_in_quad_q8(t), launcher_quad_in(t), "{t}");
            assert_eq!(smoothstep_q8(t), nitro_smooth(t), "{t}");
        }
        for (a, b) in [(0, 100), (100, 0), (-500, 700), (30000, -30000), (5, 5)] {
            for t in -300..=600 {
                assert_eq!(lerp_q8(a, b, t), launcher_lerp(a, b, t));
                assert_eq!(lerp_q8(a, b, t), nitro_lerp(a, b, t));
            }
        }
        for (from, to) in [(0, 60), (10, 11), (-20, 40), (100, 400)] {
            for t in -50..=500 {
                assert_eq!(span_q8(t, from, to), launcher_span(t, from, to));
            }
        }
        for shift in 1..=6 {
            for x in -300..=300 {
                for target in [-250, 0, 17, 255] {
                    assert_eq!(follow(x, target, shift), launcher_follow(x, target, shift));
                }
            }
        }
        let knots = [(0, 0), (40, 200), (100, 150), (240, 256)];
        for x in -20..=300 {
            assert_eq!(knots_q8(x, &knots), launcher_knots(x, &knots), "{x}");
        }
    }

    #[test]
    fn q12_curves_equal_nitroxides_copies() {
        for t in -6000..=10_000 {
            assert_eq!(ease_out_cubic_q12(t), nitro_ease_out(t), "{t}");
            assert_eq!(ease_in_cubic_q12(t), nitro_ease_in(t), "{t}");
        }
        // victory.rs's ease_out(t, len) is the same curve over `len` ticks.
        for len in [1, 7, 60, 90, 300, 1200] {
            for t in -5..=len + 5 {
                let x = t.clamp(0, len) * 4096 / len;
                assert_eq!(
                    ease_out_cubic_q12(x),
                    nitro_victory_ease_out(t, len),
                    "{t} {len}"
                );
            }
        }
    }

    #[test]
    fn curves_run_zero_to_one_and_are_const() {
        const HALF: i32 = ease_in_out_cubic_q8(128);
        assert_eq!(HALF, 128);
        for f in [
            ease_out_quart_q8,
            ease_in_out_cubic_q8,
            ease_in_quad_q8,
            smoothstep_q8,
        ] {
            assert_eq!((f(0), f(256)), (0, 256));
        }
        for f in [ease_out_cubic_q12, ease_in_cubic_q12] {
            assert_eq!((f(0), f(4096)), (0, 4096));
        }
    }
}
