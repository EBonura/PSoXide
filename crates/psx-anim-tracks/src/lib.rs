//! Host-side encoder for version 6 and 7 `.psxanim` clips (per-joint keyed
//! tracks; in v7 a child joint's translation follows its parent).
//!
//! Input is a clip as sampled poses: for every joint and frame a row-major Q12
//! rotation and an integer translation, plus the model-space points the joint
//! drives (its vertices, attachment lever arms). The encoder fits, per joint
//! and per channel, the cheapest keyed track whose worst displacement of those
//! points stays inside a budget in model units, then verifies the finished
//! blob by decoding every frame with the runtime decoder
//! ([`psx_asset::Animation`]) and rejects the clip if the bound does not hold.
//!
//! The budget is split between the two channels (rotation share and the
//! remainder for translation), so by the triangle inequality the sum cannot
//! exceed it; the final verification measures the real total.
//!
//! Rotation targets are the poses' nearest rotations (Gram-Schmidt of the
//! stored matrix, via a quaternion), so a pose the resampler's matrix lerp
//! shrank is decoded as the rotation it approximates. [`Report`] records how
//! far the shipped matrices are from those rotations separately.
//!
//! Stored pose matrices are column-major: the renderer applies `matrix[col][row]`
//! (psx-engine render3d.rs `scaled_pose_matrix`), and every displacement here is
//! measured that way.
//!
//! Version 7 ([`ClipInput::parents`] given): rotations stay model-space, and a
//! joint whose bone is rigid (one pivot keeps it on its parent to within
//! [`PIVOT_GAP_UNITS`] over the clip) stores only a residual translation:
//! `t = t_p + R_p p - R p + residual`, with the parent's DECODED pose, so the
//! residual compensates every error above it and nothing accumulates down a
//! chain. Between keys the child then swings with its parent instead of
//! sliding in a straight line.

#![allow(clippy::needless_range_loop)]

use std::collections::HashMap;

use psx_asset::tracks::{host, rotate_pivot, RotKey};
use psx_asset::Animation;
use psxed_format::animation::{tracks as fmt, MAGIC, VERSION_V6, VERSION_V7};

/// Model units a bone's pivot may miss its parent by, at any source frame,
/// before the joint keeps an absolute translation track: integer translations
/// and Q12 matrices at about 1500 units of reach leave 1-3 units of gap on a
/// rigid chain.
pub const PIVOT_GAP_UNITS: f64 = 6.0;

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

/// Row-major Q12 rotation, as stored in poses.
pub type MatQ12 = [[i16; 3]; 3];

/// Segment counts tried for every track. The last candidate is always every
/// source interval.
const LADDER: [usize; 19] = [
    1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 96, 128, 192,
];

/// One joint's input.
#[derive(Clone, Debug)]
pub struct JointInput {
    /// Rotation per frame.
    pub rotations: Vec<MatQ12>,
    /// Translation per frame, model units.
    pub translations: Vec<[i32; 3]>,
    /// Model-space points (bind positions) whose displacement bounds the error.
    pub probes: Vec<[f64; 3]>,
    /// Lever arm, model units: six more points this far from the model origin
    /// along each axis bound the error too (hurtboxes, sockets and scarf
    /// anchors read a joint far from its own vertices). 0 adds none; a joint
    /// with no probes and no reach gets 600.
    pub reach: f64,
}

/// A clip to encode.
#[derive(Clone, Debug)]
pub struct ClipInput {
    /// Source sample rate, Hz.
    pub sample_rate_hz: u16,
    /// One entry per joint; every joint has the same frame count.
    pub joints: Vec<JointInput>,
    /// Parent of every joint (the model's skeleton) for a version 7 clip, or
    /// empty for version 6.
    pub parents: Vec<Option<usize>>,
}

/// Encoder settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Worst allowed displacement of any probe at any source frame, model units.
    pub budget_units: f64,
    /// Share of the budget given to rotation; translation gets the rest.
    ///
    /// Translation keys are integers, so a small share still costs few extra
    /// bits, while rotation keys get cheaper with every unit; measured on the
    /// Graybox Reach clips 0.8 is within 1% of the best split.
    pub rotation_share: f64,
    /// Accept poses that are not rotations (animated scale), encoding their
    /// nearest rotations; [`Report::worst_vs_input`] gives the difference.
    pub rigidify: bool,
    /// Version 7: worst error of every translation track (residuals and the
    /// absolute tracks alike), model units. The rotation keeps
    /// `budget_units * rotation_share`; the verified bound is their sum.
    pub residual_units: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            budget_units: 4.0,
            rotation_share: 0.8,
            residual_units: 2.5,
            rigidify: false,
        }
    }
}

/// Why a clip was not encoded.
#[derive(Clone, Debug, PartialEq)]
pub enum Reject {
    /// Fewer than two or more than 255 frames, or no joints.
    FrameCount,
    /// A pose is not close to a rotation (animated scale, shear, reflection).
    NotRigid {
        /// Joint index.
        joint: usize,
        /// Frame index.
        frame: usize,
    },
    /// No track reaches the budget for this joint.
    Unreachable {
        /// Joint index.
        joint: usize,
        /// `"rotation"` or `"translation"`.
        channel: &'static str,
        /// Error of the finest track tried (every source frame, widest keys),
        /// model units; the budget share it had to meet is above it.
        finest: f64,
        /// Budget share for that channel, model units.
        limit: f64,
    },
    /// The key area exceeds what 16 bit offsets address.
    KeyAreaTooLarge,
    /// The finished clip, decoded by the runtime decoder, misses the budget.
    OverBudget {
        /// Worst displacement found, model units.
        worst: f64,
    },
    /// The finished clip did not parse (an encoder bug).
    Unparseable,
}

/// What was chosen for one joint.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct JointReport {
    /// Rotation segments (0 = one constant key).
    pub rot_segments: u16,
    /// Rotation size code (0..=2).
    pub rot_size: u8,
    /// Translation segments (0 = one constant key).
    pub trans_segments: u16,
    /// Translation step shift.
    pub trans_step: u8,
    /// Translation key bytes.
    pub trans_key_bytes: u8,
    /// Key bytes this joint contributes.
    pub bytes: u32,
}

/// Outcome of a successful encode.
#[derive(Clone, Debug)]
pub struct Report {
    /// Blob size.
    pub bytes: usize,
    /// Segment counts of the clip's rate table.
    pub rates: Vec<u16>,
    /// Worst displacement against the nearest rotations, model units.
    pub worst_error: f64,
    /// Worst displacement against the stored input matrices, model units.
    pub worst_vs_input: f64,
    /// Worst distance between a stored matrix and its nearest rotation, as
    /// displacement of the probes, model units.
    pub input_shrink: f64,
    /// Per joint choices.
    pub joints: Vec<JointReport>,
    /// Key bytes not stored because a track repeats an earlier track of the
    /// clip byte for byte (a joint with no motion of its own has its parent's
    /// pose); its descriptor points at the earlier copy.
    pub shared_bytes: usize,
    /// Blob size had every track kept its own copy of its keys.
    pub unshared_bytes: usize,
    /// Version 7: joints whose translation follows their parent.
    pub derived_joints: usize,
}

fn norm4(q: [f64; 4]) -> [f64; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3])
        .sqrt()
        .max(1e-12);
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

fn quat_from_mat(m: &M3) -> [f64; 4] {
    let tr = m[0][0] + m[1][1] + m[2][2];
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            0.25 * s,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            0.25 * s,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            0.25 * s,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            0.25 * s,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    norm4(q)
}

fn mat_from_quat(q: [f64; 4]) -> M3 {
    let [x, y, z, w] = norm4(q);
    [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ]
}

fn nlerp(a: [f64; 4], b: [f64; 4], f: f64) -> [f64; 4] {
    let d = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    let s = if d < 0.0 { -1.0 } else { 1.0 };
    norm4([
        a[0] + (s * b[0] - a[0]) * f,
        a[1] + (s * b[1] - a[1]) * f,
        a[2] + (s * b[2] - a[2]) * f,
        a[3] + (s * b[3] - a[3]) * f,
    ])
}

fn mat_f64(m: &MatQ12) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = f64::from(m[r][c]) / 4096.0;
        }
    }
    o
}

fn apply(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn dist(a: V3, b: V3) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// `m` applied to `v` as the renderer applies a stored pose matrix
/// (column-major: row `r` is `m[0][r] v0 + m[1][r] v1 + m[2][r] v2`).
fn apply_pose(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2],
    ]
}

/// Worst rotation-only displacement of `probes` between a Q12 matrix and a
/// reference rotation, both applied as the renderer applies them.
fn rot_err(decoded: &MatQ12, truth: &M3, probes: &[V3]) -> f64 {
    let d = mat_f64(decoded);
    let mut worst: f64 = 0.0;
    for &p in probes {
        worst = worst.max(dist(apply_pose(&d, p), apply_pose(truth, p)));
    }
    worst
}

fn is_rigid(m: &M3) -> bool {
    let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let (r0, r1, r2) = (m[0], m[1], m[2]);
    let unit = |v: V3| (0.97..=1.03).contains(&dot(v, v).sqrt());
    let ortho = |a: V3, b: V3| dot(a, b).abs() <= 0.03;
    let cross = [
        r1[1] * r2[2] - r1[2] * r2[1],
        r1[2] * r2[0] - r1[0] * r2[2],
        r1[0] * r2[1] - r1[1] * r2[0],
    ];
    unit(r0)
        && unit(r1)
        && unit(r2)
        && ortho(r0, r1)
        && ortho(r0, r2)
        && ortho(r1, r2)
        && dot(r0, cross) > 0.0
}

/// `true` when `m` is close to a rotation: unit rows within 3%, nearly
/// orthogonal, positive determinant.
pub fn is_rotation_like(m: &MatQ12) -> bool {
    is_rigid(&mat_f64(m))
}

/// Blend two rotations by `t` (`0..=1`) along the short arc and return the
/// result as a Q12 rotation, or `None` when either is not close to a rotation.
///
/// Linear interpolation of the matrices shrinks the result between two distant
/// poses (to about 0.99 of unit length at 30 degrees apart); this does not.
pub fn blend_rotations(a: &MatQ12, b: &MatQ12, t: f64) -> Option<MatQ12> {
    let (ma, mb) = (mat_f64(a), mat_f64(b));
    if !is_rigid(&ma) || !is_rigid(&mb) {
        return None;
    }
    let m = mat_from_quat(nlerp(quat_from_mat(&ma), quat_from_mat(&mb), t));
    let mut out = [[0i16; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = (m[r][c] * 4096.0).round() as i16;
        }
    }
    Some(out)
}

/// Farthest-point sample of at most `n` points (the farthest from the origin
/// first). Used to keep the search cheap; the final check uses every point.
fn reduce_probes(all: &[V3], n: usize) -> Vec<V3> {
    if all.len() <= n {
        return all.to_vec();
    }
    let norm = |a: V3| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    let first = all
        .iter()
        .copied()
        .max_by(|a, b| norm(*a).total_cmp(&norm(*b)))
        .unwrap_or([0.0; 3]);
    let mut out = vec![first];
    let mut nearest: Vec<f64> = all.iter().map(|p| dist(*p, first)).collect();
    while out.len() < n {
        let (best, _) = nearest
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap_or((0, &0.0));
        let p = all[best];
        out.push(p);
        for (i, q) in all.iter().enumerate() {
            nearest[i] = nearest[i].min(dist(*q, p));
        }
    }
    out
}

struct Truth {
    q: Vec<[f64; 4]>,
    r: Vec<M3>,
    t: Vec<V3>,
}

#[derive(Clone)]
struct Cand {
    /// Segments; 0 is the constant track.
    seg: usize,
    /// Rotation size code, or translation step shift.
    param: u8,
    key_bytes: usize,
    /// Concatenated keys.
    keys: Vec<u8>,
    bytes: usize,
    axis_bits: u16,
    min: [i16; 3],
}

fn key_pos(k: usize, seg: usize, frames: usize) -> f64 {
    k as f64 * (frames - 1) as f64 / seg as f64
}

fn sample_q(q: &[[f64; 4]], pos: f64) -> [f64; 4] {
    let i = (pos.floor() as usize).min(q.len() - 1);
    let fr = pos - i as f64;
    if fr < 1e-9 || i + 1 >= q.len() {
        q[i]
    } else {
        nlerp(q[i], q[i + 1], fr)
    }
}

fn sample_t(t: &[V3], pos: f64) -> V3 {
    let i = (pos.floor() as usize).min(t.len() - 1);
    let fr = pos - i as f64;
    if fr < 1e-9 || i + 1 >= t.len() {
        t[i]
    } else {
        [
            t[i][0] + (t[i + 1][0] - t[i][0]) * fr,
            t[i][1] + (t[i + 1][1] - t[i][1]) * fr,
            t[i][2] + (t[i + 1][2] - t[i][2]) * fr,
        ]
    }
}

/// Smallest-three key of `q` at `size_code`: omitted index, then three signed
/// codes, packed least significant bit first into six bytes (the first
/// `rotation_key_bytes(size_code)` are the key).
pub fn encode_rot_key(q: [f64; 4], size_code: u8) -> [u8; 6] {
    let q = norm4(q);
    let mut omit = 0;
    for i in 1..4 {
        if q[i].abs() > q[omit].abs() {
            omit = i;
        }
    }
    let sign = if q[omit] < 0.0 { -1.0 } else { 1.0 };
    let bits = fmt::rotation_bits(size_code);
    let levels = f64::from(fmt::component_levels(size_code));
    let mask = (1u64 << bits) - 1;
    let mut packed = omit as u64;
    let mut slot = 0;
    for i in 0..4 {
        if i == omit {
            continue;
        }
        let x = sign * q[i] * 4096.0;
        let code = (x * levels / f64::from(fmt::COMPONENT_LIMIT_Q12))
            .round()
            .clamp(-levels, levels) as i64;
        packed |= (code as u64 & mask) << (2 + bits * slot);
        slot += 1;
    }
    let b = packed.to_le_bytes();
    [b[0], b[1], b[2], b[3], b[4], b[5]]
}

fn window(keys: &[u8], index: usize, stride: usize) -> [u8; 6] {
    let mut w = [0u8; 6];
    for (i, b) in w.iter_mut().enumerate() {
        *b = keys.get(index * stride + i).copied().unwrap_or(0);
    }
    w
}

/// Rotation candidates for one joint: one per ladder entry (best size) and the
/// constant track.
fn rot_candidates(
    truth: &Truth,
    probes: &[V3],
    limit: f64,
    ladder: &[usize],
) -> (Vec<Option<Cand>>, Option<Cand>) {
    let frames = truth.q.len();
    let eval = |seg: usize, size: u8, keys: &[u8]| -> f64 {
        let stride = fmt::rotation_key_bytes(size);
        let mut worst: f64 = 0.0;
        for f in 0..frames {
            let (idx, frac) = if seg == 0 {
                (0u16, 0u16)
            } else {
                host::locate(seg as u16, frames as u16, (f as u32) << 8)
            };
            let a: RotKey = host::decode_rotation_key(&window(keys, idx as usize, stride), size);
            let b = if frac != 0 {
                Some(host::decode_rotation_key(
                    &window(keys, idx as usize + 1, stride),
                    size,
                ))
            } else {
                None
            };
            let m = host::blend_rotation(a, b, i32::from(frac));
            worst = worst.max(rot_err(&m, &truth.r[f], probes));
            if worst > limit {
                break;
            }
        }
        worst
    };
    let build = |seg: usize, size: u8| -> Vec<u8> {
        let n = if seg == 0 { 1 } else { seg + 1 };
        let stride = fmt::rotation_key_bytes(size);
        let mut keys = Vec::with_capacity(n * stride);
        for k in 0..n {
            let q = if seg == 0 {
                truth.q[0]
            } else {
                sample_q(&truth.q, key_pos(k, seg, frames))
            };
            keys.extend_from_slice(&encode_rot_key(q, size)[..stride]);
        }
        keys
    };
    let best = |seg: usize| -> Option<Cand> {
        for size in 0..3u8 {
            let keys = build(seg, size);
            if eval(seg, size, &keys) <= limit {
                let stride = fmt::rotation_key_bytes(size);
                return Some(Cand {
                    seg,
                    param: size,
                    key_bytes: stride,
                    bytes: keys.len(),
                    keys,
                    axis_bits: 0,
                    min: [0; 3],
                });
            }
        }
        None
    };
    (ladder.iter().map(|&s| best(s)).collect(), best(0))
}

/// Error of every-frame rotation keys at the widest size: the floor no budget
/// below it can beat.
fn rot_finest(truth: &Truth, probes: &[V3]) -> f64 {
    let frames = truth.q.len();
    let mut keys = Vec::new();
    for f in 0..frames {
        keys.extend_from_slice(&encode_rot_key(truth.q[f], 2));
    }
    let mut worst: f64 = 0.0;
    for f in 0..frames {
        let a = host::decode_rotation_key(&window(&keys, f, 6), 2);
        let m = host::blend_rotation(a, None, 0);
        worst = worst.max(rot_err(&m, &truth.r[f], probes));
    }
    worst
}

fn bit_length(v: u64) -> u32 {
    64 - v.leading_zeros()
}

/// Translation candidates; values are in translation units (the clip shift is
/// already divided out). Error is reported back in model units by `scale`.
fn trans_candidates(
    truth_t: &[V3],
    scale: f64,
    limit: f64,
    ladder: &[usize],
) -> (Vec<Option<Cand>>, Option<Cand>) {
    let frames = truth_t.len();
    let attempt = |seg: usize| -> Option<Cand> {
        let n = if seg == 0 { 1 } else { seg + 1 };
        let vals: Vec<V3> = (0..n)
            .map(|k| {
                if seg == 0 {
                    truth_t[0]
                } else {
                    sample_t(truth_t, key_pos(k, seg, frames))
                }
            })
            .collect();
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for v in &vals {
            for a in 0..3 {
                lo[a] = lo[a].min(v[a]);
                hi[a] = hi[a].max(v[a]);
            }
        }
        let min = [lo[0].floor(), lo[1].floor(), lo[2].floor()];
        if min.iter().any(|m| m.abs() > 32767.0) {
            return None;
        }
        for step in (0..=12u8).rev() {
            let unit = f64::from(1u32 << step);
            let mut bits = [0u32; 3];
            let mut ok = true;
            for a in 0..3 {
                let max_code = ((hi[a] - min[a]) / unit).round() as u64;
                bits[a] = bit_length(max_code);
                if bits[a] > 15 {
                    ok = false;
                }
            }
            if !ok {
                continue;
            }
            let total = (bits[0] + bits[1] + bits[2]) as usize;
            let key_bytes = total.div_ceil(8);
            let axis_bits = (bits[0] | (bits[1] << 5) | (bits[2] << 10)) as u16;
            let mut keys = Vec::with_capacity(n * key_bytes);
            for v in &vals {
                let mut packed = 0u64;
                let mut shift = 0;
                for a in 0..3 {
                    let code = (((v[a] - min[a]) / unit).round() as u64).min((1u64 << bits[a]) - 1);
                    packed |= code << shift;
                    shift += bits[a];
                }
                keys.extend_from_slice(&packed.to_le_bytes()[..key_bytes]);
            }
            let minq = [min[0] as i16, min[1] as i16, min[2] as i16];
            let mut worst: f64 = 0.0;
            for f in 0..frames {
                let (idx, frac) = if seg == 0 {
                    (0u16, 0u16)
                } else {
                    host::locate(seg as u16, frames as u16, (f as u32) << 8)
                };
                let decode = |i: usize| -> [i32; 3] {
                    let c = host::decode_translation_codes(&window(&keys, i, key_bytes), axis_bits);
                    [
                        i32::from(minq[0]) + (c[0] << step),
                        i32::from(minq[1]) + (c[1] << step),
                        i32::from(minq[2]) + (c[2] << step),
                    ]
                };
                let a = decode(idx as usize);
                let mut t = a;
                if frac != 0 {
                    let b = decode(idx as usize + 1);
                    for k in 0..3 {
                        t[k] += ((b[k] - t[k]) * i32::from(frac)) >> 8;
                    }
                }
                let got = [f64::from(t[0]), f64::from(t[1]), f64::from(t[2])];
                worst = worst.max(dist(got, truth_t[f]) * scale);
                if worst > limit {
                    break;
                }
            }
            if worst <= limit {
                return Some(Cand {
                    seg,
                    param: step,
                    key_bytes,
                    bytes: keys.len(),
                    keys,
                    axis_bits,
                    min: minq,
                });
            }
        }
        None
    };
    (ladder.iter().map(|&s| attempt(s)).collect(), attempt(0))
}

/// Cheapest assignment of at most seven segment counts to all channels.
/// Returns indices into `ladder`.
fn choose_rates(costs: &[Vec<Option<usize>>], constants: &[Option<usize>]) -> Option<Vec<usize>> {
    let n = costs.first().map_or(0, Vec::len);
    let pick = n.min(fmt::MAX_RATES);
    let mut best: Option<(usize, u32)> = None;
    for mask in 0u32..(1u32 << n) {
        if mask.count_ones() as usize != pick {
            continue;
        }
        let mut total = 0usize;
        let mut feasible = true;
        for (c, row) in costs.iter().enumerate() {
            let mut cheapest = constants[c];
            for i in 0..n {
                if mask & (1 << i) != 0 {
                    if let Some(b) = row[i] {
                        cheapest = Some(cheapest.map_or(b, |x| x.min(b)));
                    }
                }
            }
            match cheapest {
                Some(b) => total += b,
                None => {
                    feasible = false;
                    break;
                }
            }
        }
        if feasible && best.is_none_or(|(t, _)| total < t) {
            best = Some((total, mask));
        }
    }
    best.map(|(_, mask)| (0..n).filter(|i| mask & (1 << i) != 0).collect())
}

/// Rotation of a picked track at every source frame, as the runtime decodes it.
fn decode_rot_track(c: &Cand, frames: usize) -> Vec<MatQ12> {
    let stride = fmt::rotation_key_bytes(c.param);
    (0..frames)
        .map(|f| {
            let (idx, frac) = if c.seg == 0 {
                (0u16, 0u16)
            } else {
                host::locate(c.seg as u16, frames as u16, (f as u32) << 8)
            };
            let a = host::decode_rotation_key(&window(&c.keys, idx as usize, stride), c.param);
            let b = (frac != 0).then(|| {
                host::decode_rotation_key(&window(&c.keys, idx as usize + 1, stride), c.param)
            });
            host::blend_rotation(a, b, i32::from(frac))
        })
        .collect()
}

/// Translation of a picked track at every source frame, model units, as the
/// runtime decodes it (before any parent is added).
fn decode_trans_track(c: &Cand, frames: usize, tshift: u8) -> Vec<[i32; 3]> {
    let decode = |i: usize| -> [i32; 3] {
        if c.key_bytes == 0 {
            return [0, 1, 2].map(|a| i32::from(c.min[a]));
        }
        let codes = host::decode_translation_codes(&window(&c.keys, i, c.key_bytes), c.axis_bits);
        [0, 1, 2].map(|a| i32::from(c.min[a]) + (codes[a] << c.param))
    };
    (0..frames)
        .map(|f| {
            let (idx, frac) = if c.seg == 0 {
                (0u16, 0u16)
            } else {
                host::locate(c.seg as u16, frames as u16, (f as u32) << 8)
            };
            let mut t = decode(idx as usize);
            if frac != 0 {
                let b = decode(idx as usize + 1);
                for k in 0..3 {
                    t[k] += ((b[k] - t[k]) * i32::from(frac)) >> 8;
                }
            }
            t.map(|v| v << tshift)
        })
        .collect()
}

/// Least-squares pivot of every joint over the clip, `(A_j - A_p) x = t_p - t_j`
/// with `A` the renderer's rotation, and the joints whose rounded pivot keeps
/// them on their parent within [`PIVOT_GAP_UNITS`] at every frame.
fn fit_pivots(clip: &ClipInput, depth_order: &[usize]) -> Vec<Option<(usize, [i32; 3])>> {
    let joints = clip.joints.len();
    let frames = clip.joints[0].rotations.len();
    let mut out: Vec<Option<(usize, [i32; 3])>> = vec![None; joints];
    // Pivot of each joint, also for joints that end up absolute: a joint that
    // never turns against its parent takes its parent's (Tikhonov term).
    let mut piv = vec![[0.0f64; 3]; joints];
    let applied = |j: usize, f: usize| -> M3 {
        let m = mat_f64(&clip.joints[j].rotations[f]);
        [0, 1, 2].map(|r| [0, 1, 2].map(|c| m[c][r]))
    };
    for &j in depth_order {
        let Some(p) = clip.parents[j] else { continue };
        let mut ata = [[0.0f64; 3]; 3];
        let mut atb = [0.0f64; 3];
        for f in 0..frames {
            let (a, b) = (applied(j, f), applied(p, f));
            let (tj, tp) = (
                clip.joints[j].translations[f],
                clip.joints[p].translations[f],
            );
            for r in 0..3 {
                let row = [0, 1, 2].map(|c| a[r][c] - b[r][c]);
                let rhs = f64::from(tp[r] - tj[r]);
                for c in 0..3 {
                    for k in 0..3 {
                        ata[c][k] += row[c] * row[k];
                    }
                    atb[c] += row[c] * rhs;
                }
            }
        }
        let lam = 1e-6;
        for k in 0..3 {
            ata[k][k] += lam;
            atb[k] += lam * piv[p][k];
        }
        let x = solve3(ata, atb);
        piv[j] = x;
        let rounded = x.map(|v| v.round());
        if rounded.iter().any(|v| !v.is_finite() || v.abs() > 32767.0) {
            continue;
        }
        let mut worst: f64 = 0.0;
        for f in 0..frames {
            let (tj, tp) = (
                clip.joints[j].translations[f],
                clip.joints[p].translations[f],
            );
            let wc = apply(&applied(j, f), rounded);
            let wp = apply(&applied(p, f), rounded);
            let child = [0, 1, 2].map(|r| wc[r] + f64::from(tj[r]));
            let parent = [0, 1, 2].map(|r| wp[r] + f64::from(tp[r]));
            worst = worst.max(dist(child, parent));
        }
        if worst <= PIVOT_GAP_UNITS {
            out[j] = Some((p, rounded.map(|v| v as i32)));
        }
    }
    out
}

fn solve3(a: M3, b: V3) -> V3 {
    let det = |m: M3| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(a);
    [0, 1, 2].map(|c| {
        let mut m = a;
        for r in 0..3 {
            m[r][c] = b[r];
        }
        det(m) / d
    })
}

/// Joints ordered so every parent comes before its children; `None` when the
/// parents do not form a forest over the clip's joints.
fn depth_order(parents: &[Option<usize>]) -> Option<Vec<usize>> {
    let n = parents.len();
    let mut depth = vec![0usize; n];
    for j in 0..n {
        let mut k = j;
        let mut d = 0;
        while let Some(p) = parents[k] {
            if p >= n || d >= n {
                return None;
            }
            k = p;
            d += 1;
        }
        depth[j] = d;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&j| depth[j]);
    Some(order)
}

/// Cheapest of `cands` (one per clip rate, same order) and the constant track.
fn pick_slot(cands: &[Option<Cand>], constant: &Option<Cand>) -> Option<(Cand, u8)> {
    let mut best: Option<(&Cand, u8)> = constant.as_ref().map(|c| (c, fmt::RATE_CONSTANT));
    for (slot, c) in cands.iter().enumerate() {
        if let Some(c) = c {
            if best.is_none_or(|(b, _)| c.bytes < b.bytes) {
                best = Some((c, slot as u8));
            }
        }
    }
    best.map(|(c, r)| (c.clone(), r))
}

/// Encode a clip. See the crate docs for the guarantees.
pub fn encode(clip: &ClipInput, opts: &Options) -> Result<(Vec<u8>, Report), Reject> {
    let joints = clip.joints.len();
    let frames = clip.joints.first().map_or(0, |j| j.rotations.len());
    if joints == 0 || frames < 2 || frames > usize::from(fmt::MAX_FRAMES) {
        return Err(Reject::FrameCount);
    }
    let mut truths = Vec::with_capacity(joints);
    for (j, joint) in clip.joints.iter().enumerate() {
        if joint.rotations.len() != frames || joint.translations.len() != frames {
            return Err(Reject::FrameCount);
        }
        let mut q: Vec<[f64; 4]> = Vec::with_capacity(frames);
        for f in 0..frames {
            let m = mat_f64(&joint.rotations[f]);
            if !is_rigid(&m) && !opts.rigidify {
                return Err(Reject::NotRigid { joint: j, frame: f });
            }
            let mut cur = quat_from_mat(&m);
            if let Some(prev) = q.last() {
                let d: f64 = (0..4).map(|i| prev[i] * cur[i]).sum();
                if d < 0.0 {
                    cur = [-cur[0], -cur[1], -cur[2], -cur[3]];
                }
            }
            q.push(cur);
        }
        let r: Vec<M3> = q.iter().map(|&q| mat_from_quat(q)).collect();
        let t: Vec<V3> = joint
            .translations
            .iter()
            .map(|t| [f64::from(t[0]), f64::from(t[1]), f64::from(t[2])])
            .collect();
        truths.push(Truth { q, r, t });
    }
    // Version 7 needs the skeleton, at most 32 joints and at least one bone
    // that holds its pivot; otherwise the clip is version 6.
    let order = if clip.parents.len() == joints && joints <= fmt::MAX_JOINTS_V7 {
        depth_order(&clip.parents)
    } else {
        None
    };
    let hierarchy: Vec<Option<(usize, [i32; 3])>> = match &order {
        Some(order) => fit_pivots(clip, order),
        None => vec![None; joints],
    };
    let v7 = order.is_some() && hierarchy.iter().any(Option::is_some);
    let order = order.unwrap_or_else(|| (0..joints).collect());

    // Clip wide translation shift so every minimum fits an i16.
    let max_t = clip
        .joints
        .iter()
        .flat_map(|j| j.translations.iter().flatten())
        .map(|v| i64::from(*v).abs())
        .max()
        .unwrap_or(0);
    let mut tshift = 0u8;
    while (max_t >> tshift) > 30000 && tshift < 8 {
        tshift += 1;
    }
    let scale = f64::from(1u32 << tshift);

    let mut ladder: Vec<usize> = LADDER.iter().copied().filter(|&s| s < frames - 1).collect();
    ladder.push(frames - 1);
    ladder.dedup();

    let rot_limit = opts.budget_units * opts.rotation_share;
    let trans_limit = if v7 {
        opts.residual_units
    } else {
        opts.budget_units * (1.0 - opts.rotation_share)
    };
    let total_limit = if v7 {
        rot_limit + trans_limit
    } else {
        opts.budget_units
    };
    // A joint with no points would be unconstrained; bound it by a lever arm.
    // A derived joint's vertices turn about its pivot (its translation
    // compensates the rest), so its rotation is measured from there.
    // A derived joint's translation is compensated to the true one at every
    // source frame, so, as in v6, only its rotation error moves its points,
    // about the model origin.
    //
    // A derived joint's residual instead puts its pivot where the true pose
    // has it, so its rotation error moves its points only by their distance
    // from the pivot (its own bone, not the whole body's height): rotation is
    // searched on points relative to the pivot, lever arms sit at the pivot,
    // and every derived child's pivot is a point of its parent (so the
    // child's residual stays small).
    let centre = |j: usize| -> V3 { hierarchy[j].map_or([0.0; 3], |(_, p)| p.map(f64::from)) };
    let mut probe_sets: Vec<Vec<V3>> = clip
        .joints
        .iter()
        .enumerate()
        .map(|(j, joint)| {
            let reach = if joint.probes.is_empty() && joint.reach <= 0.0 {
                600.0
            } else {
                joint.reach
            };
            let mut set = joint.probes.clone();
            if reach > 0.0 {
                let c = if v7 { centre(j) } else { [0.0; 3] };
                for axis in 0..3 {
                    for r in [-reach, reach] {
                        let mut p = c;
                        p[axis] += r;
                        set.push(p);
                    }
                }
            }
            set
        })
        .collect();
    if v7 {
        for j in 0..joints {
            if let Some((p, piv)) = hierarchy[j] {
                probe_sets[p].push(piv.map(f64::from));
            }
        }
    }
    let search_probes: Vec<Vec<V3>> = probe_sets
        .iter()
        .enumerate()
        .map(|(j, set)| {
            let c = if v7 { centre(j) } else { [0.0; 3] };
            set.iter()
                .map(|v| [v[0] - c[0], v[1] - c[1], v[2] - c[2]])
                .collect()
        })
        .collect();
    let applied_truth = |j: usize, f: usize| -> M3 { truths[j].r[f] };
    // Translation targets for choosing the rates: absolute, or for a derived
    // joint the residual against the true poses (the final residual, against
    // the decoded parents, is close to it).
    let estimate: Vec<Vec<V3>> = (0..joints)
        .map(|j| match hierarchy[j] {
            None => truths[j].t.clone(),
            Some((p, piv)) => (0..frames)
                .map(|f| {
                    let x = piv.map(f64::from);
                    let a = apply_pose(&applied_truth(p, f), x);
                    let b = apply_pose(&applied_truth(j, f), x);
                    [0, 1, 2].map(|r| truths[j].t[f][r] - (truths[p].t[f][r] + a[r] - b[r]))
                })
                .collect(),
        })
        .collect();
    let mut rot_c = Vec::new();
    let mut tr_c = Vec::new();
    for j in 0..joints {
        let probes = reduce_probes(&search_probes[j], 32);
        rot_c.push(rot_candidates(&truths[j], &probes, rot_limit, &ladder));
        let units: Vec<V3> = estimate[j]
            .iter()
            .map(|t| [t[0] / scale, t[1] / scale, t[2] / scale])
            .collect();
        tr_c.push(trans_candidates(&units, scale, trans_limit, &ladder));
    }
    // Channel order: rotation of every joint, then translation of every joint.
    let mut costs: Vec<Vec<Option<usize>>> = Vec::new();
    let mut constants: Vec<Option<usize>> = Vec::new();
    for (cands, constant) in rot_c.iter().chain(tr_c.iter()) {
        costs.push(cands.iter().map(|c| c.as_ref().map(|c| c.bytes)).collect());
        constants.push(constant.as_ref().map(|c| c.bytes));
    }
    // A translation track with no data bytes costs nothing but a constant key.
    let chosen = choose_rates(&costs, &constants).ok_or_else(|| {
        // Name the first channel with no feasible option at all and measure how
        // close the finest track gets.
        for j in 0..joints {
            let (rc, rk) = &rot_c[j];
            if rk.is_none() && rc.iter().all(Option::is_none) {
                let probes = reduce_probes(&search_probes[j], 32);
                let finest = rot_finest(&truths[j], &probes);
                return Reject::Unreachable {
                    joint: j,
                    channel: "rotation",
                    finest,
                    limit: rot_limit,
                };
            }
            let (tc, tk) = &tr_c[j];
            if tk.is_none() && tc.iter().all(Option::is_none) {
                return Reject::Unreachable {
                    joint: j,
                    channel: "translation",
                    finest: f64::NAN,
                    limit: trans_limit,
                };
            }
        }
        Reject::Unreachable {
            joint: 0,
            channel: "rate selection",
            finest: f64::NAN,
            limit: 0.0,
        }
    })?;
    let rate_segs: Vec<usize> = chosen.iter().map(|&i| ladder[i]).collect();

    let pick = |cands: &(Vec<Option<Cand>>, Option<Cand>)| -> (Cand, u8) {
        let slots: Vec<Option<Cand>> = chosen.iter().map(|&li| cands.0[li].clone()).collect();
        pick_slot(&slots, &cands.1).expect("choose_rates proved a feasible option")
    };
    let rot_pick: Vec<(Cand, u8)> = rot_c.iter().map(pick).collect();
    let mut trans_pick: Vec<Option<(Cand, u8)>> = vec![None; joints];
    let mut derived = hierarchy.clone();
    if v7 {
        // Residuals against the decoded parents, parents first, each picked on
        // the clip's rates. A joint whose residual misses every rate falls back
        // to an absolute track.
        let rot_dec: Vec<Vec<MatQ12>> = rot_pick
            .iter()
            .map(|(c, _)| decode_rot_track(c, frames))
            .collect();
        let mut t_dec: Vec<Vec<[i32; 3]>> = vec![Vec::new(); joints];
        for &j in &order {
            let absolute = |j: usize| -> Option<(Cand, u8)> {
                let units: Vec<V3> = truths[j]
                    .t
                    .iter()
                    .map(|t| [t[0] / scale, t[1] / scale, t[2] / scale])
                    .collect();
                let (cands, constant) = trans_candidates(&units, scale, trans_limit, &rate_segs);
                pick_slot(&cands, &constant)
            };
            let mut chosen_track = None;
            if let Some((p, piv)) = derived[j] {
                // Residual that puts the pivot where the true pose has it:
                // t = t_true + R_true p - R_decoded p.
                let x = piv.map(f64::from);
                let target: Vec<V3> = (0..frames)
                    .map(|f| {
                        let a = rotate_pivot(&rot_dec[p][f], piv);
                        let b = rotate_pivot(&rot_dec[j][f], piv);
                        let true_pivot = apply_pose(&truths[j].r[f], x);
                        let decoded_pivot = apply_pose(&mat_f64(&rot_dec[j][f]), x);
                        [0, 1, 2].map(|r| {
                            (truths[j].t[f][r] + true_pivot[r]
                                - decoded_pivot[r]
                                - f64::from(t_dec[p][f][r] + a[r] - b[r]))
                                / scale
                        })
                    })
                    .collect();
                let (cands, constant) = trans_candidates(&target, scale, trans_limit, &rate_segs);
                if let Some((c, rate)) = pick_slot(&cands, &constant) {
                    let res = decode_trans_track(&c, frames, tshift);
                    t_dec[j] = (0..frames)
                        .map(|f| {
                            let a = rotate_pivot(&rot_dec[p][f], piv);
                            let b = rotate_pivot(&rot_dec[j][f], piv);
                            [0, 1, 2].map(|r| t_dec[p][f][r] + a[r] - b[r] + res[f][r])
                        })
                        .collect();
                    chosen_track = Some((c, rate));
                } else {
                    derived[j] = None;
                }
            }
            if chosen_track.is_none() {
                let (c, rate) = absolute(j).ok_or(Reject::Unreachable {
                    joint: j,
                    channel: "translation",
                    finest: f64::NAN,
                    limit: trans_limit,
                })?;
                t_dec[j] = decode_trans_track(&c, frames, tshift);
                chosen_track = Some((c, rate));
            }
            trans_pick[j] = chosen_track;
        }
    } else {
        for j in 0..joints {
            trans_pick[j] = Some(pick(&tr_c[j]));
        }
    }
    let v7 = v7 && derived.iter().any(Option::is_some);

    let mut area: Vec<u8> = Vec::new();
    let mut descs: Vec<u8> = Vec::with_capacity(joints * fmt::DESCRIPTOR_SIZE);
    let mut reports = Vec::with_capacity(joints);
    // Tracks already in the key area, by their key bytes and stride. A track
    // whose keys repeat one of them byte for byte reads that copy: the decoder
    // sees the same bytes at the same stride, so every pose is unchanged, and
    // the copy already passed the bounds the validator checks for this shape.
    let mut stored: HashMap<(Vec<u8>, usize), usize> = HashMap::new();
    let mut shared_bytes = 0usize;
    // The decoder reads six bytes at every key start; the slack after the key
    // area covers four. A track of 1-byte keys ending the area needs one more.
    let mut read_end = 0usize;
    let mut place = |area: &mut Vec<u8>, keys: &[u8], stride: usize| -> usize {
        if let Some(&off) = stored.get(&(keys.to_vec(), stride)) {
            shared_bytes += keys.len();
            return off;
        }
        let off = area.len();
        area.extend_from_slice(keys);
        stored.insert((keys.to_vec(), stride), off);
        read_end = read_end.max(off + keys.len() - stride + 6);
        off
    };
    for j in 0..joints {
        let (rc, rrate) = &rot_pick[j];
        let (tc, trate) = trans_pick[j].as_ref().expect("every joint has a track");
        let rot_off = place(&mut area, &rc.keys, fmt::rotation_key_bytes(rc.param));
        let trans_off = if tc.key_bytes == 0 {
            0
        } else {
            place(&mut area, &tc.keys, tc.key_bytes)
        };
        if area.len() > fmt::MAX_KEY_AREA - 8 {
            return Err(Reject::KeyAreaTooLarge);
        }
        let rot_ctl = rrate | (rc.param << 3);
        let trans_ctl = trate | (tc.param << 3);
        descs.push(rot_ctl);
        descs.push(trans_ctl);
        descs.extend_from_slice(&tc.axis_bits.to_le_bytes());
        descs.extend_from_slice(&(rot_off as u16).to_le_bytes());
        descs.extend_from_slice(&(trans_off as u16).to_le_bytes());
        for m in tc.min {
            descs.extend_from_slice(&m.to_le_bytes());
        }
        descs.push(tc.key_bytes as u8);
        descs.push(match derived[j] {
            Some((p, _)) if v7 => p as u8 + 1,
            _ => 0,
        });
        reports.push(JointReport {
            rot_segments: if *rrate == fmt::RATE_CONSTANT {
                0
            } else {
                rc.seg as u16
            },
            rot_size: rc.param,
            trans_segments: if *trate == fmt::RATE_CONSTANT {
                0
            } else {
                tc.seg as u16
            },
            trans_step: tc.param,
            trans_key_bytes: tc.key_bytes as u8,
            bytes: (rc.bytes + tc.bytes) as u32,
        });
    }
    let mut pivots: Vec<u8> = Vec::new();
    if v7 {
        for d in &derived {
            let p = d.map_or([0; 3], |(_, p)| p);
            for v in p {
                pivots.extend_from_slice(&(v as i16).to_le_bytes());
            }
        }
        pivots.resize(fmt::pivot_table_size(joints), 0);
    }
    while area.len() + fmt::SLACK_BYTES < read_end {
        area.push(0);
    }
    // Pad so the whole blob is a multiple of four bytes.
    let fixed =
        psxed_format::AssetHeader::SIZE + 8 + fmt::RATE_TABLE_SIZE + descs.len() + pivots.len();
    let unshared_bytes = (fixed + area.len() + shared_bytes + fmt::SLACK_BYTES).next_multiple_of(4);
    while !(fixed + area.len() + fmt::SLACK_BYTES).is_multiple_of(4) {
        area.push(0);
    }

    let mut blob = Vec::with_capacity(fixed + area.len() + fmt::SLACK_BYTES);
    blob.extend_from_slice(&MAGIC);
    blob.extend_from_slice(&(if v7 { VERSION_V7 } else { VERSION_V6 }).to_le_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes());
    let payload = fixed - psxed_format::AssetHeader::SIZE + area.len() + fmt::SLACK_BYTES;
    blob.extend_from_slice(&(payload as u32).to_le_bytes());
    blob.extend_from_slice(&(joints as u16).to_le_bytes());
    blob.extend_from_slice(&(frames as u16).to_le_bytes());
    blob.extend_from_slice(&clip.sample_rate_hz.to_le_bytes());
    blob.extend_from_slice(&u16::from(tshift).to_le_bytes());
    blob.push(rate_segs.len() as u8);
    blob.push(0);
    for r in 0..fmt::MAX_RATES {
        blob.extend_from_slice(&(rate_segs.get(r).copied().unwrap_or(0) as u16).to_le_bytes());
    }
    for r in 0..fmt::MAX_RATES {
        let f = rate_segs
            .get(r)
            .map_or(0, |&s| host::rate_factor(s as u16, frames as u16));
        blob.extend_from_slice(&f.to_le_bytes());
    }
    blob.extend_from_slice(&[0, 0]);
    blob.extend_from_slice(&descs);
    blob.extend_from_slice(&pivots);
    blob.extend_from_slice(&area);
    blob.extend_from_slice(&[0u8; fmt::SLACK_BYTES]);
    debug_assert_eq!(blob.len(), psxed_format::AssetHeader::SIZE + payload);

    // Verify with the runtime decoder over every probe, as the renderer applies
    // the poses.
    let decoded = Animation::from_bytes(&blob).map_err(|_| Reject::Unparseable)?;
    let (mut worst, mut worst_input, mut shrink): (f64, f64, f64) = (0.0, 0.0, 0.0);
    for (j, joint) in clip.joints.iter().enumerate() {
        for f in 0..frames {
            let pose = decoded
                .pose(f as u16, j as u16)
                .ok_or(Reject::Unparseable)?;
            let got_r = mat_f64(&pose.matrix);
            let truth_r = &truths[j].r[f];
            let input_r = mat_f64(&joint.rotations[f]);
            let got_t = [
                f64::from(pose.translation.x),
                f64::from(pose.translation.y),
                f64::from(pose.translation.z),
            ];
            let truth_t = truths[j].t[f];
            for &p in &probe_sets[j] {
                let g = apply_pose(&got_r, p);
                let g = [g[0] + got_t[0], g[1] + got_t[1], g[2] + got_t[2]];
                let t = apply_pose(truth_r, p);
                let t = [t[0] + truth_t[0], t[1] + truth_t[1], t[2] + truth_t[2]];
                let i = apply_pose(&input_r, p);
                let i = [i[0] + truth_t[0], i[1] + truth_t[1], i[2] + truth_t[2]];
                worst = worst.max(dist(g, t));
                worst_input = worst_input.max(dist(g, i));
                shrink = shrink.max(dist(t, i));
            }
        }
    }
    if worst > total_limit + 1e-9 {
        return Err(Reject::OverBudget { worst });
    }
    let report = Report {
        bytes: blob.len(),
        rates: rate_segs.iter().map(|&s| s as u16).collect(),
        worst_error: worst,
        worst_vs_input: worst_input,
        input_shrink: shrink,
        joints: reports,
        shared_bytes,
        unshared_bytes,
        derived_joints: if v7 {
            derived.iter().filter(|d| d.is_some()).count()
        } else {
            0
        },
    };
    Ok((blob, report))
}
