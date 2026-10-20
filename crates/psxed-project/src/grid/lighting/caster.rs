//! A host-side ray query against the faces of a grid world.
//!
//! The question the light bake asks beyond "is this lamp blocked" is "what
//! does this ray hit": the face, where, and what it is made of, so ambient
//! occlusion can darken a corner and a bounce can borrow the colour of the
//! wall it came from. The runtime's line of sight only answers yes or no, so
//! this walks the drawn faces themselves.
//!
//! The faces are the snapped polygons the cook writes, cut into triangles and
//! binned in a uniform grid whose cells are one sector on a side (or a
//! multiple of it for a very large world). A ray walks that grid with a 3D
//! DDA and tests the triangles of each cell it enters, nearest hit first.
//! Every face blocks from both sides, as it does in the collision world.

use super::super::cook::units::RuntimeUnits;
use super::super::faces::GridFace;
use crate::ResourceId;

/// Most cells the acceleration grid may have; the cell edge doubles until the
/// world fits.
const MAX_CELLS: usize = 4_000_000;
/// A triangle edge test tolerance, as a fraction of the triangle.
const BARY_EPSILON: f64 = 1e-9;
/// Rays shorter than this never hit anything.
const MIN_T: f64 = 1e-6;

/// One triangle of a face.
#[derive(Clone, Copy, Debug)]
struct Tri {
    /// Index of the face (in the slice [`FaceCaster::build`] was given).
    face: u32,
    /// Vertex indices into [`FaceCaster::verts`].
    v: [u32; 3],
    /// Which corners of the face the vertices are.
    corner: [u8; 3],
}

/// What a ray found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayHit {
    /// Distance along the ray (the ray direction is a unit vector).
    pub t: f64,
    /// The hit point.
    pub point: [f64; 3],
    /// Unit normal of the face's visible side.
    pub normal: [f64; 3],
    /// Whether the ray met the visible side.
    pub front: bool,
    /// Index of the face hit.
    pub face: usize,
    /// The face's material.
    pub material: Option<ResourceId>,
    /// The face corners the hit lies between, and the weight of each.
    pub corners: [u8; 3],
    pub weights: [f64; 3],
}

#[derive(Clone, Debug)]
struct FaceRec {
    normal: [f64; 3],
    material: Option<ResourceId>,
}

/// The faces of a world as a ray target.
pub struct FaceCaster {
    verts: Vec<[f64; 3]>,
    tris: Vec<Tri>,
    faces: Vec<FaceRec>,
    /// Lower corner and cell edge of the grid, runtime units.
    origin: [f64; 3],
    edge: f64,
    dims: [usize; 3],
    /// CSR: `cell_start[c]..cell_start[c + 1]` indexes `cell_tris`.
    cell_start: Vec<u32>,
    cell_tris: Vec<u32>,
    /// Box of every vertex.
    lo: [f64; 3],
    hi: [f64; 3],
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl FaceCaster {
    /// The target of `faces`, corners snapped to runtime units the way the
    /// cook snaps them.
    pub fn build(faces: &[GridFace], units: &RuntimeUnits) -> Self {
        let mut verts: Vec<[f64; 3]> = Vec::new();
        let mut tris: Vec<Tri> = Vec::new();
        let mut recs: Vec<FaceRec> = Vec::with_capacity(faces.len());
        for (index, face) in faces.iter().enumerate() {
            recs.push(FaceRec {
                normal: face.normal,
                material: face.material,
            });
            let first = verts.len() as u32;
            let n = face.grid.len();
            for g in &face.grid {
                verts.push(g.map(|v| f64::from(units.rt(v))));
            }
            // A ring of 3 or 4 corners; a fan from corner 0.
            for k in 1..n.saturating_sub(1) {
                let (a, b, c) = (0, k, k + 1);
                let area = cross(
                    sub(verts[first as usize + b], verts[first as usize + a]),
                    sub(verts[first as usize + c], verts[first as usize + a]),
                );
                if dot(area, area) <= 0.0 {
                    continue;
                }
                tris.push(Tri {
                    face: index as u32,
                    v: [first + a as u32, first + b as u32, first + c as u32],
                    corner: [a as u8, b as u8, c as u8],
                });
            }
        }
        let mut lo = [f64::MAX; 3];
        let mut hi = [f64::MIN; 3];
        for v in &verts {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(v[axis]);
                hi[axis] = hi[axis].max(v[axis]);
            }
        }
        if verts.is_empty() {
            lo = [0.0; 3];
            hi = [0.0; 3];
        }
        let mut edge = f64::from(units.sector().max(1));
        let dims_for = |edge: f64| -> [usize; 3] {
            core::array::from_fn(|a| (((hi[a] - lo[a]) / edge).floor() as usize + 1).max(1))
        };
        let mut dims = dims_for(edge);
        while dims[0].saturating_mul(dims[1]).saturating_mul(dims[2]) > MAX_CELLS {
            edge *= 2.0;
            dims = dims_for(edge);
        }
        let mut caster = Self {
            verts,
            tris,
            faces: recs,
            origin: lo,
            edge,
            dims,
            cell_start: Vec::new(),
            cell_tris: Vec::new(),
            lo,
            hi,
        };
        caster.bin();
        caster
    }

    fn cell_index(&self, c: [usize; 3]) -> usize {
        (c[1] * self.dims[2] + c[2]) * self.dims[0] + c[0]
    }

    fn cell_of(&self, p: [f64; 3]) -> [usize; 3] {
        core::array::from_fn(|a| {
            (((p[a] - self.origin[a]) / self.edge).floor().max(0.0) as usize).min(self.dims[a] - 1)
        })
    }

    /// Counting sort of the triangles into the cells their boxes touch.
    fn bin(&mut self) {
        let total = self.dims[0] * self.dims[1] * self.dims[2];
        let mut counts = vec![0u32; total + 1];
        let ranges: Vec<([usize; 3], [usize; 3])> = self
            .tris
            .iter()
            .map(|t| {
                let mut lo = [f64::MAX; 3];
                let mut hi = [f64::MIN; 3];
                for &v in &t.v {
                    let p = self.verts[v as usize];
                    for a in 0..3 {
                        lo[a] = lo[a].min(p[a]);
                        hi[a] = hi[a].max(p[a]);
                    }
                }
                (self.cell_of(lo), self.cell_of(hi))
            })
            .collect();
        for (lo, hi) in &ranges {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    for x in lo[0]..=hi[0] {
                        counts[self.cell_index([x, y, z]) + 1] += 1;
                    }
                }
            }
        }
        for i in 0..total {
            counts[i + 1] += counts[i];
        }
        let mut fill = counts.clone();
        let mut tris = vec![0u32; counts[total] as usize];
        for (t, (lo, hi)) in ranges.iter().enumerate() {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    for x in lo[0]..=hi[0] {
                        let c = self.cell_index([x, y, z]);
                        tris[fill[c] as usize] = t as u32;
                        fill[c] += 1;
                    }
                }
            }
        }
        self.cell_start = counts;
        self.cell_tris = tris;
    }

    /// Number of faces the target was built from.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// Möller-Trumbore against one triangle; `(t, weights)` of a hit in
    /// `(MIN_T, tmax]`.
    fn hit_tri(
        &self,
        tri: &Tri,
        origin: [f64; 3],
        dir: [f64; 3],
        tmax: f64,
    ) -> Option<(f64, [f64; 3])> {
        let [a, b, c] = tri.v.map(|v| self.verts[v as usize]);
        let e1 = sub(b, a);
        let e2 = sub(c, a);
        let p = cross(dir, e2);
        let det = dot(e1, p);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        let s = sub(origin, a);
        let u = dot(s, p) * inv;
        if !(-BARY_EPSILON..=1.0 + BARY_EPSILON).contains(&u) {
            return None;
        }
        let q = cross(s, e1);
        let v = dot(dir, q) * inv;
        if v < -BARY_EPSILON || u + v > 1.0 + BARY_EPSILON {
            return None;
        }
        let t = dot(e2, q) * inv;
        (t > MIN_T && t <= tmax).then_some((t, [1.0 - u - v, u, v]))
    }

    /// The nearest face a ray from `origin` along the unit vector `dir` meets
    /// within `tmax`, if any.
    pub fn cast(&self, origin: [f64; 3], dir: [f64; 3], tmax: f64) -> Option<RayHit> {
        let mut best: Option<(f64, usize, [f64; 3])> = None;
        self.walk(origin, dir, tmax, &mut |t, tri, w| {
            if best.is_none_or(|(bt, _, _)| t < bt) {
                best = Some((t, tri, w));
            }
            // Keep walking; the walk stops when the best hit is behind the
            // next cell.
            best.map(|(bt, _, _)| bt)
        });
        let (t, tri, weights) = best?;
        let tri = self.tris[tri];
        let rec = &self.faces[tri.face as usize];
        Some(RayHit {
            t,
            point: [
                origin[0] + dir[0] * t,
                origin[1] + dir[1] * t,
                origin[2] + dir[2] * t,
            ],
            normal: rec.normal,
            front: dot(dir, rec.normal) < 0.0,
            face: tri.face as usize,
            material: rec.material,
            corners: tri.corner,
            weights,
        })
    }

    /// Whether anything lies between two points (the segment `from..to`).
    pub fn blocked(&self, from: [f64; 3], to: [f64; 3]) -> bool {
        let d = sub(to, from);
        let length = dot(d, d).sqrt();
        if length <= MIN_T {
            return false;
        }
        let dir = d.map(|v| v / length);
        let mut hit = false;
        self.walk(from, dir, length, &mut |_, _, _| {
            hit = true;
            Some(0.0)
        });
        hit
    }

    /// 3D DDA over the cells the ray `origin + t * dir`, `t` in `0..=tmax`,
    /// crosses. `visit(t, tri, weights)` is called for each triangle hit; it
    /// returns the distance of the best hit so far, which ends the walk once
    /// the ray has left the cells that could hold something nearer.
    fn walk(
        &self,
        origin: [f64; 3],
        dir: [f64; 3],
        tmax: f64,
        visit: &mut dyn FnMut(f64, usize, [f64; 3]) -> Option<f64>,
    ) {
        if self.tris.is_empty() {
            return;
        }
        // Clip the ray to the grid's box.
        let mut t0 = 0.0f64;
        let mut t1 = tmax;
        for a in 0..3 {
            let lo = self.origin[a];
            let hi = self.origin[a] + self.edge * self.dims[a] as f64;
            if dir[a].abs() < 1e-15 {
                if origin[a] < lo || origin[a] > hi {
                    return;
                }
            } else {
                let inv = 1.0 / dir[a];
                let (mut ta, mut tb) = ((lo - origin[a]) * inv, (hi - origin[a]) * inv);
                if ta > tb {
                    core::mem::swap(&mut ta, &mut tb);
                }
                t0 = t0.max(ta);
                t1 = t1.min(tb);
                if t0 > t1 {
                    return;
                }
            }
        }
        let start = [
            origin[0] + dir[0] * t0,
            origin[1] + dir[1] * t0,
            origin[2] + dir[2] * t0,
        ];
        let mut cell = self.cell_of(start);
        let mut next_t = [f64::MAX; 3];
        let mut step = [0i64; 3];
        let mut delta = [f64::MAX; 3];
        for a in 0..3 {
            if dir[a] > 1e-15 {
                step[a] = 1;
                delta[a] = self.edge / dir[a];
                let boundary = self.origin[a] + (cell[a] + 1) as f64 * self.edge;
                next_t[a] = (boundary - origin[a]) / dir[a];
            } else if dir[a] < -1e-15 {
                step[a] = -1;
                delta[a] = -self.edge / dir[a];
                let boundary = self.origin[a] + cell[a] as f64 * self.edge;
                next_t[a] = (boundary - origin[a]) / dir[a];
            }
        }
        let mut best: Option<f64> = None;
        loop {
            let c = self.cell_index(cell);
            let (s, e) = (self.cell_start[c] as usize, self.cell_start[c + 1] as usize);
            for &ti in &self.cell_tris[s..e] {
                if let Some((t, w)) = self.hit_tri(&self.tris[ti as usize], origin, dir, tmax) {
                    best = visit(t, ti as usize, w).or(best);
                }
            }
            // Leave through the nearest face of the cell.
            let axis = if next_t[0] <= next_t[1] && next_t[0] <= next_t[2] {
                0
            } else if next_t[1] <= next_t[2] {
                1
            } else {
                2
            };
            let exit = next_t[axis];
            if exit > t1 || best.is_some_and(|b| b <= exit) {
                return;
            }
            let moved = cell[axis] as i64 + step[axis];
            if step[axis] == 0 || moved < 0 || moved as usize >= self.dims[axis] {
                return;
            }
            cell[axis] = moved as usize;
            next_t[axis] += delta[axis];
        }
    }

    /// [`Self::blocked`] by testing every triangle: the reference the grid
    /// walk is checked against.
    #[cfg(test)]
    fn blocked_brute(&self, from: [f64; 3], to: [f64; 3]) -> bool {
        let d = sub(to, from);
        let length = dot(d, d).sqrt();
        if length <= MIN_T {
            return false;
        }
        let dir = d.map(|v| v / length);
        self.tris
            .iter()
            .any(|t| self.hit_tri(t, from, dir, length).is_some())
    }

    /// Box of every vertex, runtime units.
    pub fn bounds(&self) -> ([f64; 3], [f64; 3]) {
        (self.lo, self.hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::cook::tests::{load_reference, rid};
    use crate::grid::faces::all_faces_with;
    use crate::grid::lighting::probe::{collision_tables, room_origins, CollisionProbe};
    use crate::grid::room::GridRoom;
    use crate::grid::surface::GridSurface;
    use crate::grid::units::GridScale;
    use crate::grid::world::GridWorld;

    /// 8 by 8 sectors of floor with a one sector pillar of height 3072 at
    /// (3, 3), the floor material 7 and the pillar top material 9.
    fn hall() -> GridWorld {
        let mut room = GridRoom::new_derived("hall", 8, 8);
        for z in 0..8 {
            for x in 0..8 {
                let (height, material) = if (x, z) == (3, 3) { (3072, 9) } else { (0, 7) };
                room.sector_or_insert(x, z)
                    .unwrap()
                    .set_ground_floor(Some(GridSurface::flat(height, Some(rid(material)))));
            }
        }
        GridWorld {
            scale: GridScale { num: 5, den: 4 },
            rooms: vec![room],
            ..GridWorld::default()
        }
    }

    fn caster_of(world: &GridWorld) -> (FaceCaster, Vec<GridFace>, RuntimeUnits) {
        let units = RuntimeUnits::new(world.scale).unwrap();
        let faces = all_faces_with(world, &());
        (FaceCaster::build(&faces, &units), faces, units)
    }

    #[test]
    fn a_ray_down_hits_the_floor_with_its_point_normal_and_material() {
        let world = hall();
        let (caster, faces, units) = caster_of(&world);
        let s = f64::from(units.sector());
        // Straight down over sector (1, 1), from 40 units up.
        let hit = caster
            .cast([1.5 * s, 40.0, 1.5 * s], [0.0, -1.0, 0.0], 1000.0)
            .expect("the floor is hit");
        assert!((hit.t - 40.0).abs() < 1e-6, "{}", hit.t);
        assert_eq!(hit.point, [1.5 * s, 0.0, 1.5 * s]);
        assert_eq!(hit.normal, [0.0, 1.0, 0.0]);
        assert!(hit.front);
        assert_eq!(hit.material, Some(rid(7)));
        assert_eq!(faces[hit.face].material, Some(rid(7)));
        assert!((hit.weights.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // From below the same floor is hit from the back.
        let under = caster
            .cast([1.5 * s, -40.0, 1.5 * s], [0.0, 1.0, 0.0], 1000.0)
            .expect("the floor is hit from below");
        assert!(!under.front);
        // Nothing within a short reach.
        assert!(caster
            .cast([1.5 * s, 40.0, 1.5 * s], [0.0, -1.0, 0.0], 10.0)
            .is_none());
    }

    #[test]
    fn a_ray_into_the_pillar_hits_its_wall_and_a_ray_over_it_hits_the_top() {
        let world = hall();
        let (caster, _faces, units) = caster_of(&world);
        let s = f64::from(units.sector());
        // Along +x at height 100 from sector (1, 3) toward the pillar at x = 3 s.
        let wall = caster
            .cast([1.5 * s, 100.0, 3.5 * s], [1.0, 0.0, 0.0], 10.0 * s)
            .expect("the pillar side");
        assert!((wall.point[0] - 3.0 * s).abs() < 1e-6, "{:?}", wall.point);
        assert_eq!(wall.normal, [-1.0, 0.0, 0.0]);
        // Straight down over the pillar from above lands on its top.
        let top = caster
            .cast([3.5 * s, 400.0, 3.5 * s], [0.0, -1.0, 0.0], 1000.0)
            .expect("the pillar top");
        assert_eq!(top.material, Some(rid(9)));
        assert!(top.point[1] > 100.0);
    }

    #[test]
    fn blocked_agrees_with_the_cast_and_a_segment_that_stops_short_is_clear() {
        let world = hall();
        let (caster, _faces, units) = caster_of(&world);
        let s = f64::from(units.sector());
        let (a, b) = ([1.5 * s, 100.0, 3.5 * s], [5.5 * s, 100.0, 3.5 * s]);
        assert!(caster.blocked(a, b));
        assert!(caster.cast(a, [1.0, 0.0, 0.0], 4.0 * s).is_some());
        // Stops before the pillar's side.
        assert!(!caster.blocked(a, [2.9 * s, 100.0, 3.5 * s]));
        // Over the pillar.
        assert!(!caster.blocked(
            [1.5 * s, 2500.0 / 16.0 * 4.0, 3.5 * s],
            [5.5 * s, 2500.0 / 16.0 * 4.0, 3.5 * s]
        ));
    }

    /// Deterministic generator for the agreement walks.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// What the two queries said over a set of segments.
    #[derive(Debug, Default)]
    struct Agreement {
        segments: usize,
        /// Segments whose answer from the faces does not change when the
        /// segment is moved a unit and a half sideways: not grazing an edge.
        robust: usize,
        both_blocked: usize,
        both_clear: usize,
        /// Disagreements, split by whether the segment is robust.
        robust_split: usize,
        grazing_split: usize,
    }

    /// Both queries over segments between the tops of the world's floors.
    /// The collision world rounds a query to whole runtime units and cooks
    /// its walls from rounded corners, so a segment that grazes an edge by a
    /// unit can fall either side; those are counted apart.
    fn agreement(world: &GridWorld, segments: usize) -> Agreement {
        let units = RuntimeUnits::new(world.scale).unwrap();
        let origins = room_origins(world, &units);
        let cols = collision_tables(world, &units).unwrap();
        let probe = CollisionProbe::new(world, &units, &origins, &cols).unwrap();
        let faces = all_faces_with(world, &());
        let caster = FaceCaster::build(&faces, &units);
        let floors: Vec<[f64; 3]> = faces
            .iter()
            .filter(|f| f.normal[1] > 0.9 && f.grid.len() >= 3)
            .map(|f| {
                let n = f.grid.len() as f64;
                let mut c = [0.0; 3];
                for g in &f.grid {
                    for k in 0..3 {
                        c[k] += f64::from(units.rt(g[k])) / n;
                    }
                }
                // A lamp-height point over the floor. The runtime's ray test
                // keeps a three unit margin over every step edge, so points
                // closer to the floor than that would measure the margin.
                [c[0], c[1] + 10.0, c[2]]
            })
            .collect();
        let mut rng = Lcg(0x5eed);
        let mut tally = Agreement::default();
        probe
            .with_occlusion(|collision_blocked| {
                for _ in 0..segments {
                    let point = |rng: &mut Lcg| {
                        let base = floors[rng.next() as usize % floors.len()];
                        // Whole runtime units: the runtime rounds a query to them.
                        [
                            (base[0] + (rng.next() % 1000) as f64 / 1000.0 * 6.0).round(),
                            base[1].round(),
                            (base[2] + (rng.next() % 1000) as f64 / 1000.0 * 6.0).round(),
                        ]
                    };
                    let (a, b) = (point(&mut rng), point(&mut rng));
                    let by_faces = caster.blocked(a, b);
                    let by_collision = collision_blocked(a, b);
                    let shifted = [[1.5, 0.0], [-1.5, 0.0], [0.0, 1.5], [0.0, -1.5]];
                    let robust = shifted.iter().all(|d| {
                        let m = |p: [f64; 3]| [p[0] + d[0], p[1], p[2] + d[1]];
                        caster.blocked(m(a), m(b)) == by_faces
                    });
                    tally.segments += 1;
                    tally.robust += usize::from(robust);
                    match (by_faces, by_collision) {
                        (true, true) => tally.both_blocked += 1,
                        (false, false) => tally.both_clear += 1,
                        _ if robust => tally.robust_split += 1,
                        _ => tally.grazing_split += 1,
                    }
                }
            })
            .unwrap();
        tally
    }

    #[test]
    fn the_grid_walk_finds_exactly_what_testing_every_triangle_finds() {
        for name in ["grid-cathedral", "grid-dungeon", "grid-cave"] {
            let project = load_reference(name).unwrap();
            let world = project.active_scene().world.grid().unwrap();
            let (caster, _faces, _units) = caster_of(world);
            let (lo, hi) = caster.bounds();
            let mut rng = Lcg(77);
            let pick = |rng: &mut Lcg| -> [f64; 3] {
                core::array::from_fn(|a| {
                    lo[a] + (rng.next() % 100_000) as f64 / 100_000.0 * (hi[a] - lo[a])
                })
            };
            let mut blocked = 0;
            for _ in 0..4000 {
                let (a, b) = (pick(&mut rng), pick(&mut rng));
                let walk = caster.blocked(a, b);
                assert_eq!(walk, caster.blocked_brute(a, b), "{name}: {a:?} -> {b:?}");
                blocked += usize::from(walk);
            }
            assert!(blocked > 400, "{name}: {blocked} blocked of 4000");
        }
    }

    #[test]
    fn the_cast_agrees_with_the_runtime_line_of_sight_on_a_hall_with_a_pillar() {
        let result = agreement(&hall(), 4000);
        assert!(
            result.both_blocked > 100 && result.both_clear > 100,
            "{result:?}"
        );
        // The few that differ are segments that run exactly along the
        // diagonal through the pillar's corner on the lattice, where the
        // runtime's walk steps past both walls; the faces see the pillar.
        assert!(result.robust_split <= 4, "{result:?}");
        // Grazing a pillar edge by under a unit can fall either side.
        assert!(result.grazing_split * 100 <= result.segments, "{result:?}");
    }

    #[test]
    fn the_cast_agrees_with_the_runtime_line_of_sight_on_the_reference_levels() {
        // Over segments between lamp-height points above the floors, the two
        // answer alike on all but a sliver. The runtime keeps a three unit
        // margin over every step edge, so it blocks a ray that clears an
        // edge by less; and it leaves out floors that are not walkable,
        // which are still drawn and so still block light.
        let mut report = String::new();
        for name in [
            "grid-starter",
            "grid-cathedral",
            "grid-dungeon",
            "grid-cave",
        ] {
            let project = load_reference(name).unwrap();
            let world = project.active_scene().world.grid().unwrap();
            let result = agreement(world, 3000);
            report.push_str(&format!("{name}: {result:?}\n"));
            assert!(
                result.robust_split * 50 <= result.segments,
                "{name}: {result:?}"
            );
        }
        eprintln!("{report}");
    }
}
