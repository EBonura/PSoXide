//! Occluded vertex-light baking for compiled brush surfaces.

use crate::brush::{Brush, Plane};
use crate::brush_compile::{normalized_plane, CompiledSurface};
use crate::ResourceId;

const SHADOW_NUDGE: f64 = 0.25;
const SHADOW_EPSILON: f64 = 1.0 / 1024.0;
const LIGHTING_NEUTRAL: f64 = 128.0;

/// One validated world-space point light for the brush bake.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushPointLight {
    pub position: [f64; 3],
    pub radius: f64,
    /// Q8.8 intensity, where 256 is one authored unit.
    pub intensity_q8: u16,
    pub color: [u8; 3],
}

/// Material tint multiplied into the accumulated light at each vertex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushMaterialTint {
    pub material: Option<ResourceId>,
    /// Neutral texture modulation is `[128, 128, 128]`.
    pub color: [u8; 3],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrushLightError {
    InvalidLight(usize),
}

/// Bake packed RGB24 colors matching every surface vertex.
pub fn bake_brush_vertex_lighting(
    surfaces: &[CompiledSurface],
    occluders: &[Brush],
    ambient: [u8; 3],
    lights: &[BrushPointLight],
    material_tints: &[BrushMaterialTint],
) -> Result<Vec<Vec<u32>>, BrushLightError> {
    for (index, light) in lights.iter().enumerate() {
        if !light.position.into_iter().all(f64::is_finite)
            || !light.radius.is_finite()
            || light.radius <= 0.0
        {
            return Err(BrushLightError::InvalidLight(index));
        }
    }
    let brush_planes = brush_occluder_planes(occluders);

    Ok(surfaces
        .iter()
        .map(|surface| {
            let (normal, _) = normalized_plane(surface.plane);
            let tint = material_tints
                .iter()
                .find(|tint| tint.material == surface.material)
                .map_or([128; 3], |tint| tint.color);
            surface
                .vertices
                .iter()
                .copied()
                .map(|vertex| bake_vertex(vertex, normal, tint, ambient, lights, &brush_planes))
                .collect()
        })
        .collect())
}

fn bake_vertex(
    vertex: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[BrushPointLight],
    brush_planes: &[Vec<Plane>],
) -> u32 {
    let mut accumulated = ambient.map(f64::from);
    let shadow_start = add(vertex, scale(normal, SHADOW_NUDGE));
    for light in lights {
        let to_light = subtract(light.position, vertex);
        let distance_squared = dot(to_light, to_light);
        if distance_squared >= light.radius * light.radius {
            continue;
        }
        let distance = distance_squared.sqrt();
        let direction = if distance > SHADOW_EPSILON {
            scale(to_light, distance.recip())
        } else {
            normal
        };
        let lambert = dot(normal, direction).max(0.0);
        if lambert <= 0.0 || segment_occluded(shadow_start, light.position, brush_planes) {
            continue;
        }
        let attenuation = 1.0 - distance / light.radius;
        let weight = attenuation * lambert * f64::from(light.intensity_q8) / 256.0;
        for (channel, color) in accumulated.iter_mut().zip(light.color) {
            *channel += f64::from(color) * weight;
        }
    }
    let color = [
        modulated(tint[0], accumulated[0]),
        modulated(tint[1], accumulated[1]),
        modulated(tint[2], accumulated[2]),
    ];
    u32::from(color[0]) | (u32::from(color[1]) << 8) | (u32::from(color[2]) << 16)
}

/// Face-plane chains of the solid brushes that occlude baked light,
/// exactly as the Release bake builds them.
pub fn brush_occluder_planes(occluders: &[Brush]) -> Vec<Vec<Plane>> {
    occluders
        .iter()
        .filter(|brush| brush.solve().is_valid())
        .map(|brush| {
            brush
                .faces
                .iter()
                .filter_map(|face| Plane::from_points(face.points))
                .collect()
        })
        .collect()
}

/// Single-point evaluation of the vertex bake, shared with the editor
/// preview so the viewport shows exactly what the cook bakes: ambient
/// plus lambert-weighted linear falloff, shadow-tested against
/// `occluders` (pass `&[]` for the Draft look), modulated by the
/// material tint.
pub fn lit_point_color(
    point: [f64; 3],
    normal: [f64; 3],
    tint: [u8; 3],
    ambient: [u8; 3],
    lights: &[BrushPointLight],
    occluders: &[Vec<Plane>],
) -> [u8; 3] {
    let packed = bake_vertex(point, normal, tint, ambient, lights, occluders);
    [
        (packed & 0xff) as u8,
        ((packed >> 8) & 0xff) as u8,
        ((packed >> 16) & 0xff) as u8,
    ]
}

fn modulated(tint: u8, light: f64) -> u8 {
    (f64::from(tint) * light.clamp(0.0, 255.0) / LIGHTING_NEUTRAL)
        .round()
        .clamp(0.0, 255.0) as u8
}

fn segment_occluded(start: [f64; 3], end: [f64; 3], brushes: &[Vec<Plane>]) -> bool {
    let direction = subtract(end, start);
    brushes
        .iter()
        .any(|planes| segment_intersects_brush(start, direction, planes))
}

fn segment_intersects_brush(start: [f64; 3], direction: [f64; 3], planes: &[Plane]) -> bool {
    let mut enter = 0.0f64;
    let mut exit = 1.0f64;
    for plane in planes {
        let (normal, distance) = normalized_plane(*plane);
        let start_distance = dot(normal, start) - distance;
        let denominator = dot(normal, direction);
        if denominator.abs() <= SHADOW_EPSILON {
            if start_distance > SHADOW_EPSILON {
                return false;
            }
            continue;
        }
        let crossing = -start_distance / denominator;
        if denominator < 0.0 {
            enter = enter.max(crossing);
        } else {
            exit = exit.min(crossing);
        }
        if enter > exit {
            return false;
        }
    }
    exit > SHADOW_EPSILON && enter < 1.0 - SHADOW_EPSILON
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(value: [f64; 3], amount: f64) -> [f64; 3] {
    [value[0] * amount, value[1] * amount, value[2] * amount]
}

#[cfg(test)]
mod tests {

    #[test]
    fn subdivided_bake_resolves_a_mid_face_hotspot() {
        use crate::brush_compile::{compile_csg_surfaces, subdivide_surfaces_for_lighting};
        // A big slab with a light hovering over its centre: without
        // subdivision the corners are ~1500 units away and dim; with it,
        // interior vertices near the light bake far brighter, the
        // Quake-style detail this exists for.
        let slab = crate::brush::Brush::cuboid([0, 0, 0], [2048, 64, 2048]);
        let light = BrushPointLight {
            position: [1024.0, 400.0, 1024.0],
            radius: 1200.0,
            intensity_q8: 256,
            color: [255, 255, 255],
        };
        let bake = |surfaces: &[crate::brush_compile::CompiledSurface]| -> u32 {
            let lit =
                bake_brush_vertex_lighting(surfaces, &[], [24; 3], &[light], &[]).expect("bake");
            lit.iter()
                .flatten()
                .map(|packed| packed & 0xff)
                .max()
                .unwrap_or(0)
        };
        let flat = compile_csg_surfaces(std::slice::from_ref(&slab));
        let coarse = bake(&flat);
        let spheres = [(light.position, light.radius)];
        let subdivided = subdivide_surfaces_for_lighting(flat, 1024.0, &spheres);
        let fine = bake(&subdivided);
        assert!(
            fine > coarse + 40,
            "subdivision must expose the hotspot: coarse {coarse}, fine {fine}"
        );
    }
    use super::*;
    use crate::brush::{BrushContents, BrushFace, FaceUv};

    fn upward_quad() -> CompiledSurface {
        CompiledSurface {
            plane: Plane::from_points([[0, 0, 0], [128, 0, 128], [128, 0, 0]]).expect("plane"),
            vertices: vec![
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 128.0],
                [128.0, 0.0, 128.0],
                [128.0, 0.0, 0.0],
            ],
            material: None,
            uv: FaceUv::default(),
            contents: BrushContents::Solid,
            source_brush: 0,
            source_face: 0,
        }
    }

    fn channels(color: u32) -> [u8; 3] {
        [color as u8, (color >> 8) as u8, (color >> 16) as u8]
    }

    #[test]
    fn ambient_and_material_tint_follow_grid_lighting_semantics() {
        let surface = upward_quad();
        let colors = bake_brush_vertex_lighting(
            &[surface],
            &[],
            [128, 64, 32],
            &[],
            &[BrushMaterialTint {
                material: None,
                color: [128, 64, 255],
            }],
        )
        .expect("bake");
        assert_eq!(channels(colors[0][0]), [128, 32, 64]);
    }

    #[test]
    fn point_light_uses_distance_falloff_and_surface_lambert() {
        let surface = upward_quad();
        let colors = bake_brush_vertex_lighting(
            &[surface],
            &[],
            [0; 3],
            &[
                BrushPointLight {
                    position: [0.0, 32.0, 0.0],
                    radius: 256.0,
                    intensity_q8: 256,
                    color: [128; 3],
                },
                BrushPointLight {
                    position: [0.0, -32.0, 0.0],
                    radius: 256.0,
                    intensity_q8: 256,
                    color: [255, 0, 0],
                },
            ],
            &[],
        )
        .expect("bake");
        let near = channels(colors[0][0]);
        let far = channels(colors[0][2]);
        assert!(near[0] > far[0]);
        assert_eq!(near[0], near[1], "light behind the face adds no red");
    }

    #[test]
    fn solid_brush_blocks_point_light_visibility() {
        let surface = upward_quad();
        let light = BrushPointLight {
            position: [64.0, 128.0, 64.0],
            radius: 512.0,
            intensity_q8: 256,
            color: [128; 3],
        };
        let open =
            bake_brush_vertex_lighting(std::slice::from_ref(&surface), &[], [16; 3], &[light], &[])
                .expect("open bake");
        let blocker = Brush::cuboid([0, 48, 0], [128, 80, 128]);
        let shadowed = bake_brush_vertex_lighting(&[surface], &[blocker], [16; 3], &[light], &[])
            .expect("shadow bake");
        assert!(channels(open[0][0])[0] > channels(shadowed[0][0])[0]);
        assert_eq!(channels(shadowed[0][0]), [16; 3]);
    }

    #[test]
    fn invalid_lights_fail_loudly() {
        let error = bake_brush_vertex_lighting(
            &[upward_quad()],
            &[],
            [0; 3],
            &[BrushPointLight {
                position: [0.0; 3],
                radius: 0.0,
                intensity_q8: 256,
                color: [255; 3],
            }],
            &[],
        )
        .expect_err("zero radius");
        assert_eq!(error, BrushLightError::InvalidLight(0));
    }

    #[test]
    fn diagonal_brush_shadow_test_is_deterministic() {
        let mut wedge = Brush::cuboid([0, 0, 0], [128, 128, 128]);
        wedge.faces[5] = BrushFace::from_points([[0, 128, 0], [0, 128, 128], [128, 0, 128]]);
        let args = || {
            bake_brush_vertex_lighting(
                &[upward_quad()],
                &[wedge.clone()],
                [32; 3],
                &[BrushPointLight {
                    position: [64.0, 192.0, 64.0],
                    radius: 512.0,
                    intensity_q8: 256,
                    color: [128; 3],
                }],
                &[],
            )
        };
        assert_eq!(args(), args());
    }
}
