// SPDX-License-Identifier: GPL-2.0-or-later
//! The one quaternion-to-matrix decode both animation formats use ([`crate::hma1`] keys and
//! [`crate::tracks`] keys): Q12 unit quaternion in, a rotation matrix of Q12 `i16` cells out.

/// A rotation matrix from the Q12 quaternion `[x, y, z, w]`.
///
/// With `renorm` the quaternion is first nudged back to unit length by one step of the
/// inverse-square-root series (a key interpolated between two others is slightly short);
/// without it the key is taken as already unit (an exact stored key, or a frame on a key).
#[inline(always)]
pub(crate) fn to_matrix(q: [i32; 4], renorm: bool) -> [[i16; 3]; 3] {
    let [mut x, mut y, mut z, mut w] = q;
    if renorm {
        // One Newton-style step of the inverse square root around unit length. The series is only
        // meaningful while the length error is at most one unit; clamping keeps a damaged key
        // (components up to 12224) from overflowing `3 * e * e`, and leaves every in-range key as it was.
        let n = (x * x + y * y + z * z + w * w + 2048) >> 12;
        let e = (4096 - n).clamp(-4096, 4096);
        let r = 4096 + (e >> 1) + ((3 * e * e) >> 15);
        x = (x * r + 2048) >> 12;
        y = (y * r + 2048) >> 12;
        z = (z * r + 2048) >> 12;
        w = (w * r + 2048) >> 12;
    }
    let m2 = |a: i32, b: i32| (a * b + 1024) >> 11;
    let (xx, yy, zz) = (m2(x, x), m2(y, y), m2(z, z));
    let (xy, xz, yz) = (m2(x, y), m2(x, z), m2(y, z));
    let (wx, wy, wz) = (m2(w, x), m2(w, y), m2(w, z));
    [
        [
            (4096 - (yy + zz)) as i16,
            (xy - wz) as i16,
            (xz + wy) as i16,
        ],
        [
            (xy + wz) as i16,
            (4096 - (xx + zz)) as i16,
            (yz - wx) as i16,
        ],
        [
            (xz - wy) as i16,
            (yz + wx) as i16,
            (4096 - (xx + yy)) as i16,
        ],
    ]
}

/// [`to_matrix`] with the renormalising step, for the keys that are always interpolated.
#[cfg(test)]
#[inline(always)]
pub(crate) fn to_matrix_renormalised(q: [i32; 4]) -> [[i16; 3]; 3] {
    to_matrix(q, true)
}
