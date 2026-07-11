use super::*;

#[cfg(feature = "world-grid-visible")]
pub(super) fn accumulate_grid_visibility_stats(
    total: &mut GridVisibilityStats,
    stats: GridVisibilityStats,
) {
    total.cells_considered = total
        .cells_considered
        .saturating_add(stats.cells_considered);
    total.cells_drawn = total.cells_drawn.saturating_add(stats.cells_drawn);
    total.cells_frustum_culled = total
        .cells_frustum_culled
        .saturating_add(stats.cells_frustum_culled);
    total.surfaces_considered = total
        .surfaces_considered
        .saturating_add(stats.surfaces_considered);
    total.projected_vertices = total
        .projected_vertices
        .saturating_add(stats.projected_vertices);
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
impl Playtest {
    pub(super) fn clear_visible_cell_caches(&mut self) {
        self.visible_cell_caches = [const { ActiveVisibleCellCache::EMPTY }; MAX_ACTIVE_ROOMS];
        self.visible_cell_cache_cursor = 0;
    }

    pub(super) fn prewarm_visible_cell_caches(&mut self) {
        if self.current_collision_room.is_none() {
            return;
        }
        let camera = self.render_camera;
        let active_draw_order = active_room_draw_order(
            &self.active_rooms,
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
            let Some(active) = self.active_rooms[active_slot] else {
                continue;
            };
            if !self.portal_visibility_draws_room(active.index) {
                continue;
            }
            // Same anchor selection as the render pass: the player's own
            // room anchors at the player, a far room at its admitting
            // portal. Anything else would warm the wrong cache key and
            // the render pass would refill anyway.
            let global_visibility_anchor = if active.index == self.room_index {
                player
            } else if let Some(anchor) = self.portal_entry_anchor(active.index, active.sector_size)
            {
                anchor
            } else {
                continue;
            };
            let visibility_anchor = RoomPoint::new(
                global_visibility_anchor.x.saturating_sub(active.offset_x),
                global_visibility_anchor.y,
                global_visibility_anchor.z.saturating_sub(active.offset_z),
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
                global_visibility_anchor,
                room_camera,
                ROOM_VISIBLE_CELL_STATIONARY_CANDIDATES
                    && !self.player_moved_last_tick
                    && self.camera_turning_last_tick
                    && active.surface_cache.ready,
            );
        }
        telemetry::stage_end(telemetry::stage::ROOM_VISIBLE_LIST);
    }

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
        let sector_size = room_sector_size.max(1);
        let anchor_x = grid_cell_for_room(anchor.x, sector_size);
        let anchor_z = grid_cell_for_room(anchor.z, sector_size);
        // The anchor is the player's position in this room's local frame.
        // For a room the player is not inside (a far room seen through a
        // portal), clamping onto the grid edge selected an arbitrary
        // boundary cell whose wall-gated PVS is often tiny or empty --
        // which culled far rooms wholesale (the arch-door hole, confirmed
        // live 2026-06-11). An outside anchor bails to the caller's
        // full-room fallback draw instead: correct by construction, and
        // the portal walk only admits genuinely visible far rooms.
        if anchor_x < 0
            || anchor_x >= room_width as i32
            || anchor_z < 0
            || anchor_z >= room_depth as i32
        {
            return None;
        }
        let (view_sin_key, view_cos_key) = visible_cell_view_keys(camera, camera_independent);
        let cache = *self.visible_cell_caches.get(active_slot)?;
        if cache.ready
            && cache.room == room_index
            && cache.anchor_x == anchor_x
            && cache.anchor_z == anchor_z
            && cache.view_sin_key == view_sin_key
            && cache.view_cos_key == view_cos_key
            && cache.camera_independent == camera_independent
        {
            let first = cache.first as usize;
            let count = cache.count as usize;
            let end = first.checked_add(count)?;
            return self
                .visible_cell_cache_cells
                .get(first..end)
                .map(|cells| (cells, cache.rejected_global));
        }

        let required_cells = room_visibility_candidate_count(room_index)?;
        let mut first = self.visible_cell_cache_cursor;
        if MAX_ACTIVE_VISIBLE_CELLS.saturating_sub(first) < required_cells {
            self.clear_visible_cell_caches();
            first = 0;
        }
        let (mut count, mut rejected_global) = {
            let cells = self.visible_cell_cache_cells.get_mut(first..)?;
            let depths = unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
            fill_precomputed_visible_cells(
                room_index,
                anchor_x,
                anchor_z,
                room_offset_x,
                room_offset_z,
                sector_size,
                global_anchor,
                camera,
                camera_independent,
                cells,
                depths,
            )
        }?;

        if first.saturating_add(count) > MAX_ACTIVE_VISIBLE_CELLS || count > u16::MAX as usize {
            self.clear_visible_cell_caches();
            first = 0;
            (count, rejected_global) = {
                let cells = self.visible_cell_cache_cells.get_mut(first..)?;
                let depths = unsafe { &mut CACHED_ROOM_ACCEPTED_CELL_DEPTHS[..] };
                fill_precomputed_visible_cells(
                    room_index,
                    anchor_x,
                    anchor_z,
                    room_offset_x,
                    room_offset_z,
                    sector_size,
                    global_anchor,
                    camera,
                    camera_independent,
                    cells,
                    depths,
                )
            }?;
            if count > MAX_ACTIVE_VISIBLE_CELLS || count > u16::MAX as usize {
                return None;
            }
        }

        self.visible_cell_caches[active_slot] = ActiveVisibleCellCache {
            room: room_index,
            anchor_x,
            anchor_z,
            view_sin_key,
            view_cos_key,
            camera_independent,
            rejected_global,
            first: first as u16,
            count: count as u16,
            ready: true,
        };
        self.visible_cell_cache_cursor = first.saturating_add(count);
        self.visible_cell_cache_cells
            .get(first..self.visible_cell_cache_cursor)
            .map(|cells| (cells, rejected_global))
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn room_visibility_candidate_count(room_index: RoomIndex) -> Option<usize> {
    ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room_index)
        .map(|visibility| visibility.cell_count as usize)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn fill_precomputed_visible_cells(
    room_index: RoomIndex,
    anchor_x: i32,
    anchor_z: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    sector_size: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
    camera_independent: bool,
    out: &mut [GridVisibleCell],
    depths: &mut [i32],
) -> Option<(usize, u16)> {
    let room_visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room_index)?;
    let room_record = ROOMS.get(room_index.to_usize())?;
    let first = room_visibility.cell_first.to_usize();
    let count = room_visibility.cell_count as usize;
    if count > out.len() || count > depths.len() || count > MAX_PRECOMPUTED_VISIBLE_CELLS {
        return None;
    }
    let room_cells = VISIBILITY_CELLS.get(first..first.checked_add(count)?)?;
    let anchor_index = visibility_cell_index_for_anchor(room_cells, anchor_x, anchor_z)
        .or_else(|| nearest_runtime_visibility_cell(room_cells, anchor_x, anchor_z))?;
    let pvs_index = (room_visibility.pvs_first as usize).checked_add(anchor_index)?;
    if anchor_index >= room_visibility.pvs_count as usize {
        return None;
    }
    let pvs = *VISIBILITY_PVS.get(pvs_index)?;
    let byte_first = pvs.byte_first as usize;
    let byte_end = byte_first.checked_add(pvs.byte_count as usize)?;
    let pvs_bits = VISIBILITY_PVS_BITS.get(byte_first..byte_end)?;
    let filter = VisibleCellFilter {
        anchor_x,
        anchor_z,
        sector_size,
        room_offset_x,
        room_offset_z,
        global_anchor,
        camera,
        camera_independent,
        far_z: room_draw_distance(room_record),
        global_radius_sectors: room_chunk_activation_radius_sectors(room_record),
    };
    let mut written = 0usize;
    let mut rejected_global = 0u16;
    let mut cell_index = 0usize;
    while cell_index < room_cells.len() {
        if visibility_pvs_bit(pvs_bits, cell_index) {
            write_visible_cell_candidate(
                room_cells[cell_index],
                filter,
                out,
                depths,
                &mut written,
                &mut rejected_global,
            );
        }
        cell_index += 1;
    }
    sort_visible_cells_for_camera(&mut out[..written], &mut depths[..written]);
    Some((written, rejected_global))
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_view_keys(camera: WorldCamera, camera_independent: bool) -> (i16, i16) {
    if camera_independent {
        let _ = camera;
        return (0, 0);
    }
    #[cfg(any(feature = "vis-anchor-cache", feature = "vis-anchor-pvs-candidates"))]
    {
        let _ = camera;
        let _ = camera_independent;
        (0, 0)
    }
    #[cfg(all(
        not(feature = "vis-anchor-cache"),
        not(feature = "vis-anchor-pvs-candidates"),
        feature = "vis-coarse-yaw"
    ))]
    {
        (
            (camera.sin_yaw.raw() / 2048) as i16,
            (camera.cos_yaw.raw() / 2048) as i16,
        )
    }
    #[cfg(all(
        not(feature = "vis-anchor-cache"),
        not(feature = "vis-anchor-pvs-candidates"),
        not(feature = "vis-coarse-yaw")
    ))]
    {
        (
            (camera.sin_yaw.raw() / 256) as i16,
            (camera.cos_yaw.raw() / 256) as i16,
        )
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn sort_visible_cells_for_camera(cells: &mut [GridVisibleCell], depths: &mut [i32]) {
    if cells.len() > depths.len() {
        return;
    }
    let mut gap = cells.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < cells.len() {
            let cell = cells[i];
            let depth = depths[i];
            let mut j = i;
            while j >= gap && depths[j - gap] < depth {
                cells[j] = cells[j - gap];
                depths[j] = depths[j - gap];
                j -= gap;
            }
            cells[j] = cell;
            depths[j] = depth;
            i += 1;
        }
        gap /= 2;
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_camera_depth_if_sphere_visible(
    cell: psx_level::LevelVisibilityCellRecord,
    camera: WorldCamera,
    sector_size: i32,
    far_z: i32,
) -> Option<i32> {
    let sector_size = sector_size.max(1);
    let half = sector_size >> 1;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    let half_height = ((cell.max_y - cell.min_y).abs() >> 1).max(half);
    let radius = sector_size.saturating_add(half_height);
    let view = camera.view_vertex(center);
    let near = camera.projection.near_z.max(1);
    let far = far_z.max(near);
    if view.z < near.saturating_sub(radius) || view.z > far.saturating_add(radius) {
        return None;
    }

    let z = view.z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(ROOM_VISIBLE_CELL_SCREEN_MARGIN)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(ROOM_VISIBLE_CELL_SCREEN_MARGIN)
        .max(1);
    let projected_x = view.x.abs().saturating_sub(radius).saturating_mul(focal);
    let projected_y = view.y.abs().saturating_sub(radius).saturating_mul(focal);
    if projected_x > half_w.saturating_mul(z) || projected_y > half_h.saturating_mul(z) {
        return None;
    }
    Some(view.z)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_camera_depth(
    cell: psx_level::LevelVisibilityCellRecord,
    camera: WorldCamera,
    sector_size: i32,
) -> i32 {
    let sector_size = sector_size.max(1);
    let half = sector_size >> 1;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    camera.view_vertex(center).z
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone)]
struct VisibleCellFilter {
    anchor_x: i32,
    anchor_z: i32,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
    camera: WorldCamera,
    camera_independent: bool,
    far_z: i32,
    global_radius_sectors: i32,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
#[derive(Copy, Clone, PartialEq, Eq)]
enum VisibleCellReject {
    GlobalRange,
    Camera,
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn write_visible_cell_candidate(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
    out: &mut [GridVisibleCell],
    depths: &mut [i32],
    written: &mut usize,
    rejected_global: &mut u16,
) {
    match visible_cell_reject_reason(cell, filter) {
        Some(VisibleCellReject::GlobalRange) => {
            *rejected_global = rejected_global.saturating_add(1);
            return;
        }
        Some(VisibleCellReject::Camera) => return,
        None => {}
    }
    if *written >= out.len() {
        return;
    }
    let visible_cell = GridVisibleCell::with_cache_cell_index(
        cell.x,
        cell.z,
        cell.min_y,
        cell.max_y,
        cell.cache_cell_index,
    );
    if filter.camera_independent || cfg!(feature = "vis-anchor-pvs-candidates") {
        out[*written] = visible_cell;
        depths[*written] = 0;
        *written += 1;
        return;
    }
    let depth = if cfg!(feature = "vis-broad-pvs") {
        visible_cell_camera_depth(cell, filter.camera, filter.sector_size)
    } else {
        let Some(depth) = visible_cell_camera_depth_if_sphere_visible(
            cell,
            filter.camera,
            filter.sector_size,
            filter.far_z,
        ) else {
            return;
        };
        out[*written] = visible_cell.with_camera_depth(GridVisibleCell::CAMERA_DEPTH_PRECULLED);
        depths[*written] = depth;
        *written += 1;
        return;
    };
    out[*written] = visible_cell;
    depths[*written] = depth;
    *written += 1;
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visible_cell_reject_reason(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
) -> Option<VisibleCellReject> {
    if visibility_cell_safety_ring(cell, filter.anchor_x, filter.anchor_z) {
        return None;
    }
    if !visibility_cell_in_global_range(
        cell.x,
        cell.z,
        filter.sector_size,
        filter.room_offset_x,
        filter.room_offset_z,
        filter.global_anchor,
        filter.global_radius_sectors,
    ) {
        return Some(VisibleCellReject::GlobalRange);
    }
    if cfg!(feature = "vis-broad-pvs") {
        return None;
    }
    if filter.camera_independent || cfg!(feature = "vis-anchor-pvs-candidates") {
        return None;
    }
    if !visibility_cell_in_view_wedge(cell, filter) {
        return Some(VisibleCellReject::Camera);
    }
    if !visibility_cell_aabb_intersects_camera(
        cell,
        filter.sector_size,
        filter.camera,
        filter.far_z,
    ) {
        return Some(VisibleCellReject::Camera);
    }
    None
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_safety_ring(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> bool {
    visibility_cell_anchor_distance(cell, anchor_x, anchor_z) <= ROOM_VISIBLE_CELL_SAFETY_RING
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_anchor_distance(
    cell: psx_level::LevelVisibilityCellRecord,
    anchor_x: i32,
    anchor_z: i32,
) -> i32 {
    let dx = (cell.x as i32).saturating_sub(anchor_x).abs();
    let dz = (cell.z as i32).saturating_sub(anchor_z).abs();
    dx.max(dz)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_in_view_wedge(
    cell: psx_level::LevelVisibilityCellRecord,
    filter: VisibleCellFilter,
) -> bool {
    let anchor_distance = visibility_cell_anchor_distance(cell, filter.anchor_x, filter.anchor_z);
    if anchor_distance <= ROOM_VISIBLE_CELL_NEAR_RING {
        return true;
    }
    if cell.blocker_mask != 0 || cell.portal_mask != 0x0f {
        return true;
    }

    let sector_size = filter.sector_size.max(1);
    let half = sector_size >> 1;
    let center_x = (cell.x as i32)
        .saturating_mul(sector_size)
        .saturating_add(half);
    let center_z = (cell.z as i32)
        .saturating_mul(sector_size)
        .saturating_add(half);
    let anchor_x = filter
        .anchor_x
        .saturating_mul(sector_size)
        .saturating_add(half);
    let anchor_z = filter
        .anchor_z
        .saturating_mul(sector_size)
        .saturating_add(half);
    let dx = center_x.saturating_sub(anchor_x);
    let dz = center_z.saturating_sub(anchor_z);
    let sin_yaw = filter.camera.sin_yaw.raw();
    let cos_yaw = filter.camera.cos_yaw.raw();
    let forward_x = -sin_yaw;
    let forward_z = -cos_yaw;
    let depth = mul_q12_i32(dx, forward_x).saturating_add(mul_q12_i32(dz, forward_z));
    if depth < 0 {
        return anchor_distance <= ROOM_VISIBLE_CELL_REAR_RING;
    }
    let lateral = mul_q12_i32(dx, cos_yaw)
        .saturating_sub(mul_q12_i32(dz, sin_yaw))
        .unsigned_abs();
    let lateral_limit = depth
        .saturating_mul(ROOM_VISIBLE_CELL_WEDGE_NUM)
        .checked_div(ROOM_VISIBLE_CELL_WEDGE_DEN.max(1))
        .unwrap_or(i32::MAX)
        .saturating_add(sector_size.saturating_mul(ROOM_VISIBLE_CELL_WEDGE_MARGIN_SECTORS))
        .max(0)
        .unsigned_abs();
    lateral <= lateral_limit
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_aabb_intersects_camera(
    cell: psx_level::LevelVisibilityCellRecord,
    sector_size: i32,
    camera: WorldCamera,
    far_z: i32,
) -> bool {
    let sector_size = sector_size.max(1);
    let margin = ROOM_VISIBLE_CELL_CAMERA_MARGIN.max(sector_size >> 2);
    let x0 = (cell.x as i32)
        .saturating_mul(sector_size)
        .saturating_sub(margin);
    let x1 = (cell.x as i32)
        .saturating_add(1)
        .saturating_mul(sector_size)
        .saturating_add(margin);
    let z0 = (cell.z as i32)
        .saturating_mul(sector_size)
        .saturating_sub(margin);
    let z1 = (cell.z as i32)
        .saturating_add(1)
        .saturating_mul(sector_size)
        .saturating_add(margin);
    let y0 = cell.min_y.saturating_sub(margin);
    let y1 = cell.max_y.saturating_add(margin);
    aabb_intersects_camera_frustum(x0, x1, y0, y1, z0, z1, camera, far_z)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn aabb_intersects_camera_frustum(
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
    z0: i32,
    z1: i32,
    camera: WorldCamera,
    far_z: i32,
) -> bool {
    let near = camera.projection.near_z.max(1);
    let far = far_z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(ROOM_VISIBLE_CELL_CAMERA_MARGIN)
        .max(1);
    let mut max_depth = i32::MIN;
    let mut min_depth = i32::MAX;
    let mut all_right = true;
    let mut all_left = true;
    let mut all_above = true;
    let mut all_below = true;
    for x in [x0, x1] {
        for y in [y0, y1] {
            for z in [z0, z1] {
                let view = camera.view_vertex(WorldVertex::new(x, y, z));
                max_depth = max_depth.max(view.z);
                min_depth = min_depth.min(view.z);
                if view.z < near {
                    all_right = false;
                    all_left = false;
                    all_above = false;
                    all_below = false;
                    continue;
                }
                let depth_limit_x = half_w.saturating_mul(view.z);
                let depth_limit_y = half_h.saturating_mul(view.z);
                let projected_x = view.x.saturating_mul(focal);
                let projected_y = view.y.saturating_mul(focal);
                if projected_x <= depth_limit_x {
                    all_right = false;
                }
                if -projected_x <= depth_limit_x {
                    all_left = false;
                }
                if projected_y <= depth_limit_y {
                    all_above = false;
                }
                if -projected_y <= depth_limit_y {
                    all_below = false;
                }
            }
        }
    }
    if max_depth < near || min_depth > far {
        return false;
    }
    !(all_right || all_left || all_above || all_below)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_in_global_range(
    x: u16,
    z: u16,
    sector_size: i32,
    room_offset_x: i32,
    room_offset_z: i32,
    global_anchor: RoomPoint,
    radius_sectors: i32,
) -> bool {
    let radius = radius_sectors.max(1).saturating_mul(sector_size);
    let x0 = room_offset_x.saturating_add((x as i32).saturating_mul(sector_size));
    let z0 = room_offset_z.saturating_add((z as i32).saturating_mul(sector_size));
    let x1 = x0.saturating_add(sector_size);
    let z1 = z0.saturating_add(sector_size);
    rect_distance_sq(global_anchor.x, global_anchor.z, x0, x1, z0, z1)
        <= square_i32_to_u32_saturating(radius)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_pvs_bit(bits: &[u8], index: usize) -> bool {
    let byte = index / 8;
    let bit = index % 8;
    bits.get(byte)
        .map(|value| value & (1 << bit) != 0)
        .unwrap_or(false)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_index_for_anchor(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: i32,
    z: i32,
) -> Option<usize> {
    if x < 0 || z < 0 || x > u16::MAX as i32 || z > u16::MAX as i32 {
        return None;
    }
    visibility_cell_index_by_coord(cells, x as u16, z as u16)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn visibility_cell_index_by_coord(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: u16,
    z: u16,
) -> Option<usize> {
    let key = visibility_cell_key(x, z);
    let mut low = 0usize;
    let mut high = cells.len();
    while low < high {
        let mid = (low + high) / 2;
        let cell = cells[mid];
        if visibility_cell_key(cell.x, cell.z) < key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let cell = cells.get(low)?;
    (visibility_cell_key(cell.x, cell.z) == key).then_some(low)
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
const fn visibility_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn nearest_runtime_visibility_cell(
    cells: &[psx_level::LevelVisibilityCellRecord],
    x: i32,
    z: i32,
) -> Option<usize> {
    let mut best_index = None;
    let mut best_score = u32::MAX;
    for (index, cell) in cells.iter().enumerate() {
        let dx = (cell.x as i32).saturating_sub(x).unsigned_abs();
        let dz = (cell.z as i32).saturating_sub(z).unsigned_abs();
        let score = dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz));
        if best_index.is_none() || score < best_score {
            best_index = Some(index);
            best_score = score;
        }
    }
    best_index
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn grid_cell_for_room(value: i32, sector_size: i32) -> i32 {
    if value >= 0 {
        value / sector_size
    } else {
        (value - sector_size + 1) / sector_size
    }
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn rect_distance_sq(x: i32, z: i32, x0: i32, x1: i32, z0: i32, z1: i32) -> u32 {
    let dx = if x < x0 {
        x0.saturating_sub(x)
    } else if x > x1 {
        x.saturating_sub(x1)
    } else {
        0
    };
    let dz = if z < z0 {
        z0.saturating_sub(z)
    } else if z > z1 {
        z.saturating_sub(z1)
    } else {
        0
    };
    square_i32_to_u32_saturating(dx).saturating_add(square_i32_to_u32_saturating(dz))
}

#[cfg(all(
    feature = "world-grid-visible",
    not(feature = "vis-full-active-chunks")
))]
fn square_i32_to_u32_saturating(value: i32) -> u32 {
    let value = value.unsigned_abs();
    if value > 65_535 {
        u32::MAX
    } else {
        value.saturating_mul(value)
    }
}
