//! Glue over `psx_game_runtime::world_cells`: threads the cooked PVS
//! tables, this example's tuning consts, and the arena-owned depth
//! scratch into the crate-owned [`VisibleCellSelector`] instance held
//! by `Playtest::visible_cells`. The prewarm orchestration (window +
//! portal visibility + player motor) stays here.
//!
//! [`VisibleCellSelector`]: psx_game_runtime::world_cells::VisibleCellSelector

use super::*;

#[cfg(feature = "world-grid-visible")]
pub(super) use psx_game_runtime::world_cells::accumulate_grid_visibility_stats;

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
use psx_game_runtime::world_cells::PvsTables;

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
impl Playtest {
    pub(super) fn clear_visible_cell_caches(&mut self) {
        self.visible_cells.clear();
    }

    pub(super) fn prewarm_visible_cell_caches(&mut self) {
        if self.current_collision_room.is_none() {
            return;
        }
        let camera = self.render_camera;
        let active_draw_order = active_room_draw_order(
            &self.window.rooms,
            camera,
            &self.visibility.result,
            self.room_index,
            cached_room_draw_order_mode(),
        );
        let player = self.motor.position();

        telemetry::stage_begin(telemetry::stage::ROOM_VISIBLE_LIST);
        for &active_slot in &active_draw_order {
            if active_slot == INVALID_ACTIVE_ROOM_SLOT {
                continue;
            }
            let active_slot = active_slot as usize;
            let Some(active) = self.window.rooms[active_slot] else {
                continue;
            };
            if !self.portal_visibility_draws_room(active.index) {
                continue;
            }
            // Same anchor selection as the render pass: the player's own
            // room anchors at the player, a far room at its admitting
            // portal. Anything else would warm the wrong cache key and
            // the render pass would refill anyway.
            let window_visibility_anchor = if active.index == self.room_index {
                player
            } else if let Some(anchor) = self.portal_entry_anchor(active.index, active.sector_size)
            {
                anchor
            } else {
                continue;
            };
            let visibility_anchor = RoomPoint::new(
                window_visibility_anchor.x.saturating_sub(active.offset_x),
                window_visibility_anchor.y,
                window_visibility_anchor.z.saturating_sub(active.offset_z),
            );
            let room_camera = camera_for_room(camera, active);
            let _ = self.cached_precomputed_visible_cells(
                active_slot,
                active.index,
                active.width,
                active.depth,
                active.sector_size,
                visibility_anchor,
                active.offset_x,
                active.offset_z,
                window_visibility_anchor,
                room_camera,
                ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                    && !self.player_moved_last_tick
                    && self.camera_turning_last_tick
                    && active.surface_cache.ready,
            );
        }
        telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
    }

    /// The crate visible-cell selection over this example's cooked PVS
    /// tables, tuning consts, and arena-owned depth scratch.
    pub(super) fn cached_precomputed_visible_cells(
        &mut self,
        active_slot: usize,
        room_index: RoomIndex,
        room_width: u16,
        room_depth: u16,
        room_sector_size: i32,
        anchor: RoomPoint,
        room_offset_x: i32,
        room_offset_z: i32,
        global_anchor: RoomPoint,
        camera: WorldCamera,
        camera_independent: bool,
    ) -> Option<(&[GridVisibleCell], u16)> {
        self.visible_cells
            .cached_precomputed_visible_cells(
            world_tables(),
            PvsTables {
                visibility_pvs: VISIBILITY_PVS,
                visibility_pvs_bits: VISIBILITY_PVS_BITS,
            },
            VISIBLE_CELL_TUNING,
            &mut cell_scratch_arena().depths[..],
            active_slot,
            room_index,
            room_width,
            room_depth,
            room_sector_size,
            anchor,
            room_offset_x,
            room_offset_z,
            global_anchor,
            camera,
            camera_independent,
        )
            // An empty PVS is never a valid reason to make an active room
            // disappear. Treat corrupt/incomplete data exactly like missing
            // data so the render path takes its existing conservative all-cell
            // fallback and reports ROOM_VISIBILITY_FALLBACK_DRAWS.
            .filter(|(cells, _)| !cells.is_empty())
    }
}
