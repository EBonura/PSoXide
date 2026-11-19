//! Measurement only: what parent-relative clips would cost and save.
//!
//! Cooks a project twice (track budget 0 = the pose tables the track encoder
//! takes as input, and the project's budget = what ships), then per clip of
//! every skinned model:
//! - `ship`: re-encodes the pose table exactly as the cook does (checked
//!   byte-identical to the shipped clip);
//! - `A` (pivot-derived translation): model-space rotations as today, only roots
//!   (and joints whose pivot does not hold, stretch) keep a translation track;
//!   every other joint's translation follows from its parent's pose and a fixed
//!   pivot: t_j = t_p + (R_p p_j >> 12) - (R_j p_j >> 12), integer, as two GTE
//!   MVMVA would give it;
//! - `B` (parent-relative rotation, error-compensated): each joint stores
//!   R_p'^T R_j where R_p' is the parent's decoded (integer-composed) matrix,
//!   encoded depth by depth so quantisation does not accumulate; the guest
//!   composes R_j' = R_p' Q_j' (one 3x3 product) and derives t as in A;
//! - tuned variants (`A4`, `B4`): the encoder budget lowered until the worst
//!   world-space vertex error against the source poses is within 4 units.
//!
//! Errors are in model units: vertex probes (the encoder's probes: the joint's
//! vertices and lever arms) and joint positions (pivots), against the source
//! pose tables at whole frames and against the shipped clip at every 1/16 frame.
//!
//! Usage: anim_parent_rel <project.ron> <model-name-substring>...

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use psx_anim_tracks::{encode, ClipInput, JointInput, Options};
use psx_asset::{Animation, JointPose, Model};
use psxed_project::{
    animation_tracks::joint_probes, playtest::build_package, ProjectDocument, ResourceData,
};

type M = [[i16; 3]; 3];

#[derive(Clone, Copy)]
struct Pose {
    m: M,
    t: [i32; 3],
}

/// Stored pose matrices are column-major (the renderer reads `matrix[col][row]`,
/// `scaled_pose_matrix` in psx-engine render3d.rs); `Pose::m` is the row-major
/// rotation the renderer applies.
fn tr(m: &M) -> M {
    [0, 1, 2].map(|r| [0, 1, 2].map(|c| m[c][r]))
}

fn from_jp(p: &JointPose) -> Pose {
    Pose {
        m: tr(&p.matrix),
        t: [p.translation.x, p.translation.y, p.translation.z],
    }
}

fn apply(p: &Pose, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| {
        (f64::from(p.m[r][0]) * v[0] + f64::from(p.m[r][1]) * v[1] + f64::from(p.m[r][2]) * v[2])
            / 4096.0
            + f64::from(p.t[r])
    })
}

fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// GTE MVMVA, sf = 1: (M v) >> 12 per row, arithmetic shift.
fn mv(m: &M, v: [i32; 3]) -> [i32; 3] {
    [0, 1, 2].map(|r| {
        (i32::from(m[r][0]) * v[0] + i32::from(m[r][1]) * v[1] + i32::from(m[r][2]) * v[2]) >> 12
    })
}

/// 3x3 product on the GTE: three MVMVA with the columns of `b`.
fn mm(a: &M, b: &M) -> M {
    let mut out = [[0i16; 3]; 3];
    for c in 0..3 {
        let col = mv(a, [b[0][c], b[1][c], b[2][c]].map(i32::from));
        for r in 0..3 {
            out[r][c] = col[r].clamp(-32768, 32767) as i16;
        }
    }
    out
}

fn transpose_mul_f(a: &M, b: &M) -> M {
    // round(a^T b) in Q12, f64 on the host.
    let mut out = [[0i16; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            let s: f64 = (0..3)
                .map(|k| f64::from(a[k][r]) * f64::from(b[k][c]))
                .sum::<f64>()
                / 4096.0;
            out[r][c] = s.round().clamp(-32768.0, 32767.0) as i16;
        }
    }
    out
}

struct Skel {
    parent: Vec<Option<usize>>,
    order: Vec<usize>,
    depth: Vec<usize>,
    names: Vec<String>,
}

fn skeleton(model: &Model<'_>, names: Vec<String>) -> Skel {
    let n = model.joint_count() as usize;
    let parent: Vec<Option<usize>> = (0..n)
        .map(|j| model.joint(j as u16).unwrap().parent().map(usize::from))
        .collect();
    let mut depth = vec![0usize; n];
    for (j, slot) in depth.iter_mut().enumerate() {
        let mut d = 0;
        let mut k = j;
        while let Some(p) = parent[k] {
            d += 1;
            k = p;
        }
        *slot = d;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&j| depth[j]);
    let names = (0..n)
        .map(|j| names.get(j).cloned().unwrap_or_else(|| format!("j{j}")))
        .collect();
    Skel {
        parent,
        order,
        depth,
        names,
    }
}

/// Least-squares pivot per joint over every frame of every clip:
/// (R_j - R_p) x = t_p - t_j. Returns pivots and the worst residual.
fn pivots(skel: &Skel, clips: &[Vec<Vec<Pose>>]) -> (Vec<[i32; 3]>, Vec<f64>) {
    let n = skel.parent.len();
    let mut piv = vec![[0i32; 3]; n];
    let mut resid = vec![0.0f64; n];
    for &j in &skel.order {
        let Some(p) = skel.parent[j] else { continue };
        let mut ata = [[0.0f64; 3]; 3];
        let mut atb = [0.0f64; 3];
        for clip in clips {
            for frame in clip {
                let (a, b) = (&frame[j], &frame[p]);
                let mut row = [[0.0; 3]; 3];
                let mut rhs = [0.0; 3];
                for r in 0..3 {
                    for (c, cell) in row[r].iter_mut().enumerate() {
                        *cell = (f64::from(a.m[r][c]) - f64::from(b.m[r][c])) / 4096.0;
                    }
                    rhs[r] = f64::from(b.t[r] - a.t[r]);
                }
                for r in 0..3 {
                    for c in 0..3 {
                        for k in 0..3 {
                            ata[c][k] += row[r][c] * row[r][k];
                        }
                        atb[c] += row[r][c] * rhs[r];
                    }
                }
            }
        }
        // Tikhonov towards the parent's pivot keeps a joint that never turns
        // against its parent well defined (any pivot works for it).
        let lam = 1e-6;
        let pp = piv[p].map(f64::from);
        for k in 0..3 {
            ata[k][k] += lam;
            atb[k] += lam * pp[k];
        }
        let x = solve3(ata, atb);
        piv[j] = x.map(|v| v.round() as i32);
        let mut worst: f64 = 0.0;
        for clip in clips {
            for frame in clip {
                let w_child = apply(&frame[j], piv[j].map(f64::from));
                let w_parent = apply(&frame[p], piv[j].map(f64::from));
                worst = worst.max(dist(w_child, w_parent));
            }
        }
        resid[j] = worst;
    }
    (piv, resid)
}

/// Model units a pivot may miss by before the joint keeps its own translation
/// track: integer translations and Q12 matrices at ~1500 unit reach leave
/// 1-3 units of gap on a rigid chain.
const GAP: f64 = 6.0;

fn residuals(skel: &Skel, clips: &[Vec<Vec<Pose>>], piv: &[[i32; 3]]) -> Vec<f64> {
    let n = skel.parent.len();
    let mut out = vec![0.0f64; n];
    for j in 0..n {
        let Some(p) = skel.parent[j] else { continue };
        for clip in clips {
            for frame in clip {
                let x = piv[j].map(f64::from);
                out[j] = out[j].max(dist(apply(&frame[j], x), apply(&frame[p], x)));
            }
        }
    }
    out
}

fn solve3(a: [[f64; 3]; 3], b: [f64; 3]) -> [f64; 3] {
    let det = |m: [[f64; 3]; 3]| {
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

/// Decoded poses of a clip at a phase, by scheme.
#[derive(Clone, Copy, PartialEq)]
enum Scheme {
    /// As shipped: every joint's matrix and translation from its tracks.
    Model,
    /// Model-space rotations, derived translation for non-kept joints.
    PivotA,
    /// Parent-relative rotations composed on the GTE, derived translation.
    RelativeB,
}

fn decode(
    anim: &Animation<'_>,
    phase: u32,
    scheme: Scheme,
    skel: &Skel,
    piv: &[[i32; 3]],
    keep_t: &[bool],
) -> Vec<Pose> {
    let s = anim.looped_pose_sample_q12(phase).unwrap();
    let n = skel.parent.len();
    let raw: Vec<Pose> = (0..n)
        .map(|j| from_jp(&s.pose(j as u16).unwrap()))
        .collect();
    derive(raw, scheme, skel, piv, keep_t)
}

/// Whole source frame `f` (no loop wrap at the last frame).
fn decode_frame(
    anim: &Animation<'_>,
    f: u16,
    scheme: Scheme,
    skel: &Skel,
    piv: &[[i32; 3]],
    keep_t: &[bool],
) -> Vec<Pose> {
    let n = skel.parent.len();
    let raw: Vec<Pose> = (0..n)
        .map(|j| from_jp(&anim.pose(f, j as u16).unwrap()))
        .collect();
    derive(raw, scheme, skel, piv, keep_t)
}

fn derive(
    raw: Vec<Pose>,
    scheme: Scheme,
    skel: &Skel,
    piv: &[[i32; 3]],
    keep_t: &[bool],
) -> Vec<Pose> {
    if scheme == Scheme::Model {
        return raw;
    }
    let mut out = raw.clone();
    for &j in &skel.order {
        let Some(p) = skel.parent[j] else { continue };
        if scheme == Scheme::RelativeB {
            out[j].m = mm(&out[p].m, &raw[j].m);
        }
        if !keep_t[j] {
            // Two GTE MVMVA: w = t_p + R_p p (cv = TR), b = R_j p (cv = none), t = w - b.
            // (Folding -p into the second MVMVA instead biases every level by
            // -1 unit per axis, floor twice the same way: 14-22 units at the hands.)
            let a = mv(&out[p].m, piv[j]);
            let b = mv(&out[j].m, piv[j]);
            // The joint's own track carries the residual (zero, one key, in plain A/B).
            out[j].t = [0, 1, 2].map(|r| out[p].t[r] + a[r] - b[r] + raw[j].t[r]);
        }
    }
    out
}

#[derive(Default, Clone, Copy)]
struct Err {
    worst_vertex: f64,
    worst_joint: f64,
    sum_joint: f64,
    count: f64,
}

impl Err {
    fn add(&mut self, o: &Err) {
        self.worst_vertex = self.worst_vertex.max(o.worst_vertex);
        self.worst_joint = self.worst_joint.max(o.worst_joint);
        self.sum_joint += o.sum_joint;
        self.count += o.count;
    }
    fn mean(&self) -> f64 {
        if self.count == 0.0 {
            0.0
        } else {
            self.sum_joint / self.count
        }
    }
}

fn compare(
    a: &[Pose],
    b: &[Pose],
    probes: &[Vec<[f64; 3]>],
    piv_bind: &[[f64; 3]],
    leaf_worst: &mut [f64],
) -> Err {
    let mut e = Err::default();
    for j in 0..a.len() {
        for &p in &probes[j] {
            e.worst_vertex = e.worst_vertex.max(dist(apply(&a[j], p), apply(&b[j], p)));
        }
        let d = dist(apply(&a[j], piv_bind[j]), apply(&b[j], piv_bind[j]));
        e.worst_joint = e.worst_joint.max(d);
        e.sum_joint += d;
        e.count += 1.0;
        leaf_worst[j] = leaf_worst[j].max(d);
    }
    e
}

struct Encoded {
    blob: Vec<u8>,
}

fn clip_input(rate: u16, frames: &[Vec<Pose>], probes: &[Vec<[f64; 3]>]) -> ClipInput {
    let n = frames[0].len();
    ClipInput {
        sample_rate_hz: rate,
        joints: (0..n)
            .map(|j| JointInput {
                rotations: frames.iter().map(|f| tr(&f[j].m)).collect(),
                translations: frames.iter().map(|f| f[j].t).collect(),
                probes: probes[j].clone(),
                reach: 0.0,
            })
            .collect(),
        parents: Vec::new(),
    }
}

fn enc(input: &ClipInput, budget: f64) -> Option<Encoded> {
    let opts = Options {
        budget_units: budget,
        ..Options::default()
    };
    match encode(input, &opts) {
        Ok((blob, _)) => Some(Encoded { blob }),
        Err(e) => {
            eprintln!("reject: {e:?}");
            None
        }
    }
}

/// Variant A input: non-kept translations constant zero (one key, no bits).
fn input_a(rate: u16, truth: &[Vec<Pose>], probes: &[Vec<[f64; 3]>], keep_t: &[bool]) -> ClipInput {
    let frames: Vec<Vec<Pose>> = truth
        .iter()
        .map(|f| {
            f.iter()
                .enumerate()
                .map(|(j, p)| Pose {
                    m: p.m,
                    t: if keep_t[j] { p.t } else { [0; 3] },
                })
                .collect()
        })
        .collect();
    clip_input(rate, &frames, probes)
}

/// Variant B, error-compensated: encode depth by depth, each joint against
/// its parent's decoded, already composed matrix at every source frame.
fn encode_b(
    rate: u16,
    truth: &[Vec<Pose>],
    probes: &[Vec<[f64; 3]>],
    keep_t: &[bool],
    skel: &Skel,
    piv: &[[i32; 3]],
    budget: f64,
) -> Option<(Encoded, Vec<Vec<Pose>>)> {
    let n = skel.parent.len();
    let max_depth = *skel.depth.iter().max().unwrap();
    let mut frames: Vec<Vec<Pose>> = truth
        .iter()
        .map(|f| {
            f.iter()
                .enumerate()
                .map(|(j, p)| Pose {
                    m: p.m,
                    t: if keep_t[j] { p.t } else { [0; 3] },
                })
                .collect()
        })
        .collect();
    let mut last = None;
    for d in 0..=max_depth {
        if d > 0 {
            // Decode composed matrices of depth d - 1 from the previous pass.
            let prev: &Encoded = last.as_ref()?;
            let anim = Animation::from_bytes(&prev.blob).ok()?;
            for (f, frame) in frames.iter_mut().enumerate() {
                let dec = decode_frame(&anim, f as u16, Scheme::RelativeB, skel, piv, keep_t);
                for j in 0..n {
                    if skel.depth[j] == d {
                        let p = skel.parent[j].unwrap();
                        frame[j].m = transpose_mul_f(&dec[p].m, &truth[f][j].m);
                    }
                }
            }
        }
        last = Some(enc(&clip_input(rate, &frames, probes), budget)?);
    }
    last.map(|e| (e, frames))
}

/// Error-compensated residual translation: each non-root joint's track holds
/// truth - derived, where derived uses the parent's DECODED pose and the
/// joint's decoded rotation; encoded depth by depth so no error accumulates.
#[allow(clippy::too_many_arguments)]
fn add_residuals(
    rate: u16,
    mut frames: Vec<Vec<Pose>>,
    truth: &[Vec<Pose>],
    probes: &[Vec<[f64; 3]>],
    keep_t: &[bool],
    skel: &Skel,
    piv: &[[i32; 3]],
    scheme: Scheme,
    budget: f64,
    first: Encoded,
) -> Option<Encoded> {
    let n = skel.parent.len();
    let max_depth = *skel.depth.iter().max().unwrap();
    let mut last = first;
    for d in 1..=max_depth {
        let anim = Animation::from_bytes(&last.blob).ok()?;
        for (f, frame) in frames.iter_mut().enumerate() {
            let dec = decode_frame(&anim, f as u16, scheme, skel, piv, keep_t);
            for j in 0..n {
                if skel.depth[j] != d || keep_t[j] {
                    continue;
                }
                let p = skel.parent[j].unwrap();
                let a = mv(&dec[p].m, piv[j]);
                let b = mv(&dec[j].m, piv[j]);
                frame[j].t = [0, 1, 2].map(|r| truth[f][j].t[r] - (dec[p].t[r] + a[r] - b[r]));
            }
        }
        if std::env::var("PR_DBG").is_ok() {
            let mut mx = 0;
            for fr in &frames {
                for j in 0..n {
                    if skel.depth[j] == d && !keep_t[j] {
                        for r in 0..3 {
                            mx = mx.max(fr[j].t[r].abs());
                        }
                    }
                }
            }
            eprintln!("depth {d} max residual {mx}");
        }
        // Rotation keeps its share (budget * 0.8); the residual track gets its own tolerance.
        let res: f64 = std::env::var("PR_RES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(budget * 0.2);
        let total = budget * 0.8 + res;
        let opts = Options {
            budget_units: total,
            rotation_share: budget * 0.8 / total,
            ..Options::default()
        };
        last = match encode(&clip_input(rate, &frames, probes), &opts) {
            Ok((blob, _)) => Encoded { blob },
            Err(e) => {
                eprintln!("reject: {e:?}");
                return None;
            }
        };
    }
    Some(last)
}

/// Flat-shaded orthographic front views (x right, y up), shipped | prototype.
fn render_pair(mesh: &Model<'_>, a: &[Pose], b: &[Pose], path: &str) {
    let (w, h) = (320usize, 400usize);
    let pose_verts = |poses: &[Pose]| -> Vec<[f64; 3]> {
        let mut out = vec![[0.0; 3]; mesh.vertex_count() as usize];
        for pi in 0..mesh.part_count() {
            let part = mesh.part(pi).unwrap();
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let vx = mesh.vertex(v).unwrap();
                let p = [
                    f64::from(vx.position.x),
                    f64::from(vx.position.y),
                    f64::from(vx.position.z),
                ];
                let mut q = apply(&poses[part.joint_index() as usize], p);
                if vx.is_blend() {
                    let r = apply(&poses[vx.joint1 as usize], p);
                    let t = f64::from(vx.blend) / 256.0;
                    q = [0, 1, 2].map(|k| q[k] * (1.0 - t) + r[k] * t);
                }
                out[v as usize] = q;
            }
        }
        out
    };
    let va = pose_verts(a);
    let vb = pose_verts(b);
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for v in va.iter().chain(vb.iter()) {
        for k in 0..2 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let scale = ((w as f64 - 20.0) / (hi[0] - lo[0])).min((h as f64 - 20.0) / (hi[1] - lo[1]));
    let mut img = vec![40u8; 2 * w * h * 3];
    for (panel, verts) in [&va, &vb].into_iter().enumerate() {
        let mut zb = vec![f64::MAX; w * h];
        let sx = |v: &[f64; 3]| -> [f64; 3] {
            [
                10.0 + (v[0] - lo[0]) * scale,
                h as f64 - 10.0 - (v[1] - lo[1]) * scale,
                -v[2],
            ]
        };
        for fi in 0..mesh.face_count() {
            let f = mesh.face(fi).unwrap();
            let p: Vec<[f64; 3]> = f
                .corners
                .iter()
                .map(|c| sx(&verts[c.vertex_index as usize]))
                .collect();
            let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
            let nz = e1[0] * e2[1] - e1[1] * e2[0];
            let nlen = ((e1[1] * e2[2] - e1[2] * e2[1]).powi(2)
                + (e1[2] * e2[0] - e1[0] * e2[2]).powi(2)
                + nz.powi(2))
            .sqrt()
            .max(1e-9);
            let shade = (60.0 + 190.0 * (nz / nlen).abs()) as u8;
            let x0 = p.iter().map(|q| q[0]).fold(f64::MAX, f64::min).max(0.0) as usize;
            let x1 = (p.iter().map(|q| q[0]).fold(f64::MIN, f64::max) as usize).min(w - 1);
            let y0 = p.iter().map(|q| q[1]).fold(f64::MAX, f64::min).max(0.0) as usize;
            let y1 = (p.iter().map(|q| q[1]).fold(f64::MIN, f64::max) as usize).min(h - 1);
            let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1])
                - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let w0 =
                        ((p[1][0] - px) * (p[2][1] - py) - (p[2][0] - px) * (p[1][1] - py)) / area;
                    let w1 =
                        ((p[2][0] - px) * (p[0][1] - py) - (p[0][0] - px) * (p[2][1] - py)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    let z = w0 * p[0][2] + w1 * p[1][2] + w2 * p[2][2];
                    if z < zb[y * w + x] {
                        zb[y * w + x] = z;
                        let o = (y * 2 * w + panel * w + x) * 3;
                        img[o] = shade;
                        img[o + 1] = shade;
                        img[o + 2] = if panel == 0 { shade / 2 + 100 } else { shade };
                    }
                }
            }
        }
    }
    let mut out = format!("P6\n{} {}\n255\n", 2 * w, h).into_bytes();
    out.extend_from_slice(&img);
    std::fs::write(path, out).unwrap();
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    out.sort();
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let project_path = PathBuf::from(&args[0]);
    let wanted: Vec<String> = args[1..].to_vec();
    let text = std::fs::read_to_string(&project_path).expect("read project");
    let root = project_path.parent().unwrap().to_path_buf();
    let mut project = ProjectDocument::from_ron_str(&text).expect("parse project");
    let budget = f64::from(project.animation_track_budget_units);
    let (shipped, report) = build_package(&project, &root);
    let shipped = shipped.unwrap_or_else(|| panic!("cook: {:?}", report.errors));
    project.animation_track_budget_units = 0;
    let (source, report0) = build_package(&project, &root);
    let source = source.unwrap_or_else(|| panic!("cook budget 0: {:?}", report0.errors));
    let _ = files;

    let mut totals: HashMap<&str, [usize; 8]> = HashMap::new();
    for (mi, model) in source.models.iter().enumerate() {
        if !wanted.iter().any(|w| model.name.contains(w.as_str())) {
            continue;
        }
        let mesh_bytes = &source.assets[model.mesh_asset_index].bytes;
        let mesh = Model::from_bytes(mesh_bytes).unwrap();
        let (sockets, names) = match project.resource(model.source_resource).map(|r| &r.data) {
            Some(ResourceData::Model(m)) => {
                let names = m
                    .skeleton
                    .and_then(|s| project.resource(s))
                    .and_then(|r| match &r.data {
                        ResourceData::Skeleton(s) => Some(s.joint_names.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                (
                    m.attachments.iter().map(|s| s.joint).collect::<Vec<u16>>(),
                    names,
                )
            }
            _ => (Vec::new(), Vec::new()),
        };
        let probes = joint_probes(&mesh, &sockets);
        let skel = skeleton(&mesh, names);
        let n = skel.parent.len();
        let shipped_model = &shipped.models[mi];
        assert_eq!(shipped_model.name, model.name);

        // Source poses of every rigid clip.
        struct Clip {
            name: String,
            truth: Vec<Vec<Pose>>,
            rate: u16,
            ship: Vec<u8>,
            rigid: bool,
        }
        let mut clips = Vec::new();
        for ci in 0..model.clip_count as usize {
            let c = &source.model_clips[model.clip_first as usize + ci];
            let bytes = &source.assets[c.animation_asset_index].bytes;
            let anim = Animation::from_bytes(bytes).unwrap();
            let truth: Vec<Vec<Pose>> = (0..anim.frame_count())
                .map(|f| {
                    (0..n)
                        .map(|j| from_jp(&anim.pose(f, j as u16).unwrap()))
                        .collect()
                })
                .collect();
            let rigid = truth
                .iter()
                .all(|f| f.iter().all(|p| psx_anim_tracks::is_rotation_like(&p.m)));
            let sc = &shipped.model_clips[shipped_model.clip_first as usize + ci];
            clips.push(Clip {
                name: c.name.clone(),
                truth,
                rate: anim.sample_rate_hz(),
                ship: shipped.assets[sc.animation_asset_index].bytes.clone(),
                rigid,
            });
        }
        let rigid_truth: Vec<Vec<Vec<Pose>>> = clips
            .iter()
            .filter(|c| c.rigid)
            .map(|c| c.truth.clone())
            .collect();
        if std::env::var("PR_DIAGT").is_ok() {
            let tr: Vec<Vec<Vec<Pose>>> = rigid_truth
                .iter()
                .map(|c| {
                    c.iter()
                        .map(|f| {
                            f.iter()
                                .map(|p| {
                                    let mut q = *p;
                                    for r in 0..3 {
                                        for k in 0..3 {
                                            q.m[r][k] = p.m[k][r];
                                        }
                                    }
                                    q
                                })
                                .collect()
                        })
                        .collect()
                })
                .collect();
            let (_, rs) = pivots(&skel, &tr);
            let line: Vec<String> = (1..n).map(|j| format!("{}:{:.0}", j, rs[j])).collect();
            println!("  diagT {}", line.join(" "));
        }
        if std::env::var("PR_DIAG").is_ok() {
            for c in clips.iter().filter(|c| c.rigid) {
                let (pv, rs) = pivots(&skel, std::slice::from_ref(&c.truth));
                let line: Vec<String> = (1..n).map(|j| format!("{}:{:.0}", j, rs[j])).collect();
                println!("  diag {:<40} {}", c.name, line.join(" "));
                let line: Vec<String> = (1..n).map(|j| format!("{:?}", pv[j])).collect();
                println!("  piv  {:<40} {}", c.name, line.join(" "));
            }
        }
        if std::env::var("PR_DIAG2").is_ok() {
            for c in clips.iter().filter(|c| c.rigid) {
                let mut line = Vec::new();
                for j in 1..n {
                    let p = skel.parent[j].unwrap();
                    let offs: Vec<[f64; 3]> = c
                        .truth
                        .iter()
                        .map(|f| {
                            let d = [0, 1, 2].map(|r| f64::from(f[j].t[r] - f[p].t[r]));
                            [0, 1, 2].map(|k| {
                                (0..3).map(|r| f64::from(f[p].m[r][k]) * d[r]).sum::<f64>() / 4096.0
                            })
                        })
                        .collect();
                    let mean = [0, 1, 2]
                        .map(|k| offs.iter().map(|o| o[k]).sum::<f64>() / offs.len() as f64);
                    let worst = offs.iter().map(|o| dist(*o, mean)).fold(0.0, f64::max);
                    line.push(format!("{}:{:.0}@{:.0}", j, worst, dist(mean, [0.0; 3])));
                }
                println!("  diag2 {:<40} {}", c.name, line.join(" "));
            }
        }
        let (mpiv, mresid) = pivots(&skel, &rigid_truth);
        println!("\n== {} ({n} joints)", model.name);
        for j in 0..n {
            println!(
                "  joint {j:2} {:<28} parent {:>3} depth {} pivot {:?} resid over all clips {:.2}",
                skel.names[j],
                skel.parent[j].map_or("-".into(), |p| p.to_string()),
                skel.depth[j],
                mpiv[j],
                mresid[j],
            );
        }
        println!(
            "  {:<34} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} | ship-vs-src  | A-vs-src     | A-vs-ship    | AR-vs-src | B-vs-src     | BR-vs-src",
            "clip", "ship", "reenc", "A", "AR", "B", "BR"
        );
        let mut leaf_a = vec![0.0f64; n];
        let mut leaf_b = vec![0.0f64; n];
        let mut leaf_ship = vec![0.0f64; n];
        let mut tot = [0usize; 8];
        let mut agg = [Err::default(); 6];
        for c in &clips {
            let ship_anim = Animation::from_bytes(&c.ship).unwrap();
            tot[0] += c.ship.len();
            if !c.rigid {
                println!(
                    "  {:<34} {:>7} (not rigid, stays a pose table)",
                    c.name,
                    c.ship.len()
                );
                for t in tot.iter_mut().skip(1) {
                    // unchanged in every variant
                    let _ = t;
                }
                tot[1] += c.ship.len();
                tot[2] += c.ship.len();
                tot[3] += c.ship.len();
                tot[4] += c.ship.len();
                tot[5] += c.ship.len();
                continue;
            }
            // Pivot per joint: the model's when it holds on this clip, else the
            // clip's own (a per-clip pivot table), else the joint keeps its track.
            let one = std::slice::from_ref(&c.truth);
            let (cpiv, cresid) = pivots(&skel, one);
            let mres = residuals(&skel, one, &mpiv);
            let mut piv = mpiv.clone();
            let mut keep_t = vec![false; n];
            let mut clip_table = false;
            for j in 0..n {
                if skel.parent[j].is_none() {
                    keep_t[j] = true;
                } else if mres[j] <= GAP {
                } else if cresid[j] <= GAP {
                    piv[j] = cpiv[j];
                    clip_table = true;
                } else {
                    keep_t[j] = true;
                }
            }
            let kept: Vec<String> = (1..n)
                .filter(|&j| keep_t[j])
                .map(|j| skel.names[j].clone())
                .collect();
            let piv_bind: Vec<[f64; 3]> = piv.iter().map(|p| p.map(f64::from)).collect();
            let table = if clip_table { n * 6 } else { 0 };
            // Shipped clip's own joint gap mid-frame: how far a child's pivot
            // drifts from where its parent carries it (bone stretch).
            let mut ship_gap: f64 = 0.0;
            let mut gap_phase = 0u32;
            {
                let mut ph = 0u32;
                let end = (c.truth.len() as u32 - 2) << 12;
                while ph < end {
                    let d = decode(&ship_anim, ph, Scheme::Model, &skel, &piv, &keep_t);
                    for j in 0..n {
                        if let (Some(p), false) = (skel.parent[j], keep_t[j]) {
                            let g = dist(apply(&d[j], piv_bind[j]), apply(&d[p], piv_bind[j]));
                            if g > ship_gap {
                                ship_gap = g;
                                gap_phase = ph;
                            }
                        }
                    }
                    ph += 256;
                }
            }
            let base_in = clip_input(c.rate, &c.truth, &probes);
            let reenc = enc(&base_in, budget).map(|e| e.blob);
            let reenc_ok = reenc.as_deref() == Some(&c.ship[..]) || !ship_anim.is_keyed_tracks();
            let ia = input_a(c.rate, &c.truth, &probes, &keep_t);
            let frames = c.truth.len();
            let errs = |blob: &[u8], scheme: Scheme, leaf: &mut [f64]| -> (Err, Err) {
                let anim = Animation::from_bytes(blob).unwrap();
                let mut vs_src = Err::default();
                let mut dummy = vec![0.0; n];
                for f in 0..frames {
                    let d = decode_frame(&anim, f as u16, scheme, &skel, &piv, &keep_t);
                    vs_src.add(&compare(&d, &c.truth[f], &probes, &piv_bind, leaf));
                }
                let mut vs_ship = Err::default();
                let tail = anim.is_keyed_tracks() != ship_anim.is_keyed_tracks();
                let end = if tail {
                    (frames as u32 - 2) << 12
                } else {
                    (frames as u32 - 1) << 12
                };
                let mut ph = 0u32;
                while ph < end {
                    let d = decode(&anim, ph, scheme, &skel, &piv, &keep_t);
                    let s = decode(&ship_anim, ph, Scheme::Model, &skel, &piv, &keep_t);
                    vs_ship.add(&compare(&d, &s, &probes, &piv_bind, &mut dummy));
                    ph += 256;
                }
                (vs_src, vs_ship)
            };
            let (ship_src, _) = errs(&c.ship, Scheme::Model, &mut leaf_ship);
            let a = enc(&ia, budget).unwrap();
            let (a_src, a_ship) = errs(&a.blob, Scheme::PivotA, &mut leaf_a);
            let a_frames = {
                let ia2 = &ia;
                (0..frames)
                    .map(|f| {
                        (0..n)
                            .map(|j| Pose {
                                m: tr(&ia2.joints[j].rotations[f]),
                                t: ia2.joints[j].translations[f],
                            })
                            .collect()
                    })
                    .collect::<Vec<Vec<Pose>>>()
            };
            let ar = add_residuals(
                c.rate,
                a_frames,
                &c.truth,
                &probes,
                &keep_t,
                &skel,
                &piv,
                Scheme::PivotA,
                budget,
                Encoded {
                    blob: a.blob.clone(),
                },
            )
            .unwrap();
            let mut dummy = vec![0.0; n];
            let (ar_src, ar_ship) = errs(&ar.blob, Scheme::PivotA, &mut dummy);
            if let Ok(dir) = std::env::var("PR_RENDER") {
                if ship_gap > 50.0 {
                    let ar_anim = Animation::from_bytes(&ar.blob).unwrap();
                    let ds = decode(&ship_anim, gap_phase, Scheme::Model, &skel, &piv, &keep_t);
                    let da = decode(&ar_anim, gap_phase, Scheme::PivotA, &skel, &piv, &keep_t);
                    let name: String = c
                        .name
                        .chars()
                        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                        .collect();
                    let path = format!("{dir}/{name}_q{}.ppm", gap_phase);
                    render_pair(&mesh, &ds, &da, &path);
                    let f0 = (gap_phase >> 12) as usize;
                    render_pair(
                        &mesh,
                        &c.truth[f0],
                        &c.truth[f0 + 1],
                        &format!("{dir}/{name}_q{}_src.ppm", gap_phase),
                    );
                    println!(
                        "      rendered {path} (frame {:.2}, ship gap {:.0})",
                        f64::from(gap_phase) / 4096.0,
                        ship_gap
                    );
                }
            }
            let a4 = (ar.blob.clone(), ar_src);
            let b4b = budget;
            let (b, b_frames) =
                encode_b(c.rate, &c.truth, &probes, &keep_t, &skel, &piv, budget).unwrap();
            let (b_src, _) = errs(&b.blob, Scheme::RelativeB, &mut leaf_b);
            let br = add_residuals(
                c.rate,
                b_frames,
                &c.truth,
                &probes,
                &keep_t,
                &skel,
                &piv,
                Scheme::RelativeB,
                budget,
                Encoded {
                    blob: b.blob.clone(),
                },
            )
            .unwrap();
            let mut dummy = vec![0.0; n];
            let (br_src, _) = errs(&br.blob, Scheme::RelativeB, &mut dummy);
            let bb = (br.blob.clone(), br_src);
            let bbud = ar_ship.worst_vertex;
            tot[1] += reenc
                .as_ref()
                .map_or(c.ship.len(), Vec::len)
                .min(c.ship.len());
            tot[2] += (a.blob.len() + table).min(c.ship.len());
            tot[3] += (a4.0.len() + table).min(c.ship.len());
            tot[4] += (b.blob.len() + table).min(c.ship.len());
            tot[5] += (bb.0.len() + table).min(c.ship.len());
            agg[0].add(&ship_src);
            agg[1].add(&a_src);
            agg[2].add(&a_ship);
            agg[3].add(&a4.1);
            agg[4].add(&b_src);
            agg[5].add(&bb.1);
            println!(
                "  {:<34} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} | {:5.2} {:5.2} {:4.2} | {:5.2} {:5.2} {:4.2} | {:5.2} {:5.2} {:4.2} | {:5.2} b{:.2} | {:5.2} {:5.2} {:4.2} | {:5.2} AR-vs-ship {:.2}{}",
                c.name,
                c.ship.len(),
                reenc.as_ref().map_or(0, Vec::len),
                a.blob.len(),
                a4.0.len(),
                b.blob.len(),
                bb.0.len(),
                ship_src.worst_vertex, ship_src.worst_joint, ship_src.mean(),
                a_src.worst_vertex, a_src.worst_joint, a_src.mean(),
                a_ship.worst_vertex, a_ship.worst_joint, a_ship.mean(),
                a4.1.worst_vertex, b4b,
                b_src.worst_vertex, b_src.worst_joint, b_src.mean(),
                bb.1.worst_vertex, bbud,
                if reenc_ok { "" } else { "  (re-encode differs from shipped)" }
            );
            println!(
                "      ship mid-frame joint gap {:.2}, pivot table {} B, kept translation: {}",
                ship_gap,
                table,
                kept.join(" ")
            );
        }
        println!(
            "  TOTAL ship {} reenc {} A {} AR {} B {} BR {} (+ model pivot table {} B)",
            tot[0],
            tot[1],
            tot[2],
            tot[3],
            tot[4],
            tot[5],
            n * 6
        );
        let names = [
            "ship-vs-src",
            "A-vs-src",
            "A-vs-ship",
            "AR-vs-src",
            "B-vs-src",
            "BR-vs-src",
        ];
        for (k, e) in agg.iter().enumerate() {
            println!(
                "  {:<12} worst vertex {:6.2} worst joint {:6.2} mean joint {:5.2}",
                names[k],
                e.worst_vertex,
                e.worst_joint,
                e.mean()
            );
        }
        println!("  chain ends (leaf joints), worst joint-position error vs source, ship / A / B:");
        for j in 0..n {
            if !skel.parent.contains(&Some(j)) || sockets.contains(&(j as u16)) {
                println!(
                    "    {j:2} {:<28} {:6.2} {:6.2} {:6.2}{}",
                    skel.names[j],
                    leaf_ship[j],
                    leaf_a[j],
                    leaf_b[j],
                    if sockets.contains(&(j as u16)) {
                        "  (socket)"
                    } else {
                        ""
                    }
                );
            }
        }
        totals.insert(Box::leak(model.name.clone().into_boxed_str()), tot);
    }
}
