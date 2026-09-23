//! HMA1 encoder: per bone and clip, pick the cheapest key rate whose
//! vertex/joint error (measured in model space through the already-chosen,
//! already-quantized ancestors) stays within `tol` of the inherited floor.

use crate::hma_dec::{self, Affine, N_RATES, POS_BIND, POS_CLIP_CONST, RATE_CONST};
use crate::{apply, mat_quat, world_of, ClipSource, Mat34, Skeleton};

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    pub qfmt: u8,
    /// allowed extra error per bone, HL units
    pub tol: f64,
    pub closed_loop: bool,
    /// model-space tracks (no hierarchy at runtime)
    pub flat: bool,
    pub fit: bool,
    pub normalize: bool,
    /// restrict to these rate codes (bitmask over 0..6); const always allowed
    pub rate_mask: u8,
    pub cubic: bool,
    /// if > 0: pick the smallest tolerance whose bytes fit budget * today
    pub budget: f64,
    /// absolute bound: every point within tol of exact (floor permitting)
    pub absolute: bool,
}

/// Frames-per-segment for each rate code.
pub const RATE_STEP: [f64; N_RATES] = [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 16.0];

pub fn seg_counts(n_int: usize) -> [u16; N_RATES] {
    let mut s = [0u16; N_RATES];
    for r in 0..N_RATES {
        s[r] = ((n_int as f64 / RATE_STEP[r]).ceil() as u16).max(1);
    }
    s
}
pub fn seg_factors(n_int: usize, seg: &[u16; N_RATES]) -> [u16; N_RATES] {
    let mut f = [0u16; N_RATES];
    for r in 0..N_RATES {
        f[r] = ((seg[r] as f64 * 32768.0 / n_int as f64).round() as u32).min(32768) as u16;
    }
    f
}

/// Segment index/fraction exactly as the decoder derives it.
pub fn segpos(pos_q8: u32, seg: u16, factor: u16) -> (usize, i32) {
    let sp = (pos_q8 * factor as u32) >> 15;
    let (mut i, mut f) = ((sp >> 8) as usize, (sp & 255) as i32);
    if i >= seg as usize {
        i = (seg as usize).saturating_sub(1);
        f = 256;
    }
    (i, f)
}

#[derive(Clone)]
pub struct Track {
    pub rc: u8,
    pub pc: u8,
    /// 12-bit rotation keys (qfmt 3 only; otherwise the clip format rules)
    pub hi: bool,
    pub rot: Vec<[i32; 4]>,
    pub pos: Vec<[i32; 3]>,
}

pub fn quantize_quat(q: [f64; 4], qfmt: u8) -> [i32; 4] {
    // returns the Q12 values the decoder will read back
    let mut out = [0i32; 4];
    for k in 0..4 {
        out[k] = match qfmt {
            0 => ((q[k] * 128.0).round().clamp(-127.0, 127.0) as i32) << 5,
            1 => ((q[k] * 2048.0).round().clamp(-2047.0, 2047.0) as i32) << 1,
            _ => (q[k] * 4096.0).round().clamp(-32768.0, 32767.0) as i32,
        };
    }
    out
}

pub fn write_quat(o: &mut Vec<u8>, q: [i32; 4], qfmt: u8) {
    match qfmt {
        0 => {
            for k in 0..4 {
                o.push((q[k] >> 5) as i8 as u8);
            }
        }
        1 => {
            let c: Vec<u32> = q.iter().map(|&v| ((v >> 1) as u32) & 0xfff).collect();
            let packed = c[0] | (c[1] << 12) | (c[2] << 24);
            let w2 = (c[2] >> 8) | (c[3] << 4);
            o.extend_from_slice(&(packed as u16).to_le_bytes());
            o.extend_from_slice(&((packed >> 16) as u16).to_le_bytes());
            o.extend_from_slice(&(w2 as u16).to_le_bytes());
        }
        _ => {
            for k in 0..4 {
                o.extend_from_slice(&(q[k] as i16).to_le_bytes());
            }
        }
    }
}

fn lerp4(a: [i32; 4], b: [i32; 4], f: i32) -> [i32; 4] {
    [
        a[0] + (((b[0] - a[0]) * f) >> 8),
        a[1] + (((b[1] - a[1]) * f) >> 8),
        a[2] + (((b[2] - a[2]) * f) >> 8),
        a[3] + (((b[3] - a[3]) * f) >> 8),
    ]
}
fn lerp3(a: [i32; 3], b: [i32; 3], f: i32) -> [i32; 3] {
    [
        a[0] + (((b[0] - a[0]) * f) >> 8),
        a[1] + (((b[1] - a[1]) * f) >> 8),
        a[2] + (((b[2] - a[2]) * f) >> 8),
    ]
}

#[allow(dead_code)]
pub struct ClipCtx {
    pub cubic: bool,
    pub n_int: usize,
    pub seg: [u16; N_RATES],
    pub factor: [u16; N_RATES],
}

impl ClipCtx {
    pub fn eval(
        &self,
        tr: &Track,
        bind_t: [i32; 3],
        pos_q8: u32,
        normalize: bool,
    ) -> ([[i16; 3]; 3], [i32; 3]) {
        let q = if tr.rc == RATE_CONST {
            tr.rot[0]
        } else {
            let (i, f) = segpos(
                pos_q8,
                self.seg[tr.rc as usize],
                self.factor[tr.rc as usize],
            );
            if self.cubic {
                let l = tr.rot.len() - 1;
                hma_dec::cr4(
                    [
                        tr.rot[i.saturating_sub(1)],
                        tr.rot[i],
                        tr.rot[i + 1],
                        tr.rot[(i + 2).min(l)],
                    ],
                    &hma_dec::cr_weights(f),
                )
            } else {
                lerp4(tr.rot[i], tr.rot[i + 1], f)
            }
        };
        let t = if tr.pc == POS_BIND {
            bind_t
        } else if tr.pc == POS_CLIP_CONST {
            tr.pos[0]
        } else {
            let (i, f) = segpos(
                pos_q8,
                self.seg[tr.pc as usize],
                self.factor[tr.pc as usize],
            );
            if self.cubic {
                let l = tr.pos.len() - 1;
                let e = |k: usize| [tr.pos[k][0], tr.pos[k][1], tr.pos[k][2], 0];
                let r = hma_dec::cr4(
                    [e(i.saturating_sub(1)), e(i), e(i + 1), e((i + 2).min(l))],
                    &hma_dec::cr_weights(f),
                );
                [r[0], r[1], r[2]]
            } else {
                lerp3(tr.pos[i], tr.pos[i + 1], f)
            }
        };
        (hma_dec::quat_to_mat(q, normalize), t)
    }
}

pub struct Encoded {
    pub bytes: Vec<u8>,
    /// source bone index for each HMA bone
    pub bones: Vec<usize>,
    pub key_bytes: usize,
    pub rate_hist: [usize; 8],
    /// [header+modes, const rot, keyed rot, pos, range-reduced estimate of keyed rot]
    pub parts: [usize; 5],
}

fn affine_f(a: &Affine) -> Mat34 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a.r[i][j] as f64 / 4096.0;
        }
    }
    let k = (1 << hma_dec::TFRAC) as f64;
    (r, [a.t[0] as f64 / k, a.t[1] as f64 / k, a.t[2] as f64 / k])
}

fn transpose_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[0][i] * b[0][j] + a[1][i] * b[1][j] + a[2][i] * b[2][j];
        }
    }
    r
}

/// Least-squares fit of piecewise-linear key values to samples (x in key
/// units, value). Tridiagonal normal equations, solved per component.
fn ls_fit(nkeys: usize, samples: &[(f64, Vec<f64>)], init: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let dim = init[0].len();
    let mut a = vec![[0.0f64; 3]; nkeys]; // sub, diag, super
    let mut rhs = vec![vec![0.0f64; dim]; nkeys];
    for (x, v) in samples {
        let i = (x.floor() as usize).min(nkeys - 2);
        let t = x - i as f64;
        let (w0, w1) = (1.0 - t, t);
        a[i][1] += w0 * w0;
        a[i][2] += w0 * w1;
        a[i + 1][0] += w0 * w1;
        a[i + 1][1] += w1 * w1;
        for k in 0..dim {
            rhs[i][k] += w0 * v[k];
            rhs[i + 1][k] += w1 * v[k];
        }
    }
    // regularise keys with no support toward their initial value
    for i in 0..nkeys {
        let lam = 1e-3;
        a[i][1] += lam;
        for k in 0..dim {
            rhs[i][k] += lam * init[i][k];
        }
    }
    // Thomas algorithm
    let mut c = vec![0.0; nkeys];
    let mut d = vec![vec![0.0; dim]; nkeys];
    for i in 0..nkeys {
        let m = a[i][1] - if i > 0 { a[i][0] * c[i - 1] } else { 0.0 };
        c[i] = a[i][2] / m;
        for k in 0..dim {
            d[i][k] = (rhs[i][k] - if i > 0 { a[i][0] * d[i - 1][k] } else { 0.0 }) / m;
        }
    }
    let mut x = vec![vec![0.0; dim]; nkeys];
    for i in (0..nkeys).rev() {
        for k in 0..dim {
            x[i][k] = d[i][k]
                - if i + 1 < nkeys {
                    c[i] * x[i + 1][k]
                } else {
                    0.0
                };
        }
    }
    x
}

/// Bones the runtime must evaluate: every bone carrying vertices or a
/// hitbox, plus all of their ancestors (parents-first order preserved).
pub fn needed_bones(parents: &[i32], carries: &[bool]) -> Vec<usize> {
    let mut need = carries.to_vec();
    for b in (0..parents.len()).rev() {
        if need[b] && parents[b] >= 0 {
            need[parents[b] as usize] = true;
        }
    }
    (0..parents.len()).filter(|&b| need[b]).collect()
}

/// Serialized size of one bone's tracks.
pub fn track_bytes(t: &Track, qfmt: u8) -> usize {
    let mut v = Vec::new();
    write_track(&mut v, t, qfmt);
    v.len()
}

fn rr_width_rot(t: &Track) -> (usize, [i32; 4]) {
    let mut base = [0i32; 4];
    let mut range = 0;
    for c in 0..4 {
        let mn = t.rot.iter().map(|q| q[c] >> 5).min().unwrap();
        let mx = t.rot.iter().map(|q| q[c] >> 5).max().unwrap();
        base[c] = mn;
        range = range.max(mx - mn);
    }
    (
        if range < 16 {
            2
        } else if range < 64 {
            3
        } else {
            4
        },
        base,
    )
}

fn rr_width_pos(t: &Track) -> (usize, [i32; 3]) {
    let mut base = [0i32; 3];
    let mut range = 0;
    for c in 0..3 {
        let mn = t.pos.iter().map(|q| q[c]).min().unwrap();
        let mx = t.pos.iter().map(|q| q[c]).max().unwrap();
        base[c] = mn;
        range = range.max(mx - mn);
    }
    (
        if range < 32 {
            2
        } else if range < 256 {
            3
        } else if range < 1024 {
            4
        } else {
            6
        },
        base,
    )
}

pub fn write_track(o: &mut Vec<u8>, t: &Track, qfmt: u8) {
    if qfmt != 3 {
        for q in &t.rot {
            write_quat(o, *q, qfmt);
        }
        for p in &t.pos {
            for k in 0..3 {
                o.extend_from_slice(&(p[k].clamp(-32768, 32767) as i16).to_le_bytes());
            }
        }
        return;
    }
    // qfmt 3: 12-bit raw when hi, else 8-bit; keyed 8-bit tracks range-reduced
    if t.hi || t.rc == RATE_CONST {
        for q in &t.rot {
            write_quat(o, *q, if t.hi { 1 } else { 0 });
        }
    } else {
        let (kb, base) = rr_width_rot(t);
        for c in 0..4 {
            o.push(base[c] as i8 as u8);
        }
        o.push(kb as u8);
        o.push(0);
        let w = kb as u32 * 2; // bits per component
        for q in &t.rot {
            let mut word = 0u32;
            for c in 0..4 {
                word |= (((q[c] >> 5) - base[c]) as u32) << (w * c as u32);
            }
            o.extend_from_slice(&word.to_le_bytes()[..kb]);
        }
    }
    if t.pc == POS_CLIP_CONST {
        let p = t.pos[0];
        for k in 0..3 {
            o.extend_from_slice(&(p[k].clamp(-32768, 32767) as i16).to_le_bytes());
        }
    } else if (t.pc as usize) < N_RATES {
        let (kb, base) = rr_width_pos(t);
        for c in 0..3 {
            o.extend_from_slice(&(base[c].clamp(-32768, 32767) as i16).to_le_bytes());
        }
        o.push(kb as u8);
        o.push(0);
        let w: u32 = match kb {
            2 => 5,
            3 => 8,
            4 => 10,
            _ => 16,
        };
        for p in &t.pos {
            let mut word = 0u64;
            for c in 0..3 {
                word |= (((p[c] - base[c]) as u64) & ((1u64 << w) - 1)) << (w * c as u32);
            }
            o.extend_from_slice(&word.to_le_bytes()[..kb]);
        }
    }
}

const CLIP_HEADER_BYTES: usize = hma_dec::CLIP_HEADER;

/// Encode `clips` for the skeleton's `bones` (see [`needed_bones`]).
pub fn encode(sk: &Skeleton, clips: &[&dyn ClipSource], bones: &[usize], o: Opts) -> Encoded {
    let scale = sk.scale;
    let nb = bones.len();
    let mut remap = vec![usize::MAX; sk.parents.len()];
    for (i, &b) in bones.iter().enumerate() {
        remap[b] = i;
    }
    let parent: Vec<Option<usize>> = bones
        .iter()
        .map(|&b| {
            if o.flat || sk.parents[b] < 0 {
                None
            } else {
                Some(remap[sk.parents[b] as usize])
            }
        })
        .collect();
    let children: Vec<Vec<usize>> = (0..nb)
        .map(|i| (0..nb).filter(|&c| parent[c] == Some(i)).collect())
        .collect();
    let bind_t: Vec<[i32; 3]> = bones
        .iter()
        .map(|&b| {
            if o.flat {
                return [0; 3];
            }
            let v = sk.bind_t[b];
            let k = (1 << hma_dec::TFRAC) as f64;
            [
                (v[0] * k).round() as i32,
                (v[1] * k).round() as i32,
                (v[2] * k).round() as i32,
            ]
        })
        .collect();
    let tol_c = o.tol * scale;

    let mut blob = Vec::new();
    blob.extend_from_slice(&(nb as u16).to_le_bytes());
    blob.extend_from_slice(&(clips.len() as u16).to_le_bytes());
    for i in 0..nb {
        blob.push(parent[i].map_or(0xff, |p| p as u8));
    }
    while blob.len() % 2 != 0 {
        blob.push(0);
    }
    for t in &bind_t {
        for k in 0..3 {
            blob.extend_from_slice(&(t[k].clamp(-32768, 32767) as i16).to_le_bytes());
        }
    }
    while blob.len() % 4 != 0 {
        blob.push(0);
    }
    let table = blob.len();
    blob.resize(table + clips.len() * 4, 0);
    let mut key_bytes = 0usize;
    let mut rate_hist = [0usize; 8];
    let mut parts = [0usize; 5];
    let mut seen: Vec<(usize, usize)> = Vec::new();

    for (ci, clip) in clips.iter().enumerate() {
        if let Some(&(_, off)) = seen.iter().find(|(s, _)| *s == clip.key()) {
            blob[table + ci * 4..table + ci * 4 + 4].copy_from_slice(&(off as u32).to_le_bytes());
            continue;
        }
        while blob.len() % 2 != 0 {
            blob.push(0);
        }
        let off = blob.len();
        seen.push((clip.key(), off));
        blob[table + ci * 4..table + ci * 4 + 4].copy_from_slice(&(off as u32).to_le_bytes());
        let n_int = clip.n_int().max(1);
        let seg = seg_counts(n_int);
        let factor = seg_factors(n_int, &seg);
        let ctx = ClipCtx {
            cubic: o.cubic,
            n_int,
            seg,
            factor,
        };
        // exact data at integer frames
        let frames: Vec<usize> = (0..=n_int).collect();
        let wex: Vec<Vec<Mat34>> = frames
            .iter()
            .map(|&f| world_of(&sk.parents, &clip.local_at(f as f64)))
            .collect();
        let lex: Vec<Vec<Mat34>> = frames.iter().map(|&f| clip.local_at(f as f64)).collect();
        // decoded world per frame, filled parents-first
        let mut wdec: Vec<Vec<Affine>> = vec![vec![Affine::IDENTITY; nb]; frames.len()];
        let mut tracks: Vec<Track> = Vec::with_capacity(nb);

        // key position world/local cache helpers
        let wcache: std::cell::RefCell<std::collections::HashMap<u64, std::rc::Rc<Vec<Mat34>>>> =
            Default::default();
        let lcache: std::cell::RefCell<std::collections::HashMap<u64, std::rc::Rc<Vec<Mat34>>>> =
            Default::default();
        let exact_world_at = |pos: f64| -> std::rc::Rc<Vec<Mat34>> {
            wcache
                .borrow_mut()
                .entry(pos.to_bits())
                .or_insert_with(|| std::rc::Rc::new(world_of(&sk.parents, &clip.local_at(pos))))
                .clone()
        };
        let exact_local_at = |pos: f64| -> std::rc::Rc<Vec<Mat34>> {
            lcache
                .borrow_mut()
                .entry(pos.to_bits())
                .or_insert_with(|| std::rc::Rc::new(clip.local_at(pos)))
                .clone()
        };

        // Lever arm of each bone over everything it carries: descendant
        // vertices' distance from its origin (frame 0). Axis points at that
        // radius make a bone's rotation error visible even when its children
        // sit exactly on its origin (common for Bip01 roots and gun rigs).
        let mut radius = vec![0.0f64; nb];
        for bi in 0..nb {
            let origin = wex[0][bones[bi]].1;
            for (vb, verts) in sk.verts.iter().enumerate() {
                if verts.is_empty() || remap[vb] == bi || remap[vb] == usize::MAX {
                    continue;
                }
                // is vb a descendant of bones[bi]?
                let mut c = parent[remap[vb]];
                let mut hit = false;
                while let Some(p) = c {
                    if p == bi {
                        hit = true;
                        break;
                    }
                    c = parent[p];
                }
                if !hit {
                    continue;
                }
                for v in verts {
                    let w = apply(&wex[0][vb], *v);
                    let d = ((w[0] - origin[0]).powi(2)
                        + (w[1] - origin[1]).powi(2)
                        + (w[2] - origin[2]).powi(2))
                    .sqrt();
                    if d > radius[bi] {
                        radius[bi] = d;
                    }
                }
            }
        }
        for bi in 0..nb {
            let sb = bones[bi];
            // points in this bone's frame for each integer frame
            let mut pts_local: Vec<[f64; 3]> = sk.verts[sb].clone();
            let child_pts: Vec<usize> = children[bi].clone();
            if radius[bi] > 0.0 {
                let r = radius[bi];
                pts_local.extend_from_slice(&[[r, 0.0, 0.0], [0.0, r, 0.0], [0.0, 0.0, r]]);
            }
            let has_pts = !pts_local.is_empty() || !child_pts.is_empty();
            let npts_v = pts_local.len();
            let _ = &mut pts_local;
            // decoded parent at arbitrary pos_q8 (recursive through chosen tracks)
            let dec_chain = |pos_q8: u32, tracks: &Vec<Track>| -> Affine {
                let mut chain = Vec::new();
                let mut c = parent[bi];
                while let Some(p) = c {
                    chain.push(p);
                    c = parent[p];
                }
                let mut acc = Affine::IDENTITY;
                let mut first = true;
                for &p in chain.iter().rev() {
                    let (r, t) = ctx.eval(&tracks[p], bind_t[p], pos_q8, o.normalize);
                    acc = if first {
                        Affine { r, t }
                    } else {
                        hma_dec::compose(&acc, &r, t)
                    };
                    first = false;
                }
                acc
            };
            // target local (rotation quat + translation) at fractional pos
            let target_at = |pos: f64, tracks: &Vec<Track>| -> ([f64; 4], [f64; 3]) {
                if o.flat {
                    let w = &exact_world_at(pos)[sb];
                    return (mat_quat(&w.0), w.1);
                }
                let l = &exact_local_at(pos)[sb];
                if o.closed_loop && parent[bi].is_some() {
                    let pd = affine_f(&dec_chain((pos * 256.0).round() as u32, tracks));
                    let w = &exact_world_at(pos)[sb];
                    let r = transpose_mul(&pd.0, &w.0);
                    (mat_quat(&r), l.1)
                } else {
                    (mat_quat(&l.0), l.1)
                }
            };
            // error of a candidate local evaluator over integer frames
            let err_of = |local: &dyn Fn(usize) -> Affine| -> f64 {
                let mut worst = 0.0f64;
                for (fi, _) in frames.iter().enumerate() {
                    let pa = parent[bi].map(|p| wdec[fi][p]);
                    let loc = local(fi);
                    let w = match pa {
                        Some(pa) => hma_dec::compose(&pa, &loc.r, loc.t),
                        None => loc,
                    };
                    let wf = affine_f(&w);
                    let ex = &wex[fi][sb];
                    let mut check = |p: [f64; 3]| {
                        let a = apply(&wf, p);
                        let b = apply(ex, p);
                        let d =
                            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2))
                                .sqrt();
                        if d > worst {
                            worst = d;
                        }
                    };
                    for p in &pts_local[..npts_v] {
                        check(*p);
                    }
                    for &c in &child_pts {
                        check(lex[fi][bones[c]].1);
                    }
                }
                worst
            };
            // translation behaviour over the clip (local, or world if flat)
            let tr_f: Vec<[f64; 3]> = frames
                .iter()
                .enumerate()
                .map(|(fi, _)| {
                    let t = if o.flat { wex[fi][sb].1 } else { lex[fi][sb].1 };
                    let k = (1 << hma_dec::TFRAC) as f64;
                    [t[0] * k, t[1] * k, t[2] * k]
                })
                .collect();
            let dev_bind = tr_f
                .iter()
                .map(|t| {
                    (0..3)
                        .map(|k| (t[k] - bind_t[bi][k] as f64).abs())
                        .fold(0.0, f64::max)
                })
                .fold(0.0, f64::max);
            let mean: [f64; 3] = {
                let mut s = [0.0; 3];
                for t in &tr_f {
                    for k in 0..3 {
                        s[k] += t[k];
                    }
                }
                [
                    s[0] / tr_f.len() as f64,
                    s[1] / tr_f.len() as f64,
                    s[2] / tr_f.len() as f64,
                ]
            };
            let dev_mean = tr_f
                .iter()
                .map(|t| (0..3).map(|k| (t[k] - mean[k]).abs()).fold(0.0, f64::max))
                .fold(0.0, f64::max);
            let pos_mode_fixed = if !o.flat && dev_bind <= 1.0 {
                Some(POS_BIND)
            } else if dev_mean <= 1.0 {
                Some(POS_CLIP_CONST)
            } else {
                None
            };

            // inherited floor: exact local (float) through decoded parents
            let floor = if has_pts {
                err_of(&|fi: usize| {
                    let l = &lex[fi][sb];
                    let src = if o.flat { &wex[fi][sb] } else { l };
                    let mut r = [[0i16; 3]; 3];
                    for i in 0..3 {
                        for j in 0..3 {
                            r[i][j] = (src.0[i][j] * 4096.0).round() as i16;
                        }
                    }
                    let k = (1 << hma_dec::TFRAC) as f64;
                    Affine {
                        r,
                        t: [
                            (src.1[0] * k).round() as i32,
                            (src.1[1] * k).round() as i32,
                            (src.1[2] * k).round() as i32,
                        ],
                    }
                })
            } else {
                0.0
            };

            let build = |rc: u8, pc: u8, hi: bool, tracks: &Vec<Track>| -> Track {
                let kfmt = if o.qfmt == 3 {
                    if hi {
                        1
                    } else {
                        0
                    }
                } else {
                    o.qfmt
                };
                let mut prev = [0.0f64, 0.0, 0.0, 1.0];
                let mut rot_f: Vec<[f64; 4]> = Vec::new();
                let nk_r = hma_dec::key_count(&seg, rc);
                for k in 0..nk_r {
                    let pos = if rc == RATE_CONST {
                        0.0
                    } else {
                        (k as f64 * n_int as f64 / seg[rc as usize] as f64).min(n_int as f64)
                    };
                    let (mut q, _) = target_at(pos, tracks);
                    let dot: f64 = (0..4).map(|i| q[i] * prev[i]).sum();
                    if dot < 0.0 {
                        for v in &mut q {
                            *v = -*v;
                        }
                    }
                    prev = q;
                    rot_f.push(q);
                }
                if rc == RATE_CONST && nk_r == 1 {
                    // average over the clip for a constant key
                    let mut s = [0.0f64; 4];
                    for fi in 0..frames.len() {
                        let (mut q, _) = target_at(fi as f64, tracks);
                        let dot: f64 = (0..4).map(|i| q[i] * rot_f[0][i]).sum();
                        if dot < 0.0 {
                            for v in &mut q {
                                *v = -*v;
                            }
                        }
                        for i in 0..4 {
                            s[i] += q[i];
                        }
                    }
                    let n = (s.iter().map(|v| v * v).sum::<f64>()).sqrt();
                    if n > 0.0 {
                        rot_f[0] = [s[0] / n, s[1] / n, s[2] / n, s[3] / n];
                    }
                }
                if o.fit && !o.cubic && rc != RATE_CONST && nk_r > 2 {
                    let samples: Vec<(f64, Vec<f64>)> = (0..frames.len())
                        .map(|fi| {
                            let x = fi as f64 * seg[rc as usize] as f64 / n_int as f64;
                            let (mut q, _) = target_at(fi as f64, tracks);
                            // align with the nearest key's hemisphere
                            let ki = (x.round() as usize).min(nk_r - 1);
                            let dot: f64 = (0..4).map(|i| q[i] * rot_f[ki][i]).sum();
                            if dot < 0.0 {
                                for v in &mut q {
                                    *v = -*v;
                                }
                            }
                            (x, q.to_vec())
                        })
                        .collect();
                    let init: Vec<Vec<f64>> = rot_f.iter().map(|q| q.to_vec()).collect();
                    let fitted = ls_fit(nk_r, &samples, &init);
                    for (k, q) in fitted.iter().enumerate() {
                        let n = (q.iter().map(|v| v * v).sum::<f64>()).sqrt().max(1e-9);
                        rot_f[k] = [q[0] / n, q[1] / n, q[2] / n, q[3] / n];
                    }
                }
                let rot = rot_f.iter().map(|q| quantize_quat(*q, kfmt)).collect();
                let pos = match pc {
                    POS_BIND => Vec::new(),
                    POS_CLIP_CONST => vec![[
                        mean[0].round() as i32,
                        mean[1].round() as i32,
                        mean[2].round() as i32,
                    ]],
                    _ => {
                        let nk = hma_dec::key_count(&seg, pc);
                        (0..nk)
                            .map(|k| {
                                let pos = (k as f64 * n_int as f64 / seg[pc as usize] as f64)
                                    .min(n_int as f64);
                                let (_, t) = target_at(pos, tracks);
                                let k = (1 << hma_dec::TFRAC) as f64;
                                [
                                    (t[0] * k).round() as i32,
                                    (t[1] * k).round() as i32,
                                    (t[2] * k).round() as i32,
                                ]
                            })
                            .collect()
                    }
                };
                Track {
                    rc,
                    pc,
                    hi,
                    rot,
                    pos,
                }
            };

            // Rotation first (translation exact or fixed), then translation,
            // each the cheapest serialized option inside the error bound.
            let rates: Vec<u8> = (0..N_RATES as u8)
                .rev()
                .filter(|r| o.rate_mask & (1 << r) != 0 || *r == 0)
                .collect();
            let his: Vec<bool> = if o.qfmt == 3 {
                vec![false, true]
            } else {
                vec![false]
            };
            let bound = |floor: f64| {
                if o.absolute {
                    tol_c.max(floor + 0.05 * tol_c)
                } else {
                    floor + tol_c
                }
            };
            let err_tr = |tr: &Track| -> f64 {
                err_of(&|fi: usize| {
                    let (r, t) = ctx.eval(tr, bind_t[bi], (fi * 256) as u32, o.normalize);
                    Affine { r, t }
                })
            };
            let pc0 = pos_mode_fixed.unwrap_or(0);
            let mut rot_cands: Vec<(u8, bool)> = Vec::new();
            if pos_mode_fixed.is_some() || o.qfmt == 3 {
                for &h in &his {
                    rot_cands.push((RATE_CONST, h));
                }
            }
            for &r in &rates {
                for &h in &his {
                    rot_cands.push((r, h));
                }
            }
            let joint = pos_mode_fixed.is_none() && o.qfmt != 3; // legacy: translation shares the rate
            let chosen: Option<Track>;
            if !has_pts {
                let (rc, h) = rot_cands[0];
                let pc = if joint { rc.min(5) } else { pc0 };
                chosen = Some(build(rc, pc, h, &tracks));
            } else {
                let mut best: Option<(usize, Track)> = None;
                let mut finest: Option<Track> = None;
                for &(rc, h) in &rot_cands {
                    if joint && rc == RATE_CONST {
                        continue;
                    }
                    let pc = if joint { rc.min(5) } else { pc0 };
                    let tr = build(rc, pc, h, &tracks);
                    let cost = track_bytes(&tr, o.qfmt);
                    if best.as_ref().is_some_and(|(c, _)| cost >= *c) {
                        continue;
                    }
                    if err_tr(&tr) <= bound(floor) {
                        best = Some((cost, tr));
                    } else if rc == 0 && (finest.is_none() || h) {
                        finest = Some(tr);
                    }
                }
                let mut tr = best.map(|(_, t)| t).or(finest).unwrap();
                if pos_mode_fixed.is_none() && !joint {
                    // now the cheapest translation rate that keeps the bound
                    let e_ref = err_tr(&tr).max(bound(floor));
                    let mut tbest: Option<(usize, Track)> = None;
                    for pc in (0..6u8).rev() {
                        let cand = build(tr.rc, pc, tr.hi, &tracks);
                        let cost = track_bytes(&cand, o.qfmt);
                        if tbest.as_ref().is_some_and(|(c, _)| cost >= *c) {
                            continue;
                        }
                        if err_tr(&cand) <= e_ref {
                            tbest = Some((cost, cand));
                        }
                    }
                    if let Some((_, t)) = tbest {
                        tr = t;
                    }
                }
                chosen = Some(tr);
            }
            let tr = chosen.unwrap();
            rate_hist[tr.rc as usize] += 1;
            // commit decoded world for children
            for fi in 0..frames.len() {
                let (r, t) = ctx.eval(&tr, bind_t[bi], (fi * 256) as u32, o.normalize);
                wdec[fi][bi] = match parent[bi] {
                    Some(p) => hma_dec::compose(&wdec[fi][p], &r, t),
                    None => Affine { r, t },
                };
            }
            tracks.push(tr);
        }
        // serialize clip
        blob.extend_from_slice(&(n_int as u16).to_le_bytes());
        blob.push(if clip.looping() { 1 } else { 0 } | if o.cubic { 2 } else { 0 });
        blob.push(o.qfmt);
        for r in 0..N_RATES {
            blob.extend_from_slice(&seg[r].to_le_bytes());
        }
        for r in 0..N_RATES {
            blob.extend_from_slice(&factor[r].to_le_bytes());
        }
        for t in &tracks {
            blob.push(t.rc | (t.pc << 3) | if t.hi { 0x40 } else { 0 });
        }
        while blob.len() % 2 != 0 {
            blob.push(0);
        }
        parts[0] += CLIP_HEADER_BYTES + nb + 4;
        let qbytes = hma_dec::quat_bytes(o.qfmt);
        for t in &tracks {
            if t.rc == RATE_CONST {
                parts[1] += qbytes;
            } else {
                parts[2] += t.rot.len() * qbytes;
                // range reduction estimate: per component bits for the same step
                let step = match o.qfmt {
                    0 => 32,
                    1 => 2,
                    _ => 1,
                };
                let mut bits = 0u32;
                for c in 0..4 {
                    let mn = t.rot.iter().map(|q| q[c]).min().unwrap();
                    let mx = t.rot.iter().map(|q| q[c]).max().unwrap();
                    let levels = ((mx - mn) / step + 1) as u32;
                    bits += 32 - (levels.max(1) - 1).leading_zeros();
                }
                parts[4] += 4 + (t.rot.len() * bits as usize).div_ceil(8);
            }
            parts[3] += t.pos.len() * 6;
        }
        let kstart = blob.len();
        for t in tracks.iter() {
            write_track(&mut blob, t, o.qfmt);
        }
        key_bytes += blob.len() - kstart;
    }
    Encoded {
        bytes: blob,
        bones: bones.to_vec(),
        key_bytes,
        rate_hist,
        parts,
    }
}
