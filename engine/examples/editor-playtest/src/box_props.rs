//! Glue over `psx_game_runtime::box_props`: threads the cooked box
//! records, the player motor state, this example's GTE toggle, and the
//! arena-owned debris cache into the crate-owned [`BoxProps`] instance
//! held by `Playtest::box_props`, keeping the old call-site
//! signatures. The profile stage wrappers stay here with their
//! env-gated toggle.
//!
//! [`BoxProps`]: psx_game_runtime::box_props::BoxProps

use super::*;

pub(super) use psx_game_runtime::box_props::box_prop_movement_break_trigger;

impl Playtest {
    pub(super) fn rebuild_box_prop_runtime(&mut self) {
        self.box_props.rebuild(BOX_PROPS);
    }

    pub(super) fn advance_box_prop_break_events(&mut self, delta_vblanks: u16) {
        self.box_props.advance_break_events(delta_vblanks);
    }

    pub(super) fn advance_box_prop_falls(&mut self, delta_vblanks: u16) {
        self.box_props
            .advance_falls(BOX_PROPS, self.room_index, delta_vblanks);
    }

    pub(super) fn break_box_props_for_movement(
        &mut self,
        trigger: u16,
        input: CharacterMotorInput,
        config: CharacterMotorConfig,
        delta_vblanks: u16,
    ) {
        self.box_props.break_for_movement(
            BOX_PROPS,
            self.room_index,
            self.motor.position(),
            self.motor.yaw(),
            trigger,
            input,
            config,
            delta_vblanks,
        );
    }

    pub(super) fn break_box_props_for_attack(&mut self, config: CharacterMotorConfig) {
        self.box_props.break_for_attack(
            BOX_PROPS,
            self.room_index,
            self.motor.position(),
            self.motor.yaw(),
            config,
        );
    }

    pub(super) fn collect_static_prop_aabb_blockers(
        &self,
        out: &mut [CharacterCollisionAabb],
    ) -> usize {
        let count = self
            .box_props
            .collect_collision_blockers(BOX_PROPS, self.room_index, out);
        let count = count
            + psx_game_runtime::arch_props::collect_arch_prop_collision_blockers(
                ARCH_PROPS,
                ARCH_PROP_COLLISIONS,
                self.room_index,
                &mut out[count..],
            );
        count
            + psx_game_runtime::image_props::collect_image_prop_collision_blockers(
                IMAGE_PROPS,
                self.room_index,
                &mut out[count..],
            )
    }

    /// Checked fixed-capacity collection for the resident BSP provider.
    /// Invalid generated tables or more blockers than the explicit stack
    /// budget return `None`; no partially filled slice is consumed.
    pub(super) fn collect_static_prop_aabb_blockers_checked(
        &self,
        out: &mut [CharacterCollisionAabb],
    ) -> Option<usize> {
        let mut count =
            self.box_props
                .collect_collision_blockers_checked(BOX_PROPS, self.room_index, out)?;
        count += psx_game_runtime::arch_props::collect_arch_prop_collision_blockers_checked(
            ARCH_PROPS,
            ARCH_PROP_COLLISIONS,
            self.room_index,
            out.get_mut(count..)?,
        )?;
        count += psx_game_runtime::image_props::collect_image_prop_collision_blockers_checked(
            IMAGE_PROPS,
            self.room_index,
            out.get_mut(count..)?,
        )?;
        Some(count)
    }
}

#[inline(always)]
pub(super) fn box_prop_profile_begin(stage_id: u16) {
    if BOX_PROP_PROFILE_ENABLED {
        telemetry::stage_begin(stage_id);
    }
}

#[inline(always)]
pub(super) fn box_prop_profile_end(stage_id: u16) {
    if BOX_PROP_PROFILE_ENABLED {
        telemetry::stage_end(stage_id);
    }
}

/// Draw the unbroken box props of `current_room` through the crate
/// policy.
pub(super) fn draw_box_props<T>(
    props: &[LevelBoxPropRecord],
    generated_surfaces: &[psx_level::LevelBoxPropSurfaceRecord],
    state: &RuntimeBoxProps,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>
        + PrimitiveSink<QuadTexturedGouraud>,
{
    psx_game_runtime::box_props::draw_box_props(
        props,
        generated_surfaces,
        state,
        current_room,
        camera,
        options,
        lighting,
        prop_texture_slot,
        triangles,
        world,
    );
}

/// Draw the settled floor debris of the broken box props through the
/// crate policy and the arena-owned debris cache.
pub(super) fn draw_box_prop_floor_debris<T>(
    props: &[LevelBoxPropRecord],
    state: &RuntimeBoxProps,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    psx_game_runtime::box_props::draw_box_prop_floor_debris::<
        T,
        MAX_BOX_PROP_STATE,
        BOX_PROP_BROKEN_WORDS,
        MAX_BOX_PROP_BREAK_EVENTS,
        BOX_PROP_GTE_PROJECT_ENABLED,
        OT_DEPTH,
    >(
        props,
        state,
        debris_cache_arena(),
        current_room,
        camera,
        options,
        lighting,
        prop_texture_slot,
        triangles,
        world,
    );
}

/// Draw the live break bursts of `current_room` through the crate
/// policy.
pub(super) fn draw_box_prop_break_events<T>(
    props: &[LevelBoxPropRecord],
    state: &RuntimeBoxProps,
    current_room: RoomIndex,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    lighting: &RuntimeRoomLighting,
    triangles: &mut T,
    world: &mut WorldRenderPass<'_, '_, OT_DEPTH>,
) where
    T: PrimitiveSink<QuadTexturedGouraud>
        + PrimitiveSink<TriTextured>
        + PrimitiveSink<TriTexturedGouraud>,
{
    psx_game_runtime::box_props::draw_box_prop_break_events::<
        T,
        MAX_BOX_PROP_STATE,
        BOX_PROP_BROKEN_WORDS,
        MAX_BOX_PROP_BREAK_EVENTS,
        BOX_PROP_GTE_PROJECT_ENABLED,
        OT_DEPTH,
    >(
        props,
        state,
        current_room,
        camera,
        options,
        lighting,
        prop_texture_slot,
        triangles,
        world,
    );
}
