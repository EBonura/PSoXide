//! Optional, bounded metadata for exact solid-cell reduction. Invalid/unsupported
//! maps and exhausted work or byte budgets simply keep the full runtime path.
use crate::solid_cell_constraints::{reduce_bounded, verify, Halfspace, Reduction};
use psx_bsp::{pxbsp_resident::PxbspResidentMap, CompactPlane, SliceReader};

pub const BYTE_BUDGET: usize = 16 * 1024;
const MAX_DEPTH: usize = 72;
const MAX_PROPOSALS: usize = 64_000_000;
const MAX_CELL_PROPOSALS: usize = 4_000_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SolidCellProofs {
    pub domain: [[i32; 3]; 2],
    /// Sorted directed-edge key, blob offset, blob byte length.
    pub index: Vec<[u16; 3]>,
    pub data: Vec<u8>,
}
impl SolidCellProofs {
    pub fn resident_bytes(&self) -> usize {
        if self.index.is_empty() && self.data.is_empty() {
            0
        } else {
            24 + self.index.len() * 6 + self.data.len()
        }
    }
}

fn normal(plane: &CompactPlane) -> [i64; 3] {
    if plane.kind < 3 {
        let mut n = [0; 3];
        n[plane.kind as usize] = 4096;
        n
    } else {
        [
            plane.normal.x as i64,
            plane.normal.y as i64,
            plane.normal.z as i64,
        ]
    }
}
fn unwrapped(plane: &CompactPlane, domain: [[i32; 3]; 2]) -> bool {
    let n = normal(plane);
    let mut sum = [0i64; 2];
    for axis in 0..3 {
        let a = n[axis] * i64::from(domain[0][axis]);
        let b = n[axis] * i64::from(domain[1][axis]);
        let range = [a.min(b), a.max(b)];
        if range.iter().any(|&v| i32::try_from(v).is_err()) {
            return false;
        }
        for side in 0..2 {
            sum[side] += range[side];
        }
        if sum.iter().any(|&v| i32::try_from(v).is_err()) {
            return false;
        }
    }
    sum.iter()
        .all(|&v| i32::try_from(v - i64::from(plane.distance)).is_ok())
}

fn encode(terms: &[Halfspace], count: usize, reduction: &Reduction) -> Option<Vec<u8>> {
    if count == 0 || count > MAX_DEPTH || terms.len() != count + 6 {
        return None;
    }
    let mut data = vec![count as u8];
    data.resize(1 + count.div_ceil(8), 0);
    let mut active: Vec<_> = (0..terms.len()).collect();
    let mut previous = None;
    for proof in &reduction.proofs {
        if proof.removed >= count
            || previous.is_some_and(|p| proof.removed <= p)
            || proof.support.is_empty()
            || !verify(terms, &active, proof)
        {
            return None;
        }
        previous = Some(proof.removed);
        data[1 + proof.removed / 8] |= 1 << (proof.removed % 8);
        for (i, &ordinal) in proof.support.iter().enumerate() {
            if ordinal >= terms.len() || ordinal >= 128 {
                return None;
            }
            data.push(ordinal as u8 | if i + 1 == proof.support.len() { 128 } else { 0 });
        }
        active.retain(|&i| i != proof.removed);
    }
    (active == reduction.retained).then_some(data)
}

pub fn cook(bytes: &[u8]) -> SolidCellProofs {
    let mut map = PxbspResidentMap::with_capacity(bytes.len());
    if map.load(0, &mut SliceReader::new(bytes)).is_err() {
        return SolidCellProofs::default();
    }
    let Some(model) = map.brush_models().get(0) else {
        return SolidCellProofs::default();
    };
    let domain = [
        [
            model.mins.x as i32,
            model.mins.y as i32,
            model.mins.z as i32,
        ],
        [
            model.maxs.x as i32,
            model.maxs.y as i32,
            model.maxs.z as i32,
        ],
    ];
    if (0..3).any(|i| domain[0][i] > domain[1][i]) {
        return SolidCellProofs::default();
    }
    let Some(hull) = map.model_collision_hull(0, 0) else {
        return SolidCellProofs::default();
    };
    let mut parents = vec![0u16; map.nodes().len()];
    if !hull.build_solid_cell_parents(&mut parents) {
        return SolidCellProofs::default();
    }
    let mut candidates = Vec::new();
    let mut work = MAX_PROPOSALS;
    for edge in 0..parents.len() * 2 {
        if work == 0 {
            break;
        }
        let mut path = [0u16; MAX_DEPTH];
        let Some(count) = hull.solid_cell_constraints_into(&parents, edge as u16, &mut path) else {
            continue;
        };
        let mut terms = Vec::with_capacity(count + 6);
        for &constraint in &path[..count] {
            let Some(plane) = map.planes().get((constraint >> 1) as usize) else {
                break;
            };
            if !unwrapped(plane, domain) {
                break;
            }
            // Front is closed n.p>=d; back is open n.p<d.
            let sign = if constraint & 1 == 0 { -1 } else { 1 };
            terms.push(Halfspace {
                normal: normal(plane).map(|n| n * sign),
                bound: i64::from(plane.distance) * sign,
                strict: constraint & 1 != 0,
            });
        }
        if terms.len() != count {
            continue;
        }
        for axis in 0..3 {
            let mut n = [0; 3];
            n[axis] = 1;
            terms.push(Halfspace {
                normal: n,
                bound: domain[1][axis] as i64,
                strict: false,
            });
            n[axis] = -1;
            terms.push(Halfspace {
                normal: n,
                bound: -(domain[0][axis] as i64),
                strict: false,
            });
        }
        let allowance = work.min(MAX_CELL_PROPOSALS);
        let mut remaining = allowance;
        let reduction = reduce_bounded(&terms, count, &mut remaining);
        work -= allowance - remaining;
        if reduction.proofs.is_empty() || reduction.proofs.len() == count {
            continue;
        }
        let Some(data) = encode(&terms, count, &reduction) else {
            continue;
        };
        candidates.push((edge as u16, reduction.proofs.len(), data));
    }
    // Highest removed-plane benefit per resident byte first; stable edge tie.
    candidates.sort_by(|a, b| {
        (b.1 * (a.2.len() + 6))
            .cmp(&(a.1 * (b.2.len() + 6)))
            .then(a.0.cmp(&b.0))
    });
    let mut used = 24;
    candidates.retain(|candidate| {
        let size = candidate.2.len() + 6;
        if used + size > BYTE_BUDGET {
            false
        } else {
            used += size;
            true
        }
    });
    candidates.sort_by_key(|c| c.0);
    let mut result = SolidCellProofs {
        domain,
        ..SolidCellProofs::default()
    };
    for (edge, _, data) in candidates {
        result
            .index
            .push([edge, result.data.len() as u16, data.len() as u16]);
        result.data.extend(data);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn term(normal: [i64; 3], bound: i64, strict: bool) -> Halfspace {
        Halfspace {
            normal,
            bound,
            strict,
        }
    }
    #[test]
    fn encoded_ordinals_bitmap_and_terminators_match_verified_removal_order() {
        let terms = vec![
            term([1, 0, 0], 10, false),
            term([1, 1, 0], 20, false),
            term([1, 1, 1], 30, false),
            term([1, 0, 0], 8, false),
            term([-1, 0, 0], 8, false),
            term([0, 1, 0], 8, false),
            term([0, -1, 0], 8, false),
            term([0, 0, 1], 8, false),
            term([0, 0, -1], 8, false),
        ];
        let mut budget = 10_000;
        let reduction = reduce_bounded(&terms, 3, &mut budget);
        let data = encode(&terms, 3, &reduction).unwrap();
        assert_eq!(data[0], 3);
        assert_eq!(data[1], 7);
        let mut cursor = 2;
        for proof in &reduction.proofs {
            let mut decoded = Vec::new();
            loop {
                let byte = data[cursor];
                cursor += 1;
                decoded.push((byte & 127) as usize);
                if byte & 128 != 0 {
                    break;
                }
                assert!(decoded.len() < 3);
            }
            assert_eq!(decoded, proof.support);
        }
        assert_eq!(cursor, data.len());
        assert!(encode(&terms, 73, &reduction).is_none());
    }
    #[test]
    fn generation_budget_exhaustion_keeps_original_constraints() {
        let terms = vec![term([1, 0, 0], 2, false), term([1, 0, 0], 1, false)];
        let result = reduce_bounded(&terms, 1, &mut 0);
        assert_eq!(result.retained, vec![0, 1]);
        assert!(result.proofs.is_empty());
        assert_eq!(cook(b"not a bsp"), SolidCellProofs::default());
    }
    #[test]
    fn original_axial_kind_ignores_stored_normal_and_general_rejects_partial_overflow() {
        let mut p = CompactPlane {
            normal: psx_bsp::Vec3I16 {
                x: 32767,
                y: 32767,
                z: 32767,
            },
            kind: 0,
            sign_bits: 255,
            distance: 0,
        };
        let domain = [[-32768; 3], [32767; 3]];
        assert!(unwrapped(&p, domain));
        p.kind = 3;
        assert!(!unwrapped(&p, domain));
        p.normal = psx_bsp::Vec3I16 { x: 1, y: 0, z: 0 };
        p.distance = i32::MIN;
        assert!(!unwrapped(&p, domain));
    }
}
