//! Deterministic low-poly erosion for authored box props.
//!
//! The eight editable BoxProp vertices remain the authoring cage. This module
//! samples that cage as a coarse boundary lattice, moves boundary samples
//! inward according to six independently configured directional fields, then
//! emits one closed quad mesh. The same function feeds editor preview and the
//! playtest cooker so the two views cannot drift.

use serde::{Deserialize, Serialize};

use crate::{BOX_PROP_FACE_COUNT, BOX_PROP_VERTEX_COUNT};

/// Highest surface subdivision accepted by the generator.
pub const BOX_PROP_EROSION_MAX_DETAIL: u8 = 6;

/// One direction's contribution to the shared box erosion field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoxPropErosionDirection {
    /// Whether this side may push its boundary into the box.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum inward displacement as a percentage of that axis' size.
    #[serde(default = "default_erosion_amount")]
    pub amount: u8,
    /// Percentage of sampled regions affected by erosion.
    #[serde(default = "default_erosion_coverage")]
    pub coverage: u8,
    /// Variation in the depth of affected samples.
    #[serde(default = "default_erosion_roughness")]
    pub roughness: u8,
    /// Number of adjacent lattice samples sharing one random feature.
    #[serde(default = "default_erosion_feature_size")]
    pub feature_size: u8,
    /// Attenuation applied at this face's perimeter, useful for clean joins.
    #[serde(default)]
    pub edge_protection: u8,
}

const fn default_erosion_amount() -> u8 {
    24
}

const fn default_erosion_coverage() -> u8 {
    62
}

const fn default_erosion_roughness() -> u8 {
    72
}

const fn default_erosion_feature_size() -> u8 {
    1
}

impl Default for BoxPropErosionDirection {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: default_erosion_amount(),
            coverage: default_erosion_coverage(),
            roughness: default_erosion_roughness(),
            feature_size: default_erosion_feature_size(),
            edge_protection: 0,
        }
    }
}

/// Directional erosion modifier stored on every BoxProp.
///
/// Direction order deliberately matches `BOX_PROP_FACE_NAMES`:
/// Front (-Z), Right (+X), Back (+Z), Left (-X), Top (+Y), Bottom (-Y).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoxPropErosion {
    /// Stable variation seed. Resizing never changes the sampled pattern.
    #[serde(default = "default_erosion_seed")]
    pub seed: u32,
    /// Subdivisions along the cage's longest axis. Shorter axes receive a
    /// proportional count so long, thin walls do not waste polygons.
    #[serde(default = "default_erosion_detail")]
    pub detail: u8,
    /// Per-cardinal-face erosion controls.
    #[serde(default)]
    pub directions: [BoxPropErosionDirection; BOX_PROP_FACE_COUNT],
}

const fn default_erosion_seed() -> u32 {
    1
}

const fn default_erosion_detail() -> u8 {
    3
}

impl Default for BoxPropErosion {
    fn default() -> Self {
        Self {
            seed: default_erosion_seed(),
            detail: default_erosion_detail(),
            directions: [BoxPropErosionDirection::default(); BOX_PROP_FACE_COUNT],
        }
    }
}

impl BoxPropErosion {
    /// No generated topology is necessary while every direction is disabled.
    pub fn is_enabled(self) -> bool {
        self.directions.iter().any(|direction| direction.enabled)
    }

    /// Disable all erosion without discarding the user's directional values.
    pub fn clear(&mut self) {
        for direction in &mut self.directions {
            direction.enabled = false;
        }
    }

    /// Settings-only template for a ruined wall or parapet.
    pub fn apply_broken_top_template(&mut self) {
        self.clear();
        self.detail = 5;
        self.directions[4] = BoxPropErosionDirection {
            enabled: true,
            amount: 38,
            coverage: 68,
            roughness: 78,
            feature_size: 1,
            edge_protection: 70,
        };
    }

    /// Settings-only template for a coarse faceted rock.
    pub fn apply_boulder_template(&mut self) {
        self.detail = 2;
        for direction in &mut self.directions {
            *direction = BoxPropErosionDirection {
                enabled: true,
                amount: 23,
                coverage: 100,
                roughness: 88,
                feature_size: 1,
                edge_protection: 0,
            };
        }
        // Preserve a useful planted underside while still disturbing it enough
        // to avoid a visibly perfect cube silhouette.
        self.directions[5].amount = 7;
        self.directions[5].roughness = 45;
    }
}

/// One generated closed-surface quad in BoxProp-local coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedBoxPropQuad {
    /// Material direction in `BOX_PROP_FACE_NAMES` order.
    pub source_face: u8,
    /// Local cage-space vertices in outward winding.
    pub vertices: [[i16; 3]; 4],
    /// UV coordinates across the original face in Q0.8 (`0..=255`).
    pub uv_q8: [[u8; 2]; 4],
}

/// Generate a watertight, directionally eroded surface from an authored cage.
/// Returns an empty vector when erosion is disabled; callers should then use
/// the existing six BoxProp faces directly.
pub fn generate_box_prop_erosion_quads(
    vertices: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
    erosion: BoxPropErosion,
) -> Vec<GeneratedBoxPropQuad> {
    if !erosion.is_enabled() {
        return Vec::new();
    }

    let segments = proportional_axis_segments(vertices, erosion.detail);
    let [nx, ny, nz] = segments;
    let mut out = Vec::with_capacity(
        2 * (usize::from(nx) * usize::from(ny)
            + usize::from(nx) * usize::from(nz)
            + usize::from(ny) * usize::from(nz)),
    );

    // Front (-Z) and back (+Z).
    for iy in 0..ny {
        for ix in 0..nx {
            push_quad(
                &mut out,
                0,
                [
                    [ix, iy + 1, 0],
                    [ix + 1, iy + 1, 0],
                    [ix + 1, iy, 0],
                    [ix, iy, 0],
                ],
                segments,
                vertices,
                erosion,
            );
            push_quad(
                &mut out,
                2,
                [
                    [ix + 1, iy + 1, nz],
                    [ix, iy + 1, nz],
                    [ix, iy, nz],
                    [ix + 1, iy, nz],
                ],
                segments,
                vertices,
                erosion,
            );
        }
    }

    // Right (+X) and left (-X).
    for iy in 0..ny {
        for iz in 0..nz {
            push_quad(
                &mut out,
                1,
                [
                    [nx, iy + 1, iz],
                    [nx, iy + 1, iz + 1],
                    [nx, iy, iz + 1],
                    [nx, iy, iz],
                ],
                segments,
                vertices,
                erosion,
            );
            push_quad(
                &mut out,
                3,
                [
                    [0, iy + 1, iz + 1],
                    [0, iy + 1, iz],
                    [0, iy, iz],
                    [0, iy, iz + 1],
                ],
                segments,
                vertices,
                erosion,
            );
        }
    }

    // Top (+Y) and bottom (-Y).
    for iz in 0..nz {
        for ix in 0..nx {
            push_quad(
                &mut out,
                4,
                [
                    [ix, ny, iz + 1],
                    [ix + 1, ny, iz + 1],
                    [ix + 1, ny, iz],
                    [ix, ny, iz],
                ],
                segments,
                vertices,
                erosion,
            );
            push_quad(
                &mut out,
                5,
                [
                    [ix, 0, iz],
                    [ix + 1, 0, iz],
                    [ix + 1, 0, iz + 1],
                    [ix, 0, iz + 1],
                ],
                segments,
                vertices,
                erosion,
            );
        }
    }
    out
}

fn push_quad(
    out: &mut Vec<GeneratedBoxPropQuad>,
    source_face: u8,
    lattice: [[u8; 3]; 4],
    segments: [u8; 3],
    cage: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
    erosion: BoxPropErosion,
) {
    let vertices = lattice.map(|point| eroded_lattice_vertex(point, segments, cage, erosion));
    let uv_q8 = face_uvs(source_face, lattice, segments);
    out.push(GeneratedBoxPropQuad {
        source_face,
        vertices,
        uv_q8,
    });
}

fn proportional_axis_segments(vertices: [[i16; 3]; BOX_PROP_VERTEX_COUNT], detail: u8) -> [u8; 3] {
    let detail = detail.clamp(1, BOX_PROP_EROSION_MAX_DETAIL);
    let mut spans = [1u32; 3];
    for axis in 0..3 {
        let mut min = i32::MAX;
        let mut max = i32::MIN;
        for vertex in vertices {
            min = min.min(i32::from(vertex[axis]));
            max = max.max(i32::from(vertex[axis]));
        }
        spans[axis] = max.saturating_sub(min).max(1) as u32;
    }
    let longest = spans[0].max(spans[1]).max(spans[2]);
    spans.map(|span| {
        let scaled = span
            .saturating_mul(u32::from(detail))
            .saturating_add(longest - 1)
            / longest;
        scaled.clamp(1, u32::from(detail)) as u8
    })
}

fn eroded_lattice_vertex(
    point: [u8; 3],
    segments: [u8; 3],
    cage: [[i16; 3]; BOX_PROP_VERTEX_COUNT],
    erosion: BoxPropErosion,
) -> [i16; 3] {
    const ONE_Q12: i32 = 4096;
    let mut param = [0i32; 3];
    for axis in 0..3 {
        param[axis] = i32::from(point[axis]) * ONE_Q12 / i32::from(segments[axis].max(1));
    }

    // Face order: Front (-Z), Right (+X), Back (+Z), Left (-X),
    // Top (+Y), Bottom (-Y).
    let boundaries = [
        (2usize, false, 0usize),
        (0usize, true, 1usize),
        (2usize, true, 2usize),
        (0usize, false, 3usize),
        (1usize, true, 4usize),
        (1usize, false, 5usize),
    ];
    for (axis, positive, direction_index) in boundaries {
        let at_boundary = if positive {
            point[axis] == segments[axis]
        } else {
            point[axis] == 0
        };
        if !at_boundary {
            continue;
        }
        let direction = erosion.directions[direction_index];
        if !direction.enabled {
            continue;
        }
        let inset = erosion_inset_q12(
            erosion.seed,
            direction_index as u8,
            point,
            segments,
            direction,
        );
        param[axis] = if positive { ONE_Q12 - inset } else { inset };
    }

    trilinear_cage_point(cage, param)
}

fn erosion_inset_q12(
    seed: u32,
    direction_index: u8,
    point: [u8; 3],
    segments: [u8; 3],
    settings: BoxPropErosionDirection,
) -> i32 {
    let feature = settings.feature_size.clamp(1, BOX_PROP_EROSION_MAX_DETAIL);
    let tangents = match direction_index {
        0 | 2 => [0usize, 1usize],
        1 | 3 => [2usize, 1usize],
        _ => [0usize, 2usize],
    };
    let a = point[tangents[0]] / feature;
    let b = point[tangents[1]] / feature;
    let random = hash_u32(
        seed ^ (u32::from(direction_index) + 1).wrapping_mul(0x9E37_79B9),
        u32::from(a),
        u32::from(b),
    );
    let gate = (random & 0xff) as u8;
    let coverage_threshold = u16::from(settings.coverage.min(100)) * 255 / 100;
    if u16::from(gate) > coverage_threshold {
        return 0;
    }

    let roughness = i32::from(settings.roughness.min(100));
    let variation = ((random >> 8) & 0xff) as i32;
    // Roughness 0 is a uniform cut. At 100, depths span roughly 50-100%
    // of the configured amount without producing fragile zero-height slivers.
    let factor_q8 = 256 - roughness * 128 / 100 + variation * roughness * 128 / (255 * 100);
    let mut inset = 4096 * i32::from(settings.amount.min(45)) * factor_q8 / (100 * 256);

    if settings.edge_protection > 0
        && (point[tangents[0]] == 0
            || point[tangents[0]] == segments[tangents[0]]
            || point[tangents[1]] == 0
            || point[tangents[1]] == segments[tangents[1]])
    {
        inset = inset * i32::from(100 - settings.edge_protection.min(100)) / 100;
    }
    inset.clamp(0, 1843) // 45% of the corresponding cage axis.
}

fn hash_u32(seed: u32, a: u32, b: u32) -> u32 {
    let mut value = seed
        .wrapping_add(a.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(b.wrapping_mul(0xC2B2_AE35));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^ (value >> 16)
}

fn trilinear_cage_point(cage: [[i16; 3]; BOX_PROP_VERTEX_COUNT], param_q12: [i32; 3]) -> [i16; 3] {
    let [x, y, z] = param_q12;
    let mut out = [0i16; 3];
    for axis in 0..3 {
        let bottom_front = lerp_q12(cage[0][axis], cage[1][axis], x);
        let bottom_back = lerp_q12(cage[3][axis], cage[2][axis], x);
        let top_front = lerp_q12(cage[4][axis], cage[5][axis], x);
        let top_back = lerp_q12(cage[7][axis], cage[6][axis], x);
        let bottom = lerp_i32_q12(bottom_front, bottom_back, z);
        let top = lerp_i32_q12(top_front, top_back, z);
        out[axis] = lerp_i32_q12(bottom, top, y).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    out
}

fn lerp_q12(a: i16, b: i16, t_q12: i32) -> i32 {
    lerp_i32_q12(i32::from(a), i32::from(b), t_q12)
}

fn lerp_i32_q12(a: i32, b: i32, t_q12: i32) -> i32 {
    a.saturating_add(b.saturating_sub(a).saturating_mul(t_q12.clamp(0, 4096)) / 4096)
}

fn face_uvs(face: u8, lattice: [[u8; 3]; 4], segments: [u8; 3]) -> [[u8; 2]; 4] {
    lattice.map(|point| {
        let (u_axis, v_axis, flip_u, flip_v) = match face {
            0 => (0usize, 1usize, false, true),
            1 => (2usize, 1usize, false, true),
            2 => (0usize, 1usize, true, true),
            3 => (2usize, 1usize, true, true),
            4 => (0usize, 2usize, false, true),
            _ => (0usize, 2usize, false, false),
        };
        let mut u = u16::from(point[u_axis]) * 255 / u16::from(segments[u_axis].max(1));
        let mut v = u16::from(point[v_axis]) * 255 / u16::from(segments[v_axis].max(1));
        if flip_u {
            u = 255 - u;
        }
        if flip_v {
            v = 255 - v;
        }
        [u as u8, v as u8]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::box_prop_vertices_for_size;

    #[test]
    fn disabled_erosion_keeps_legacy_face_path() {
        assert!(generate_box_prop_erosion_quads(
            box_prop_vertices_for_size(1024),
            BoxPropErosion::default()
        )
        .is_empty());
    }

    #[test]
    fn broken_top_is_deterministic_closed_and_stays_inside_cage() {
        let cage = box_prop_vertices_for_size(1024);
        let mut erosion = BoxPropErosion::default();
        erosion.apply_broken_top_template();
        let first = generate_box_prop_erosion_quads(cage, erosion);
        let second = generate_box_prop_erosion_quads(cage, erosion);
        assert_eq!(first, second);
        assert!(!first.is_empty());
        assert!(first.iter().all(|quad| quad.vertices.iter().all(|vertex| {
            (-512..=512).contains(&vertex[0])
                && (0..=1024).contains(&vertex[1])
                && (-512..=512).contains(&vertex[2])
        })));
        assert!(first
            .iter()
            .flat_map(|quad| quad.vertices)
            .any(|vertex| vertex[1] < 1024 && vertex[1] > 0));
    }

    #[test]
    fn boulder_template_erodes_every_direction() {
        let cage = box_prop_vertices_for_size(1024);
        let mut erosion = BoxPropErosion::default();
        erosion.apply_boulder_template();
        let quads = generate_box_prop_erosion_quads(cage, erosion);
        assert_eq!(quads.len(), 24);
        for face in 0..BOX_PROP_FACE_COUNT as u8 {
            assert!(quads.iter().any(|quad| quad.source_face == face));
        }
    }
}
