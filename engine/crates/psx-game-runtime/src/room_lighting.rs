//! Room lighting policy, carved out of `editor-playtest`'s
//! `room_lighting_runtime` module (phase 2 of docs/game-runtime-plan.md).
//! [`RuntimeRoomLighting`] is the per-room shading view (ambient + cooked
//! point lights + authored fog) the world render pass consumes; the cooked
//! `LIGHTS` table arrives as `&'static` psx-level records.

use psx_engine::{
    MaterialTint, PointLightSample, Rgb8, RoomPoint, WorldCamera, WorldRenderMaterial,
    WorldSurfaceLighting, WorldSurfaceSample, WorldVertex, Q8,
};
use psx_gpu::material::TextureMaterial;
use psx_level::{PointLightRecord, RoomIndex};

/// Per-room shading view: the room's ambient colour, its cooked point
/// lights, the render camera (for fog depth), and the authored fog
/// band. Built per drawn room each frame; `Copy` by design.
#[derive(Copy, Clone)]
pub struct RuntimeRoomLighting {
    /// Room this lighting view shades (scopes the point-light query).
    pub room_index: RoomIndex,
    /// Room ambient colour.
    pub ambient: Rgb8,
    /// Render camera the fog depth is measured against.
    pub camera: WorldCamera,
    /// Whether the room's authored fog is enabled.
    pub fog_enabled: bool,
    /// Fog colour tints converge to.
    pub fog_rgb: Rgb8,
    /// View depth where fog starts.
    pub fog_near: i32,
    /// View depth where fog saturates.
    pub fog_far: i32,
    /// Cooked point-light slice for this room only.
    pub lights: &'static [PointLightRecord],
}

impl RuntimeRoomLighting {
    /// Shade a model material's tint at `point` (lights + fog).
    #[inline]
    pub fn shade_model_material(
        &self,
        point: WorldVertex,
        material: TextureMaterial,
    ) -> TextureMaterial {
        material.with_tint(self.shade_tint_at(point, material.tint()))
    }

    /// Shade one rigid model using its current world-space bounding sphere.
    ///
    /// Model rendering uses one PSX material tint for the complete mesh. Treat
    /// the receiver as a sphere rather than a dimensionless point so a light
    /// begins affecting a visible limb or edge when it enters the light volume,
    /// instead of waiting for the actor's root to cross the cutoff.
    #[inline]
    pub fn shade_model_material_sphere(
        &self,
        center: WorldVertex,
        radius: i32,
        material: TextureMaterial,
    ) -> TextureMaterial {
        material.with_tint(self.shade_tint_at_sphere(center, radius, material.tint()))
    }

    /// Shade `base` at `point` with ambient + point lights, then fog by
    /// the camera-view depth of `point`.
    #[inline]
    pub fn shade_tint_at(&self, point: RoomPoint, base: (u8, u8, u8)) -> (u8, u8, u8) {
        let tint = psx_engine::shade_material_tint_with_lights(
            MaterialTint::from_tuple(base),
            point.to_array(),
            self.ambient,
            self.point_lights(),
        )
        .to_tuple();
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return tint;
        }
        let depth = self.camera.view_vertex(point).z;
        self.apply_fog_at_depth(tint, depth)
    }

    /// Shade a rigid receiver represented by a world-space bounding sphere.
    #[inline]
    pub fn shade_tint_at_sphere(
        &self,
        center: RoomPoint,
        radius: i32,
        base: (u8, u8, u8),
    ) -> (u8, u8, u8) {
        let tint = psx_engine::shade_material_tint_with_lights(
            MaterialTint::from_tuple(base),
            center.to_array(),
            self.ambient,
            self.point_lights_for_receiver_radius(radius),
        )
        .to_tuple();
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return tint;
        }
        let depth = self.camera.view_vertex(center).z;
        self.apply_fog_at_depth(tint, depth)
    }

    /// Shade `base` at `point` with a caller-precomputed fog weight.
    pub fn shade_tint_at_depth(
        &self,
        point: RoomPoint,
        base: (u8, u8, u8),
        fog_weight: i32,
    ) -> (u8, u8, u8) {
        let tint = psx_engine::shade_material_tint_with_lights(
            MaterialTint::from_tuple(base),
            point.to_array(),
            self.ambient,
            self.point_lights(),
        )
        .to_tuple();
        self.apply_fog_weight(tint, fog_weight)
    }

    /// Fog `tint` by the weight at view depth `depth`.
    #[inline]
    pub fn apply_fog_at_depth(&self, tint: (u8, u8, u8), depth: i32) -> (u8, u8, u8) {
        self.apply_fog_weight(tint, self.fog_weight_at_depth(depth))
    }

    /// Fog `tint` by a precomputed `weight` (0..=256).
    #[inline]
    pub fn apply_fog_weight(&self, tint: (u8, u8, u8), weight: i32) -> (u8, u8, u8) {
        apply_room_fog_weight(tint, self.fog_rgb, weight)
    }

    /// Fog weight (0..=256) at view depth `depth`.
    #[inline]
    pub fn fog_weight_at_depth(&self, depth: i32) -> i32 {
        room_fog_weight(depth, self.fog_enabled, self.fog_near, self.fog_far)
    }

    #[inline]
    fn point_lights(&self) -> impl Iterator<Item = PointLightSample> + '_ {
        self.point_lights_for_receiver_radius(0)
    }

    #[inline]
    fn point_lights_for_receiver_radius(
        &self,
        receiver_radius: i32,
    ) -> impl Iterator<Item = PointLightSample> + '_ {
        let receiver_radius = receiver_radius.max(0);
        self.lights.iter().map(move |light| {
            debug_assert_eq!(light.room, self.room_index);
            PointLightSample::from_rgb_intensity(
                [light.x, light.y, light.z],
                (light.radius as i32).saturating_add(receiver_radius),
                Rgb8::from_array(light.color),
                Q8::from_raw_u16(light.intensity_q8),
            )
        })
    }

    /// Fog a baked vertex colour by `vertex`'s camera-view depth.
    #[inline]
    pub fn apply_vertex_fog(&self, rgb: (u8, u8, u8), vertex: WorldVertex) -> (u8, u8, u8) {
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return rgb;
        }
        let depth = self.camera.view_vertex(vertex).z;
        self.apply_fog_at_depth(rgb, depth)
    }

    /// Fog a baked vertex colour by a precomputed weight.
    #[inline]
    pub fn apply_vertex_fog_weight(&self, rgb: (u8, u8, u8), weight: i32) -> (u8, u8, u8) {
        self.apply_fog_weight(rgb, weight)
    }
}

/// Return the contiguous cooked light slice for `room`.
///
/// The cooker sorts the global table by room once. Two binary partition
/// searches replace the former complete-table filter in every shade call.
#[inline]
pub fn room_light_slice(
    lights: &'static [PointLightRecord],
    room: RoomIndex,
) -> &'static [PointLightRecord] {
    let first = lights.partition_point(|light| light.room < room);
    let count = lights[first..].partition_point(|light| light.room == room);
    &lights[first..first + count]
}

impl WorldSurfaceLighting for RuntimeRoomLighting {
    #[inline]
    fn shade(
        &self,
        sample: WorldSurfaceSample,
        material: WorldRenderMaterial,
    ) -> WorldRenderMaterial {
        material.with_tint(self.shade_tint_at(sample.center, material.texture.tint()))
    }

    #[inline]
    fn shade_vertex(
        &self,
        _sample: WorldSurfaceSample,
        vertex: RoomPoint,
        material: WorldRenderMaterial,
    ) -> (u8, u8, u8) {
        self.shade_tint_at(vertex, material.texture.tint())
    }

    #[inline]
    fn shade_vertices(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        if let Some(vertex_rgb) = sample.baked_vertex_rgb {
            if !self.fog_enabled || self.fog_far <= self.fog_near {
                return vertex_rgb;
            }
            return [
                self.apply_vertex_fog(vertex_rgb[0], vertices[0]),
                self.apply_vertex_fog(vertex_rgb[1], vertices[1]),
                self.apply_vertex_fog(vertex_rgb[2], vertices[2]),
                self.apply_vertex_fog(vertex_rgb[3], vertices[3]),
            ];
        }
        [
            self.shade_vertex(sample, vertices[0], material),
            self.shade_vertex(sample, vertices[1], material),
            self.shade_vertex(sample, vertices[2], material),
            self.shade_vertex(sample, vertices[3], material),
        ]
    }

    #[inline]
    fn shade_vertices_with_depths(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        depths: [i32; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        if let Some(vertex_rgb) = sample.baked_vertex_rgb {
            if !self.fog_enabled || self.fog_far <= self.fog_near {
                return vertex_rgb;
            }
            return [
                self.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
                self.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
                self.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
                self.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
            ];
        }
        [
            self.shade_tint_at_depth(vertices[0], material.texture.tint(), depths[0]),
            self.shade_tint_at_depth(vertices[1], material.texture.tint(), depths[1]),
            self.shade_tint_at_depth(vertices[2], material.texture.tint(), depths[2]),
            self.shade_tint_at_depth(vertices[3], material.texture.tint(), depths[3]),
        ]
    }

    #[inline]
    fn shade_cached_baked_vertices(
        &self,
        sample: WorldSurfaceSample,
        depths: Option<[i32; 4]>,
        _material: WorldRenderMaterial,
    ) -> Option<[(u8, u8, u8); 4]> {
        let vertex_rgb = sample.baked_vertex_rgb?;
        if !self.fog_enabled || self.fog_far <= self.fog_near {
            return Some(vertex_rgb);
        }
        let depths = depths?;
        Some([
            self.apply_vertex_fog_weight(vertex_rgb[0], depths[0]),
            self.apply_vertex_fog_weight(vertex_rgb[1], depths[1]),
            self.apply_vertex_fog_weight(vertex_rgb[2], depths[2]),
            self.apply_vertex_fog_weight(vertex_rgb[3], depths[3]),
        ])
    }

    #[inline]
    fn uses_vertex_depths(&self) -> bool {
        self.fog_enabled && self.fog_far > self.fog_near
    }

    #[inline]
    fn uses_direct_baked_vertex_rgb(&self) -> bool {
        !self.fog_enabled || self.fog_far <= self.fog_near
    }

    #[inline]
    fn prepare_vertex_depth(&self, depth: i32) -> i32 {
        self.fog_weight_at_depth(depth)
    }

    #[inline]
    fn needs_surface_sample_center(&self, sample_has_baked_rgb: bool) -> bool {
        !sample_has_baked_rgb
    }
}

#[inline(always)]
fn room_fog_weight(depth: i32, enabled: bool, fog_near: i32, fog_far: i32) -> i32 {
    if !enabled || fog_far <= fog_near || depth <= fog_near {
        return 0;
    }
    (((depth - fog_near).saturating_mul(256)) / (fog_far - fog_near)).clamp(0, 256)
}

#[inline(always)]
fn apply_room_fog_weight(tint: (u8, u8, u8), fog_rgb: Rgb8, weight: i32) -> (u8, u8, u8) {
    if weight <= 0 {
        return tint;
    }
    if weight >= 256 {
        return (fog_rgb.r, fog_rgb.g, fog_rgb.b);
    }
    let keep = 256 - weight;
    (
        blend_channel(tint.0, fog_rgb.r, keep, weight),
        blend_channel(tint.1, fog_rgb.g, keep, weight),
        blend_channel(tint.2, fog_rgb.b, keep, weight),
    )
}

/// Apply the exact room-fog blend when the cooked fog colour is black.
///
/// The cooker selects this only for projects whose every fog-enabled room has
/// `fog_rgb == [0, 0, 0]`. Removing the identically-zero fog product preserves
/// the generic blend's integer arithmetic and endpoint behavior.
#[inline(always)]
pub fn apply_black_room_fog_weight(tint: (u8, u8, u8), weight: i32) -> (u8, u8, u8) {
    if weight <= 0 {
        return tint;
    }
    if weight >= 256 {
        return (0, 0, 0);
    }
    let keep = 256 - weight;
    (
        black_fog_channel(tint.0, keep),
        black_fog_channel(tint.1, keep),
        black_fog_channel(tint.2, keep),
    )
}

#[inline(always)]
fn black_fog_channel(src: u8, keep: i32) -> u8 {
    (((src as i32) * keep) >> 8) as u8
}

#[inline(always)]
fn blend_channel(src: u8, fog: u8, keep: i32, weight: i32) -> u8 {
    (((src as i32) * keep + (fog as i32) * weight) >> 8) as u8
}

/// `[r, g, b]` array to the `(r, g, b)` tuple the GPU material API takes.
pub const fn rgb_tuple(rgb: [u8; 3]) -> (u8, u8, u8) {
    (rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_fog_specialization_matches_generic_channel_exhaustively() {
        for src in 0..=u8::MAX {
            for weight in 0..=256 {
                let specialized = apply_black_room_fog_weight((src, src, src), weight).0;
                let generic =
                    apply_room_fog_weight((src, src, src), Rgb8::from_array([0, 0, 0]), weight).0;
                assert_eq!(specialized, generic, "src={src} weight={weight}");
            }
        }
    }

    const TEST_LIGHTS: &[PointLightRecord] = &[
        PointLightRecord {
            room: RoomIndex(0),
            x: 0,
            y: 0,
            z: 0,
            radius: 64,
            intensity_q8: 256,
            color: [255; 3],
            flags: 0,
        },
        PointLightRecord {
            room: RoomIndex(2),
            x: 1,
            y: 0,
            z: 0,
            radius: 64,
            intensity_q8: 256,
            color: [255; 3],
            flags: 0,
        },
        PointLightRecord {
            room: RoomIndex(2),
            x: 2,
            y: 0,
            z: 0,
            radius: 64,
            intensity_q8: 256,
            color: [255; 3],
            flags: 0,
        },
        PointLightRecord {
            room: RoomIndex(4),
            x: 3,
            y: 0,
            z: 0,
            radius: 64,
            intensity_q8: 256,
            color: [255; 3],
            flags: 0,
        },
    ];

    const RECEIVER_LIGHTS: &[PointLightRecord] = &[PointLightRecord {
        room: RoomIndex(0),
        x: 0,
        y: 0,
        z: 0,
        radius: 100,
        intensity_q8: 256,
        color: [255; 3],
        flags: 0,
    }];

    #[test]
    fn room_light_slice_returns_exact_contiguous_range() {
        assert!(room_light_slice(TEST_LIGHTS, RoomIndex(1)).is_empty());
        let room_two = room_light_slice(TEST_LIGHTS, RoomIndex(2));
        assert_eq!(room_two.len(), 2);
        assert_eq!(room_two[0].x, 1);
        assert_eq!(room_two[1].x, 2);
    }

    #[test]
    fn model_sphere_is_lit_when_its_visible_edge_enters_light_radius() {
        let projection = psx_engine::WorldProjection::new(160, 120, 200, 40);
        let lighting = RuntimeRoomLighting {
            room_index: RoomIndex(0),
            ambient: Rgb8::new(16, 16, 16),
            camera: WorldCamera::orbit_yaw(
                projection,
                WorldVertex::ZERO,
                0,
                200,
                psx_engine::Angle::ZERO,
            ),
            fog_enabled: false,
            fog_rgb: Rgb8::BLACK,
            fog_near: 0,
            fog_far: 0,
            lights: RECEIVER_LIGHTS,
        };
        let center = WorldVertex::new(110, 0, 0);
        let point_tint = lighting.shade_tint_at(center, (128, 128, 128));
        let sphere_tint = lighting.shade_tint_at_sphere(center, 20, (128, 128, 128));

        assert_eq!(point_tint, (16, 16, 16));
        assert!(sphere_tint.0 > point_tint.0, "got {sphere_tint:?}");
        assert_eq!(sphere_tint.0, sphere_tint.1);
        assert_eq!(sphere_tint.1, sphere_tint.2);
    }
}
