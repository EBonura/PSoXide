use super::*;
use psx_engine::{CullMode, DepthPolicy};

const OCTAGON_Q12: [(i32, i32); 8] = [
    (4096, 0),
    (2896, 2896),
    (0, 4096),
    (-2896, 2896),
    (-4096, 0),
    (-2896, -2896),
    (0, -4096),
    (2896, -2896),
];

impl Playtest {
    /// A horizontal glyph follows the actor while the new phase rises through
    /// the body. Small tiles let its far and near edges sort around the mesh.
    pub(super) fn draw_phase_change_ring(
        &self,
        camera: WorldCamera,
        packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) {
        let (Some(material), Some(character)) =
            (self.vitality_circle_material, self.character.as_ref())
        else {
            return;
        };
        let elapsed = self.player_stance.swap_elapsed_ticks();
        let duration = self.player_stance_config.swap_duration_ticks;
        let lifetime = duration.saturating_add(12);
        if duration == 0 || elapsed >= lifetime {
            return;
        }
        let progress = i32::from(
            self.player_stance
                .swap_progress_q12(&self.player_stance_config),
        );
        let position = self.motor.position();
        let height = player_phase_height(character);
        let y = position
            .y
            .saturating_add(2 + ((height + 6) * progress >> 12))
            .saturating_add(i32::from(elapsed.saturating_sub(duration)) * 2);
        let radius = (character.radius * 3).max(character.height / 2);
        let radius = radius * i32::from(elapsed.min(4) + 4) / 8;
        let fade_in = i32::from(elapsed.min(6)) * 256 / 6;
        let fade_out = i32::from((lifetime - elapsed).min(12)) * 256 / 12;
        let fade = fade_in.min(fade_out);
        // Smoothstep eases both ends without dimming the sweep through the body.
        let fade = ((fade * fade) >> 8) * (768 - 2 * fade) >> 8;
        let (r, g, b) = vitality_circle_tint(self.player_stance.active().index() as u8, true);
        let material = material
            .with_raw_texture(false)
            .with_tint((
                (i32::from(r) * fade >> 8) as u8,
                (i32::from(g) * fade >> 8) as u8,
                (i32::from(b) * fade >> 8) as u8,
            ))
            .with_blend_mode(BlendMode::Add);
        let options = current_actor_surface_options(self.room_index, self.bsp.is_some())
            .with_depth_policy(DepthPolicy::Average)
            .with_cull_mode(CullMode::None)
            .with_material_layer(material);
        let u = psx_game_runtime::vram::VITALITY_CIRCLE_TEXEL_U;
        let v = psx_game_runtime::vram::VITALITY_CIRCLE_TEXEL_V;
        let rotation = Angle::from_q12(elapsed.wrapping_mul(24));
        let sin = rotation.sin_q12();
        let cos = rotation.cos_q12();
        let mut vertices = [WorldVertex::new(0, y, 0); 25];
        for row in 0..5 {
            for column in 0..5 {
                let x = radius * (column as i32 - 2) / 2;
                let z = radius * (row as i32 - 2) / 2;
                vertices[row * 5 + column] = WorldVertex::new(
                    position.x + ((x * cos + z * sin) >> 12),
                    y,
                    position.z + ((z * cos - x * sin) >> 12),
                );
            }
        }
        for row in 0..4 {
            for column in 0..4 {
                let u0 = u + (column * 63 / 4) as u8;
                let u1 = u + ((column + 1) * 63 / 4) as u8;
                let v0 = v + (row * 63 / 4) as u8;
                let v1 = v + ((row + 1) * 63 / 4) as u8;
                let _ = world.submit_textured_world_quad(
                    packets,
                    camera,
                    [
                        vertices[row * 5 + column],
                        vertices[row * 5 + column + 1],
                        vertices[(row + 1) * 5 + column + 1],
                        vertices[(row + 1) * 5 + column],
                    ],
                    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)],
                    material,
                    options,
                );
            }
        }
    }

    /// Draw each field as two machine-glyph decals plus a sparse moving line
    /// cage. The 64x64 texture carries the detail; the bounded line count only
    /// communicates activity and direction.
    pub(super) fn draw_vitality_circles_world(
        &self,
        camera: WorldCamera,
        elapsed: SimTick,
        packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) {
        let Some(base_material) = self.vitality_circle_material else {
            return;
        };
        let options = current_actor_surface_options(self.room_index, self.bsp.is_some());
        for (index, circle) in VITALITY_CIRCLES.iter().enumerate() {
            if circle.room != self.room_index {
                continue;
            }
            let claimed = self.vitality_circles.is_claimed(index);
            let tick = elapsed.as_u32().wrapping_add((index as u32) * 11);
            let phase = (tick & 31) as i32;
            let triangle = if phase < 16 { phase } else { 31 - phase };
            let pulse = if claimed { triangle / 3 } else { triangle / 8 };
            let tint = vitality_circle_tint(circle.axis, claimed);
            let material = base_material
                .with_raw_texture(false)
                .with_tint(tint)
                .with_blend_mode(BlendMode::Add);
            let uv_origin = (
                psx_game_runtime::vram::VITALITY_CIRCLE_TEXEL_U,
                psx_game_runtime::vram::VITALITY_CIRCLE_TEXEL_V,
            );

            draw_ground_decal(
                circle.x,
                circle.y,
                circle.z,
                i32::from(circle.radius).saturating_add(pulse),
                uv_origin,
                &camera,
                options,
                material,
                packets,
                world,
            );

            // A counter-pulsing core makes the field read as layered machine
            // work instead of one billboard stretched over the floor.
            let inner_radius = i32::from(circle.radius)
                .saturating_mul(3)
                .checked_div(5)
                .unwrap_or(i32::from(circle.radius))
                .saturating_sub(pulse / 2);
            draw_ground_decal(
                circle.x,
                circle.y.saturating_add(1),
                circle.z,
                inner_radius,
                uv_origin,
                &camera,
                options.with_depth_bias(options.depth_bias.saturating_sub(1)),
                material.with_tint(dim_circle_tint(tint, claimed)),
                packets,
                world,
            );

            draw_vitality_circle_cage(
                circle.x,
                circle.y.saturating_add(3),
                circle.z,
                i32::from(circle.radius),
                tick,
                tint,
                camera,
                options,
                packets,
                world,
            );
        }
    }
}

fn vitality_circle_tint(axis: u8, claimed: bool) -> (u8, u8, u8) {
    match (axis, claimed) {
        (0, true) => (255, 92, 46),
        (0, false) => (92, 30, 18),
        (_, true) => (82, 255, 220),
        (_, false) => (20, 82, 72),
    }
}

fn dim_circle_tint((r, g, b): (u8, u8, u8), claimed: bool) -> (u8, u8, u8) {
    let divisor = if claimed { 2 } else { 3 };
    (r / divisor, g / divisor, b / divisor)
}

#[allow(clippy::too_many_arguments)]
fn draw_vitality_circle_cage(
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    tick: u32,
    color: (u8, u8, u8),
    camera: WorldCamera,
    options: WorldSurfaceOptions,
    packets: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let line_options = options
        .with_cull_mode(psx_engine::CullMode::None)
        .with_depth_bias(options.depth_bias.saturating_sub(2));
    let chase = ((tick >> 2) & 7) as usize;

    for arc in 0..4usize {
        let start = (chase + arc * 2) & 7;
        submit_circle_line(
            circle_point(x, y, z, radius, start),
            circle_point(x, y, z, radius, (start + 1) & 7),
            color,
            camera,
            line_options,
            packets,
            world,
        );
    }

    let inner = radius.saturating_mul(5) / 8;
    for edge in 0..4usize {
        let start = edge * 2;
        submit_circle_line(
            circle_point(x, y, z, inner, start),
            circle_point(x, y, z, inner, (start + 2) & 7),
            dim_circle_tint(color, true),
            camera,
            line_options,
            packets,
            world,
        );
    }
    let tick_inner = radius.saturating_mul(3) / 4;
    for cardinal in [0usize, 2, 4, 6] {
        submit_circle_line(
            circle_point(x, y, z, inner, cardinal),
            circle_point(x, y, z, tick_inner, cardinal),
            color,
            camera,
            line_options,
            packets,
            world,
        );
    }
}

fn circle_point(x: i32, y: i32, z: i32, radius: i32, point: usize) -> WorldVertex {
    let (qx, qz) = OCTAGON_Q12[point & 7];
    WorldVertex::new(
        x.saturating_add(radius.saturating_mul(qx) / 4096),
        y,
        z.saturating_add(radius.saturating_mul(qz) / 4096),
    )
}

#[allow(clippy::too_many_arguments)]
fn submit_circle_line(
    from: WorldVertex,
    to: WorldVertex,
    color: (u8, u8, u8),
    camera: WorldCamera,
    options: WorldSurfaceOptions,
    packets: &mut PrimitivePacketArena<'_>,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) {
    let Some(from) = camera.project_world(from) else {
        return;
    };
    let Some(to) = camera.project_world(to) else {
        return;
    };
    let _ = world.submit_projected_line(packets, [from, to], color, options);
}
