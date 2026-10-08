//! Host-side encoder for version 6 `.psxanim` clips (per-joint keyed tracks).
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

#![allow(clippy::needless_range_loop)]

use psx_asset::tracks::{host, RotKey};
use psx_asset::Animation;
use psxed_format::animation::tracks as fmt;
use psxed_format::animation::{MAGIC, VERSION_V6};

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
}

/// A clip to encode.
#[derive(Clone, Debug)]
pub struct ClipInput {
    /// Source sample rate, Hz.
    pub sample_rate_hz: u16,
    /// One entry per joint; every joint has the same frame count.
    pub joints: Vec<JointInput>,
}

/// Encoder settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Worst allowed displacement of any probe at any source frame, model units.
    pub budget_units: f64,
    /// Share of the budget given to rotation; translation gets the rest.
    pub rotation_share: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            budget_units: 4.0,
            rotation_share: 0.6,
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

/// Worst rotation-only displacement of `probes` between a Q12 matrix and a
/// reference rotation.
fn rot_err(decoded: &MatQ12, truth: &M3, probes: &[V3]) -> f64 {
    let d = mat_f64(decoded);
    let mut worst: f64 = 0.0;
    for &p in probes {
        worst = worst.max(dist(apply(&d, p), apply(truth, p)));
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
            if !is_rigid(&m) {
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
    let trans_limit = opts.budget_units * (1.0 - opts.rotation_share);
    // A joint with no points would be unconstrained; bound it by a lever arm.
    let probe_sets: Vec<Vec<V3>> = clip
        .joints
        .iter()
        .map(|j| {
            if j.probes.is_empty() {
                vec![
                    [600.0, 0.0, 0.0],
                    [-600.0, 0.0, 0.0],
                    [0.0, 600.0, 0.0],
                    [0.0, -600.0, 0.0],
                    [0.0, 0.0, 600.0],
                    [0.0, 0.0, -600.0],
                ]
            } else {
                j.probes.clone()
            }
        })
        .collect();
    let mut rot_c = Vec::new();
    let mut tr_c = Vec::new();
    for (j, _) in clip.joints.iter().enumerate() {
        let probes = reduce_probes(&probe_sets[j], 32);
        let truth = &truths[j];
        rot_c.push(rot_candidates(truth, &probes, rot_limit, &ladder));
        let units: Vec<V3> = truth
            .t
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
        // Name the first joint with no feasible option at all.
        let joint = (0..joints)
            .find(|&j| {
                [&rot_c[j], &tr_c[j]]
                    .iter()
                    .any(|(c, k)| k.is_none() && c.iter().all(Option::is_none))
            })
            .unwrap_or(0);
        Reject::Unreachable { joint }
    })?;
    let rate_segs: Vec<usize> = chosen.iter().map(|&i| ladder[i]).collect();

    let pick = |cands: &(Vec<Option<Cand>>, Option<Cand>)| -> (Cand, u8) {
        let mut best: Option<(&Cand, u8)> = cands.1.as_ref().map(|c| (c, fmt::RATE_CONSTANT));
        for (slot, &li) in chosen.iter().enumerate() {
            if let Some(c) = &cands.0[li] {
                if best.is_none_or(|(b, _)| c.bytes < b.bytes) {
                    best = Some((c, slot as u8));
                }
            }
        }
        let (c, r) = best.expect("choose_rates proved a feasible option");
        (c.clone(), r)
    };

    let mut area: Vec<u8> = Vec::new();
    let mut descs: Vec<u8> = Vec::with_capacity(joints * fmt::DESCRIPTOR_SIZE);
    let mut reports = Vec::with_capacity(joints);
    for j in 0..joints {
        let (rc, rrate) = pick(&rot_c[j]);
        let (tc, trate) = pick(&tr_c[j]);
        let rot_off = area.len();
        area.extend_from_slice(&rc.keys);
        let trans_off = if tc.key_bytes == 0 { 0 } else { area.len() };
        if tc.key_bytes != 0 {
            area.extend_from_slice(&tc.keys);
        }
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
        descs.push(0);
        reports.push(JointReport {
            rot_segments: if rrate == fmt::RATE_CONSTANT {
                0
            } else {
                rc.seg as u16
            },
            rot_size: rc.param,
            trans_segments: if trate == fmt::RATE_CONSTANT {
                0
            } else {
                tc.seg as u16
            },
            trans_step: tc.param,
            trans_key_bytes: tc.key_bytes as u8,
            bytes: (rc.bytes + tc.bytes) as u32,
        });
    }
    // Pad so the whole blob is a multiple of four bytes.
    let fixed = psxed_format::AssetHeader::SIZE + 8 + fmt::RATE_TABLE_SIZE + descs.len();
    while (fixed + area.len() + fmt::SLACK_BYTES) % 4 != 0 {
        area.push(0);
    }

    let mut blob = Vec::with_capacity(fixed + area.len() + fmt::SLACK_BYTES);
    blob.extend_from_slice(&MAGIC);
    blob.extend_from_slice(&VERSION_V6.to_le_bytes());
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
    blob.extend_from_slice(&area);
    blob.extend_from_slice(&[0u8; fmt::SLACK_BYTES]);
    debug_assert_eq!(blob.len(), psxed_format::AssetHeader::SIZE + payload);

    // Verify with the runtime decoder over every probe.
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
                let g = apply(&got_r, p);
                let g = [g[0] + got_t[0], g[1] + got_t[1], g[2] + got_t[2]];
                let t = apply(truth_r, p);
                let t = [t[0] + truth_t[0], t[1] + truth_t[1], t[2] + truth_t[2]];
                let i = apply(&input_r, p);
                let i = [i[0] + truth_t[0], i[1] + truth_t[1], i[2] + truth_t[2]];
                worst = worst.max(dist(g, t));
                worst_input = worst_input.max(dist(g, i));
                shrink = shrink.max(dist(t, i));
            }
        }
    }
    if worst > opts.budget_units + 1e-9 {
        return Err(Reject::OverBudget { worst });
    }
    let report = Report {
        bytes: blob.len(),
        rates: rate_segs.iter().map(|&s| s as u16).collect(),
        worst_error: worst,
        worst_vs_input: worst_input,
        input_shrink: shrink,
        joints: reports,
    };
    Ok((blob, report))
}
