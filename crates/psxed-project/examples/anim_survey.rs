//! Host prototypes of animation compression techniques over a directory
//! written by `anim_dump`. Measures bytes and worst vertex displacement
//! (model units) against the clips as they ship today.
//!
//! Usage: anim_survey <dump-dir> <error-budget-model-units> [csv-out]

#![allow(clippy::needless_range_loop)]

use std::path::PathBuf;

use psx_asset::{Animation, Model};

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

fn mat_from_q12(m: [[i16; 3]; 3]) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = m[r][c] as f64 / 4096.0;
        }
    }
    o
}

fn quat_from_mat(m: &M3) -> [f64; 4] {
    // [x, y, z, w], row-major rotation matrix.
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

fn norm4(q: [f64; 4]) -> [f64; 4] {
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3])
        .sqrt()
        .max(1e-12);
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
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

fn round_q12(m: M3) -> M3 {
    let mut o = m;
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = (m[r][c] * 4096.0).round() / 4096.0;
        }
    }
    o
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

/// Smallest-three quantisation with `bits` per component.
fn quantise_quat(q: [f64; 4], bits: u32) -> [f64; 4] {
    let q = norm4(q);
    let mut big = 0;
    for i in 1..4 {
        if q[i].abs() > q[big].abs() {
            big = i;
        }
    }
    let sign = if q[big] < 0.0 { -1.0 } else { 1.0 };
    let max = std::f64::consts::FRAC_1_SQRT_2;
    let levels = ((1u64 << (bits - 1)) - 1) as f64;
    let mut o = [0.0; 4];
    let mut sum = 0.0;
    for i in 0..4 {
        if i == big {
            continue;
        }
        let v = (sign * q[i]).clamp(-max, max);
        let code = (v / max * levels).round();
        o[i] = code / levels * max;
        sum += o[i] * o[i];
    }
    o[big] = (1.0 - sum).max(0.0).sqrt();
    o
}

fn lerp3(a: V3, b: V3, f: f64) -> V3 {
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

struct JointTrack {
    m: Vec<M3>,
    q: Vec<[f64; 4]>,
    t: Vec<V3>,
}

struct ClipData {
    hz: u16,
    frames: usize,
    bytes: usize,
    joints: Vec<JointTrack>,
}

fn load_clip(path: &std::path::Path) -> ClipData {
    let b = std::fs::read(path).unwrap();
    let a = Animation::from_bytes(&b).unwrap();
    let (jn, f) = (a.joint_count() as usize, a.frame_count() as usize);
    let mut joints = Vec::new();
    for j in 0..jn {
        let mut tr = JointTrack {
            m: vec![],
            q: vec![],
            t: vec![],
        };
        for ff in 0..f {
            let p = a.pose(ff as u16, j as u16).unwrap();
            let m = mat_from_q12(p.matrix);
            let mut q = quat_from_mat(&m);
            if let Some(prev) = tr.q.last() {
                let d: f64 = (0..4).map(|i| prev[i] * q[i]).sum();
                if d < 0.0 {
                    q = [-q[0], -q[1], -q[2], -q[3]];
                }
            }
            tr.m.push(m);
            tr.q.push(q);
            tr.t.push([
                p.translation.x as f64,
                p.translation.y as f64,
                p.translation.z as f64,
            ]);
        }
        joints.push(tr);
    }
    ClipData {
        hz: a.sample_rate_hz(),
        frames: f,
        bytes: b.len().next_multiple_of(4),
        joints,
    }
}

/// Farthest-point sample of at most `n` vertices (always includes the farthest from the origin).
fn sample_verts(all: &[V3], n: usize) -> Vec<V3> {
    if all.len() <= n {
        return all.to_vec();
    }
    let mut out = vec![*all
        .iter()
        .max_by(|a, b| norm3(**a).partial_cmp(&norm3(**b)).unwrap())
        .unwrap()];
    while out.len() < n {
        let next = all
            .iter()
            .max_by(|a, b| {
                let da = out.iter().map(|o| dist3(**a, *o)).fold(f64::MAX, f64::min);
                let db = out.iter().map(|o| dist3(**b, *o)).fold(f64::MAX, f64::min);
                da.partial_cmp(&db).unwrap()
            })
            .unwrap();
        out.push(*next);
    }
    out
}
fn norm3(a: V3) -> f64 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn dist3(a: V3, b: V3) -> f64 {
    norm3([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}
fn apply(m: &M3, v: V3) -> V3 {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Worst rotation-only displacement of `verts` between two matrices.
fn rot_err(a: &M3, b: &M3, verts: &[V3]) -> f64 {
    let mut worst: f64 = 0.0;
    for &v in verts {
        let pa = apply(a, v);
        let pb = apply(b, v);
        worst = worst.max(dist3(pa, pb));
    }
    worst
}

/// Key values of a channel sampled at `s` segments over `f` frames.
fn key_pos(k: usize, s: usize, f: usize) -> f64 {
    k as f64 * (f - 1) as f64 / s as f64
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
        lerp3(t[i], t[i + 1], fr)
    }
}

/// Rotation channel: reconstruct frame `fr` from `s` segments of keys (already quantised).
fn recon_q(keys: &[[f64; 4]], s: usize, f: usize, fr: usize) -> [f64; 4] {
    if s == 0 {
        return keys[0];
    }
    let t = fr as f64 * s as f64 / (f - 1) as f64;
    let i = (t.floor() as usize).min(s - 1);
    let frac = t - i as f64;
    nlerp(keys[i], keys[i + 1], frac)
}
fn recon_t(keys: &[V3], s: usize, f: usize, fr: usize) -> V3 {
    if s == 0 {
        return keys[0];
    }
    let t = fr as f64 * s as f64 / (f - 1) as f64;
    let i = (t.floor() as usize).min(s - 1);
    lerp3(keys[i], keys[i + 1], t - i as f64)
}

fn rot_bytes(bits: u32) -> usize {
    (3 * bits as usize + 2).div_ceil(8)
}

/// Best rotation channel for one joint: (segments, bits, bytes, worst error).
fn best_rot(
    j: &JointTrack,
    f: usize,
    verts: &[V3],
    budget: f64,
    allow_quant: bool,
) -> (usize, u32, usize, f64) {
    let env_sizes: Vec<u32> = std::env::var("ROT_SIZES")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or_else(|| vec![7, 10, 12, 15]);
    let sizes: &[u32] = if allow_quant { &env_sizes } else { &[0] };
    let mut best: Option<(usize, u32, usize, f64)> = None;
    for &bits in sizes {
        let per_key = if bits == 0 { 10 } else { rot_bytes(bits) };
        for s in 0..f {
            // s == 0: one key; otherwise s segments, s + 1 keys.
            let keys: Vec<[f64; 4]> = if s == 0 {
                vec![j.q[0]]
            } else {
                (0..=s).map(|k| sample_q(&j.q, key_pos(k, s, f))).collect()
            };
            let keys: Vec<[f64; 4]> = if bits == 0 {
                keys
            } else {
                keys.into_iter().map(|q| quantise_quat(q, bits)).collect()
            };
            let mut worst: f64 = 0.0;
            for fr in 0..f {
                let r = round_q12(mat_from_quat(recon_q(&keys, s, f, fr)));
                worst = worst.max(rot_err(&r, &j.m[fr], verts));
                if worst > budget {
                    break;
                }
            }
            if worst <= budget {
                let bytes = (if s == 0 { 1 } else { s + 1 }) * per_key;
                if best.is_none_or(|b| bytes < b.2) {
                    best = Some((s, bits, bytes, worst));
                }
                break;
            }
        }
    }
    best.unwrap_or_else(|| {
        let bytes = f * 10;
        (f - 1, 0, bytes, 0.0)
    })
}

/// Best translation channel: (segments, bytes_per_key, bytes, worst error).
fn best_trans(j: &JointTrack, f: usize, budget: f64, quant: bool) -> (usize, usize, usize, f64) {
    let step = (budget / 3f64.sqrt()).max(1e-6);
    let mut result = (f - 1, 6, f * 6, 0.0);
    for s in 0..f {
        let keys: Vec<V3> = if s == 0 {
            vec![j.t[0]]
        } else {
            (0..=s).map(|k| sample_t(&j.t, key_pos(k, s, f))).collect()
        };
        let (keys, per_key) = if quant {
            let mut lo = [f64::MAX; 3];
            let mut hi = [f64::MIN; 3];
            for k in &keys {
                for a in 0..3 {
                    lo[a] = lo[a].min(k[a]);
                    hi[a] = hi[a].max(k[a]);
                }
            }
            let mut bits = 0usize;
            for a in 0..3 {
                let range = hi[a] - lo[a];
                if range > 0.0 {
                    bits += ((range / step + 1.0).log2().ceil() as usize).max(1);
                }
            }
            let q: Vec<V3> = keys
                .iter()
                .map(|k| {
                    let mut o = *k;
                    for a in 0..3 {
                        if hi[a] > lo[a] {
                            o[a] = lo[a] + ((k[a] - lo[a]) / step).round() * step;
                        } else {
                            o[a] = lo[a];
                        }
                    }
                    o
                })
                .collect();
            (q, bits.div_ceil(8))
        } else {
            (
                keys.iter()
                    .map(|k| [k[0].round(), k[1].round(), k[2].round()])
                    .collect(),
                6,
            )
        };
        let mut worst: f64 = 0.0;
        for fr in 0..f {
            let r = recon_t(&keys, s, f, fr);
            worst = worst.max(dist3(r, j.t[fr]));
            if worst > budget {
                break;
            }
        }
        if worst <= budget {
            let n = if s == 0 { 1 } else { s + 1 };
            result = (s, per_key, n * per_key, worst);
            break;
        }
    }
    result
}

fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    o
}
fn mat_t(a: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = a[c][r];
        }
    }
    o
}

/// Closed-loop local (parent-relative) track estimate, HMA1 style: every joint
/// stores its pose relative to its reconstructed parent, so rigid joints
/// collapse to constants. Returns total bytes (keys plus `hdr` per joint).
fn local_variant(
    clip: &ClipData,
    parents: &[Option<usize>],
    sampled: &[Vec<V3>],
    budget: f64,
    hdr: usize,
) -> (usize, f64) {
    let f = clip.frames;
    let n = clip.joints.len();
    // Reconstructed model-space pose per joint per frame.
    let mut recon_m: Vec<Vec<M3>> = vec![vec![]; n];
    let mut recon_t: Vec<Vec<V3>> = vec![vec![]; n];
    let mut bytes = 0usize;
    let mut worst_total: f64 = 0.0;
    for j in 0..n {
        let jt = &clip.joints[j];
        let verts = &sampled[j];
        // Local truth.
        let (lq, lt): (Vec<[f64; 4]>, Vec<V3>) = {
            let mut qs: Vec<[f64; 4]> = vec![];
            let mut ts = vec![];
            for fr in 0..f {
                match parents[j] {
                    None => {
                        let mut q = jt.q[fr];
                        if let Some(prev) = qs.last() {
                            let d: f64 = (0..4).map(|i| prev[i] * q[i]).sum();
                            if d < 0.0 {
                                q = [-q[0], -q[1], -q[2], -q[3]];
                            }
                        }
                        qs.push(q);
                        ts.push(jt.t[fr]);
                    }
                    Some(p) => {
                        let pj = &clip.joints[p];
                        let lm = mat_mul(&mat_t(&pj.m[fr]), &jt.m[fr]);
                        let mut q = quat_from_mat(&lm);
                        if let Some(prev) = qs.last() {
                            let d: f64 = (0..4).map(|i| prev[i] * q[i]).sum();
                            if d < 0.0 {
                                q = [-q[0], -q[1], -q[2], -q[3]];
                            }
                        }
                        qs.push(q);
                        let dt = [
                            jt.t[fr][0] - pj.t[fr][0],
                            jt.t[fr][1] - pj.t[fr][1],
                            jt.t[fr][2] - pj.t[fr][2],
                        ];
                        ts.push(apply(&mat_t(&pj.m[fr]), dt));
                    }
                }
            }
            (qs, ts)
        };
        // Compose helper against the reconstructed parent.
        let compose = |lrot: M3, ltr: V3, fr: usize| -> (M3, V3) {
            match parents[j] {
                None => (
                    round_q12(lrot),
                    [ltr[0].round(), ltr[1].round(), ltr[2].round()],
                ),
                Some(p) => {
                    let r = round_q12(mat_mul(&recon_m[p][fr], &lrot));
                    let t = apply(&recon_m[p][fr], ltr);
                    (
                        r,
                        [
                            (t[0] + recon_t[p][fr][0]).round(),
                            (t[1] + recon_t[p][fr][1]).round(),
                            (t[2] + recon_t[p][fr][2]).round(),
                        ],
                    )
                }
            }
        };
        let total_err = |rm: &M3, rt: V3, fr: usize| -> f64 {
            let mut w: f64 = 0.0;
            for &v in verts.iter() {
                let a = apply(rm, v);
                let b = apply(&jt.m[fr], v);
                let da = [a[0] + rt[0], a[1] + rt[1], a[2] + rt[2]];
                let db = [b[0] + jt.t[fr][0], b[1] + jt.t[fr][1], b[2] + jt.t[fr][2]];
                w = w.max(dist3(da, db));
            }
            w
        };
        if verts.is_empty() || f < 2 {
            let mut rm = vec![];
            let mut rt = vec![];
            for fr in 0..f {
                let (m, t) = compose(mat_from_quat(lq[fr]), lt[fr], fr);
                rm.push(m);
                rt.push(t);
            }
            recon_m[j] = rm;
            recon_t[j] = rt;
            bytes += hdr + 6;
            continue;
        }
        // Stage 1: rotation channel (translation exact), budget 0.6 E.
        let mut best_rot: Option<(usize, u32, usize)> = None;
        for &bits in &[7u32, 10, 12, 15] {
            for s in 0..f {
                let keys: Vec<[f64; 4]> = if s == 0 {
                    vec![lq[0]]
                } else {
                    (0..=s).map(|k| sample_q(&lq, key_pos(k, s, f))).collect()
                };
                let keys: Vec<[f64; 4]> =
                    keys.into_iter().map(|q| quantise_quat(q, bits)).collect();
                let mut worst: f64 = 0.0;
                for fr in 0..f {
                    let (m, t) = compose(mat_from_quat(recon_q(&keys, s, f, fr)), lt[fr], fr);
                    worst = worst.max(total_err(&m, t, fr));
                    if worst > budget * 0.6 {
                        break;
                    }
                }
                if worst <= budget * 0.6 {
                    let b = (if s == 0 { 1 } else { s + 1 }) * rot_bytes(bits);
                    if best_rot.is_none_or(|x| b < x.2) {
                        best_rot = Some((s, bits, b));
                    }
                    break;
                }
            }
        }
        let (rs, rbits, rbytes) = best_rot.unwrap_or((f - 1, 15, f * rot_bytes(15)));
        let rkeys: Vec<[f64; 4]> = if rs == 0 {
            vec![lq[0]]
        } else {
            (0..=rs).map(|k| sample_q(&lq, key_pos(k, rs, f))).collect()
        };
        let rkeys: Vec<[f64; 4]> = rkeys.into_iter().map(|q| quantise_quat(q, rbits)).collect();
        let rot_at = |fr: usize| mat_from_quat(recon_q(&rkeys, rs, f, fr));
        // Stage 2: translation channel closed loop on the total error.
        let step_base = (budget * 0.4 / 3f64.sqrt()).max(1e-6);
        let mut chosen: Option<(usize, usize, usize, Vec<V3>)> = None;
        for s in 0..f {
            let keys: Vec<V3> = if s == 0 {
                vec![lt[0]]
            } else {
                (0..=s).map(|k| sample_t(&lt, key_pos(k, s, f))).collect()
            };
            let mut lo = [f64::MAX; 3];
            let mut hi = [f64::MIN; 3];
            for k in &keys {
                for a in 0..3 {
                    lo[a] = lo[a].min(k[a]);
                    hi[a] = hi[a].max(k[a]);
                }
            }
            let mut bits = 0usize;
            for a in 0..3 {
                if hi[a] > lo[a] {
                    bits += ((((hi[a] - lo[a]) / step_base) + 1.0).log2().ceil() as usize).max(1);
                }
            }
            let qkeys: Vec<V3> = keys
                .iter()
                .map(|k| {
                    let mut o = *k;
                    for a in 0..3 {
                        o[a] = if hi[a] > lo[a] {
                            lo[a] + ((k[a] - lo[a]) / step_base).round() * step_base
                        } else {
                            lo[a]
                        };
                    }
                    o
                })
                .collect();
            let mut worst: f64 = 0.0;
            for fr in 0..f {
                let (m, t) = compose(rot_at(fr), recon_t_local(&qkeys, s, f, fr), fr);
                worst = worst.max(total_err(&m, t, fr));
                if worst > budget {
                    break;
                }
            }
            if worst <= budget {
                let per_key = bits.div_ceil(8);
                let n = if s == 0 { 1 } else { s + 1 };
                chosen = Some((s, per_key, n * per_key, qkeys));
                worst_total = worst_total.max(worst);
                break;
            }
        }
        let (ts, _pk, tbytes, tkeys) = chosen.unwrap_or_else(|| (f - 1, 6, f * 6, lt.clone()));
        let mut rm = vec![];
        let mut rt = vec![];
        for fr in 0..f {
            let (m, t) = compose(rot_at(fr), recon_t_local(&tkeys, ts, f, fr), fr);
            rm.push(m);
            rt.push(t);
        }
        recon_m[j] = rm;
        recon_t[j] = rt;
        bytes += rbytes + tbytes + hdr;
    }
    (bytes, worst_total)
}
fn recon_t_local(keys: &[V3], s: usize, f: usize, fr: usize) -> V3 {
    if keys.len() == f && s == f - 1 && s != 0 {
        return keys[fr];
    }
    recon_t(keys, s, f, fr)
}

/// Dictionary vector quantisation estimate: greedily merge a joint's poses that
/// stay within `budget` model units of an existing representative. Decode is
/// unchanged from v5 (index into 16-byte records). Returns (clusters, worst err).
fn vq_joint(jt: &JointTrack, f: usize, verts: &[V3], budget: f64) -> (usize, f64) {
    let mut reps: Vec<usize> = vec![];
    let mut worst: f64 = 0.0;
    for fr in 0..f {
        let mut hit = false;
        for &r in &reps {
            // Pose error between frame `fr` and representative `r`.
            let mut e: f64 = 0.0;
            for &v in verts {
                let a = apply(&jt.m[fr], v);
                let b = apply(&jt.m[r], v);
                let da = [a[0] + jt.t[fr][0], a[1] + jt.t[fr][1], a[2] + jt.t[fr][2]];
                let db = [b[0] + jt.t[r][0], b[1] + jt.t[r][1], b[2] + jt.t[r][2]];
                e = e.max(dist3(da, db));
                if e > budget {
                    break;
                }
            }
            if e <= budget {
                worst = worst.max(e);
                hit = true;
                break;
            }
        }
        if !hit {
            reps.push(fr);
        }
    }
    (reps.len(), worst)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(&args[1]);
    let budget: f64 = args[2].parse().unwrap();
    let mut csv = String::from("model,clip,hz,frames,joints,now_bytes,keysonly_bytes,quantonly_bytes,both_bytes,worst_err\n");
    let mut mdirs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    mdirs.sort();
    let hdr_joint = 12usize;
    let mut totals = (0usize, 0usize, 0usize, 0usize);
    for mdir in mdirs {
        let mbytes = std::fs::read(mdir.join("model.psxmdl")).unwrap();
        let model = Model::from_bytes(&mbytes).unwrap();
        // Vertices touching each joint (primary part joint, plus blend secondaries).
        let mut per_joint: Vec<Vec<V3>> = vec![vec![]; model.joint_count() as usize];
        for p in 0..model.part_count() {
            let part = model.part(p).unwrap();
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let vert = model.vertex(v).unwrap();
                let pos = [
                    vert.position.x as f64,
                    vert.position.y as f64,
                    vert.position.z as f64,
                ];
                per_joint[part.joint_index() as usize].push(pos);
                if vert.is_blend() && (vert.joint1 as usize) < per_joint.len() {
                    per_joint[vert.joint1 as usize].push(pos);
                }
            }
        }
        let sampled: Vec<Vec<V3>> = per_joint.iter().map(|v| sample_verts(v, 24)).collect();
        let parents: Vec<Option<usize>> = (0..model.joint_count())
            .map(|j| model.joint(j).unwrap().parent().map(|p| p as usize))
            .collect();
        let parents_first = parents
            .iter()
            .enumerate()
            .all(|(j, p)| p.is_none_or(|p| p < j));
        println!("MODEL {} parents_first={parents_first}", mdir.display());
        let mname = mdir.file_name().unwrap().to_string_lossy().to_string();
        println!("MODEL {mname}");
        let mut clips: Vec<_> = std::fs::read_dir(&mdir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "psxanim"))
            .collect();
        clips.sort();
        let mut mt = (0usize, 0usize, 0usize, 0usize);
        let mut mt_local = 0usize;
        let (mut mt_vq16, mut mt_vq8) = (0usize, 0usize);
        for c in clips {
            let cname = c.file_stem().unwrap().to_string_lossy().to_string();
            let clip = load_clip(&c);
            let f = clip.frames;
            let (mut keys_only, mut quant_only, mut both) = (0usize, 0usize, 0usize);
            let mut worst_all: f64 = 0.0;
            let (mut vq_clusters, mut vq_err) = (0usize, 0.0f64);
            for (ji, jt) in clip.joints.iter().enumerate() {
                let (c, e) = if sampled[ji].is_empty() {
                    (1, 0.0)
                } else {
                    vq_joint(jt, f, &sampled[ji], budget)
                };
                vq_clusters += c;
                vq_err = vq_err.max(e);
            }
            let vq16 = 20 + (clip.joints.len() * f * 2).next_multiple_of(4) + vq_clusters * 16;
            let vq8 = if vq_clusters <= 256 {
                20 + (clip.joints.len() * f).next_multiple_of(4) + vq_clusters * 16
            } else {
                vq16
            };
            let (local_bytes, local_err) = if parents_first {
                local_variant(&clip, &parents, &sampled, budget, hdr_joint)
            } else {
                (0, 0.0)
            };
            for (ji, jt) in clip.joints.iter().enumerate() {
                let verts = &sampled[ji];
                if verts.is_empty() || f < 2 {
                    // Joint drives no vertices: one identity-quality key suffices.
                    keys_only += 16 + 3;
                    quant_only += rot_bytes(10) + 6 + hdr_joint;
                    both += rot_bytes(10) + 6 + hdr_joint;
                    continue;
                }
                let half = budget * 0.5;
                // (a) keys only: full-precision 10 B rotation + 6 B translation keys.
                let r = best_rot(jt, f, verts, half, false);
                let t = best_trans(jt, f, half, false);
                keys_only += r.2 + t.2 + 3;
                // (b) quantisation only: every frame kept, rotation and translation packed.
                let rq = {
                    // force s = f - 1 by trying the sizes at full key count
                    let mut best = (rot_bytes(15), 15);
                    for bits in [7u32, 10, 12, 15] {
                        let keys: Vec<[f64; 4]> =
                            jt.q.iter().map(|q| quantise_quat(*q, bits)).collect();
                        let mut w: f64 = 0.0;
                        for fr in 0..f {
                            w = w.max(rot_err(
                                &round_q12(mat_from_quat(keys[fr])),
                                &jt.m[fr],
                                verts,
                            ));
                        }
                        if w <= half * 0.5 {
                            best = (rot_bytes(bits), bits as usize);
                            break;
                        }
                    }
                    best.0
                };
                let tq = {
                    let step = (half * 0.5 / 3f64.sqrt()).max(1e-6);
                    let mut bits = 0usize;
                    for a in 0..3 {
                        let lo = jt.t.iter().map(|k| k[a]).fold(f64::MAX, f64::min);
                        let hi = jt.t.iter().map(|k| k[a]).fold(f64::MIN, f64::max);
                        if hi > lo {
                            bits += ((((hi - lo) / step) + 1.0).log2().ceil() as usize).max(1);
                        }
                    }
                    bits.div_ceil(8)
                };
                quant_only += f * (rq + tq) + hdr_joint;
                // (c) both: independent per-channel segments with packed keys.
                let r = best_rot(jt, f, verts, half, true);
                let t = best_trans(jt, f, half, true);
                both += r.2 + t.2 + hdr_joint;
                // worst error actually incurred by (c)
                worst_all = worst_all.max(r.3 + t.3);
            }
            println!(
                "  {:<36} {:>2}Hz f={:>3} j={:>2} now={:>6} keys={:>6} quant={:>6} both={:>6} ({:.0}%) err<={:.2} local={:>6} (err {:.2}) vq16={:>6} vq8={:>6} (err {:.2})",
                cname, clip.hz, f, clip.joints.len(), clip.bytes, keys_only, quant_only, both,
                100.0 * both as f64 / clip.bytes as f64, worst_all, local_bytes, local_err, vq16, vq8, vq_err
            );
            csv.push_str(&format!(
                "{mname},{cname},{},{f},{},{},{keys_only},{quant_only},{both},{worst_all:.3}\n",
                clip.hz,
                clip.joints.len(),
                clip.bytes
            ));
            mt.0 += clip.bytes;
            mt.1 += keys_only;
            mt.2 += quant_only;
            mt.3 += both;
            mt_local += local_bytes;
            mt_vq16 += vq16;
            mt_vq8 += vq8;
        }
        println!(
            "  MODEL TOTAL now={} keys-only={} quant-only={} both={} local={} vq16={} vq8={} (saved {} B)",
            mt.0, mt.1, mt.2, mt.3, mt_local, mt_vq16, mt_vq8, mt.0 as i64 - mt.3 as i64
        );
        totals.0 += mt.0;
        totals.1 += mt.1;
        totals.2 += mt.2;
        totals.3 += mt.3;
    }
    println!(
        "GRAND now={} keys-only={} quant-only={} both={}",
        totals.0, totals.1, totals.2, totals.3
    );
    if let Some(out) = args.get(3) {
        std::fs::write(out, csv).unwrap();
    }
}
