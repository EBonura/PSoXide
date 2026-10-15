//! Allocation-free verification of cooked solid-cell redundancy certificates.
//! Metadata is only a proposal: coefficients and strictness come from the live
//! validated ancestor path. Failure leaves the caller's full list untouched.
// Frozen copy used as a test oracle; kept as written.
#![allow(clippy::needless_range_loop, clippy::manual_div_ceil)]
use crate::{CompactPlane, Vec3I32};

pub type Domain = [[i32; 3]; 2];
#[derive(Clone, Copy, Debug)]
struct Term {
    n: [i64; 3],
    b: i64,
    strict: bool,
}

pub fn contains_integral_segment(domain: &Domain, a: &Vec3I32, b: &Vec3I32) -> bool {
    let a = [a.x, a.y, a.z];
    let b = [b.x, b.y, b.z];
    (0..3).all(|i| {
        domain[0][i] <= domain[1][i]
            && a[i] & 4095 == 0
            && b[i] & 4095 == 0
            && (a[i] >> 12) >= domain[0][i]
            && (a[i] >> 12) <= domain[1][i]
            && (b[i] >> 12) >= domain[0][i]
            && (b[i] >> 12) <= domain[1][i]
    })
}

/// Prove that original integral plane evaluation has no product, partial-sum,
/// or final subtraction wrap throughout this closed integer-coordinate box.
pub fn plane_unwrapped(p: &CompactPlane, domain: &Domain) -> bool {
    if (0..3)
        .any(|i| domain[0][i] > domain[1][i] || domain[0][i] < -524288 || domain[1][i] > 524287)
    {
        return false;
    }
    let n = normal(p);
    let (mut lo, mut hi) = (0i64, 0i64);
    for i in 0..3 {
        let a = n[i] * i64::from(domain[0][i]);
        let b = n[i] * i64::from(domain[1][i]);
        let (l, h) = (a.min(b), a.max(b));
        if l < i64::from(i32::MIN) || h > i64::from(i32::MAX) {
            return false;
        }
        lo += l;
        hi += h;
        if lo < i64::from(i32::MIN) || hi > i64::from(i32::MAX) {
            return false;
        }
    }
    lo -= i64::from(p.distance);
    hi -= i64::from(p.distance);
    lo >= i64::from(i32::MIN) && hi <= i64::from(i32::MAX)
}
fn normal(p: &CompactPlane) -> [i64; 3] {
    if p.kind < 3 {
        let mut n = [0; 3];
        n[usize::from(p.kind)] = 4096;
        n
    } else {
        [
            i64::from(p.normal.x),
            i64::from(p.normal.y),
            i64::from(p.normal.z),
        ]
    }
}
fn term(planes: &[CompactPlane], path: &[u16], domain: &Domain, index: usize) -> Option<Term> {
    if index < path.len() {
        let code = path[index];
        let p = planes.get(usize::from(code >> 1))?;
        let mut n = normal(p);
        let mut b = i64::from(p.distance);
        let strict = code & 1 != 0;
        if !strict {
            n = n.map(|v| -v);
            b = -b;
        }
        Some(Term { n, b, strict })
    } else {
        let d = index.checked_sub(path.len())?;
        if d >= 6 {
            return None;
        }
        let axis = d / 2;
        let mut n = [0; 3];
        n[axis] = if d & 1 == 0 { 1 } else { -1 };
        let b = if d & 1 == 0 {
            i64::from(domain[1][axis])
        } else {
            -i64::from(domain[0][axis])
        };
        Some(Term {
            n,
            b,
            strict: false,
        })
    }
}
fn det(a: [i128; 3], b: [i128; 3], c: [i128; 3]) -> Option<i128> {
    let cross = |i: usize, j: usize| b[i].checked_mul(c[j])?.checked_sub(b[j].checked_mul(c[i])?);
    a[0].checked_mul(cross(1, 2)?)?
        .checked_sub(a[1].checked_mul(cross(0, 2)?)?)?
        .checked_add(a[2].checked_mul(cross(0, 1)?)?)
}
fn implication(target: Term, supports: &[Term]) -> Option<bool> {
    let t = target.n.map(i128::from);
    let mut c = [[0i128; 3]; 3];
    for (a, b) in c.iter_mut().zip(supports) {
        *a = b.n.map(i128::from);
    }
    let mut w = [0i128; 3];
    let mut d = match supports.len() {
        1 => {
            let axis = (0..3).find(|&i| c[0][i] != 0)?;
            w[0] = t[axis];
            c[0][axis]
        }
        2 => {
            let mut found = None;
            for (i, j) in [(0, 1), (0, 2), (1, 2)] {
                let d = c[0][i]
                    .checked_mul(c[1][j])?
                    .checked_sub(c[0][j].checked_mul(c[1][i])?)?;
                if d != 0 {
                    w[0] = t[i]
                        .checked_mul(c[1][j])?
                        .checked_sub(t[j].checked_mul(c[1][i])?)?;
                    w[1] = c[0][i]
                        .checked_mul(t[j])?
                        .checked_sub(c[0][j].checked_mul(t[i])?)?;
                    found = Some(d);
                    break;
                }
            }
            found?
        }
        3 => {
            w = [
                det(t, c[1], c[2])?,
                det(c[0], t, c[2])?,
                det(c[0], c[1], t)?,
            ];
            det(c[0], c[1], c[2])?
        }
        _ => return None,
    };
    if d == 0 {
        return None;
    }
    if d < 0 {
        d = d.checked_neg()?;
        for v in &mut w {
            *v = v.checked_neg()?;
        }
    }
    if w.iter().any(|&v| v < 0) {
        return None;
    }
    let mut n = [0i128; 3];
    let mut b = 0i128;
    let mut strict = false;
    for (support, &weight) in supports.iter().zip(&w) {
        for i in 0..3 {
            n[i] = n[i].checked_add(weight.checked_mul(i128::from(support.n[i]))?)?;
        }
        b = b.checked_add(weight.checked_mul(i128::from(support.b))?)?;
        strict |= weight > 0 && support.strict;
    }
    for i in 0..3 {
        if n[i] != t[i].checked_mul(d)? {
            return Some(false);
        }
    }
    let bound = i128::from(target.b).checked_mul(d)?;
    Some(b < bound || (b == bound && (!target.strict || strict)))
}

/// Verify every omitted term, then compact in place. `path` must have been
/// obtained from the live hull's fully validated solid ancestor extractor.
/// A successful reduction is valid ONLY for integral segment endpoints inside
/// `domain`; other queries must use the original certificate. No write occurs
/// on any failure, including malformed/trailing bytes or arithmetic overflow.
pub fn reduce_live(
    planes: &[CompactPlane],
    path: &mut [u16],
    domain: &Domain,
    bytes: &[u8],
) -> Option<usize> {
    let count = path.len();
    if count == 0 || count > 72 || bytes.first().copied()? as usize != count {
        return None;
    }
    for &code in path.iter() {
        if !plane_unwrapped(planes.get(usize::from(code >> 1))?, domain) {
            return None;
        }
    }
    let bitmap_len = (count + 7) / 8;
    let bitmap = bytes.get(1..1 + bitmap_len)?;
    if count & 7 != 0 && bitmap[bitmap_len - 1] >> (count & 7) != 0 {
        return None;
    }
    let mut active = [true; 78];
    let mut cursor = 1 + bitmap_len;
    for removed in 0..count {
        if bitmap[removed / 8] & (1 << (removed & 7)) == 0 {
            continue;
        }
        let mut supports = [Term {
            n: [0; 3],
            b: 0,
            strict: false,
        }; 3];
        let mut indices = [usize::MAX; 3];
        let mut len = 0;
        loop {
            if len == 3 {
                return None;
            }
            let byte = *bytes.get(cursor)?;
            cursor += 1;
            let i = usize::from(byte & 127);
            if i >= count + 6 || i == removed || !active[i] || indices[..len].contains(&i) {
                return None;
            }
            indices[len] = i;
            supports[len] = term(planes, path, domain, i)?;
            len += 1;
            if byte & 128 != 0 {
                break;
            }
        }
        if implication(term(planes, path, domain, removed)?, &supports[..len]) != Some(true) {
            return None;
        }
        active[removed] = false;
    }
    if cursor != bytes.len() {
        return None;
    }
    let retained = active[..count].iter().filter(|&&x| x).count();
    // Empty certificate requires a separate whole-domain solid representation.
    if retained == 0 {
        return None;
    }
    let mut out = 0;
    for i in 0..count {
        if active[i] {
            path[out] = path[i];
            out += 1;
        }
    }
    Some(out)
}

/// Index must be strictly edge-sorted and cover the blob once without overlap.
/// The complete entry is still independently verified against the live path.
pub fn entry<'a>(index: &[[u16; 3]], data: &'a [u8], edge: u16) -> Option<&'a [u8]> {
    if index
        .len()
        .checked_mul(6)?
        .checked_add(data.len())?
        .checked_add(24)?
        > 16384
    {
        return None;
    }
    let mut end = 0usize;
    let mut previous = None;
    let mut found = None;
    for &[key, offset, len] in index {
        if key == u16::MAX
            || previous.is_some_and(|v| v >= key)
            || usize::from(offset) != end
            || len == 0
        {
            return None;
        }
        end = end.checked_add(usize::from(len))?;
        let bytes = data.get(usize::from(offset)..end)?;
        if key == edge {
            found = Some(bytes);
        }
        previous = Some(key);
    }
    if end != data.len() {
        return None;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vec3I16;
    fn plane(n: [i16; 3], distance: i32) -> CompactPlane {
        CompactPlane {
            normal: Vec3I16 {
                x: n[0],
                y: n[1],
                z: n[2],
            },
            distance,
            kind: 3,
            sign_bits: 0,
        }
    }
    #[test]
    fn exact_reduction_strict_and_transactional_decode() {
        let planes = [
            plane([4096, 4096, 0], 8192),
            plane([4096, 0, 0], 4096),
            plane([0, 4096, 0], 4096),
        ];
        let domain = [[-10; 3], [10; 3]];
        // x+y<2 follows from x<1 and y<1. Retain original order.
        let original = [1, 3, 5];
        let mut path = original;
        assert_eq!(
            reduce_live(&planes, &mut path, &domain, &[3, 1, 1, 130]),
            Some(2)
        );
        assert_eq!(&path[..2], &[3, 5]);
        for bad in [
            &[3, 129, 1, 130][..],
            &[3, 1, 1][..],
            &[3, 1, 128][..],
            &[3, 1, 1, 129][..],
            &[3, 1, 1, 130, 0][..],
            &[2, 1, 1, 130][..],
            &[3, 1, 9 | 128][..],
            &[3, 1, 1, 2, 3, 132][..],
        ] {
            let mut path = original;
            assert_eq!(reduce_live(&planes, &mut path, &domain, bad), None);
            assert_eq!(path, original);
        }
        // Later failed implication must not publish the earlier valid removal.
        let mut path = original;
        assert_eq!(
            reduce_live(&planes, &mut path, &domain, &[3, 3, 1, 130, 128]),
            None
        );
        assert_eq!(path, original);
        let closed = Term {
            n: [1, 0, 0],
            b: 1,
            strict: false,
        };
        let strict = Term {
            strict: true,
            ..closed
        };
        assert_eq!(implication(strict, &[closed]), Some(false));
        assert_eq!(implication(closed, &[strict]), Some(true));
    }
    #[test]
    fn domain_gate_and_original_kind_overflow() {
        let d = [[-10; 3], [10; 3]];
        let a = Vec3I32 {
            x: -40960,
            y: 0,
            z: 40960,
        };
        assert!(contains_integral_segment(&d, &a, &a));
        assert!(!contains_integral_segment(&d, &Vec3I32 { x: 1, ..a }, &a));
        assert!(!contains_integral_segment(
            &d,
            &Vec3I32 { x: 45056, ..a },
            &a
        ));
        let mut p = plane([32767; 3], 0);
        assert!(plane_unwrapped(&p, &d));
        assert!(!plane_unwrapped(&p, &[[-524288; 3], [524287; 3]]));
        p.kind = 0;
        p.distance = i32::MAX;
        assert!(!plane_unwrapped(&p, &d));
        p.distance = 0;
        assert!(plane_unwrapped(&p, &d)); // authored normal ignored for axial kind
    }
    #[test]
    fn index_requires_bounded_complete_nonoverlapping_order() {
        assert_eq!(
            entry(&[[2, 0, 2], [4, 2, 1]], &[1, 2, 3], 4),
            Some(&[3][..])
        );
        for idx in [
            &[[2, 0, 2], [2, 2, 1]][..],
            &[[4, 0, 2], [2, 2, 1]][..],
            &[[2, 0, 2], [4, 1, 1]][..],
            &[[2, 0, 2]][..],
        ] {
            assert!(entry(idx, &[1, 2, 3], 2).is_none());
        }
    }
}
