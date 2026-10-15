//! Offline-only exact halfspace implication experiment. Not wired into cooks or
//! runtime yet. Floating arithmetic proposes supports; only checked integer
//! identities authorize a removal. Any uncertainty retains the original plane.

/// `normal · point <= bound`, or `< bound` when strict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Halfspace {
    pub normal: [i64; 3],
    pub bound: i64,
    pub strict: bool,
}

#[derive(Clone, Debug)]
pub struct Implication {
    pub removed: usize,
    pub support: Vec<usize>,
    pub numerators: Vec<i128>,
    pub denominator: i128,
}

#[derive(Debug)]
pub struct Reduction {
    /// Original constraints followed by permanent domain bounds. Indices retain
    /// their original ordering; no plane coefficients or boundary ties change.
    pub retained: Vec<usize>,
    pub proofs: Vec<Implication>,
}

fn det(a: [i128; 3], b: [i128; 3], c: [i128; 3]) -> Option<i128> {
    let cross = |i: usize, j: usize| b[i].checked_mul(c[j])?.checked_sub(b[j].checked_mul(c[i])?);
    a[0].checked_mul(cross(1, 2)?)?
        .checked_sub(a[1].checked_mul(cross(0, 2)?)?)?
        .checked_add(a[2].checked_mul(cross(0, 1)?)?)
}

/// Independently verify a nonnegative rational Farkas certificate. Supports
/// must still be active when this proof is applied. A strict target at an equal
/// bound needs a positive-weight strict support, preserving open boundaries.
pub fn verify(terms: &[Halfspace], active: &[usize], proof: &Implication) -> bool {
    let Some(target) = terms.get(proof.removed) else {
        return false;
    };
    if !active.contains(&proof.removed)
        || proof.denominator <= 0
        || proof.support.len() != proof.numerators.len()
        || proof.support.len() > 3
    {
        return false;
    }
    let mut normal = [0i128; 3];
    let mut bound = 0i128;
    let mut strict = false;
    for (&index, &weight) in proof.support.iter().zip(&proof.numerators) {
        if index == proof.removed || !active.contains(&index) || weight < 0 {
            return false;
        }
        let Some(term) = terms.get(index) else {
            return false;
        };
        for (acc, &component) in normal.iter_mut().zip(&term.normal) {
            let Some(v) = weight
                .checked_mul(i128::from(component))
                .and_then(|v| acc.checked_add(v))
            else {
                return false;
            };
            *acc = v;
        }
        let Some(v) = weight
            .checked_mul(i128::from(term.bound))
            .and_then(|v| bound.checked_add(v))
        else {
            return false;
        };
        bound = v;
        strict |= weight > 0 && term.strict;
    }
    for (&expected, &have) in target.normal.iter().zip(&normal) {
        if i128::from(expected).checked_mul(proof.denominator) != Some(have) {
            return false;
        }
    }
    let Some(wanted) = i128::from(target.bound).checked_mul(proof.denominator) else {
        return false;
    };
    bound < wanted || (bound == wanted && (!target.strict || strict))
}

fn propose(terms: &[Halfspace], removed: usize, support: &[usize]) -> Option<Implication> {
    let target = terms[removed].normal.map(i128::from);
    let columns: Vec<_> = support
        .iter()
        .map(|&i| terms[i].normal.map(i128::from))
        .collect();
    let (mut denominator, mut numerators) = match columns.len() {
        1 => {
            let axis = (0..3).find(|&i| columns[0][i] != 0)?;
            (columns[0][axis], vec![target[axis]])
        }
        2 => {
            let mut result = None;
            for (i, j) in [(0, 1), (0, 2), (1, 2)] {
                let d = columns[0][i]
                    .checked_mul(columns[1][j])?
                    .checked_sub(columns[0][j].checked_mul(columns[1][i])?)?;
                if d != 0 {
                    let a = target[i]
                        .checked_mul(columns[1][j])?
                        .checked_sub(target[j].checked_mul(columns[1][i])?)?;
                    let b = columns[0][i]
                        .checked_mul(target[j])?
                        .checked_sub(columns[0][j].checked_mul(target[i])?)?;
                    result = Some((d, vec![a, b]));
                    break;
                }
            }
            result?
        }
        3 => (
            det(columns[0], columns[1], columns[2])?,
            vec![
                det(target, columns[1], columns[2])?,
                det(columns[0], target, columns[2])?,
                det(columns[0], columns[1], target)?,
            ],
        ),
        _ => return None,
    };
    if denominator == 0 {
        return None;
    }
    if denominator < 0 {
        denominator = denominator.checked_neg()?;
        for v in &mut numerators {
            *v = v.checked_neg()?;
        }
    }
    if numerators.iter().any(|&n| n < 0) {
        return None;
    }
    Some(Implication {
        removed,
        support: support.to_vec(),
        numerators,
        denominator,
    })
}

// This filter may reject valid implications because of rounding, which merely
// leaves an extra original constraint. It can never authorize a removal.
fn plausible_three(terms: &[Halfspace], target: usize, support: [usize; 3]) -> bool {
    let columns = support.map(|i| terms[i].normal.map(|v| v as f64));
    let t = terms[target].normal.map(|v| v as f64);
    let determinant = |a: [f64; 3], b: [f64; 3], c: [f64; 3]| {
        a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0])
    };
    let d = determinant(columns[0], columns[1], columns[2]);
    if d == 0.0 || !d.is_finite() {
        return false;
    }
    let weights = [
        determinant(t, columns[1], columns[2]) / d,
        determinant(columns[0], t, columns[2]) / d,
        determinant(columns[0], columns[1], t) / d,
    ];
    if weights.iter().any(|&w| !w.is_finite() || w < -1e-9) {
        return false;
    }
    let bound: f64 = (0..3)
        .map(|i| weights[i] * terms[support[i]].bound as f64)
        .sum();
    bound <= terms[target].bound as f64 + 1e-6 * (1.0 + bound.abs())
}

/// Remove proven redundant original terms, keeping permanent domain bounds.
/// Only original indices below `original_count` may be removed. An invalid
/// count or arithmetic failure produces a conservative unchanged/partial set.
// Offline experiment: exercised by the tests and the halfspace probes, not yet
// wired into the cook.
#[cfg_attr(not(test), allow(dead_code))]
pub fn reduce(terms: &[Halfspace], original_count: usize) -> Reduction {
    let mut attempts = usize::MAX;
    reduce_bounded(terms, original_count, &mut attempts)
}

/// The support-proposal work budget is shared by the caller across cells.
/// Exhaustion retains all remaining terms; already exact-verified proofs stay valid.
pub fn reduce_bounded(
    terms: &[Halfspace],
    original_count: usize,
    attempts: &mut usize,
) -> Reduction {
    let mut result = Reduction {
        retained: (0..terms.len()).collect(),
        proofs: Vec::new(),
    };
    if original_count > terms.len() {
        return result;
    }
    for target in 0..original_count {
        let others: Vec<_> = result
            .retained
            .iter()
            .copied()
            .filter(|&i| i != target)
            .collect();
        let mut found = None;
        'search: for size in 1..=3 {
            for i in 0..others.len() {
                let end_j = if size == 1 { i + 1 } else { others.len() };
                for j in i..end_j {
                    if size > 1 && j == i {
                        continue;
                    }
                    let end_k = if size < 3 { j + 1 } else { others.len() };
                    for k in j..end_k {
                        if size == 3 && k == j {
                            continue;
                        }
                        if *attempts == 0 {
                            return result;
                        }
                        *attempts -= 1;
                        let indices = [others[i], others[j], others[k]];
                        if size == 3 && !plausible_three(terms, target, indices) {
                            continue;
                        }
                        if let Some(proof) = propose(terms, target, &indices[..size]) {
                            if verify(terms, &result.retained, &proof) {
                                found = Some(proof);
                                break 'search;
                            }
                        }
                    }
                }
            }
        }
        if let Some(proof) = found {
            result.retained.retain(|&i| i != target);
            result.proofs.push(proof);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(n: [i64; 3], b: i64, s: bool) -> Halfspace {
        Halfspace {
            normal: n,
            bound: b,
            strict: s,
        }
    }
    #[test]
    fn strict_target_needs_strict_support_at_equal_bound() {
        let mut t = [
            h([1, 1, 0], 2, true),
            h([1, 0, 0], 1, false),
            h([0, 1, 0], 1, false),
        ];
        let p = propose(&t, 0, &[1, 2]).unwrap();
        assert!(!verify(&t, &[0, 1, 2], &p));
        t[1].strict = true;
        assert!(verify(&t, &[0, 1, 2], &p));
        t[1].strict = false;
        t[0].bound = 3;
        assert!(verify(&t, &[0, 1, 2], &p));
    }
    #[test]
    fn three_plane_exact_implication_and_invalid_certificates() {
        let t = [
            h([1, 1, 1], 3, false),
            h([1, 0, 0], 1, false),
            h([0, 1, 0], 1, false),
            h([0, 0, 1], 1, false),
        ];
        let p = propose(&t, 0, &[1, 2, 3]).unwrap();
        assert!(verify(&t, &[0, 1, 2, 3], &p));
        let mut bad = p.clone();
        bad.numerators[0] = -1;
        assert!(!verify(&t, &[0, 1, 2, 3], &bad));
        bad = p.clone();
        bad.denominator = i128::MAX;
        assert!(!verify(&t, &[0, 1, 2, 3], &bad));
        assert!(!verify(&t, &[0, 1, 2], &p));
        assert_eq!(reduce(&t, 1).retained, vec![1, 2, 3]);
    }
    #[test]
    fn overflow_is_rejected_without_panics() {
        let t = [h([i64::MAX; 3], i64::MAX, false); 4];
        assert!(propose(&t, 0, &[1, 2, 3]).is_none());
        let p = Implication {
            removed: 0,
            support: vec![1],
            numerators: vec![i128::MAX],
            denominator: 1,
        };
        assert!(!verify(&t, &[0, 1], &p));
    }
    #[test]
    fn duplicate_opposite_and_zero_weight_strict_terms() {
        let t = [
            h([1, 0, 0], 1, true),
            h([1, 0, 0], 1, false),
            h([0, 1, 0], 0, true),
            h([-1, 0, 0], -1, false),
        ];
        let p = Implication {
            removed: 0,
            support: vec![1, 2],
            numerators: vec![1, 0],
            denominator: 1,
        };
        assert!(!verify(&t, &[0, 1, 2, 3], &p));
        assert!(!verify(
            &t,
            &[0, 1, 2, 3],
            &Implication {
                removed: 0,
                support: vec![3],
                numerators: vec![1],
                denominator: 1
            }
        ));
        assert!(reduce(&t, 4).retained.contains(&0));
    }
}
