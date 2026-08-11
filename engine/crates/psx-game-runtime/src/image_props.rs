//! Image-prop (billboard/card) rendering policy, carved out of
//! `editor-playtest`'s `image_props_runtime` module (phase 2 of
//! docs/game-runtime-plan.md). Cooked prop records arrive as psx-level
//! values, the OT depth arrives as a `const N` generic, the GTE
//! projection toggle arrives as a value, and the example's VRAM slot
//! resolver arrives as a closure until its glue fully migrates.

use psx_engine::{
    Angle, CharacterCollisionAabb, CullMode, DepthPolicy, LoadedWorldCameraGte, PrimitiveSink,
    ProjectedVertex, RoomPoint, WorldCamera, WorldRenderPass, WorldSurfaceOptions, WorldVertex,
};
use psx_gpu::{
    material::TextureMaterial,
    prim::{TriTextured, TriTexturedGouraud},
};
use psx_level::{image_prop_flags, AssetId, LevelImagePropRecord, RoomIndex};
use psx_math::int32::mul_q12_i32;

use crate::model_rendering::{model_render_uv_max, sphere_visible_to_camera};
use crate::room_lighting::RuntimeRoomLighting;
use crate::vram::VramSlot;

const IMAGE_PROP_DEPTH_BIAS: i32 = 256;

/// Append one cooker-enclosed AABB for each collidable ImageProp in `room`.
///
/// Bounds are precomputed by the host because static cards may carry pitch,
/// yaw, and roll. Runtime collection is therefore allocation-free and avoids
/// trigonometry on the PS1.
pub fn collect_image_prop_collision_blockers(
    props: &[LevelImagePropRecord],
    room: RoomIndex,
    out: &mut [CharacterCollisionAabb],
) -> usize {
    let mut count = 0usize;
    for prop in props {
        if prop.room != room
            || prop.flags & image_prop_flags::COLLISION_ENABLED == 0
            || count >= out.len()
        {
            continue;
        }
        let min = RoomPoint::new(
            prop.collision_min[0],
            prop.collision_min[1],
            prop.collision_min[2],
        );
        let max = RoomPoint::new(
            prop.collision_max[0],
            prop.collision_max[1],
            prop.collision_max[2],
        );
        if min.x >= max.x || min.y >= max.y || min.z >= max.z {
            continue;
        }
        out[count] = CharacterCollisionAabb::new(min, max);
        count += 1;
    }
    count
}

/// Draw the authored image props of `current_room`: lit, fogged,
/// optionally GTE-projected textured cards. `GTE_PROJECT` is a const
/// parameter so a game that disables the GTE path pays nothing for it
/// (the flag was a compile-time const before the carve).
#[inline]
pub fn draw_image_props<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    props: &[LevelImagePropRecord],
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut prop_texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    let mut projector = None;
    for prop in props {
        if prop.room != current_room {
            continue;
        }
        let origin = WorldVertex::new(prop.x, prop.y, prop.z);
        let verts = image_prop_vertices(
            origin,
            prop.width,
            prop.height,
            prop.pitch,
            prop.yaw,
            prop.roll,
            prop.flags,
            *camera,
        );
        let (center, radius) = image_prop_cull_bounds(verts);
        if !sphere_visible_to_camera(camera, options, center, radius, 96) {
            continue;
        }
        let Some(slot) = prop_texture_slot(prop.texture_asset) else {
            continue;
        };
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
            .with_texture_window(slot.texture_window);
        let u_max = model_render_uv_max(slot.texture_width);
        let v_max = model_render_uv_max(slot.texture_height);
        let uvs = [(0, 0), (u_max, 0), (u_max, v_max), (0, v_max)];
        if GTE_PROJECT {
            let projector = match projector {
                Some(projector) => projector,
                None => {
                    let loaded = LoadedWorldCameraGte::load(*camera);
                    projector = Some(loaded);
                    loaded
                }
            };
            if let Some(projected) = projector.project_world_quad(verts) {
                let colors = [
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[0],
                        lighting.fog_weight_at_depth(projected[0].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[1],
                        lighting.fog_weight_at_depth(projected[1].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[2],
                        lighting.fog_weight_at_depth(projected[2].sz),
                    ),
                    lighting.apply_vertex_fog_weight(
                        prop.baked_vertex_rgb[3],
                        lighting.fog_weight_at_depth(projected[3].sz),
                    ),
                ];
                let sort_depth =
                    image_prop_sort_depth_projected(projected, camera.projection.near_z);
                let depth_bias = options
                    .depth_bias
                    .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
                let opts = options
                    .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                    .with_depth_bias(depth_bias)
                    .with_cull_mode(CullMode::None)
                    .with_material_layer(material)
                    .with_textured_triangle_splitting(true)
                    .with_textured_triangle_max_edge(0);
                let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                    triangles,
                    [projected[0], projected[1], projected[2]],
                    [uvs[0], uvs[1], uvs[2]],
                    [colors[0], colors[1], colors[2]],
                    material,
                    opts,
                );
                let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                    triangles,
                    [projected[0], projected[2], projected[3]],
                    [uvs[0], uvs[2], uvs[3]],
                    [colors[0], colors[2], colors[3]],
                    material,
                    opts,
                );
                continue;
            }
        }
        let colors = [
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[0], verts[0]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[1], verts[1]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[2], verts[2]),
            lighting.apply_vertex_fog(prop.baked_vertex_rgb[3], verts[3]),
        ];
        if let Some(projected) = camera.project_world_quad(verts) {
            let sort_depth = image_prop_sort_depth_projected(projected, camera.projection.near_z);
            let depth_bias = options
                .depth_bias
                .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
            let opts = options
                .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                .with_depth_bias(depth_bias)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);
            let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                triangles,
                [projected[0], projected[1], projected[2]],
                [uvs[0], uvs[1], uvs[2]],
                [colors[0], colors[1], colors[2]],
                material,
                opts,
            );
            let _ = world.submit_textured_gouraud_triangle_prescreened_u8(
                triangles,
                [projected[0], projected[2], projected[3]],
                [uvs[0], uvs[2], uvs[3]],
                [colors[0], colors[2], colors[3]],
                material,
                opts,
            );
        } else {
            let tint = average_vertex_rgb(colors);
            let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, tint)
                .with_texture_window(slot.texture_window);
            let sort_depth = image_prop_sort_depth(camera, verts);
            let depth_bias = options
                .depth_bias
                .saturating_sub(image_prop_depth_bias(prop.width, prop.height));
            let opts = options
                .with_depth_policy(DepthPolicy::Fixed(sort_depth))
                .with_depth_bias(depth_bias)
                .with_cull_mode(CullMode::None)
                .with_material_layer(material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);
            let _ =
                world.submit_textured_world_quad(triangles, *camera, verts, uvs, material, opts);
        }
    }
}

fn image_prop_depth_bias(width: u16, height: u16) -> i32 {
    IMAGE_PROP_DEPTH_BIAS.saturating_add((width.max(height) as i32) >> 1)
}

fn image_prop_cull_bounds(verts: [WorldVertex; 4]) -> (WorldVertex, i32) {
    let center = WorldVertex::new(
        average4_i32(verts[0].x, verts[1].x, verts[2].x, verts[3].x),
        average4_i32(verts[0].y, verts[1].y, verts[2].y, verts[3].y),
        average4_i32(verts[0].z, verts[1].z, verts[2].z, verts[3].z),
    );
    let mut radius = 32;
    for vertex in verts {
        let dx = abs_delta_i32(vertex.x, center.x);
        let dy = abs_delta_i32(vertex.y, center.y);
        let dz = abs_delta_i32(vertex.z, center.z);
        radius = radius.max(dx.saturating_add(dy).saturating_add(dz));
    }
    (center, radius)
}

/// Saturating average of four i32s (prop centres, quad centres).
#[inline(always)]
pub fn average4_i32(a: i32, b: i32, c: i32, d: i32) -> i32 {
    a.saturating_add(b).saturating_add(c).saturating_add(d) / 4
}

/// Saturating absolute difference of two i32s.
#[inline(always)]
pub fn abs_delta_i32(a: i32, b: i32) -> i32 {
    if a >= b {
        a.saturating_sub(b)
    } else {
        b.saturating_sub(a)
    }
}

/// Average four vertex colours into one flat tint.
#[inline]
pub fn average_vertex_rgb(colors: [(u8, u8, u8); 4]) -> (u8, u8, u8) {
    let mut r = 0u16;
    let mut g = 0u16;
    let mut b = 0u16;
    for color in colors {
        r += color.0 as u16;
        g += color.1 as u16;
        b += color.2 as u16;
    }
    ((r / 4) as u8, (g / 4) as u8, (b / 4) as u8)
}

fn image_prop_sort_depth(camera: &WorldCamera, verts: [WorldVertex; 4]) -> i32 {
    let mut nearest = i32::MAX;
    for vertex in verts {
        nearest = nearest.min(camera.view_vertex(vertex).z);
    }
    nearest.max(camera.projection.near_z)
}

fn image_prop_sort_depth_projected(verts: [ProjectedVertex; 4], near_z: i32) -> i32 {
    let mut nearest = i32::MAX;
    for vertex in verts {
        nearest = nearest.min(vertex.sz);
    }
    nearest.max(near_z)
}

fn image_prop_vertices(
    origin: WorldVertex,
    width: u16,
    height: u16,
    pitch: i16,
    yaw: i16,
    roll: i16,
    flags: u16,
    camera: WorldCamera,
) -> [WorldVertex; 4] {
    if flags & image_prop_flags::CYLINDRICAL_BILLBOARD != 0 {
        let half_width = (width as i32) >> 1;
        let right_x = mul_q12_i32(half_width, camera.cos_yaw.raw());
        let right_z = -mul_q12_i32(half_width, camera.sin_yaw.raw());
        let top_y = origin.y.saturating_add(height as i32);
        return [
            WorldVertex::new(origin.x - right_x, top_y, origin.z - right_z),
            WorldVertex::new(origin.x + right_x, top_y, origin.z + right_z),
            WorldVertex::new(origin.x + right_x, origin.y, origin.z + right_z),
            WorldVertex::new(origin.x - right_x, origin.y, origin.z - right_z),
        ];
    }

    let half_width = (width as i32) >> 1;
    let h = height as i32;
    let locals = [
        [-half_width, h, 0],
        [half_width, h, 0],
        [half_width, 0, 0],
        [-half_width, 0, 0],
    ];
    let mut out = [WorldVertex::new(0, 0, 0); 4];
    let mut i = 0usize;
    while i < locals.len() {
        let rotated = rotate_z_q12(
            rotate_y_q12(rotate_x_q12(locals[i], pitch as u16), yaw as u16),
            roll as u16,
        );
        out[i] = WorldVertex::new(
            origin.x.saturating_add(rotated[0]),
            origin.y.saturating_add(rotated[1]),
            origin.z.saturating_add(rotated[2]),
        );
        i += 1;
    }
    out
}

/// Rotate `v` around the X axis by a Q12 angle.
#[inline]
pub fn rotate_x_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        v[0],
        mul_q12_i32(v[1], c) - mul_q12_i32(v[2], s),
        mul_q12_i32(v[1], s) + mul_q12_i32(v[2], c),
    ]
}

/// Rotate `v` around the Y axis by a Q12 angle.
#[inline]
pub fn rotate_y_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        mul_q12_i32(v[0], c) + mul_q12_i32(v[2], s),
        v[1],
        -mul_q12_i32(v[0], s) + mul_q12_i32(v[2], c),
    ]
}

/// Rotate `v` around the Z axis by a Q12 angle.
#[inline]
pub fn rotate_z_q12(v: [i32; 3], angle_q12: u16) -> [i32; 3] {
    let angle = Angle::from_q12(angle_q12);
    let s = angle.sin().raw();
    let c = angle.cos().raw();
    [
        mul_q12_i32(v[0], c) - mul_q12_i32(v[1], s),
        mul_q12_i32(v[0], s) + mul_q12_i32(v[1], c),
        v[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(room: u16, min: [i32; 3], max: [i32; 3], flags: u16) -> LevelImagePropRecord {
        LevelImagePropRecord {
            room: RoomIndex(room),
            texture_asset: AssetId(0),
            x: 0,
            y: 0,
            z: 0,
            pitch: 0,
            yaw: 0,
            roll: 0,
            width: 1,
            height: 1,
            tint_rgb: [0x80; 3],
            baked_vertex_rgb: [(0x80, 0x80, 0x80); 4],
            collision_min: min,
            collision_max: max,
            flags,
        }
    }

    #[test]
    fn image_prop_collision_filters_stably_and_honours_output_capacity() {
        let enabled = image_prop_flags::COLLISION_ENABLED;
        let props = [
            prop(2, [1, 2, 3], [4, 5, 6], enabled),
            prop(1, [10, 20, 30], [40, 50, 60], 0),
            prop(1, [100, 200, 300], [400, 500, 600], enabled),
            prop(1, [7, 8, 9], [7, 10, 11], enabled),
            prop(1, [-40, -50, -60], [-10, -20, -30], enabled),
        ];
        let mut one = [CharacterCollisionAabb::EMPTY; 1];
        assert_eq!(
            collect_image_prop_collision_blockers(&props, RoomIndex(1), &mut one),
            1
        );
        assert_eq!(one[0].min, RoomPoint::new(100, 200, 300));
        assert_eq!(one[0].max, RoomPoint::new(400, 500, 600));

        let mut two = [CharacterCollisionAabb::EMPTY; 2];
        assert_eq!(
            collect_image_prop_collision_blockers(&props, RoomIndex(1), &mut two),
            2
        );
        assert_eq!(two[0], one[0]);
        assert_eq!(two[1].min, RoomPoint::new(-40, -50, -60));
        assert_eq!(two[1].max, RoomPoint::new(-10, -20, -30));
    }
}
