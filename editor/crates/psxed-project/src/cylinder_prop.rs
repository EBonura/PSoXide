//! Deterministic low-poly CylinderProp geometry shared by the editor preview
//! and playtest cooker.
//!
//! CylinderProp deliberately remains separate from BoxProp. Its authored
//! representation is a compact radial profile (shaft, optional collars, and
//! optional broken ends), while this module expands that profile into a small
//! bounded list of triangles and quads at preview/cook time.

use serde::{Deserialize, Serialize};

use crate::{GridUvTransform, ResourceId, DEFAULT_WORLD_SECTOR_SIZE};

pub const CYLINDER_PROP_MATERIAL_COUNT: usize = 4;
pub const CYLINDER_PROP_MATERIAL_NAMES: [&str; CYLINDER_PROP_MATERIAL_COUNT] =
    ["Sides", "Top", "Bottom", "Fracture"];
pub const CYLINDER_PROP_MIN_SIDES: u8 = 4;
pub const CYLINDER_PROP_MAX_SIDES: u8 = 12;
pub const DEFAULT_CYLINDER_PROP_SIDES: u8 = 6;

pub const CYLINDER_PROP_MATERIAL_SIDE: u8 = 0;
pub const CYLINDER_PROP_MATERIAL_TOP: u8 = 1;
pub const CYLINDER_PROP_MATERIAL_BOTTOM: u8 = 2;
pub const CYLINDER_PROP_MATERIAL_FRACTURE: u8 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CylinderBrokenEnds {
    #[default]
    None,
    Top,
    Bottom,
    Both,
}

impl CylinderBrokenEnds {
    pub const fn has_top(self) -> bool {
        matches!(self, Self::Top | Self::Both)
    }

    pub const fn has_bottom(self) -> bool {
        matches!(self, Self::Bottom | Self::Both)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Top => "Top",
            Self::Bottom => "Bottom",
            Self::Both => "Both",
        }
    }
}

/// Optional widened end profile used for simple column plinths and capitals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CylinderEndBulge {
    #[serde(default)]
    pub enabled: bool,
    /// Radius relative to the adjoining shaft (`100 = unchanged`).
    #[serde(default = "default_bulge_radius_percent")]
    pub radius_percent: u16,
    /// Height occupied by the collar as a percentage of total height.
    #[serde(default = "default_bulge_height_percent")]
    pub height_percent: u8,
}

impl Default for CylinderEndBulge {
    fn default() -> Self {
        Self {
            enabled: false,
            radius_percent: default_bulge_radius_percent(),
            height_percent: default_bulge_height_percent(),
        }
    }
}

const fn default_bulge_radius_percent() -> u16 {
    125
}

const fn default_bulge_height_percent() -> u8 {
    12
}

/// Compact authoring recipe for one low-poly CylinderProp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CylinderPropGeometry {
    /// Elliptical X/Z radii in engine units.
    #[serde(default = "default_cylinder_prop_radius")]
    pub radius: [u16; 2],
    /// Bottom-to-top height in engine units.
    #[serde(default = "default_cylinder_prop_height")]
    pub height: u16,
    /// Radial segment count. Clamped to `4..=12` by the generator.
    #[serde(default = "default_cylinder_prop_sides")]
    pub sides: u8,
    /// Top shaft radius relative to the bottom shaft (`100 = straight`).
    #[serde(default = "default_top_radius_percent")]
    pub top_radius_percent: u16,
    #[serde(default)]
    pub base_bulge: CylinderEndBulge,
    #[serde(default)]
    pub top_bulge: CylinderEndBulge,
    #[serde(default)]
    pub broken_ends: CylinderBrokenEnds,
    /// Maximum terminal-ring height variation as a percentage of total height.
    #[serde(default = "default_fracture_depth_percent")]
    pub fracture_depth_percent: u8,
    /// Radial and vertical irregularity of the fracture (`0..=100`).
    #[serde(default = "default_fracture_roughness")]
    pub fracture_roughness: u8,
    /// Stable authored variation seed.
    #[serde(default = "default_cylinder_seed")]
    pub seed: u32,
}

impl Default for CylinderPropGeometry {
    fn default() -> Self {
        Self {
            radius: default_cylinder_prop_radius(),
            height: default_cylinder_prop_height(),
            sides: default_cylinder_prop_sides(),
            top_radius_percent: default_top_radius_percent(),
            base_bulge: CylinderEndBulge::default(),
            top_bulge: CylinderEndBulge::default(),
            broken_ends: CylinderBrokenEnds::None,
            fracture_depth_percent: default_fracture_depth_percent(),
            fracture_roughness: default_fracture_roughness(),
            seed: default_cylinder_seed(),
        }
    }
}

const fn default_cylinder_prop_radius() -> [u16; 2] {
    let radius = DEFAULT_WORLD_SECTOR_SIZE as u16 / 2;
    [radius, radius]
}

const fn default_cylinder_prop_height() -> u16 {
    DEFAULT_WORLD_SECTOR_SIZE as u16 * 2
}

const fn default_cylinder_prop_sides() -> u8 {
    DEFAULT_CYLINDER_PROP_SIDES
}

const fn default_top_radius_percent() -> u16 {
    100
}

const fn default_fracture_depth_percent() -> u8 {
    18
}

const fn default_fracture_roughness() -> u8 {
    45
}

const fn default_cylinder_seed() -> u32 {
    1
}

pub(crate) const fn default_cylinder_prop_materials(
) -> [Option<ResourceId>; CYLINDER_PROP_MATERIAL_COUNT] {
    [None; CYLINDER_PROP_MATERIAL_COUNT]
}

pub(crate) const fn default_cylinder_prop_uvs() -> [GridUvTransform; CYLINDER_PROP_MATERIAL_COUNT] {
    [GridUvTransform::IDENTITY; CYLINDER_PROP_MATERIAL_COUNT]
}

/// One generated polygon. Triangles repeat their final vertex in slot three so
/// the record stays fixed-size and allocation-free downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCylinderPropSurface {
    pub vertices: [[i16; 3]; 4],
    pub uv_q8: [[u8; 2]; 4],
    pub vertex_count: u8,
    pub material_slot: u8,
}

#[derive(Clone)]
struct Ring {
    vertices: Vec<[i16; 3]>,
}

/// Expand an authored radial profile into bounded, outward-wound surfaces.
pub fn generate_cylinder_prop_surfaces(
    geometry: CylinderPropGeometry,
) -> Vec<GeneratedCylinderPropSurface> {
    let sides = usize::from(
        geometry
            .sides
            .clamp(CYLINDER_PROP_MIN_SIDES, CYLINDER_PROP_MAX_SIDES),
    );
    let height = i32::from(geometry.height.max(1));
    let base_radius = [
        i32::from(geometry.radius[0].max(1)),
        i32::from(geometry.radius[1].max(1)),
    ];
    let top_radius = [
        scale_percent(base_radius[0], geometry.top_radius_percent.clamp(10, 300)),
        scale_percent(base_radius[1], geometry.top_radius_percent.clamp(10, 300)),
    ];

    let base_terminal_y = terminal_heights(
        sides,
        0,
        height,
        geometry.broken_ends.has_bottom(),
        geometry.fracture_depth_percent,
        geometry.fracture_roughness,
        geometry.seed ^ 0xB073_0A11,
        false,
    );
    let top_terminal_y = terminal_heights(
        sides,
        height,
        height,
        geometry.broken_ends.has_top(),
        geometry.fracture_depth_percent,
        geometry.fracture_roughness,
        geometry.seed ^ 0x70F0_5EED,
        true,
    );

    let mut rings = Vec::with_capacity(6);
    let base_end_radius = bulge_radius(base_radius, geometry.base_bulge);
    rings.push(make_ring(
        sides,
        base_end_radius,
        &base_terminal_y,
        geometry,
        geometry.broken_ends.has_bottom(),
        0x194A_711D,
    ));

    if geometry.base_bulge.enabled {
        let y = collar_height(height, geometry.base_bulge.height_percent);
        rings.push(make_flat_ring(sides, base_end_radius, y));
        let transition_y = (y.saturating_mul(2)).min(height / 2).max(y + 1);
        rings.push(make_flat_ring(
            sides,
            interpolated_radius(base_radius, top_radius, transition_y, height),
            transition_y,
        ));
    }

    if geometry.top_bulge.enabled {
        let collar = collar_height(height, geometry.top_bulge.height_percent);
        let transition_y = (height - collar.saturating_mul(2)).max(height / 2);
        if rings
            .last()
            .and_then(|ring| ring.vertices.first())
            .is_none_or(|vertex| i32::from(vertex[1]) < transition_y)
        {
            rings.push(make_flat_ring(
                sides,
                interpolated_radius(base_radius, top_radius, transition_y, height),
                transition_y,
            ));
        }
        rings.push(make_flat_ring(
            sides,
            bulge_radius(top_radius, geometry.top_bulge),
            (height - collar).max(1),
        ));
    }

    let top_end_radius = bulge_radius(top_radius, geometry.top_bulge);
    rings.push(make_ring(
        sides,
        top_end_radius,
        &top_terminal_y,
        geometry,
        geometry.broken_ends.has_top(),
        0xA7E5_011D,
    ));

    deduplicate_adjacent_rings(&mut rings);

    let mut surfaces = Vec::with_capacity((rings.len() - 1) * sides + sides * 2);
    for ring_pair in rings.windows(2) {
        let lower = &ring_pair[0];
        let upper = &ring_pair[1];
        for side in 0..sides {
            let next = (side + 1) % sides;
            let u0 = fraction_u8(side, sides);
            let u1 = fraction_u8(side + 1, sides);
            let v0 = height_uv(i32::from(lower.vertices[side][1]), height);
            let v1 = height_uv(i32::from(upper.vertices[side][1]), height);
            surfaces.push(GeneratedCylinderPropSurface {
                vertices: [
                    upper.vertices[side],
                    upper.vertices[next],
                    lower.vertices[next],
                    lower.vertices[side],
                ],
                uv_q8: [[u0, v1], [u1, v1], [u1, v0], [u0, v0]],
                vertex_count: 4,
                material_slot: CYLINDER_PROP_MATERIAL_SIDE,
            });
        }
    }

    append_cap(
        &mut surfaces,
        &rings[0],
        sides,
        false,
        if geometry.broken_ends.has_bottom() {
            CYLINDER_PROP_MATERIAL_FRACTURE
        } else {
            CYLINDER_PROP_MATERIAL_BOTTOM
        },
        base_end_radius,
    );
    append_cap(
        &mut surfaces,
        rings.last().expect("at least two cylinder rings"),
        sides,
        true,
        if geometry.broken_ends.has_top() {
            CYLINDER_PROP_MATERIAL_FRACTURE
        } else {
            CYLINDER_PROP_MATERIAL_TOP
        },
        top_end_radius,
    );
    surfaces
}

fn make_flat_ring(sides: usize, radius: [i32; 2], y: i32) -> Ring {
    make_ring_from_heights(sides, radius, &vec![y; sides], None)
}

fn make_ring(
    sides: usize,
    radius: [i32; 2],
    heights: &[i32],
    geometry: CylinderPropGeometry,
    fractured: bool,
    salt: u32,
) -> Ring {
    let radial_jitter = if fractured {
        i32::from(geometry.fracture_roughness.min(100))
    } else {
        0
    };
    make_ring_from_heights(
        sides,
        radius,
        heights,
        Some((geometry.seed ^ salt, radial_jitter)),
    )
}

fn make_ring_from_heights(
    sides: usize,
    radius: [i32; 2],
    heights: &[i32],
    jitter: Option<(u32, i32)>,
) -> Ring {
    let mut vertices = Vec::with_capacity(sides);
    // `side` drives the ring angle and the noise seed, not just a lookup.
    #[allow(clippy::needless_range_loop)]
    for side in 0..sides {
        let angle = core::f32::consts::TAU * side as f32 / sides as f32;
        let mut radius_percent = 100;
        if let Some((seed, roughness)) = jitter {
            let noise = signed_noise(seed, side as u32);
            radius_percent += noise * roughness / 600;
        }
        let x =
            ((angle.sin() * scale_percent(radius[0], radius_percent as u16) as f32).round()) as i32;
        let z = ((-angle.cos() * scale_percent(radius[1], radius_percent as u16) as f32).round())
            as i32;
        vertices.push([clamp_i16(x), clamp_i16(heights[side]), clamp_i16(z)]);
    }
    Ring { vertices }
}

fn terminal_heights(
    sides: usize,
    terminal: i32,
    height: i32,
    fractured: bool,
    depth_percent: u8,
    roughness: u8,
    seed: u32,
    top: bool,
) -> Vec<i32> {
    if !fractured {
        return vec![terminal; sides];
    }
    let depth = scale_percent(height, u16::from(depth_percent.clamp(2, 80))).max(1);
    let roughness = i32::from(roughness.min(100));
    (0..sides)
        .map(|side| {
            let noise = unsigned_noise(seed, side as u32);
            let amount = depth / 4 + depth * noise * roughness / (255 * 100);
            if top {
                (terminal - amount).clamp(0, height)
            } else {
                (terminal + amount).clamp(0, height)
            }
        })
        .collect()
}

fn append_cap(
    out: &mut Vec<GeneratedCylinderPropSurface>,
    ring: &Ring,
    sides: usize,
    top: bool,
    material_slot: u8,
    radius: [i32; 2],
) {
    let center_y = ring
        .vertices
        .iter()
        .map(|vertex| i32::from(vertex[1]))
        .sum::<i32>()
        / sides as i32;
    let center = [0, clamp_i16(center_y), 0];
    for side in 0..sides {
        let next = (side + 1) % sides;
        let (a, b) = if top {
            (ring.vertices[next], ring.vertices[side])
        } else {
            (ring.vertices[side], ring.vertices[next])
        };
        let uv_a = cap_uv(a, radius);
        let uv_b = cap_uv(b, radius);
        out.push(GeneratedCylinderPropSurface {
            vertices: [center, a, b, b],
            uv_q8: [[128, 128], uv_a, uv_b, uv_b],
            vertex_count: 3,
            material_slot,
        });
    }
}

fn cap_uv(vertex: [i16; 3], radius: [i32; 2]) -> [u8; 2] {
    let u = 128 + i32::from(vertex[0]) * 127 / radius[0].max(1);
    let v = 128 + i32::from(vertex[2]) * 127 / radius[1].max(1);
    [u.clamp(0, 255) as u8, v.clamp(0, 255) as u8]
}

fn deduplicate_adjacent_rings(rings: &mut Vec<Ring>) {
    rings.dedup_by(|a, b| a.vertices == b.vertices);
    if rings.len() == 1 {
        rings.push(rings[0].clone());
    }
}

fn interpolated_radius(base: [i32; 2], top: [i32; 2], y: i32, height: i32) -> [i32; 2] {
    [
        base[0] + (top[0] - base[0]) * y / height.max(1),
        base[1] + (top[1] - base[1]) * y / height.max(1),
    ]
}

fn bulge_radius(radius: [i32; 2], bulge: CylinderEndBulge) -> [i32; 2] {
    if !bulge.enabled {
        radius
    } else {
        let percent = bulge.radius_percent.clamp(100, 250);
        [
            scale_percent(radius[0], percent),
            scale_percent(radius[1], percent),
        ]
    }
}

fn collar_height(height: i32, percent: u8) -> i32 {
    scale_percent(height, u16::from(percent.clamp(2, 45))).max(1)
}

fn scale_percent(value: i32, percent: u16) -> i32 {
    value.saturating_mul(i32::from(percent)) / 100
}

fn height_uv(y: i32, height: i32) -> u8 {
    (y.clamp(0, height) * 255 / height.max(1)) as u8
}

fn fraction_u8(numerator: usize, denominator: usize) -> u8 {
    ((numerator * 255) / denominator.max(1)).min(255) as u8
}

fn unsigned_noise(seed: u32, index: u32) -> i32 {
    let mut value = seed ^ index.wrapping_mul(0x9E37_79B9);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    (value & 0xFF) as i32
}

fn signed_noise(seed: u32, index: u32) -> i32 {
    unsigned_noise(seed, index) - 128
}

fn clamp_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_column_is_six_sided_and_bounded() {
        let geometry = CylinderPropGeometry::default();
        let surfaces = generate_cylinder_prop_surfaces(geometry);
        assert_eq!(surfaces.len(), 18);
        assert_eq!(
            surfaces
                .iter()
                .filter(|surface| surface.vertex_count == 4)
                .count(),
            6
        );
        assert_eq!(
            surfaces
                .iter()
                .filter(|surface| surface.vertex_count == 3)
                .count(),
            12
        );
        for surface in surfaces {
            for vertex in surface
                .vertices
                .iter()
                .take(usize::from(surface.vertex_count))
            {
                assert!(i32::from(vertex[0]).abs() <= i32::from(geometry.radius[0]));
                assert!(i32::from(vertex[2]).abs() <= i32::from(geometry.radius[1]));
                assert!((0..=i32::from(geometry.height)).contains(&i32::from(vertex[1])));
            }
        }
    }

    #[test]
    fn cap_winding_faces_outward() {
        let surfaces = generate_cylinder_prop_surfaces(CylinderPropGeometry::default());
        let normal_y = |surface: &GeneratedCylinderPropSurface| {
            let a = surface.vertices[0];
            let b = surface.vertices[1];
            let c = surface.vertices[2];
            let ab = [
                i32::from(b[0]) - i32::from(a[0]),
                i32::from(b[1]) - i32::from(a[1]),
                i32::from(b[2]) - i32::from(a[2]),
            ];
            let ac = [
                i32::from(c[0]) - i32::from(a[0]),
                i32::from(c[1]) - i32::from(a[1]),
                i32::from(c[2]) - i32::from(a[2]),
            ];
            ab[2] * ac[0] - ab[0] * ac[2]
        };
        assert!(surfaces
            .iter()
            .filter(|surface| surface.material_slot == CYLINDER_PROP_MATERIAL_TOP)
            .all(|surface| normal_y(surface) > 0));
        assert!(surfaces
            .iter()
            .filter(|surface| surface.material_slot == CYLINDER_PROP_MATERIAL_BOTTOM)
            .all(|surface| normal_y(surface) < 0));
    }

    #[test]
    fn bulges_add_profile_rings_and_expand_radius() {
        let mut geometry = CylinderPropGeometry::default();
        geometry.base_bulge.enabled = true;
        geometry.top_bulge.enabled = true;
        let surfaces = generate_cylinder_prop_surfaces(geometry);
        assert!(surfaces.len() > 18);
        assert!(surfaces
            .iter()
            .flat_map(|surface| surface.vertices)
            .any(|vertex| {
                i32::from(vertex[0]).abs() > i32::from(geometry.radius[0])
                    || i32::from(vertex[2]).abs() > i32::from(geometry.radius[1])
            }));
    }

    #[test]
    fn broken_top_is_seeded_and_uses_fracture_material() {
        let geometry = CylinderPropGeometry {
            broken_ends: CylinderBrokenEnds::Top,
            seed: 42,
            ..Default::default()
        };
        let first = generate_cylinder_prop_surfaces(geometry);
        let second = generate_cylinder_prop_surfaces(geometry);
        assert_eq!(first, second);
        assert!(first.iter().any(|surface| {
            surface.vertex_count == 3 && surface.material_slot == CYLINDER_PROP_MATERIAL_FRACTURE
        }));
        let top_heights: std::collections::BTreeSet<_> = first
            .iter()
            .filter(|surface| surface.material_slot == CYLINDER_PROP_MATERIAL_FRACTURE)
            .flat_map(|surface| {
                surface
                    .vertices
                    .iter()
                    .take(usize::from(surface.vertex_count))
                    .map(|vertex| vertex[1])
            })
            .collect();
        assert!(top_heights.len() > 1);
    }

    #[test]
    fn side_count_is_clamped_to_safe_budget() {
        let mut geometry = CylinderPropGeometry {
            sides: 1,
            ..Default::default()
        };
        assert_eq!(
            generate_cylinder_prop_surfaces(geometry)
                .iter()
                .filter(|surface| surface.vertex_count == 4)
                .count(),
            usize::from(CYLINDER_PROP_MIN_SIDES)
        );
        geometry.sides = 99;
        assert_eq!(
            generate_cylinder_prop_surfaces(geometry)
                .iter()
                .filter(|surface| surface.vertex_count == 4)
                .count(),
            usize::from(CYLINDER_PROP_MAX_SIDES)
        );
    }
}
