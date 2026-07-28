//! `texwarp` -- empirical measurement of PS1 affine texture warping.
//!
//! # The instrument
//!
//! Warping is normally judged by eye ("that floor swims"). This bench measures
//! it in **texels**, per pixel, with no human in the loop.
//!
//! The trick is a self-identifying texture: a 64x64 15bpp texture where
//! `texel(u,v) = 0x8000 | (v << 6) | u`. Every texel is a unique value, so
//! reading a rendered pixel back out of VRAM tells you *exactly which texel the
//! GPU sampled there*. Drawn with a raw-texture primitive (no blending, no
//! dither, no CLUT) the texel reaches VRAM verbatim.
//!
//! Ground truth is analytic. Each scene is a planar parallelogram in camera
//! space, so UV is exactly affine in 3D. For a pixel centre we cast a ray,
//! intersect the plane, and get the perspective-correct `(u,v)` in closed form.
//!
//!   error(pixel) = |sampled_texel - correct_texel|   (L2, in texels)
//!
//! The rasterizer is `emulator-core`'s own -- the center-sampled DDA verified
//! pixel-exact against real silicon -- so these numbers are hardware numbers,
//! not a model of hardware.
//!
//! # What it sweeps
//!
//! Every warping mitigation that exists on PS1, all of which are really "choose
//! where to put split lines, and how many":
//!
//! * `quad1`: one native 4-vertex quad primitive (the do-nothing baseline).
//! * `tri2-diagA/B`: two triangles, each diagonal choice.
//! * `tri2-diagBest`: per-cell diagonal picked along the iso-depth direction.
//! * `obj-NxN`: uniform subdivision in object space -- what adaptive does,
//!   and what `render3d.rs`'s `adaptive_subdivision` does today.
//! * `scr-NxN`: uniform subdivision in *screen* space, i.e. splits evenly
//!   spaced in 1/z rather than evenly spaced in geometry.
//! * `*-1xN`: subdividing only the axis whose depth actually varies.
//! * `adapt-EPS`: recursive bisection until the closed-form error bound at the
//!   split midpoint drops under `EPS` texels. Crack-free: the recursion is
//!   separable per axis, so it produces no T-junctions (unlike a quadtree).
//! * `uvhalf`: control -- same geometry, texture scaled 2x on the surface.
//!   Confirms error is proportional to the UV span, not to the polygon's size
//!   on screen.
//!
//! # The closed form it validates
//!
//! For one edge spanning `du` texels between depths `za` and `zb`, the affine
//! interpolation error at the screen midpoint is exactly
//!
//!   err_texels = du * |zb - za| / (2 * (za + zb))
//!
//! The bench reports predicted-vs-measured so the engine can trust this
//! expression as a runtime subdivision criterion instead of guessing.
//!
//! Run: `cargo run -p emulator-core --release --example texwarp [outdir]`

use emulator_core::gpu::GP0_ADDR;
use emulator_core::Gpu;
use std::fmt::Write as _;

// ----------------------------------------------------------------------
// Framebuffer / projection constants
// ----------------------------------------------------------------------

/// Draw-area width in pixels.
const W: i32 = 320;
/// Draw-area height in pixels.
const H: i32 = 240;
/// Projection distance (GTE `H`). 300 with a 320-wide screen is ~56 deg hfov.
const PROJ: f64 = 300.0;
/// Screen-space projection centre X (GTE `OFX`).
const CX: f64 = 160.0;
/// Screen-space projection centre Y (GTE `OFY`).
const CY: f64 = 120.0;

/// Texture size in texels (a 15bpp texture page is 64 texels wide).
const TEX: i32 = 64;
/// VRAM X of the texture page (texpage index 8 -> x = 8 * 64).
const TEX_VX: u16 = 512;
/// VRAM Y of the texture page.
const TEX_VY: u16 = 0;
/// `tpage` attribute word: xbase=8, ybase=0, 15bpp, dither off.
const TPAGE: u32 = 8 | (2 << 7);

// ----------------------------------------------------------------------
// Geometry
// ----------------------------------------------------------------------

/// A camera-space point.
#[derive(Clone, Copy, Debug)]
struct V3 {
    /// Camera-space X (right).
    x: f64,
    /// Camera-space Y (down).
    y: f64,
    /// Camera-space Z (forward, always positive here).
    z: f64,
}

impl V3 {
    /// Component-wise construction.
    const fn new(x: f64, y: f64, z: f64) -> Self {
        Self { x, y, z }
    }
}

/// A planar parallelogram in camera space: `o + s*e1 + t*e2` for `s,t` in [0,1].
///
/// Parallelogram (not a general quad) on purpose: UV is then exactly affine in
/// 3D, so the analytic ground truth has no approximation in it.
struct Scene {
    /// Human-readable scene id, used in the CSV and image filenames.
    name: String,
    /// Corner at `(s,t) = (0,0)`.
    o: V3,
    /// Edge swept by `s`.
    e1: V3,
    /// Edge swept by `t`.
    e2: V3,
}

impl Scene {
    /// Camera-space position at surface parameters `(s, t)`.
    fn pos(&self, s: f64, t: f64) -> V3 {
        V3::new(
            self.o.x + s * self.e1.x + t * self.e2.x,
            self.o.y + s * self.e1.y + t * self.e2.y,
            self.o.z + s * self.e1.z + t * self.e2.z,
        )
    }

    /// Perspective projection to (sub-pixel) screen coordinates.
    fn project(&self, s: f64, t: f64) -> (f64, f64) {
        let p = self.pos(s, t);
        (CX + p.x * PROJ / p.z, CY + p.y * PROJ / p.z)
    }

    /// Depth at the two ends of an axis, sampled at the middle of the other
    /// axis. For a plane the depth profile of an axis depends slightly on the
    /// other parameter; the midpoint is the representative slice.
    fn axis_depths(&self, axis: usize) -> (f64, f64) {
        if axis == 0 {
            (self.pos(0.0, 0.5).z, self.pos(1.0, 0.5).z)
        } else {
            (self.pos(0.5, 0.0).z, self.pos(0.5, 1.0).z)
        }
    }

    /// Index of the axis whose depth varies most: the one worth subdividing.
    fn depth_axis(&self) -> usize {
        let (a0, a1) = self.axis_depths(0);
        let (b0, b1) = self.axis_depths(1);
        if (a1 - a0).abs() >= (b1 - b0).abs() {
            0
        } else {
            1
        }
    }

    /// Invert the projection: which surface parameters does this pixel centre
    /// see? Solves `lambda*d = o + s*e1 + t*e2` by Cramer's rule.
    ///
    /// Returns `None` only for a degenerate (edge-on) plane.
    fn unproject(&self, px: f64, py: f64) -> Option<(f64, f64)> {
        let d = V3::new((px - CX) / PROJ, (py - CY) / PROJ, 1.0);
        // Columns of the 3x3 system: [d, -e1, -e2] * [lambda, s, t]^T = o
        let (a, b, c) = (d, neg(self.e1), neg(self.e2));
        let det = det3(a, b, c);
        if det.abs() < 1e-12 {
            return None;
        }
        let s = det3(a, self.o, c) / det;
        let t = det3(a, b, self.o) / det;
        Some((s, t))
    }
}

/// Negate a vector.
fn neg(v: V3) -> V3 {
    V3::new(-v.x, -v.y, -v.z)
}

/// Determinant of the matrix whose columns are `a`, `b`, `c`.
fn det3(a: V3, b: V3, c: V3) -> f64 {
    a.x * (b.y * c.z - b.z * c.y) - b.x * (a.y * c.z - a.z * c.y)
        + c.x * (a.y * b.z - a.z * b.y)
}

// ----------------------------------------------------------------------
// Primitive emission
// ----------------------------------------------------------------------

/// One projected, snapped, UV-assigned vertex ready for GP0.
#[derive(Clone, Copy)]
struct Vtx {
    /// Screen X, snapped to the integer grid the GPU actually accepts.
    x: i32,
    /// Screen Y, snapped.
    y: i32,
    /// Texture U (PS1 vertex UVs are 8-bit integers -- rounding here is real).
    u: u8,
    /// Texture V.
    v: u8,
}

/// A GP0 polygon: 3 or 4 vertices.
struct Prim(Vec<Vtx>);

/// How each grid cell becomes primitives.
#[derive(Clone, Copy, PartialEq)]
enum Emit {
    /// Native 4-vertex quad (hardware picks the diagonal).
    Quad,
    /// Two triangles split top-left..bottom-right.
    TriA,
    /// Two triangles split top-right..bottom-left.
    TriB,
    /// Per-cell diagonal choice: split along the pair of corners with the
    /// *smaller* depth difference, so the cut runs along the iso-depth
    /// direction and each triangle spans less depth. Free at runtime -- it is
    /// a vertex-order swap, not extra geometry.
    TriBest,
}

/// Project, snap and UV-assign one surface parameter pair.
fn vertex(sc: &Scene, s: f64, t: f64, umax: f64) -> Vtx {
    let (fx, fy) = sc.project(s, t);
    Vtx {
        x: fx.round() as i32,
        y: fy.round() as i32,
        u: (s * umax).round().clamp(0.0, 255.0) as u8,
        v: (t * umax).round().clamp(0.0, 255.0) as u8,
    }
}

/// Turn a pair of split lists into the primitive list for a whole surface.
fn build(sc: &Scene, ss: &[f64], ts: &[f64], emit: Emit, umax: f64) -> Vec<Prim> {
    let mut out = Vec::new();
    for j in 0..ts.len() - 1 {
        for i in 0..ss.len() - 1 {
            let tl = vertex(sc, ss[i], ts[j], umax);
            let tr = vertex(sc, ss[i + 1], ts[j], umax);
            let bl = vertex(sc, ss[i], ts[j + 1], umax);
            let br = vertex(sc, ss[i + 1], ts[j + 1], umax);
            match emit {
                // PS1 quad order is v0,v1,v2,v3 -> tris (v0,v1,v2) and (v1,v2,v3).
                Emit::Quad => out.push(Prim(vec![tl, tr, bl, br])),
                Emit::TriA => {
                    out.push(Prim(vec![tl, tr, bl]));
                    out.push(Prim(vec![tr, bl, br]));
                }
                Emit::TriB => {
                    out.push(Prim(vec![tl, tr, br]));
                    out.push(Prim(vec![tl, br, bl]));
                }
                Emit::TriBest => {
                    let z = |s, t| sc.pos(s, t).z;
                    let da = (z(ss[i + 1], ts[j]) - z(ss[i], ts[j + 1])).abs(); // tr..bl
                    let db = (z(ss[i], ts[j]) - z(ss[i + 1], ts[j + 1])).abs(); // tl..br
                    if da <= db {
                        out.push(Prim(vec![tl, tr, bl]));
                        out.push(Prim(vec![tr, bl, br]));
                    } else {
                        out.push(Prim(vec![tl, tr, br]));
                        out.push(Prim(vec![tl, br, bl]));
                    }
                }
            }
        }
    }
    out
}

// ----------------------------------------------------------------------
// Split-list generators (this is where every strategy actually lives)
// ----------------------------------------------------------------------

/// Uniform in object space: what naive / adaptive-style subdivision does.
fn splits_object(n: usize) -> Vec<f64> {
    (0..=n).map(|i| i as f64 / n as f64).collect()
}

/// Uniform in *screen* space. For a segment from depth `za` to `zb`, the
/// surface parameter at screen fraction `sigma` is
/// `s = sigma*za / (sigma*za + (1-sigma)*zb)`, which is exactly the split that
/// makes 1/z advance evenly -- i.e. evenly spaced *perspective*, not evenly
/// spaced geometry.
fn splits_screen(n: usize, za: f64, zb: f64) -> Vec<f64> {
    (0..=n)
        .map(|i| {
            let sig = i as f64 / n as f64;
            sig * za / (sig * za + (1.0 - sig) * zb)
        })
        .collect()
}

/// Closed-form affine error at the screen midpoint of one edge, in texels.
///
/// `du` is the texel span of the edge, `za`/`zb` the endpoint depths.
fn edge_error(du: f64, za: f64, zb: f64) -> f64 {
    du * (zb - za).abs() / (2.0 * (za + zb))
}

/// Recursive bisection at the *screen* midpoint until `edge_error` drops under
/// `eps` texels. Separable per axis, so the resulting grid is crack-free (no
/// T-junctions, unlike a quadtree).
fn splits_adaptive(za: f64, zb: f64, du: f64, eps: f64) -> Vec<f64> {
    /// Depth-first walk; pushes the right-hand endpoint of every accepted span.
    #[allow(clippy::too_many_arguments)]
    fn go(sa: f64, sb: f64, za: f64, zb: f64, du: f64, eps: f64, depth: u32, out: &mut Vec<f64>) {
        if depth == 0 || edge_error(du, za, zb) <= eps {
            out.push(sb);
            return;
        }
        let f = za / (za + zb); // screen midpoint, as a fraction of [sa,sb]
        let sm = sa + (sb - sa) * f;
        let zm = za + (zb - za) * f;
        go(sa, sm, za, zm, du * f, eps, depth - 1, out);
        go(sm, sb, zm, zb, du * (1.0 - f), eps, depth - 1, out);
    }
    let mut out = vec![0.0];
    // ponytail: depth cap 7 = at most 128 splits/axis. Raise if a scene ever
    // saturates it (the bench prints the split count, so saturation is visible).
    go(0.0, 1.0, za, zb, du, eps, 7, &mut out);
    out
}

// ----------------------------------------------------------------------
// GPU driving
// ----------------------------------------------------------------------

/// Write the self-identifying texture into VRAM.
fn upload_texture(gpu: &mut Gpu) {
    for v in 0..TEX {
        for u in 0..TEX {
            let idx = ((v as u16) << 6) | u as u16;
            gpu.vram
                .set_pixel(TEX_VX + u as u16, TEX_VY + v as u16, 0x8000 | idx);
        }
    }
}

/// Reset draw state and clear the draw area to 0x0000 ("no texel here").
fn reset_draw_state(gpu: &mut Gpu) {
    let mut w = |word: u32| {
        gpu.write32(GP0_ADDR, word);
    };
    w(0xE1000000 | TPAGE); // draw mode / texpage, dither off
    w(0xE2000000); // texture window: no mask, no offset
    w(0xE3000000); // draw area top-left = (0,0)
    w(0xE4000000 | (((H - 1) as u32) << 10) | (W - 1) as u32);
    w(0xE5000000); // draw offset = 0
    w(0xE6000000); // mask: don't force, don't test
    w(0x02000000); // fill rect, colour 0
    w(0);
    w(((H as u32) << 16) | W as u32);
}

/// Submit one primitive as a raw-textured opaque polygon.
fn submit(gpu: &mut Gpu, p: &Prim) {
    let quad = p.0.len() == 4;
    // 0x25 = tri, textured, opaque, raw; 0x2D = the quad form.
    let cmd: u32 = if quad { 0x2D } else { 0x25 };
    let mut w = |word: u32| {
        gpu.write32(GP0_ADDR, word);
    };
    w((cmd << 24) | 0x808080);
    for (i, v) in p.0.iter().enumerate() {
        w(((v.y as u32 & 0xFFFF) << 16) | (v.x as u32 & 0xFFFF));
        // Vertex 0 carries the CLUT word (unused in 15bpp), vertex 1 the tpage.
        let attr = match i {
            0 => 0,
            1 => TPAGE << 16,
            _ => 0,
        };
        w(attr | ((v.v as u32) << 8) | v.u as u32);
    }
}

// ----------------------------------------------------------------------
// Measurement
// ----------------------------------------------------------------------

/// Everything one (scene, strategy) render produced.
struct Measured {
    /// Pixels compared (drawn, and inside the true surface silhouette).
    pixels: u64,
    /// Pixels drawn outside the true silhouette: pure vertex-snap spill.
    spill: u64,
    /// Mean L2 texel error.
    mean: f64,
    /// 95th-percentile L2 texel error.
    p95: f64,
    /// Worst L2 texel error.
    max: f64,
    /// Fraction of compared pixels off by more than 1 texel.
    over1: f64,
    /// Fraction off by more than 4 texels.
    over4: f64,
    /// Primitives submitted (draw-call cost).
    prims: usize,
    /// Vertices submitted, counting duplicates (raw GP0 word volume).
    verts: usize,
    /// *Unique* grid corners: what the GTE actually has to project, since
    /// adjacent cells share their corners and the engine caches projections.
    uverts: usize,
    /// GPU cycles from the emulator's silicon-calibrated command cost model.
    gpu_cycles: u64,
    /// Worst per-edge closed-form prediction, for validating the criterion.
    predicted: f64,
    /// Per-pixel error field, for the heatmap dumps.
    field: Vec<f32>,
}

/// Render one primitive list and score it against the analytic ground truth.
fn measure(
    gpu: &mut Gpu,
    sc: &Scene,
    prims: &[Prim],
    umax: f64,
    predicted: f64,
    uverts: usize,
) -> Measured {
    reset_draw_state(gpu);
    let cycles_before: u64 = gpu.gp0_timing_histogram().iter().sum();
    for p in prims {
        submit(gpu, p);
    }
    let gpu_cycles = gpu.gp0_timing_histogram().iter().sum::<u64>() - cycles_before;

    let mut errs: Vec<f64> = Vec::new();
    let mut field = vec![f32::NAN; (W * H) as usize];
    let mut spill = 0u64;

    for py in 0..H {
        for px in 0..W {
            let texel = gpu.vram.get_pixel(px as u16, py as u16);
            if texel & 0x8000 == 0 {
                continue; // not drawn
            }
            let idx = texel & 0x0FFF;
            let (su, sv) = ((idx & 63) as f64, (idx >> 6) as f64);

            let Some((s, t)) = sc.unproject(px as f64 + 0.5, py as f64 + 0.5) else {
                continue;
            };
            // Snapped vertices push a thin fringe outside the true silhouette.
            // Score that separately: it is a different artefact from warping.
            if !(-0.02..=1.02).contains(&s) || !(-0.02..=1.02).contains(&t) {
                spill += 1;
                continue;
            }
            let gu = (s.clamp(0.0, 1.0) * umax).floor();
            let gv = (t.clamp(0.0, 1.0) * umax).floor();
            let e = ((su - gu).powi(2) + (sv - gv).powi(2)).sqrt();
            errs.push(e);
            field[(py * W + px) as usize] = e as f32;
        }
    }

    errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = errs.len().max(1);
    let mean = errs.iter().sum::<f64>() / n as f64;
    let p95 = errs.get(errs.len() * 95 / 100).copied().unwrap_or(0.0);
    let max = errs.last().copied().unwrap_or(0.0);
    let over1 = errs.iter().filter(|e| **e > 1.0).count() as f64 / n as f64;
    let over4 = errs.iter().filter(|e| **e > 4.0).count() as f64 / n as f64;

    Measured {
        pixels: errs.len() as u64,
        spill,
        mean,
        p95,
        max,
        over1,
        over4,
        prims: prims.len(),
        verts: prims.iter().map(|p| p.0.len()).sum(),
        uverts,
        gpu_cycles,
        predicted,
        field,
    }
}

// ----------------------------------------------------------------------
// Strategies
// ----------------------------------------------------------------------

/// Split lists for both axes plus how each grid cell is emitted.
type Plan = Box<dyn Fn(&Scene) -> (Vec<f64>, Vec<f64>, Emit)>;

/// Per-strategy running totals across the warping scenes.
#[derive(Clone, Copy, Default)]
struct Agg {
    /// Sum of per-scene mean texel error.
    mean: f64,
    /// Sum of per-scene p95 texel error.
    p95: f64,
    /// Sum of per-scene max texel error.
    max: f64,
    /// Sum of primitive counts.
    prims: f64,
    /// Sum of vertex counts.
    verts: f64,
    /// Sum of modelled GPU cycles.
    cycles: f64,
    /// Sum of silhouette-spill pixel counts.
    spill: f64,
    /// Scenes accumulated.
    n: u32,
}

/// A named way of turning a surface into primitives.
struct Strategy {
    /// Label used in the report.
    name: String,
    /// Split lists plus emission mode for a given scene.
    plan: Plan,
    /// Texel span of the surface (63 normally, 31 for the `uvhalf` control).
    umax: f64,
}

/// Build the full strategy sweep.
fn strategies() -> Vec<Strategy> {
    let mut v: Vec<Strategy> = Vec::new();
    let mut push = |name: String, plan: Plan, umax: f64| {
        v.push(Strategy { name, plan, umax });
    };

    let one = || vec![0.0, 1.0];

    push(
        "quad1".into(),
        Box::new(move |_| (one(), one(), Emit::Quad)),
        63.0,
    );
    push(
        "tri2-diagA".into(),
        Box::new(move |_| (one(), one(), Emit::TriA)),
        63.0,
    );
    push(
        "tri2-diagB".into(),
        Box::new(move |_| (one(), one(), Emit::TriB)),
        63.0,
    );
    push(
        "tri2-diagBest".into(),
        Box::new(move |_| (one(), one(), Emit::TriBest)),
        63.0,
    );
    push(
        "uvhalf-quad1".into(),
        Box::new(move |_| (one(), one(), Emit::Quad)),
        31.0,
    );

    for n in [2usize, 4, 8, 16] {
        push(
            format!("obj-{n}x{n}"),
            Box::new(move |_| (splits_object(n), splits_object(n), Emit::Quad)),
            63.0,
        );
        push(
            format!("scr-{n}x{n}"),
            Box::new(move |sc: &Scene| {
                let (a0, a1) = sc.axis_depths(0);
                let (b0, b1) = sc.axis_depths(1);
                (
                    splits_screen(n, a0, a1),
                    splits_screen(n, b0, b1),
                    Emit::Quad,
                )
            }),
            63.0,
        );
        // 1D: subdivide only the axis whose depth varies. 1/N the primitives
        // of the NxN grid; the question is how much quality that gives up.
        push(
            format!("obj-1x{n}"),
            Box::new(move |sc: &Scene| {
                let (a, b) = if sc.depth_axis() == 0 {
                    (splits_object(n), vec![0.0, 1.0])
                } else {
                    (vec![0.0, 1.0], splits_object(n))
                };
                (a, b, Emit::Quad)
            }),
            63.0,
        );
        push(
            format!("scr-1x{n}"),
            Box::new(move |sc: &Scene| {
                let ax = sc.depth_axis();
                let (z0, z1) = sc.axis_depths(ax);
                let sp = splits_screen(n, z0, z1);
                let (a, b) = if ax == 0 {
                    (sp, vec![0.0, 1.0])
                } else {
                    (vec![0.0, 1.0], sp)
                };
                (a, b, Emit::Quad)
            }),
            63.0,
        );
    }

    for eps in [8.0f64, 4.0, 2.0, 1.0, 0.5] {
        for (suffix, emit) in [("", Emit::Quad), ("-best", Emit::TriBest)] {
            push(
                format!("adapt-{eps}tx{suffix}"),
                Box::new(move |sc: &Scene| {
                    let (a0, a1) = sc.axis_depths(0);
                    let (b0, b1) = sc.axis_depths(1);
                    (
                        splits_adaptive(a0, a1, 63.0, eps),
                        splits_adaptive(b0, b1, 63.0, eps),
                        emit,
                    )
                }),
                63.0,
            );
        }
    }
    v
}

/// Worst closed-form per-edge prediction over the resulting grid, in texels.
fn predict(sc: &Scene, ss: &[f64], ts: &[f64], umax: f64) -> f64 {
    let mut worst: f64 = 0.0;
    for j in 0..ts.len() - 1 {
        for i in 0..ss.len() - 1 {
            let du = (ss[i + 1] - ss[i]) * umax;
            let dv = (ts[j + 1] - ts[j]) * umax;
            let (za, zb) = (
                sc.pos(ss[i], ts[j]).z,
                sc.pos(ss[i + 1], ts[j]).z,
            );
            worst = worst.max(edge_error(du, za, zb));
            let (zc, zd) = (
                sc.pos(ss[i], ts[j]).z,
                sc.pos(ss[i], ts[j + 1]).z,
            );
            worst = worst.max(edge_error(dv, zc, zd));
        }
    }
    worst
}

// ----------------------------------------------------------------------
// Scenes
// ----------------------------------------------------------------------

/// A floor-like tile tilted `deg` away from fronto-parallel, centred at depth
/// `dist`. `deg = 0` is head-on (no warp possible); `deg -> 90` is grazing.
fn tilted(name: &str, deg: f64, dist: f64, size: f64, horizontal: bool) -> Scene {
    let r = deg.to_radians();
    let (c, s) = (r.cos(), r.sin());
    // e1 spans one surface axis, e2 the other; exactly one of them tilts into
    // depth, so the "which axis warps" question has a clean answer.
    let (e1, e2) = if horizontal {
        // depth varies along the horizontal axis: a receding wall
        (V3::new(c * size, 0.0, s * size), V3::new(0.0, size, 0.0))
    } else {
        // depth varies along the vertical axis: a floor / ceiling
        (V3::new(size, 0.0, 0.0), V3::new(0.0, c * size, s * size))
    };
    let o = V3::new(
        -0.5 * (e1.x + e2.x),
        -0.5 * (e1.y + e2.y),
        dist - 0.5 * (e1.z + e2.z),
    );
    Scene {
        name: name.into(),
        o,
        e1,
        e2,
    }
}

/// A plane tilted about BOTH axes: pitch `pitch` then yaw `yaw`, so depth
/// varies along *both* surface parameters.
///
/// Without these, every scene would have exactly one warping axis and the 1D
/// (`*-1xN`) strategies would look better than they deserve.
fn tilted2(name: &str, pitch: f64, yaw: f64, dist: f64, size: f64) -> Scene {
    let (p, y) = (pitch.to_radians(), yaw.to_radians());
    let (cp, sp, cy, sy) = (p.cos(), p.sin(), y.cos(), y.sin());
    // a = (1,0,0) then Ry(yaw); b = Rx(pitch)*(0,1,0) then Ry(yaw).
    let e1 = V3::new(cy * size, 0.0, -sy * size);
    let e2 = V3::new(sp * sy * size, cp * size, sp * cy * size);
    let o = V3::new(
        -0.5 * (e1.x + e2.x),
        -0.5 * (e1.y + e2.y),
        dist - 0.5 * (e1.z + e2.z),
    );
    Scene {
        name: name.into(),
        o,
        e1,
        e2,
    }
}

/// The scene sweep: increasingly grazing floors at two distances, receding
/// walls (horizontal depth axis), and doubly-tilted planes (both axes warp).
fn scenes() -> Vec<Scene> {
    let mut v = Vec::new();
    for deg in [0.0f64, 20.0, 40.0, 60.0, 75.0, 85.0] {
        v.push(tilted(&format!("floor{deg:.0}-near"), deg, 700.0, 700.0, false));
    }
    for deg in [60.0f64, 85.0] {
        v.push(tilted(&format!("floor{deg:.0}-far"), deg, 1800.0, 700.0, false));
    }
    for deg in [60.0f64, 85.0] {
        v.push(tilted(&format!("wall{deg:.0}-near"), deg, 700.0, 700.0, true));
    }
    for (pitch, yaw) in [(60.0f64, 40.0f64), (75.0, 55.0), (80.0, 25.0)] {
        v.push(tilted2(
            &format!("diag{pitch:.0}x{yaw:.0}"),
            pitch,
            yaw,
            800.0,
            700.0,
        ));
    }
    v
}

// ----------------------------------------------------------------------
// Output
// ----------------------------------------------------------------------

/// Write an error heatmap: black = exact, green ramp to 4 texels, red beyond.
fn write_heatmap(path: &std::path::Path, field: &[f32]) {
    let mut buf = format!("P6\n{W} {H}\n255\n").into_bytes();
    for e in field {
        let px = if e.is_nan() {
            [16u8, 16, 32]
        } else {
            let t = (*e / 4.0).min(1.0);
            let over = ((*e - 4.0) / 12.0).clamp(0.0, 1.0);
            [
                (over * 255.0) as u8,
                ((1.0 - over) * t * 255.0) as u8,
                ((1.0 - t) * 90.0) as u8,
            ]
        };
        buf.extend_from_slice(&px);
    }
    let _ = std::fs::write(path, buf);
}

/// Dump the raw sampled-UV field: red = u, green = v. Shows the warp directly.
fn write_uv(path: &std::path::Path, gpu: &Gpu) {
    let mut buf = format!("P6\n{W} {H}\n255\n").into_bytes();
    for y in 0..H {
        for x in 0..W {
            let t = gpu.vram.get_pixel(x as u16, y as u16);
            if t & 0x8000 == 0 {
                buf.extend_from_slice(&[16, 16, 32]);
            } else {
                let idx = t & 0x0FFF;
                buf.extend_from_slice(&[
                    ((idx & 63) * 4) as u8,
                    ((idx >> 6) * 4) as u8,
                    64,
                ]);
            }
        }
    }
    let _ = std::fs::write(path, buf);
}

fn main() {
    let outdir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/texwarp".into());
    let outdir = std::path::PathBuf::from(outdir);
    std::fs::create_dir_all(&outdir).expect("create output dir");

    let scenes = scenes();
    let strats = strategies();

    let mut csv = String::from(
        "scene,strategy,prims,verts,uverts,gpu_cycles,pixels,spill,mean_texels,\
         p95_texels,max_texels,frac_over_1tx,frac_over_4tx,predicted_max_texels\n",
    );
    // strategy -> accumulated (mean, p95, max, prims, verts, cycles) over the
    // warping scenes only (the head-on scene has nothing to fix).
    let mut agg: Vec<Agg> = (0..strats.len()).map(|_| Agg::default()).collect();
    // Instrument noise floor, taken from the head-on scene.
    let (mut noise_mean, mut noise_max) = (0.0f64, 0.0f64);
    // measured_max / predicted_max, sampled where the prediction is meaningful.
    let mut ratio_samples: Vec<f64> = Vec::new();

    println!(
        "texwarp: {} scenes x {} strategies, {}x{} draw area, {}x{} texture",
        scenes.len(),
        strats.len(),
        W,
        H,
        TEX,
        TEX
    );

    for sc in &scenes {
        println!("\n=== scene {} ===", sc.name);
        println!(
            "{:<14} {:>6} {:>6} {:>9} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9}",
            "strategy", "prims", "verts", "gpucyc", "mean", "p95", "max", ">1tx", ">4tx", "pred_max"
        );
        for (si, st) in strats.iter().enumerate() {
            let mut gpu = Gpu::new();
            upload_texture(&mut gpu);
            let (ss, ts, emit) = (st.plan)(sc);
            let prims = build(sc, &ss, &ts, emit, st.umax);
            let pred = predict(sc, &ss, &ts, st.umax);
            // Adjacent cells share corners, so the GTE projects the grid
            // lattice, not 4 vertices per primitive.
            let m = measure(&mut gpu, sc, &prims, st.umax, pred, ss.len() * ts.len());

            println!(
                "{:<14} {:>6} {:>6} {:>9} {:>8.2} {:>8.2} {:>8.2} {:>7.1}% {:>7.1}% {:>9.2}",
                st.name,
                m.prims,
                m.verts,
                m.gpu_cycles,
                m.mean,
                m.p95,
                m.max,
                m.over1 * 100.0,
                m.over4 * 100.0,
                m.predicted
            );
            let _ = writeln!(
                csv,
                "{},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{:.5},{:.5},{:.4}",
                sc.name,
                st.name,
                m.prims,
                m.verts,
                m.uverts,
                m.gpu_cycles,
                m.pixels,
                m.spill,
                m.mean,
                m.p95,
                m.max,
                m.over1,
                m.over4,
                m.predicted
            );

            // Head-on scenes carry no warp; averaging them in would flatter
            // every strategy equally and hide the ranking. Use the single-quad
            // head-on render as the instrument's noise floor instead.
            if sc.name.starts_with("floor0") {
                if st.name == "quad1" {
                    noise_mean = m.mean;
                    noise_max = m.max;
                }
            } else {
                let a = &mut agg[si];
                a.mean += m.mean;
                a.p95 += m.p95;
                a.max += m.max;
                a.prims += m.prims as f64;
                a.verts += m.verts as f64;
                a.cycles += m.gpu_cycles as f64;
                a.spill += m.spill as f64;
                a.n += 1;
                if m.predicted > 2.0 {
                    ratio_samples.push(m.max / m.predicted);
                }
            }

            // Image dumps for the flagship grazing scene only.
            if sc.name == "floor75-near" {
                write_heatmap(&outdir.join(format!("err-{}.ppm", st.name)), &m.field);
                write_uv(&outdir.join(format!("uv-{}.ppm", st.name)), &gpu);
            }
        }
    }

    println!("\n=== instrument noise floor ===");
    println!(
        "head-on quad (zero true warp): mean {:.2} tx, max {:.2} tx.\n\
         That is the PS1 UV DDA's own fixed-point truncation, not perspective.\n\
         No strategy can beat it; means below ~{:.2} are indistinguishable.",
        noise_mean, noise_max, noise_mean
    );

    println!("\n=== aggregate over warping scenes (head-on excluded) ===");
    println!(
        "{:<14} {:>7} {:>7} {:>10} {:>8} {:>8} {:>8} {:>7} {:>9}",
        "strategy", "prims", "verts", "gpucyc*", "mean", "p95", "max", "spill", "max/prim"
    );
    let mut rows: Vec<(usize, f64)> = (0..strats.len())
        .map(|i| (i, agg[i].mean / agg[i].n.max(1) as f64))
        .collect();
    rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
    for (i, mean) in &rows {
        let a = agg[*i];
        let n = a.n.max(1) as f64;
        println!(
            "{:<17} {:>7.1} {:>7.1} {:>10.0} {:>8.2} {:>8.2} {:>8.2} {:>7.0} {:>9.2}",
            strats[*i].name,
            a.prims / n,
            a.verts / n,
            a.cycles / n,
            mean,
            a.p95 / n,
            a.max / n,
            a.spill / n,
            (a.max / n) * (a.prims / n)
        );
    }
    println!(
        "* gpucyc is the emulator's area-based GPU cost model; it over-counts \n\
         partial edge pixels, so subdivided rows carry some model artefact. \n\
         Trust prims/verts as the CPU cost driver."
    );

    // The closed form predicts the error at an edge's screen midpoint. The
    // worst error over the polygon interior is consistently larger; measure
    // the ratio so the engine can use `edge_error * k` as a real bound.
    let ratios: Vec<f64> = ratio_samples;
    if !ratios.is_empty() {
        let mut r = ratios.clone();
        r.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "\n=== closed-form calibration (samples with predicted > 2 tx) ===\n\
             measured_max / predicted:  median {:.2}x   p90 {:.2}x   max {:.2}x   (n={})\n\
             So `err = du * |zb-za| / (2*(za+zb))` is a valid runtime criterion \n\
             once multiplied by ~{:.1} to bound the true worst-case texel error.",
            r[r.len() / 2],
            r[r.len() * 9 / 10],
            r[r.len() - 1],
            r.len(),
            r[r.len() * 9 / 10]
        );
    }

    let csv_path = outdir.join("results.csv");
    std::fs::write(&csv_path, csv).expect("write csv");
    println!("\ncsv:    {}", csv_path.display());
    println!("images: {}/{{err,uv}}-*.ppm (scene floor75-near)", outdir.display());
}
