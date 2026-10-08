//! Editable low-poly heightfields expanded to watertight, closed BSP solids.
use crate::brush::{Brush, BrushContents, BrushFace, Plane};
use crate::ResourceId;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CELLS: usize = 16;
pub const HEIGHT_STEP: i32 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerrainShape {
    Hills,
    Ridge,
    Island,
    Flat,
}
impl TerrainShape {
    pub const ALL: [Self; 4] = [Self::Hills, Self::Ridge, Self::Island, Self::Flat];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hills => "Rolling hills",
            Self::Ridge => "Rocky ridge",
            Self::Island => "Island",
            Self::Flat => "Flat",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SculptMode {
    Raise,
    Lower,
    Smooth,
    Flatten,
}
impl SculptMode {
    pub const ALL: [Self; 4] = [Self::Raise, Self::Lower, Self::Smooth, Self::Flatten];
    pub const fn label(self) -> &'static str {
        match self {
            Self::Raise => "Raise",
            Self::Lower => "Lower",
            Self::Smooth => "Smooth",
            Self::Flatten => "Flatten",
        }
    }
}
/// World Y heights, row-major along X then Z. Adjacent triangles share
/// quantized vertices, including when sculpted or converted to collision.
#[derive(Clone, Debug, PartialEq)]
pub struct Terrain {
    pub cells: [usize; 2],
    pub spacing: [i32; 2],
    pub origin: [i32; 2],
    pub bottom: i32,
    pub heights: Vec<f64>,
}
impl Terrain {
    /// Five sky-aperture solids closing the sides and top. The terrain itself
    /// closes the bottom. Keep these in a separate group so sculpt recovery
    /// still sees only the heightfield wedges.
    pub fn sky_enclosure(&self, ceiling: i32, material: ResourceId) -> Result<Vec<Brush>, String> {
        self.validate()?;
        let limit = crate::brush::BRUSH_EDIT_EXTENT_LIMIT as i32;
        let x = self.origin[0];
        let z = self.origin[1];
        let x1 = x + self.cells[0] as i32 * self.spacing[0];
        let z1 = z + self.cells[1] as i32 * self.spacing[1];
        let thickness = 256;
        let peak = self
            .heights
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        if ceiling % HEIGHT_STEP != 0
            || f64::from(ceiling) < peak + 256.0
            || ceiling > limit - thickness
            || x < -limit + thickness
            || z < -limit + thickness
            || x1 > limit - thickness
            || z1 > limit - thickness
        {
            return Err("Sky ceiling must be on the 16-unit grid, at least 256 above the terrain, and the enclosure must fit the world bounds.".into());
        }
        Ok([
            (
                [x - thickness, self.bottom, z - thickness],
                [x, ceiling, z1 + thickness],
            ),
            (
                [x1, self.bottom, z - thickness],
                [x1 + thickness, ceiling, z1 + thickness],
            ),
            ([x, self.bottom, z - thickness], [x1, ceiling, z]),
            ([x, self.bottom, z1], [x1, ceiling, z1 + thickness]),
            (
                [x - thickness, ceiling, z - thickness],
                [x1 + thickness, ceiling + thickness, z1 + thickness],
            ),
        ]
        .into_iter()
        .map(|(min, max)| {
            let mut b = Brush::cuboid(min, max);
            for f in &mut b.faces {
                f.material = Some(material);
            }
            b
        })
        .collect())
    }

    pub fn generate(
        cells: [usize; 2],
        spacing: [i32; 2],
        origin: [i32; 3],
        amplitude: i32,
        seed: u32,
        shape: TerrainShape,
        roughness: f64,
    ) -> Result<Self, String> {
        if cells.iter().any(|&n| n == 0 || n > MAX_CELLS) {
            return Err("Use 1 to 16 cells per axis.".into());
        }
        if spacing
            .iter()
            .any(|&n| n < HEIGHT_STEP || n % HEIGHT_STEP != 0)
        {
            return Err("Cell size must be a positive multiple of 16 units.".into());
        }
        if !(0..=8192).contains(&amplitude)
            || !roughness.is_finite()
            || origin
                .iter()
                .any(|v| i64::from(*v).abs() > crate::brush::BRUSH_EDIT_EXTENT_LIMIT as i64)
        {
            return Err("Invalid terrain height or position.".into());
        }
        let mut terrain = Self {
            cells,
            spacing,
            origin: [origin[0], origin[2]],
            bottom: origin[1] - 256,
            heights: vec![],
        };
        let roughness = roughness.clamp(0.0, 1.0);
        for z in 0..=cells[1] {
            for x in 0..=cells[0] {
                let u = x as f64 / cells[0] as f64;
                let v = z as f64 / cells[1] as f64;
                let n = (noise(u * 3.0, v * 3.0, seed)
                    + roughness * noise(u * 7.0, v * 7.0, seed.wrapping_add(79)))
                    / (1.0 + roughness);
                let h = match shape {
                    TerrainShape::Flat => 0.0,
                    TerrainShape::Hills => n,
                    TerrainShape::Ridge => {
                        (1.0 - (2.0 * n - 1.0).abs()) * (1.0 - (2.0 * v - 1.0).abs() * 0.8)
                    }
                    TerrainShape::Island => {
                        let edge = (2.0 * u - 1.0).abs().max((2.0 * v - 1.0).abs());
                        (1.0 - edge * edge).max(0.0) * (0.35 + n * 0.65)
                    }
                };
                terrain
                    .heights
                    .push(f64::from(origin[1]) + h * f64::from(amplitude));
            }
        }
        terrain.validate()?;
        Ok(terrain)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.cells.iter().any(|&n| n == 0 || n > MAX_CELLS)
            || self.heights.len() != (self.cells[0] + 1) * (self.cells[1] + 1)
        {
            return Err("Invalid terrain grid.".into());
        }
        if self
            .spacing
            .iter()
            .any(|&n| n < HEIGHT_STEP || n % HEIGHT_STEP != 0)
        {
            return Err("Cell size must be a positive multiple of 16 units.".into());
        }
        let limit = crate::brush::BRUSH_EDIT_EXTENT_LIMIT as i64;
        for i in 0..2 {
            let end = i64::from(self.origin[i]) + self.cells[i] as i64 * i64::from(self.spacing[i]);
            if i64::from(self.origin[i]).abs() > limit || end.abs() > limit {
                return Err("Terrain extends beyond the editor world bounds.".into());
            }
        }
        if i64::from(self.bottom).abs() > limit
            || self.heights.iter().any(|&h| {
                !h.is_finite() || h.abs() > limit as f64 || h < f64::from(self.bottom) + 16.0
            })
        {
            return Err(
                "Terrain must stay above its solid base and inside the world bounds.".into(),
            );
        }
        Ok(())
    }
    pub fn vertex(&self, x: usize, z: usize) -> [i32; 3] {
        [
            self.origin[0] + x as i32 * self.spacing[0],
            snap(self.heights[z * (self.cells[0] + 1) + x]),
            self.origin[1] + z as i32 * self.spacing[1],
        ]
    }
    pub fn triangles(&self) -> Vec<[[i32; 3]; 3]> {
        let mut out = Vec::with_capacity(self.cells[0] * self.cells[1] * 2);
        for z in 0..self.cells[1] {
            for x in 0..self.cells[0] {
                let a = self.vertex(x, z);
                let b = self.vertex(x + 1, z);
                let c = self.vertex(x + 1, z + 1);
                let d = self.vertex(x, z + 1);
                if (x + z) % 2 == 0 {
                    out.extend([[a, c, b], [a, d, c]]);
                } else {
                    out.extend([[a, d, b], [b, d, c]]);
                }
            }
        }
        out
    }
    /// Dab in grid coordinates. Amount is units for raise/lower, a 0..1
    /// blend for smooth/flatten. Smoothing reads a snapshot, not earlier edits.
    pub fn sculpt(
        &mut self,
        center: [f64; 2],
        radius: f64,
        amount: f64,
        mode: SculptMode,
        flatten_y: f64,
    ) {
        if !radius.is_finite()
            || radius <= 0.0
            || !amount.is_finite()
            || amount <= 0.0
            || center.iter().any(|v| !v.is_finite())
            || !flatten_y.is_finite()
        {
            return;
        }
        let old = self.heights.clone();
        let width = self.cells[0] + 1;
        for z in 0..=self.cells[1] {
            for x in 0..=self.cells[0] {
                let d = ((x as f64 - center[0]).powi(2) + (z as f64 - center[1]).powi(2)).sqrt()
                    / radius;
                if d >= 1.0 {
                    continue;
                }
                let weight = (1.0 - d * d).powi(2);
                let i = z * width + x;
                let h = match mode {
                    SculptMode::Raise => old[i] + amount * weight,
                    SculptMode::Lower => old[i] - amount * weight,
                    SculptMode::Flatten => {
                        old[i] + (flatten_y - old[i]) * (amount * weight).min(1.0)
                    }
                    SculptMode::Smooth => {
                        let mut sum = 0.0;
                        let mut count = 0;
                        for nz in z.saturating_sub(1)..=(z + 1).min(self.cells[1]) {
                            for nx in x.saturating_sub(1)..=(x + 1).min(self.cells[0]) {
                                sum += old[nz * width + nx];
                                count += 1;
                            }
                        }
                        old[i] + (sum / count as f64 - old[i]) * (amount * weight).min(1.0)
                    }
                };
                self.heights[i] = h.clamp(
                    f64::from(self.bottom) + 16.0,
                    crate::brush::BRUSH_EDIT_EXTENT_LIMIT,
                );
            }
        }
    }
    pub fn brushes(&self, material: Option<ResourceId>) -> Result<Vec<Brush>, String> {
        self.validate()?;
        Ok(self
            .triangles()
            .into_iter()
            .map(|top| wedge(top, self.bottom, material))
            .collect())
    }
    /// Reopen saved, duplicated or translated ordinary brushes without
    /// external metadata. Reject destructive edits instead of discarding them.
    pub fn from_brushes(brushes: &[Brush]) -> Result<Self, String> {
        let invalid = || {
            "Select a complete terrain group. Cut or rotated patches can still be edited with the ordinary brush tools.".to_string()
        };
        if brushes.is_empty() || brushes.len() > MAX_CELLS * MAX_CELLS * 2 {
            return Err(invalid());
        }
        let mut points = BTreeMap::new();
        let mut xs = BTreeSet::new();
        let mut zs = BTreeSet::new();
        let mut tops = Vec::new();
        let mut bottom = None;
        for brush in brushes {
            if brush.contents != BrushContents::Solid
                || brush.mover.is_some()
                || brush.faces.len() != 5
            {
                return Err(invalid());
            }
            let top = brush
                .faces
                .iter()
                .find(|f| Plane::from_points(f.points).is_some_and(|p| p.normal[1] > 0))
                .ok_or_else(invalid)?
                .points;
            let base = brush
                .faces
                .iter()
                .find(|f| {
                    Plane::from_points(f.points)
                        .is_some_and(|p| p.normal[1] < 0 && p.normal[0] == 0 && p.normal[2] == 0)
                })
                .ok_or_else(invalid)?
                .points[0][1];
            if bottom.is_some_and(|b| b != base) {
                return Err(invalid());
            }
            bottom = Some(base);
            let expected = wedge(top, base, None);
            if !expected
                .faces
                .iter()
                .all(|e| brush.faces.iter().any(|f| same_plane(f.points, e.points)))
            {
                return Err(invalid());
            }
            for [x, y, z] in top {
                if points.insert((x, z), y).is_some_and(|old| old != y) {
                    return Err(invalid());
                }
                xs.insert(x);
                zs.insert(z);
            }
            tops.push(triangle_key(top));
        }
        let xs: Vec<_> = xs.into_iter().collect();
        let zs: Vec<_> = zs.into_iter().collect();
        if xs.len() < 2 || zs.len() < 2 {
            return Err(invalid());
        }
        let dx = xs[1] - xs[0];
        let dz = zs[1] - zs[0];
        if xs.windows(2).any(|w| w[1] - w[0] != dx) || zs.windows(2).any(|w| w[1] - w[0] != dz) {
            return Err(invalid());
        }
        let mut heights = vec![];
        for &z in &zs {
            for &x in &xs {
                heights.push(f64::from(*points.get(&(x, z)).ok_or_else(invalid)?));
            }
        }
        let terrain = Self {
            cells: [xs.len() - 1, zs.len() - 1],
            spacing: [dx, dz],
            origin: [xs[0], zs[0]],
            bottom: bottom.unwrap(),
            heights,
        };
        terrain.validate()?;
        let mut expected: Vec<_> = terrain.triangles().into_iter().map(triangle_key).collect();
        expected.sort();
        tops.sort();
        if expected != tops {
            return Err(invalid());
        }
        Ok(terrain)
    }
}
fn snap(h: f64) -> i32 {
    (h / f64::from(HEIGHT_STEP)).round() as i32 * HEIGHT_STEP
}
fn triangle_key(mut t: [[i32; 3]; 3]) -> [[i32; 3]; 3] {
    t.sort();
    t
}
fn same_plane(a: [[i32; 3]; 3], b: [[i32; 3]; 3]) -> bool {
    let (Some(a), Some(b)) = (Plane::from_points(a), Plane::from_points(b)) else {
        return false;
    };
    let axis = (0..3).find(|&i| a.normal[i] != 0).unwrap();
    if a.normal[axis].signum() != b.normal[axis].signum() {
        return false;
    }
    (0..3).all(|i| {
        i128::from(a.normal[i]) * i128::from(b.normal[axis])
            == i128::from(b.normal[i]) * i128::from(a.normal[axis])
    }) && i128::from(a.dist) * i128::from(b.normal[axis])
        == i128::from(b.dist) * i128::from(a.normal[axis])
}
fn wedge(top: [[i32; 3]; 3], bottom: i32, material: Option<ResourceId>) -> Brush {
    let base = top.map(|p| [p[0], bottom, p[2]]);
    let mut faces = vec![
        BrushFace::from_points(top),
        BrushFace::from_points([base[2], base[1], base[0]]),
    ];
    for i in 0..3 {
        let j = (i + 1) % 3;
        faces.push(BrushFace::from_points([top[j], top[i], base[i]]));
    }
    for f in &mut faces {
        f.material = material;
    }
    Brush {
        faces,
        ..Brush::default()
    }
}
fn noise(x: f64, z: f64, seed: u32) -> f64 {
    let ix = x.floor() as u32;
    let iz = z.floor() as u32;
    let hash = |x: u32, z: u32| {
        let mut h = seed ^ x.wrapping_mul(0x9e3779b9) ^ z.wrapping_mul(0x85ebca6b);
        h = (h ^ (h >> 16)).wrapping_mul(0x7feb352d);
        h = (h ^ (h >> 15)).wrapping_mul(0x846ca68b);
        f64::from(h ^ (h >> 16)) / f64::from(u32::MAX)
    };
    let smooth = |t: f64| t * t * (3.0 - 2.0 * t);
    let u = smooth(x.fract());
    let v = smooth(z.fract());
    let a = hash(ix, iz) * (1.0 - u) + hash(ix + 1, iz) * u;
    let b = hash(ix, iz + 1) * (1.0 - u) + hash(ix + 1, iz + 1) * u;
    a * (1.0 - v) + b * v
}
#[cfg(test)]
mod tests {
    use super::*;
    fn patch() -> Terrain {
        Terrain::generate(
            [4, 4],
            [256, 256],
            [-512, 0, -512],
            512,
            17,
            TerrainShape::Hills,
            0.4,
        )
        .unwrap()
    }
    #[test]
    fn seeded_generation_is_repeatable() {
        assert_eq!(patch(), patch());
        assert_ne!(
            patch().heights,
            Terrain::generate(
                [4, 4],
                [256, 256],
                [-512, 0, -512],
                512,
                18,
                TerrainShape::Hills,
                0.4
            )
            .unwrap()
            .heights
        );
    }
    #[test]
    fn wedges_are_valid_and_csg_discards_internal_walls() {
        let p = patch();
        let brushes = p.brushes(None).unwrap();
        assert_eq!(brushes.len(), 32);
        for b in &brushes {
            let s = b.solve();
            assert!(s.is_valid() && s.within_extent(crate::brush::BRUSH_EDIT_EXTENT_LIMIT));
            assert_eq!(s.polygons.iter().flatten().count(), 5);
        }
        let surfaces = crate::brush_compile::compile_csg_surfaces(&brushes);
        for s in surfaces.iter().filter(|s| s.plane.normal[1] == 0) {
            assert!(
                s.vertices.iter().all(|v| v[0] == -512.0)
                    || s.vertices.iter().all(|v| v[0] == 512.0)
                    || s.vertices.iter().all(|v| v[2] == -512.0)
                    || s.vertices.iter().all(|v| v[2] == 512.0),
                "interior wall survived: {:?}",
                s.vertices
            );
        }
        assert_eq!(
            surfaces.iter().filter(|s| s.plane.normal[1] > 0).count(),
            32
        );
    }
    #[test]
    fn translated_brushes_reopen_and_damaged_patch_is_rejected() {
        let mut brushes = patch().brushes(None).unwrap();
        for b in &mut brushes {
            b.translate([256, 128, -256]);
        }
        let recovered = Terrain::from_brushes(&brushes).unwrap();
        assert_eq!(recovered.origin, [-256, -768]);
        assert_eq!(recovered.bottom, -128);
        assert_eq!(recovered.brushes(None).unwrap(), brushes);
        brushes.pop();
        assert!(Terrain::from_brushes(&brushes).is_err());
    }
    #[test]
    fn sculpt_is_local_smooths_and_cannot_punch_through_base() {
        let mut p = patch();
        let before = p.heights.clone();
        p.sculpt([2.0, 2.0], 1.5, 256.0, SculptMode::Raise, 0.0);
        assert_eq!(p.heights[0], before[0]);
        assert_eq!(p.heights[12], before[12] + 256.0);
        let peak = p.heights[12];
        p.sculpt([2.0, 2.0], 1.5, 1.0, SculptMode::Smooth, 0.0);
        assert!(p.heights[12] < peak);
        p.sculpt([2.0, 2.0], 1.5, 1.0, SculptMode::Flatten, 128.0);
        assert_eq!(p.heights[12], 128.0);
        p.sculpt([2.0, 2.0], 1.5, 1e8, SculptMode::Lower, 0.0);
        assert_eq!(p.heights[12], f64::from(p.bottom + 16));
        assert!(p.brushes(None).is_ok());
    }
    #[test]
    fn invalid_recipes_reject() {
        assert!(Terrain::generate(
            [usize::MAX, 4],
            [256; 2],
            [0; 3],
            512,
            1,
            TerrainShape::Hills,
            0.5
        )
        .is_err());
        assert!(
            Terrain::generate([4; 2], [1; 2], [0; 3], 512, 1, TerrainShape::Hills, 0.5).is_err()
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use psx_bsp::collision::{CollisionHull, Trace, TraceScratch};
    use psx_bsp::{ClipNode, Plane as BspPlane, RecordSlice, Vec3I32};
    #[test]
    fn downward_collision_hits_every_rendered_triangle() {
        let t = Terrain::generate(
            [4; 2],
            [256; 2],
            [-512, 0, -512],
            512,
            42,
            TerrainShape::Hills,
            0.4,
        )
        .unwrap();
        let collision = crate::brush_compile::compile_collision(&t.brushes(None).unwrap());
        let hull = CollisionHull::new(
            RecordSlice::<BspPlane>::new(&collision.planes).unwrap(),
            RecordSlice::<ClipNode>::new(&collision.clipnodes).unwrap(),
            collision.head_node,
        )
        .unwrap();
        for tri in t.triangles() {
            let center: [f64; 3] = std::array::from_fn(|axis| {
                tri.iter().map(|p| f64::from(p[axis])).sum::<f64>() / 3.0
            });
            let at = |y: f64| Vec3I32 {
                x: (center[0] * 4096.0) as i32,
                y: (y * 4096.0) as i32,
                z: (center[2] * 4096.0) as i32,
            };
            let mut trace = Trace::default();
            assert!(hull.trace_into(
                &at(1024.0),
                &at(-512.0),
                &mut TraceScratch::new(),
                &mut trace
            ));
            assert!(!trace.start_solid.is_set());
            assert!(trace.fraction < 4096);
            assert!(
                (trace.end.y as f64 / 4096.0 - center[1]).abs() < 2.0,
                "collision differs from rendered triangle"
            );
        }
    }
    #[test]
    fn sky_enclosure_seals_and_rejects_low_ceiling() {
        let mut p = crate::ProjectDocument::new("Sealed terrain");
        let mut m = crate::MaterialResource::opaque(None);
        m.sky_aperture = true;
        let sky = p.add_resource("Sky", crate::ResourceData::Material(m));
        let t = Terrain::generate(
            [4; 2],
            [256; 2],
            [-512, 0, -512],
            512,
            18,
            TerrainShape::Hills,
            0.4,
        )
        .unwrap();
        assert!(t.sky_enclosure(256, sky).is_err());
        assert!(t.sky_enclosure(2049, sky).is_err());
        let mut brushes = t.brushes(None).unwrap();
        let shell = t.sky_enclosure(2048, sky).unwrap();
        assert_eq!(shell.len(), 5);
        assert!(shell
            .iter()
            .all(|b| b.solve().is_valid() && b.faces.iter().all(|f| f.material == Some(sky))));
        brushes.extend(shell);
        let scene = p.active_scene_mut();
        scene.brushes = brushes;
        let spawn = scene.add_node(
            scene.root,
            "Spawn",
            crate::NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        scene.node_mut(spawn).unwrap().transform.translation = [0.0, 1024.0, 0.0];
        assert!(crate::brush_world::diagnose_brush_world_leak(p.clone())
            .unwrap()
            .is_empty());
        p.active_scene_mut().brushes.pop();
        assert!(!crate::brush_world::diagnose_brush_world_leak(p)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn default_patch_cooks_to_runtime_world() {
        let mut p = crate::ProjectDocument::new("Terrain study");
        let t = Terrain::generate(
            [8; 2],
            [512; 2],
            [-2048, 0, -2048],
            768,
            42,
            TerrainShape::Hills,
            0.4,
        )
        .unwrap();
        let root = p.active_scene().root;
        let group = p
            .active_scene_mut()
            .add_node(root, "Terrain", crate::NodeKind::Group);
        let mut brushes = t.brushes(None).unwrap();
        for b in &mut brushes {
            b.group = Some(group);
        }
        p.active_scene_mut().brushes = brushes;
        if let Ok(path) = std::env::var("TERRAIN_REVIEW_PROJECT") {
            p.save_to_path(path).unwrap();
        }
        crate::units::scale_project_to_engine_units(&mut p);
        crate::brush_world::compile_brush_world(
            &p,
            crate::brush_world::BrushWorldCookOptions {
                project_root: std::path::Path::new("."),
                mode: crate::brush_world::BrushWorldCookMode::Draft,
                ambient: [64; 3],
                texture_asset_base: 0,
                collision_hulls: Default::default(),
            },
        )
        .expect("generated terrain must cook through the regular world pipeline");
    }
}
