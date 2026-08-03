//! Sparse, non-colliding water-surface rendering. Gameplay depth lookup uses
//! the same sorted cell table; rendering stays stateless and therefore does
//! not consume breakable BoxProp state or collision budget.

use psx_engine::{
    CullMode, DepthPolicy, LoadedWorldCameraGte, PrimitiveSink, WorldCamera,
    WorldMaterialAnimation, WorldRenderPass, WorldSurfaceOptions, WorldVertex,
};
use psx_gpu::{
    material::TextureMaterial,
    prim::{TriTextured, TriTexturedGouraud},
};
use psx_level::{AssetId, LevelMaterialAnimation, LevelWaterCellRecord, RoomIndex};

use crate::model_rendering::{
    model_override_blend_mode, model_render_uv_max, sphere_visible_to_camera,
};
use crate::room_lighting::RuntimeRoomLighting;
use crate::vram::VramSlot;

/// Draw visible water cells in one runtime room. Each cell is a single tiled
/// quad; there is no collision query and no mutable per-cell state.
pub fn draw_water_cells<T, const GTE_PROJECT: bool, const OT_DEPTH: usize>(
    cells: &[LevelWaterCellRecord],
    current_room: RoomIndex,
    sector_size: i32,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    mut texture_slot: impl FnMut(AssetId) -> Option<VramSlot>,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured> + PrimitiveSink<TriTexturedGouraud>,
{
    if sector_size <= 0 {
        return;
    }
    let first = cells.partition_point(|cell| cell.room < current_room);
    let end = cells[first..]
        .partition_point(|cell| cell.room == current_room)
        .saturating_add(first);
    let mut projector = None;
    for cell in &cells[first..end] {
        let Some(texture_asset) = cell.texture_asset else {
            continue;
        };
        let Some(slot) = texture_slot(texture_asset) else {
            continue;
        };
        let x0 = i32::from(cell.x).saturating_mul(sector_size);
        let z0 = i32::from(cell.z).saturating_mul(sector_size);
        let x1 = x0.saturating_add(sector_size);
        let z1 = z0.saturating_add(sector_size);
        let verts = [
            WorldVertex::new(x0, cell.surface_y, z1),
            WorldVertex::new(x1, cell.surface_y, z1),
            WorldVertex::new(x1, cell.surface_y, z0),
            WorldVertex::new(x0, cell.surface_y, z0),
        ];
        let center = WorldVertex::new(
            x0.saturating_add(sector_size / 2),
            cell.surface_y,
            z0.saturating_add(sector_size / 2),
        );
        if !sphere_visible_to_camera(camera, options, center, sector_size, 96) {
            continue;
        }
        let material = TextureMaterial::opaque(slot.clut_word, slot.tpage_word, (0x80, 0x80, 0x80))
            .with_blend_mode(model_override_blend_mode(cell.blend_mode))
            .with_texture_window(slot.texture_window);
        let uvs = water_surface_uvs(
            cell.animation,
            slot.texture_width,
            slot.texture_height,
            options.material_animation_tick,
            options.material_animation_hz,
        );
        let projected = if GTE_PROJECT {
            let loaded = match projector {
                Some(projector) => projector,
                None => {
                    let loaded = LoadedWorldCameraGte::load(*camera);
                    projector = Some(loaded);
                    loaded
                }
            };
            loaded.project_world_quad(verts)
        } else {
            camera.project_world_quad(verts)
        };
        let Some(projected) = projected else {
            continue;
        };
        let colors = [
            lighting.apply_vertex_fog_weight(
                cell.tint_rgb.into(),
                lighting.fog_weight_at_depth(projected[0].sz),
            ),
            lighting.apply_vertex_fog_weight(
                cell.tint_rgb.into(),
                lighting.fog_weight_at_depth(projected[1].sz),
            ),
            lighting.apply_vertex_fog_weight(
                cell.tint_rgb.into(),
                lighting.fog_weight_at_depth(projected[2].sz),
            ),
            lighting.apply_vertex_fog_weight(
                cell.tint_rgb.into(),
                lighting.fog_weight_at_depth(projected[3].sz),
            ),
        ];
        let sort_depth = projected.iter().map(|vertex| vertex.sz).sum::<i32>() / 4;
        let opts = options
            .with_depth_policy(DepthPolicy::Fixed(sort_depth.max(camera.projection.near_z)))
            .with_depth_bias(options.depth_bias.saturating_sub(64))
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
    }
}

fn water_surface_uvs(
    animation: LevelMaterialAnimation,
    texture_width: u16,
    texture_height: u16,
    tick: u32,
    hz: u16,
) -> [(u8, u8); 4] {
    let (frame_width, frame_height, animation) = match animation {
        LevelMaterialAnimation::Static => (
            texture_width,
            texture_height,
            WorldMaterialAnimation::Static,
        ),
        LevelMaterialAnimation::UvScroll(motion) if motion.enabled => (
            texture_width,
            texture_height,
            WorldMaterialAnimation::UvScroll {
                speed_u_q8: motion.speed_u_q8,
                speed_v_q8: motion.speed_v_q8,
                phase_u: motion.phase_u,
                phase_v: motion.phase_v,
            },
        ),
        LevelMaterialAnimation::UvScroll(_) => (
            texture_width,
            texture_height,
            WorldMaterialAnimation::Static,
        ),
        LevelMaterialAnimation::Flipbook(flipbook) => {
            let columns = u16::from(flipbook.columns.max(1));
            let rows = u16::from(flipbook.rows.max(1));
            (
                (texture_width / columns).max(1),
                (texture_height / rows).max(1),
                WorldMaterialAnimation::Flipbook {
                    columns: flipbook.columns.max(1),
                    frame_count: flipbook
                        .frame_count
                        .max(1)
                        .min(flipbook.columns.max(1).saturating_mul(flipbook.rows.max(1))),
                    ticks_per_frame: flipbook.ticks_per_frame.max(1),
                    phase: flipbook.phase,
                },
            )
        }
    };
    let width = frame_width.min(u16::from(u8::MAX)) as u8;
    let height = frame_height.min(u16::from(u8::MAX)) as u8;
    let (offset_u, offset_v) = animation.uv_offset(tick, hz, width, height);
    let u_max = model_render_uv_max(frame_width);
    let v_max = model_render_uv_max(frame_height);
    [
        (offset_u, offset_v),
        (u_max.wrapping_add(offset_u), offset_v),
        (u_max.wrapping_add(offset_u), v_max.wrapping_add(offset_v)),
        (offset_u, v_max.wrapping_add(offset_v)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_level::{LevelMaterialFlipbook, LevelMaterialUvMotion};

    #[test]
    fn water_uv_scroll_uses_the_gameplay_material_clock() {
        let animation = LevelMaterialAnimation::UvScroll(LevelMaterialUvMotion {
            enabled: true,
            speed_u_q8: 8 * 256,
            speed_v_q8: 4 * 256,
            phase_u: 0,
            phase_v: 0,
        });
        assert_eq!(water_surface_uvs(animation, 64, 64, 0, 60)[0], (0, 0));
        assert_eq!(water_surface_uvs(animation, 64, 64, 60, 60)[0], (8, 4));
    }

    #[test]
    fn water_flipbook_uses_one_atlas_cell_per_frame() {
        let animation = LevelMaterialAnimation::Flipbook(LevelMaterialFlipbook {
            columns: 2,
            rows: 2,
            frame_count: 4,
            ticks_per_frame: 6,
            phase: 0,
        });
        assert_eq!(
            water_surface_uvs(animation, 64, 64, 0, 60),
            [(0, 0), (31, 0), (31, 31), (0, 31),]
        );
        assert_eq!(water_surface_uvs(animation, 64, 64, 6, 60)[0], (32, 0));
    }
}
