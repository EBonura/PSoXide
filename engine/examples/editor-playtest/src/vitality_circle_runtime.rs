use super::*;

impl Playtest {
    /// Submit each field as a two-packet ground decal. The existing 64x64
    /// shadow pixels are reused; colour and pulse are packet state, so the
    /// animation allocates neither another texture nor a frame buffer.
    pub(super) fn draw_vitality_circles_world(
        &self,
        camera: WorldCamera,
        elapsed: SimTick,
        packets: &mut PrimitivePacketArena<'_>,
        world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
    ) {
        let Some(base_material) = self.shadow_material else {
            return;
        };
        let options = current_actor_surface_options(self.room_index, self.bsp.is_some());
        for (index, circle) in VITALITY_CIRCLES.iter().enumerate() {
            if circle.room != self.room_index {
                continue;
            }
            let claimed = self.vitality_circles.is_claimed(index);
            let phase = ((elapsed.as_u32().wrapping_add((index as u32) * 11)) & 31) as i32;
            let triangle = if phase < 16 { phase } else { 31 - phase };
            let pulse = if claimed { triangle / 4 } else { 0 };
            let tint = match (circle.axis, claimed) {
                (0, true) => (255, 74, 38),
                (0, false) => (70, 22, 14),
                (_, true) => (70, 238, 206),
                (_, false) => (16, 62, 56),
            };
            let material = base_material
                .with_raw_texture(false)
                .with_tint(tint)
                .with_blend_mode(BlendMode::Add);
            draw_actor_shadow(
                circle.x,
                circle.y,
                circle.z,
                i32::from(circle.radius).saturating_add(pulse),
                &camera,
                options,
                material,
                packets,
                world,
            );
        }
    }
}
