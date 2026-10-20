//! The global-light gather: ambient occlusion, sky light and bounced light,
//! solved on the host for every corner of every face and baked into the
//! vertex colours.
//!
//! Each corner of each face is a sample (corners shared by faces of one
//! orientation are one sample). A sample sends a fixed set of cosine-weighted
//! rays into the hemisphere above it. A ray that meets a face within the
//! world's reach is *blocked*: it takes away part of the ambient (occlusion)
//! and brings in the light that face sends back (bounce). A ray that goes on
//! past the reach is *open* and brings in the room ambient; one that leaves
//! the world altogether brings in the sky colour.
//!
//! The first pass has no bounce to bring in; each later pass reads the pass
//! before it at the point each ray landed, so the light that reached a wall
//! is what the wall hands on. Lamps are not rays here: their shadowed light is
//! added when a corner is coloured, exactly as without the gather, and the
//! gather only supplies what replaces the flat room ambient.
//!
//! The ray set of a sample is a Hammersley pattern turned by a hash of the
//! sample's position and normal, so a given scene always bakes to the same
//! bytes, a corner shared by two faces gets the same light on both, and the
//! pattern's banding does not line up across the level.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::caster::FaceCaster;
use super::probe::CollisionProbe;
use super::settings::GlobalLight;
use crate::grid::cook::units::RuntimeUnits;
use crate::grid::faces::{all_faces_with, GridFace, GridFaceKind};
use crate::grid::light::{accumulate_direct, GridLight, PLANE_SHADOW_NUDGE};
use crate::grid::world::GridWorld;
use crate::ResourceId;

/// Start offset of an occlusion ray along the surface normal, runtime units.
const RAY_NUDGE: f64 = 0.5;
/// How far a ray starts from its corner toward the middle of the face,
/// runtime units.
const INWARD_STEP: f64 = 1.0;
/// Start offset of a shadow ray, matching [`crate::grid::light`].
const SHADOW_NUDGE: f64 = 0.25;
/// What a face that is drawn at its tint with no light reflects, in the
/// bake's units (128 is the texture as it is).
const UNLIT_RADIANCE: f64 = 128.0;
/// Albedo of a face with no material or no readable texture.
pub const FALLBACK_ALBEDO: [f32; 3] = [0.5, 0.5, 0.5];

/// Reflectance of a material: the average colour of its texture (texels
/// 0..=1) times its tint over the neutral value.
pub trait BakeMaterials: Sync {
    /// The reflectance per channel of `material`.
    fn albedo(&self, material: Option<ResourceId>) -> [f32; 3];

    /// Whether a face of `material` stops a ray. A sky aperture is a hole to
    /// the sky and a translucent material lets light through, so rays pass
    /// them; everything else blocks.
    fn occludes(&self, _material: Option<ResourceId>) -> bool {
        true
    }
}

/// Materials that all reflect the same.
pub struct FlatAlbedo(pub [f32; 3]);

impl BakeMaterials for FlatAlbedo {
    fn albedo(&self, _: Option<ResourceId>) -> [f32; 3] {
        self.0
    }
}

/// Identity of a sample: room, the corner in grid units and the side the
/// light lands on (the quantised normal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SampleKey {
    room: u16,
    grid: [i32; 3],
    normal: [i8; 3],
}

impl SampleKey {
    /// The key of the corner at grid position `grid` of a face in `room`
    /// whose lit side faces `normal` (unit length).
    pub fn new(room: usize, grid: [i32; 3], normal: [f64; 3]) -> Self {
        Self {
            room: room.min(usize::from(u16::MAX)) as u16,
            grid,
            normal: normal.map(|v| (v * 127.0).round().clamp(-127.0, 127.0) as i8),
        }
    }

    /// Key of corner `corner` of `face`, on its visible side (`back` false) or
    /// the hidden one.
    pub fn of_corner(face: &GridFace, corner: usize, back: bool) -> Self {
        let normal = if back {
            face.normal.map(|v| -v)
        } else {
            face.normal
        };
        Self::new(face.room, face.grid[corner], normal)
    }

    fn hash(&self) -> u32 {
        // FNV-1a over the key's bytes.
        let mut h = 0x811c_9dc5u32;
        let mut feed = |bytes: &[u8]| {
            for &b in bytes {
                h ^= u32::from(b);
                h = h.wrapping_mul(0x0100_0193);
            }
        };
        feed(&self.room.to_le_bytes());
        for v in self.grid {
            feed(&v.to_le_bytes());
        }
        feed(&self.normal.map(|v| v as u8));
        // A final avalanche so near keys give unrelated angles.
        h ^= h >> 16;
        h = h.wrapping_mul(0x85eb_ca6b);
        h ^= h >> 13;
        h = h.wrapping_mul(0xc2b2_ae35);
        h ^ (h >> 16)
    }
}

/// What the gather found for a world: the ambient (occlusion, sky and
/// bounce) each sample takes, in the bake's 128-neutral units.
#[derive(Default)]
pub struct GiField {
    map: HashMap<SampleKey, [f32; 3]>,
    /// What the solve did, for the log.
    pub stats: GatherStats,
}

impl GiField {
    /// The gathered ambient of the sample `key`, if the solve had it.
    pub fn get(&self, key: &SampleKey) -> Option<[f64; 3]> {
        self.map.get(key).map(|v| v.map(f64::from))
    }

    /// Samples held.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the field holds no sample.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Counters of one solve.
#[derive(Clone, Copy, Debug, Default)]
pub struct GatherStats {
    pub faces: usize,
    pub samples: usize,
    pub rays: u64,
    /// Passes run (the first gather plus the bounces).
    pub passes: usize,
    pub threads: usize,
}

/// Everything the gather reads.
pub struct GatherInput<'a> {
    pub world: &'a GridWorld,
    pub units: RuntimeUnits,
    /// Point lights, runtime units. Their shadowed light is what walls hand
    /// on when a ray lands on them.
    pub lights: &'a [GridLight],
    /// The collision the lamps' shadows are shot against.
    pub probe: &'a CollisionProbe,
    pub materials: &'a dyn BakeMaterials,
    /// Set to stop a solve that is no longer wanted; it then returns `None`.
    pub cancel: Option<&'a AtomicBool>,
}

/// One point the hemisphere is sampled at.
struct Sample {
    point: [f64; 3],
    normal: [f64; 3],
    nudge: f64,
    /// Ambient of the room the sample lies in, 128-neutral units.
    ambient: [f64; 3],
    sky: [f64; 3],
    /// A step from the corner into its face, so a ray does not start on
    /// the plane of the wall or floor next to it.
    inward: [f64; 3],
    /// Drawn at its tint with no light; takes nothing from the gather.
    unlit: bool,
    seed: u32,
}

/// A ray's outcome, packed: 12 bytes.
#[derive(Clone, Copy)]
struct Ray {
    /// Face hit; [`OPEN`] or [`SKY`] when none within reach.
    face: u32,
    w1: u16,
    w2: u16,
    corners: [u8; 3],
}

const OPEN: u32 = u32::MAX;
const SKY: u32 = u32::MAX - 1;
const BACK: u32 = 1 << 31;

const TAU: f64 = core::f64::consts::TAU;

fn radical_inverse(mut i: u32) -> f64 {
    i = i.rotate_right(16);
    i = ((i & 0x5555_5555) << 1) | ((i >> 1) & 0x5555_5555);
    i = ((i & 0x3333_3333) << 2) | ((i >> 2) & 0x3333_3333);
    i = ((i & 0x0f0f_0f0f) << 4) | ((i >> 4) & 0x0f0f_0f0f);
    i = ((i & 0x00ff_00ff) << 8) | ((i >> 8) & 0x00ff_00ff);
    f64::from(i) / 4_294_967_296.0
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn add3(acc: &mut [f64; 3], v: [f64; 3]) {
    for (a, b) in acc.iter_mut().zip(v) {
        *a += b;
    }
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if l > 0.0 {
        a.map(|v| v / l)
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// The i-th of `count` cosine-weighted directions about `normal`, the pattern
/// turned by `turn` (0..1 of a full circle).
pub fn hemisphere_dir(normal: [f64; 3], i: u32, count: u32, turn: f64) -> [f64; 3] {
    let up = if normal[1].abs() < 0.99 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t = norm(cross(up, normal));
    let b = cross(normal, t);
    let u1 = (f64::from(i) + 0.5) / f64::from(count);
    let phi = TAU * (radical_inverse(i) + turn);
    let r = u1.sqrt();
    let (x, y, z) = (r * phi.cos(), r * phi.sin(), (1.0 - u1).sqrt());
    core::array::from_fn(|k| t[k] * x + b[k] * y + normal[k] * z)
}

/// A step of [`INWARD_STEP`] from the corner `point` of the face with grid
/// corners `ring` toward the middle of the face (never past a quarter of the
/// way there). A corner sits on the plane of whatever walls or floors meet
/// the face, and a ray that starts exactly on such a plane passes through it
/// unseen.
fn inward_step(ring: &[[i32; 3]], units: &RuntimeUnits, point: [f64; 3]) -> [f64; 3] {
    let n = ring.len().max(1) as f64;
    let mut centre = [0.0; 3];
    for g in ring {
        for a in 0..3 {
            centre[a] += f64::from(units.rt(g[a])) / n;
        }
    }
    let d: [f64; 3] = core::array::from_fn(|a| centre[a] - point[a]);
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if length < 1e-9 {
        return [0.0; 3];
    }
    let step = INWARD_STEP.min(length * 0.25);
    d.map(|v| v / length * step)
}

fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 8)
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|c| c.load(Ordering::Relaxed))
}

/// Run `work(range_start, chunk)` over `out` split into one chunk per thread.
/// Returns false when cancelled.
fn par_chunks<T: Send>(
    out: &mut [T],
    per_item: usize,
    cancel: Option<&AtomicBool>,
    work: &(dyn Fn(usize, &mut [T]) + Sync),
) -> bool {
    let n_threads = threads();
    let items = out.len() / per_item.max(1);
    let chunk_items = items.div_ceil(n_threads).max(1);
    std::thread::scope(|scope| {
        for (k, chunk) in out.chunks_mut(chunk_items * per_item.max(1)).enumerate() {
            scope.spawn(move || {
                // Small slices so a cancel lands quickly.
                let step = 256 * per_item.max(1);
                for (j, piece) in chunk.chunks_mut(step).enumerate() {
                    if cancelled(cancel) {
                        return;
                    }
                    work(k * chunk_items + j * 256, piece);
                }
            });
        }
    });
    !cancelled(cancel)
}

/// Solve the gather of `input.world`; `None` when cancelled. The result is a
/// pure function of the inputs.
pub fn solve(input: &GatherInput<'_>) -> Option<GiField> {
    let world = input.world;
    let settings: GlobalLight = world.global_light.clamped();
    let units = input.units;
    let faces = all_faces_with(world, &());
    let caster = FaceCaster::build(&faces, &units, &|face| {
        input.materials.occludes(face.material)
    });

    // Samples, shared by faces of one orientation at one corner.
    let mut index: HashMap<SampleKey, u32> = HashMap::new();
    let mut samples: Vec<Sample> = Vec::new();
    let mut keys: Vec<SampleKey> = Vec::new();
    let mut corner_sample: Vec<[u32; 4]> = Vec::with_capacity(faces.len());
    let ambient_of = |room: usize| -> [f64; 3] {
        world
            .rooms
            .get(room)
            .map_or([96.0; 3], |r| r.ambient.map(f64::from))
    };
    for face in &faces {
        let plane = matches!(
            face.kind,
            GridFaceKind::Plane { .. } | GridFaceKind::Mesh { .. }
        );
        let nudge = if plane {
            PLANE_SHADOW_NUDGE
        } else {
            SHADOW_NUDGE
        };
        let ambient = ambient_of(face.room);
        let sky = settings.sky.map_or(ambient, |c| c.map(f64::from));
        let unlit = plane && face.unlit;
        let mut ids = [0u32; 4];
        for (corner, slot) in ids.iter_mut().enumerate().take(face.grid.len().min(4)) {
            let two_sided = plane && face.double_sided;
            for back in [false, true] {
                if back && !two_sided {
                    continue;
                }
                let key = SampleKey::of_corner(face, corner, back);
                let id = *index.entry(key).or_insert_with(|| {
                    let normal = if back {
                        face.normal.map(|v| -v)
                    } else {
                        face.normal
                    };
                    let point = face.grid[corner].map(|g| f64::from(units.rt(g)));
                    samples.push(Sample {
                        point,
                        normal,
                        nudge,
                        ambient,
                        sky,
                        inward: inward_step(&face.grid, &units, point),
                        unlit,
                        seed: key.hash(),
                    });
                    keys.push(key);
                    (samples.len() - 1) as u32
                });
                if !back {
                    *slot = id;
                }
            }
        }
        corner_sample.push(ids);
    }
    let count = samples.len();
    let n_rays = u32::from(settings.rays);
    let reach = f64::from(settings.reach) * f64::from(units.sector());
    let sky_differs = samples.iter().any(|s| s.sky != s.ambient);
    let far = {
        let (lo, hi) = caster.bounds();
        let d: [f64; 3] = core::array::from_fn(|a| hi[a] - lo[a]);
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() * 2.0 + reach
    };
    let tmax = if sky_differs { far } else { reach };

    // Lamps' shadowed light at every sample.
    let mut direct = vec![[0.0f32; 3]; count];
    let ok = par_chunks(&mut direct, 1, input.cancel, &|first, out| {
        let _ = input.probe.with_occlusion(|occluded| {
            for (k, slot) in out.iter_mut().enumerate() {
                let s = &samples[first + k];
                if s.unlit {
                    *slot = [UNLIT_RADIANCE as f32; 3];
                    continue;
                }
                let mut acc = [0.0f64; 3];
                accumulate_direct(&mut acc, s.point, s.normal, input.lights, occluded, s.nudge);
                *slot = acc.map(|v| v as f32);
            }
        });
    });
    if !ok {
        return None;
    }

    // The rays, once: the hemisphere does not change between passes.
    let mut rays = vec![
        Ray {
            face: OPEN,
            w1: 0,
            w2: 0,
            corners: [0; 3],
        };
        count * n_rays as usize
    ];
    let ok = par_chunks(&mut rays, n_rays as usize, input.cancel, &|first, out| {
        for (k, per) in out.chunks_mut(n_rays as usize).enumerate() {
            let s = &samples[first + k];
            if s.unlit {
                continue;
            }
            let turn = f64::from(s.seed) / 4_294_967_296.0;
            let lift = RAY_NUDGE.max(s.nudge);
            let origin: [f64; 3] =
                core::array::from_fn(|a| s.point[a] + s.normal[a] * lift + s.inward[a]);
            for (i, slot) in per.iter_mut().enumerate() {
                let dir = hemisphere_dir(s.normal, i as u32, n_rays, turn);
                *slot = match caster.cast(origin, dir, tmax) {
                    None => Ray {
                        face: if sky_differs { SKY } else { OPEN },
                        ..*slot
                    },
                    Some(hit) if hit.t > reach => Ray {
                        face: OPEN,
                        ..*slot
                    },
                    Some(hit) => Ray {
                        face: hit.face as u32 | if hit.front { 0 } else { BACK },
                        w1: (hit.weights[1].clamp(0.0, 1.0) * 65535.0).round() as u16,
                        w2: (hit.weights[2].clamp(0.0, 1.0) * 65535.0).round() as u16,
                        corners: hit.corners,
                    },
                };
            }
        }
    });
    if !ok {
        return None;
    }

    // Reflectance per face.
    let mut albedo_cache: HashMap<Option<ResourceId>, [f32; 3]> = HashMap::new();
    let face_albedo: Vec<[f32; 3]> = faces
        .iter()
        .map(|f| {
            *albedo_cache
                .entry(f.material)
                .or_insert_with(|| input.materials.albedo(f.material))
        })
        .collect();

    // Pass 0 is the occlusion and sky gather; each bounce reads the pass
    // before it.
    let occlusion = f64::from(settings.occlusion) / 100.0;
    let gain = f64::from(settings.bounce_gain) / 100.0;
    let passes = 1 + usize::from(settings.bounces);
    let mut gi: Vec<[f32; 3]> = vec![[0.0; 3]; count];
    for pass in 0..passes {
        let previous = std::mem::take(&mut gi);
        gi = vec![[0.0; 3]; count];
        let bounce = pass > 0;
        let ok = par_chunks(&mut gi, 1, input.cancel, &|first, out| {
            for (k, slot) in out.iter_mut().enumerate() {
                let id = first + k;
                let s = &samples[id];
                if s.unlit {
                    *slot = [0.0; 3];
                    continue;
                }
                let mut sum = [0.0f64; 3];
                for ray in &rays[id * n_rays as usize..(id + 1) * n_rays as usize] {
                    match ray.face {
                        OPEN => add3(&mut sum, s.ambient),
                        SKY => add3(&mut sum, s.sky),
                        packed => {
                            add3(&mut sum, s.ambient.map(|a| (1.0 - occlusion) * a));
                            if bounce && packed & BACK == 0 && occlusion > 0.0 {
                                let f = (packed & !BACK) as usize;
                                let w1 = f64::from(ray.w1) / 65535.0;
                                let w2 = f64::from(ray.w2) / 65535.0;
                                let w = [1.0 - w1 - w2, w1, w2];
                                let ids = corner_sample[f];
                                let mut e = [0.0f64; 3];
                                for (wi, corner) in w.iter().zip(ray.corners) {
                                    let t = ids[usize::from(corner).min(3)] as usize;
                                    add3(
                                        &mut e,
                                        core::array::from_fn(|c| {
                                            wi * (f64::from(direct[t][c])
                                                + f64::from(previous[t][c]))
                                        }),
                                    );
                                }
                                let rho = face_albedo[f];
                                add3(
                                    &mut sum,
                                    core::array::from_fn(|c| {
                                        occlusion * gain * f64::from(rho[c]) * e[c]
                                    }),
                                );
                            }
                        }
                    }
                }
                let inv = 1.0 / f64::from(n_rays);
                *slot = sum.map(|v| (v * inv) as f32);
            }
        });
        if !ok {
            return None;
        }
    }

    let stats = GatherStats {
        faces: faces.len(),
        samples: count,
        rays: count as u64 * u64::from(n_rays),
        passes,
        threads: threads(),
    };
    let map = keys.into_iter().zip(gi).collect();
    Some(GiField { map, stats })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hemisphere_pattern_is_unit_length_above_the_surface_and_even() {
        let n = norm([0.3, 0.8, -0.2]);
        let count = 64;
        let mut mean = [0.0; 3];
        for i in 0..count {
            let d = hemisphere_dir(n, i, count, 0.37);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            assert!((len - 1.0).abs() < 1e-9);
            let up = d[0] * n[0] + d[1] * n[1] + d[2] * n[2];
            assert!(up > 0.0, "ray {i} points into the surface");
            for k in 0..3 {
                mean[k] += d[k] / f64::from(count);
            }
        }
        // Cosine weighting: the mean direction leans along the normal by
        // 2/3 of the way to the pole.
        let along = mean[0] * n[0] + mean[1] * n[1] + mean[2] * n[2];
        assert!((along - 2.0 / 3.0).abs() < 0.03, "{along}");
    }

    #[test]
    fn the_same_key_always_hashes_the_same() {
        let a = SampleKey::new(1, [10, 20, 30], [0.0, 1.0, 0.0]);
        let b = SampleKey::new(1, [10, 20, 30], [0.0, 1.0, 0.0]);
        let c = SampleKey::new(1, [10, 20, 30], [0.0, -1.0, 0.0]);
        assert_eq!(a.hash(), b.hash());
        assert_ne!(a.hash(), c.hash());
    }
}
