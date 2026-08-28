//! Stateless rendering and collision for cooker-expanded tile-native arches.

use crate::image_props::average_vertex_rgb;
use crate::model_rendering::{model_override_blend_mode, sphere_visible_to_camera};
use crate::room_lighting::RuntimeRoomLighting;
use crate::vram::VramSlot;
use psx_engine::{
    BoundedSink, CharacterCollisionAabb, CullMode, DepthPolicy, LoadedWorldCameraGte,
    PrimitiveSink, RoomPoint, SliceSink, WorldCamera, WorldRenderPass, WorldSurfaceOptions,
    WorldVertex,
};
use psx_gpu::{
    material::TextureMaterial,
    prim::{QuadTexturedGouraud, TriTextured, TriTexturedGouraud},
};
use psx_level::{
    arch_prop_flags, AssetId, LevelArchPropCollisionRecord, LevelArchPropRecord,
    LevelArchPropSurfaceRecord, RoomIndex,
};

#[derive(Clone, Copy)]
struct ArchTextureRuntime {
    asset: AssetId,
    material: TextureMaterial,
}

/// Draw cooked arch quads in `current_room`.
pub fn draw_arch_props<T, const OT_DEPTH: usize>(
    props: &[LevelArchPropRecord],
    surfaces: &[LevelArchPropSurfaceRecord],
    current_room: RoomIndex,
    mut object_visible: impl FnMut(usize) -> bool,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut prop_texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    let mut projector = None;
    let mut texture_cache: Option<ArchTextureRuntime> = None;
    for (index, prop) in props.iter().enumerate() {
        if prop.room != current_room
            || !object_visible(index)
            || !sphere_visible_to_camera(
                camera,
                options,
                WorldVertex::new(prop.center[0], prop.center[1], prop.center[2]),
                prop.cull_radius,
                96,
            )
        {
            continue;
        }
        let loaded = match projector {
            Some(projector) => projector,
            None => {
                let loaded = LoadedWorldCameraGte::load(*camera);
                projector = Some(loaded);
                loaded
            }
        };
        let first = usize::from(prop.surface_first);
        let end = first
            .saturating_add(usize::from(prop.surface_count))
            .min(surfaces.len());
        for surface in surfaces.get(first..end).unwrap_or(&[]) {
            let slot = usize::from(surface.material_slot);
            if slot >= psx_level::ARCH_PROP_MATERIAL_COUNT || !surface_front_facing(camera, surface)
            {
                continue;
            }
            let Some(texture_asset) = prop.texture_assets[slot] else {
                continue;
            };
            let texture = match texture_cache {
                Some(cached) if cached.asset == texture_asset => cached,
                _ => {
                    let Some(vram) = prop_texture_slot(texture_asset) else {
                        continue;
                    };
                    let runtime = ArchTextureRuntime {
                        asset: texture_asset,
                        material: TextureMaterial::opaque(
                            vram.clut_word,
                            vram.tpage_word,
                            (0x80, 0x80, 0x80),
                        )
                        .with_blend_mode(model_override_blend_mode(prop.blend_modes[slot]))
                        .with_texture_window(vram.texture_window),
                    };
                    texture_cache = Some(runtime);
                    runtime
                }
            };
            let vertices = surface.vertices.map(|v| WorldVertex::new(v[0], v[1], v[2]));
            let uvs = surface.uv_q8.map(|uv| arch_prop_uv_at(prop.uvs[slot], uv));
            let opts = options
                .with_depth_policy(DepthPolicy::Average)
                .with_cull_mode(CullMode::None)
                .with_material_layer(texture.material)
                .with_textured_triangle_splitting(true)
                .with_textured_triangle_max_edge(0);
            let Some(projected) = loaded.project_world_quad(vertices) else {
                let material = texture
                    .material
                    .with_tint(average_vertex_rgb(surface.baked_vertex_rgb));
                let _ = world
                    .submit_textured_world_quad(triangles, *camera, vertices, uvs, material, opts);
                continue;
            };
            let colors = [
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[0], projected[0].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[1], projected[1].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[2], projected[2].sz),
                lighting.apply_fog_at_depth(surface.baked_vertex_rgb[3], projected[3].sz),
            ];
            let _ = world.submit_textured_gouraud_quad_prescreened_u8(
                triangles,
                &projected,
                &uvs,
                &colors,
                texture.material,
                opts,
            );
        }
    }
}

/// Append the per-segment AABBs for collidable arches in a room.
pub fn collect_arch_prop_collision_blockers(
    props: &[LevelArchPropRecord],
    collisions: &[LevelArchPropCollisionRecord],
    room: RoomIndex,
    out: &mut [CharacterCollisionAabb],
) -> usize {
    let mut sink = SliceSink::new(out);
    collect_arch_prop_collision_blockers_into(props, collisions, room, &mut sink)
}

/// Append arch blockers to bounded scratch without clearing unused entries.
pub fn collect_arch_prop_collision_blockers_into<S: BoundedSink<CharacterCollisionAabb>>(
    props: &[LevelArchPropRecord],
    collisions: &[LevelArchPropCollisionRecord],
    room: RoomIndex,
    out: &mut S,
) -> usize {
    let mut count = 0;
    for prop in props {
        if prop.room != room || prop.flags & arch_prop_flags::COLLISION_ENABLED == 0 {
            continue;
        }
        let first = usize::from(prop.collision_first);
        let end = first
            .saturating_add(usize::from(prop.collision_count))
            .min(collisions.len());
        for collision in collisions.get(first..end).unwrap_or(&[]) {
            let blocker = CharacterCollisionAabb::new(
                RoomPoint::new(collision.min[0], collision.min[1], collision.min[2]),
                RoomPoint::new(collision.max[0], collision.max[1], collision.max[2]),
            );
            if !out.try_push(blocker) {
                return count;
            }
            count += 1;
        }
    }
    count
}

/// Append every live arch segment, rejecting malformed indices, bounds, and
/// output overflow instead of truncating them.
pub fn collect_arch_prop_collision_blockers_checked(
    props: &[LevelArchPropRecord],
    collisions: &[LevelArchPropCollisionRecord],
    room: RoomIndex,
    out: &mut [CharacterCollisionAabb],
) -> Option<usize> {
    let mut sink = SliceSink::new(out);
    collect_arch_prop_collision_blockers_checked_into(props, collisions, room, &mut sink)
}

/// Checked arch-blocker append for bounded scratch.
pub fn collect_arch_prop_collision_blockers_checked_into<S: BoundedSink<CharacterCollisionAabb>>(
    props: &[LevelArchPropRecord],
    collisions: &[LevelArchPropCollisionRecord],
    room: RoomIndex,
    out: &mut S,
) -> Option<usize> {
    let mut count = 0usize;
    for prop in props {
        if prop.room != room || prop.flags & arch_prop_flags::COLLISION_ENABLED == 0 {
            continue;
        }
        let first = usize::from(prop.collision_first);
        let end = first.checked_add(usize::from(prop.collision_count))?;
        let records = collisions.get(first..end)?;
        for collision in records {
            let blocker = CharacterCollisionAabb::new(
                RoomPoint::new(collision.min[0], collision.min[1], collision.min[2]),
                RoomPoint::new(collision.max[0], collision.max[1], collision.max[2]),
            );
            if !blocker.is_strictly_valid() {
                return None;
            }
            if !out.try_push(blocker) {
                return None;
            }
            count += 1;
        }
    }
    Some(count)
}

fn surface_front_facing(camera: &WorldCamera, surface: &LevelArchPropSurfaceRecord) -> bool {
    let vx = camera.position.x.saturating_sub(surface.center[0]);
    let vy = camera.position.y.saturating_sub(surface.center[1]);
    let vz = camera.position.z.saturating_sub(surface.center[2]);
    surface.normal[0]
        .saturating_mul(vx)
        .saturating_add(surface.normal[1].saturating_mul(vy))
        .saturating_add(surface.normal[2].saturating_mul(vz))
        > 0
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod collision_tests {
    use super::*;

    const fn prop(room: u16, first: u16, count: u8, enabled: bool) -> LevelArchPropRecord {
        LevelArchPropRecord {
            room: RoomIndex(room),
            texture_assets: [None; psx_level::ARCH_PROP_MATERIAL_COUNT],
            blend_modes: [0; psx_level::ARCH_PROP_MATERIAL_COUNT],
            uvs: [[(0, 0); 4]; psx_level::ARCH_PROP_MATERIAL_COUNT],
            surface_first: 0,
            surface_count: 0,
            collision_first: first,
            collision_count: count,
            center: [0; 3],
            cull_radius: 1,
            flags: if enabled {
                arch_prop_flags::COLLISION_ENABLED
            } else {
                0
            },
        }
    }

    #[test]
    fn checked_arch_collection_rejects_bad_ranges_bounds_and_capacity() {
        let collisions = [
            LevelArchPropCollisionRecord {
                min: [10, 0, -4],
                max: [14, 8, 4],
            },
            LevelArchPropCollisionRecord {
                min: [20, 0, -4],
                max: [24, 8, 4],
            },
        ];
        let mut out = [CharacterCollisionAabb::EMPTY; 2];
        assert_eq!(
            collect_arch_prop_collision_blockers_checked(
                &[prop(0, 0, 2, true)],
                &collisions,
                RoomIndex(0),
                &mut out,
            ),
            Some(2)
        );
        assert_eq!(out[1].min, RoomPoint::new(20, 0, -4));
        assert_eq!(
            collect_arch_prop_collision_blockers_checked(
                &[prop(0, 1, 2, true)],
                &collisions,
                RoomIndex(0),
                &mut out,
            ),
            None
        );
        assert_eq!(
            collect_arch_prop_collision_blockers_checked(
                &[prop(0, 0, 2, true)],
                &collisions,
                RoomIndex(0),
                &mut out[..1],
            ),
            None
        );
        let malformed = [LevelArchPropCollisionRecord {
            min: [10, 8, -4],
            max: [14, 0, 4],
        }];
        assert_eq!(
            collect_arch_prop_collision_blockers_checked(
                &[prop(0, 0, 1, true)],
                &malformed,
                RoomIndex(0),
                &mut out,
            ),
            None
        );
        assert_eq!(
            collect_arch_prop_collision_blockers_checked(
                &[prop(0, 9, 7, false), prop(1, 9, 7, true)],
                &[],
                RoomIndex(0),
                &mut out,
            ),
            Some(0),
            "non-collidable and other-room records must not inspect ranges"
        );
    }
}

fn arch_prop_uv_at(corners: [(u8, u8); 4], uv_q8: [u8; 2]) -> (u8, u8) {
    let u = u32::from(uv_q8[0]);
    let v = u32::from(uv_q8[1]);
    let inv_u = 255 - u;
    let inv_v = 255 - v;
    let interpolate = |axis: usize| {
        let values = if axis == 0 {
            [
                u32::from(corners[0].0),
                u32::from(corners[1].0),
                u32::from(corners[2].0),
                u32::from(corners[3].0),
            ]
        } else {
            [
                u32::from(corners[0].1),
                u32::from(corners[1].1),
                u32::from(corners[2].1),
                u32::from(corners[3].1),
            ]
        };
        let top = values[0] * inv_u + values[1] * u;
        let bottom = values[3] * inv_u + values[2] * u;
        ((top * inv_v + bottom * v + 32_512) / 65_025).min(255) as u8
    };
    (interpolate(0), interpolate(1))
}
