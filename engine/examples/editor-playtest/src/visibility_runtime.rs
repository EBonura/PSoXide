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
            &self.portal_visibility,
            self.room_index,
            cached_room_draw_order_mode(),
        );
        let player = self.motor.position();
        let global_visibility_anchor = player;

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
            let visibility_anchor = RoomPoint::new(
                global_visibility_anchor.x.saturating_sub(active.offset_x),
                player.y,
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
        let anchor_x = grid_cell_for_room(anchor.x, sector_size).clamp(0, room_width as i32 - 1);
        let anchor_z = grid_cell_for_room(anchor.z, sector_size).clamp(0, room_depth as i32 - 1);
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

pub(super) const INVALID_ACTIVE_ROOM_SLOT: u8 = u8::MAX;

pub(super) fn active_room_draw_order(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
    mode: CachedRoomDrawOrderMode,
) -> [u8; MAX_ACTIVE_ROOMS] {
    match mode {
        CachedRoomDrawOrderMode::Distance => {
            active_room_draw_order_by_distance(active_rooms, camera, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Portal => {
            active_room_draw_order_by_portal(active_rooms, visibility, current_room)
        }
        CachedRoomDrawOrderMode::Slot => {
            active_room_draw_order_by_slot(active_rooms, visibility, current_room)
        }
    }
}

fn active_room_draw_order_by_distance(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    camera: WorldCamera,
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut depths = [i32::MIN; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if !portal_visibility_result_draws_room(visibility, current_room, active.index) {
                slot += 1;
                continue;
            }
            let depth = active_room_sort_depth(active, camera);
            let mut insert = count;
            while insert > 0 && depth > depths[insert - 1] {
                depths[insert] = depths[insert - 1];
                order[insert] = order[insert - 1];
                insert -= 1;
            }
            depths[insert] = depth;
            order[insert] = slot as u8;
            count += 1;
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_portal(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut visible_index = 0usize;
    while visible_index < visibility.room_count.min(MAX_ACTIVE_ROOMS) && count < MAX_ACTIVE_ROOMS {
        let room = visibility.rooms[visible_index].room;
        if let Some(slot) = active_room_slot_for_room(active_rooms, room) {
            order[count] = slot;
            count += 1;
        }
        visible_index += 1;
    }
    if count == 0 {
        if let Some(slot) = active_room_slot_for_room(active_rooms, current_room) {
            order[count] = slot;
            count += 1;
        }
    }
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS && count < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index)
                && !active_draw_order_contains(&order, count, slot as u8)
            {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_draw_order_by_slot(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    visibility: &RuntimePortalVisibility,
    current_room: RoomIndex,
) -> [u8; MAX_ACTIVE_ROOMS] {
    let mut order = [INVALID_ACTIVE_ROOM_SLOT; MAX_ACTIVE_ROOMS];
    let mut count = 0usize;
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if let Some(active) = active_rooms[slot] {
            if portal_visibility_result_draws_room(visibility, current_room, active.index) {
                order[count] = slot as u8;
                count += 1;
            }
        }
        slot += 1;
    }
    order
}

fn active_room_slot_for_room(
    active_rooms: &[Option<ActiveRuntimeRoom>; MAX_ACTIVE_ROOMS],
    room: RoomIndex,
) -> Option<u8> {
    let mut slot = 0usize;
    while slot < MAX_ACTIVE_ROOMS {
        if active_rooms[slot].is_some_and(|active| active.index == room) {
            return Some(slot as u8);
        }
        slot += 1;
    }
    None
}

fn active_draw_order_contains(order: &[u8; MAX_ACTIVE_ROOMS], count: usize, slot: u8) -> bool {
    let mut i = 0usize;
    while i < count.min(MAX_ACTIVE_ROOMS) {
        if order[i] == slot {
            return true;
        }
        i += 1;
    }
    false
}

fn portal_visibility_result_draws_room(
    _visibility: &RuntimePortalVisibility,
    _current_room: RoomIndex,
    _index: RoomIndex,
) -> bool {
    // Reachability draw: the draw-order builders only pass rooms from the active
    // window (the camera ring), so every one is drawn -- no frustum/far-distance
    // cull gates room drawing here. Per-cell frustum + per-polygon backface and
    // screen culling still trim the off-screen geometry. This is the draw-order
    // twin of `portal_visibility_draws_room`; the reachability-draw rework
    // flipped that one but missed this, so a reachable-but-not-frustum-visible
    // room (e.g. the room behind the player) was dropped from the draw order.
    true
}

fn active_room_sort_depth(active: ActiveRuntimeRoom, camera: WorldCamera) -> i32 {
    let sector_size = active.sector_size.max(1);
    let center_x = active
        .offset_x
        .saturating_add((active.width as i32).saturating_mul(sector_size) >> 1);
    let center_z = active
        .offset_z
        .saturating_add((active.depth as i32).saturating_mul(sector_size) >> 1);
    camera
        .view_vertex(WorldVertex::new(center_x, 0, center_z))
        .z
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

pub(super) fn room_origin_x(record: &LevelRoomRecord) -> i32 {
    record.origin_x.saturating_mul(record.sector_size)
}

pub(super) fn room_origin_z(record: &LevelRoomRecord) -> i32 {
    record.origin_z.saturating_mul(record.sector_size)
}

/// Vertical origin of a room in engine units. Unlike X/Z (`origin_*` in
/// sectors), `origin_y` is already stored in engine units, so it is used
/// directly. Drives Y rebasing across room transitions for stacked floors.
pub(super) fn room_origin_y(record: &LevelRoomRecord) -> i32 {
    record.origin_y
}

#[derive(Copy, Clone)]
pub(super) struct ActiveRoomView {
    pub(super) position: RoomPoint,
    pub(super) sin_yaw: i32,
    pub(super) cos_yaw: i32,
    pub(super) sin_pitch: i32,
    pub(super) cos_pitch: i32,
}

impl ActiveRoomView {
    pub(super) fn from_camera(camera: WorldCamera) -> Self {
        Self {
            position: RoomPoint::new(camera.position.x, camera.position.y, camera.position.z),
            sin_yaw: camera.sin_yaw.raw(),
            cos_yaw: camera.cos_yaw.raw(),
            sin_pitch: camera.sin_pitch.raw(),
            cos_pitch: camera.cos_pitch.raw(),
        }
    }
}

#[derive(Copy, Clone)]
pub(super) struct PortalVisibilitySpace {
    pub(super) room: RoomIndex,
    pub(super) view: ActiveRoomView,
    pub(super) camera_global: RoomPoint,
}

pub(super) fn portal_visibility_space_for_view(
    current_index: RoomIndex,
    view: ActiveRoomView,
) -> PortalVisibilitySpace {
    let camera_global = local_to_global_room_point(current_index, view.position);
    let visibility_index =
        room_index_containing_global_from(current_index, camera_global).unwrap_or(current_index);
    let visibility_view = if visibility_index == current_index {
        view
    } else {
        ActiveRoomView {
            position: global_to_local_room_point(visibility_index, camera_global),
            ..view
        }
    };
    PortalVisibilitySpace {
        room: visibility_index,
        view: visibility_view,
        camera_global,
    }
}

pub(super) fn portal_visibility_view_keys(view: ActiveRoomView) -> (i16, i16, i16, i16) {
    (
        (view.sin_yaw / 64) as i16,
        (view.cos_yaw / 64) as i16,
        (view.sin_pitch / 64) as i16,
        (view.cos_pitch / 64) as i16,
    )
}

pub(super) fn authored_room_for_chunk(index: RoomIndex) -> Option<u32> {
    chunk_record_for_room(index).map(|chunk| chunk.authored_room)
}

pub(super) fn chunk_record_for_room(index: RoomIndex) -> Option<&'static LevelChunkRecord> {
    if let Some(chunk) = ROOM_CHUNKS.get(index.to_usize()) {
        if chunk.room == index {
            return Some(chunk);
        }
    }
    ROOM_CHUNKS.iter().find(|chunk| chunk.room == index)
}

pub(super) fn chunk_overlaps_collision_window(
    chunk: LevelChunkRecord,
    current_record: &LevelRoomRecord,
    chunk_record: &LevelRoomRecord,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = chunk_record.sector_size.max(1);
    let x0 = room_origin_x(chunk_record).saturating_sub(room_origin_x(current_record));
    let z0 = room_origin_z(chunk_record).saturating_sub(room_origin_z(current_record));
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
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

fn axis_moved_at_least(a: i32, b: i32, threshold: i32) -> bool {
    let threshold = threshold.max(0);
    if a >= b {
        a.saturating_sub(b) >= threshold
    } else {
        b.saturating_sub(a) >= threshold
    }
}

pub(super) fn point_xz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold) || axis_moved_at_least(a.z, b.z, threshold)
}

pub(super) fn point_xyz_axis_moved_at_least(a: RoomPoint, b: RoomPoint, threshold: i32) -> bool {
    axis_moved_at_least(a.x, b.x, threshold)
        || axis_moved_at_least(a.y, b.y, threshold)
        || axis_moved_at_least(a.z, b.z, threshold)
}

fn room_bounds(record: &LevelRoomRecord, room: RuntimeRoom<'_>) -> (i32, i32, i32, i32) {
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((room.width() as i32).saturating_mul(record.sector_size));
    let z1 = z0.saturating_add((room.depth() as i32).saturating_mul(record.sector_size));
    (x0, x1, z0, z1)
}

pub(super) fn collect_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
) -> usize {
    let mut count = 0usize;
    for visibility in ROOM_VISIBILITY {
        let Some(record) = ROOMS.get(visibility.room.to_usize()) else {
            continue;
        };
        let first = visibility.cell_first.to_usize();
        let end = first.saturating_add(visibility.cell_count as usize);
        let Some(cells) = VISIBILITY_CELLS.get(first..end) else {
            continue;
        };
        let sector_size = record.sector_size.max(1);
        let room_x0 = room_origin_x(record);
        let room_z0 = room_origin_z(record);
        for cell in cells {
            if cell.flags & visibility_cell_flags::HAS_GEOMETRY == 0 {
                continue;
            }
            let x0 = room_x0.saturating_add((cell.x as i32).saturating_mul(sector_size));
            let z0 = room_z0.saturating_add((cell.z as i32).saturating_mul(sector_size));
            count = push_portal_room_bounds(
                out,
                count,
                visibility.room,
                x0,
                x0.saturating_add(sector_size),
                z0,
                z0.saturating_add(sector_size),
            );
        }
    }
    if count > 0 {
        return count;
    }

    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            let (x0, x1, z0, z1) = chunk_global_bounds(*chunk, record);
            count = push_portal_room_bounds(out, count, chunk.room, x0, x1, z0, z1);
        }
        return count;
    }

    for (raw_index, record) in ROOMS.iter().enumerate() {
        if raw_index >= u16::MAX as usize {
            break;
        }
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        count =
            push_portal_room_bounds(out, count, RoomIndex::new(raw_index as u16), x0, x1, z0, z1);
    }
    count
}

fn push_portal_room_bounds(
    out: &mut [PortalRoomBounds; MAX_PORTAL_ROOM_BOUNDS],
    count: usize,
    room: RoomIndex,
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
) -> usize {
    if count >= out.len() || min_x >= max_x || min_z >= max_z {
        return count;
    }
    out[count] = PortalRoomBounds {
        room,
        min_x,
        max_x,
        min_y: PORTAL_ROOM_BOUNDS_MIN_Y,
        max_y: PORTAL_ROOM_BOUNDS_MAX_Y,
        min_z,
        max_z,
    };
    count + 1
}

pub(super) fn collision_room_collected(
    collected_rooms: &[RoomIndex; MAX_COLLISION_ROOMS],
    count: usize,
    index: RoomIndex,
) -> bool {
    let mut i = 0usize;
    while i < count.min(collected_rooms.len()) {
        if collected_rooms[i] == index {
            return true;
        }
        i += 1;
    }
    false
}

fn room_index_containing_global(point: RoomPoint) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        for chunk in ROOM_CHUNKS {
            let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
                continue;
            };
            if chunk_contains_global_point(*chunk, record, point) {
                return Some(chunk.room);
            }
        }
        return None;
    }
    for (raw_index, record) in ROOMS.iter().enumerate() {
        let Some(room) = parse_runtime_room(record) else {
            continue;
        };
        let (x0, x1, z0, z1) = room_bounds(record, room);
        if point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1 {
            return Some(RoomIndex::new(raw_index as u16));
        }
    }
    None
}

pub(super) fn room_index_containing_global_from(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    if !ROOM_CHUNKS.is_empty() {
        let current_authored = authored_room_for_chunk(current);
        return room_index_containing_global_by_neighbours(current, point).or_else(|| {
            room_index_containing_global_in_authored(point, current_authored).or_else(|| {
                if current_authored.is_none() {
                    room_index_containing_global(point)
                } else {
                    None
                }
            })
        });
    }
    room_index_containing_global(point)
}

fn room_index_containing_global_by_neighbours(
    current: RoomIndex,
    point: RoomPoint,
) -> Option<RoomIndex> {
    let current_authored = authored_room_for_chunk(current);
    // Manual portal rooms can be L-shaped; topology comes from cells, not bboxes.
    let mut queue = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut visited = [INVALID_ROOM_INDEX; MAX_PORTAL_ROOM_BOUNDS];
    let mut head = 0usize;
    let mut tail = 0usize;
    let mut visited_count = 0usize;
    push_room_search(
        current,
        &mut queue,
        &mut tail,
        &mut visited,
        &mut visited_count,
    );

    while head < tail {
        let index = queue[head];
        head += 1;
        if current_authored.is_some() && authored_room_for_chunk(index) != current_authored {
            continue;
        }
        let Some(chunk) = chunk_record_for_room(index) else {
            continue;
        };
        let Some(record) = ROOMS.get(index.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
            return Some(index);
        }
        for neighbour in chunk_neighbours(*chunk) {
            push_room_search(
                neighbour,
                &mut queue,
                &mut tail,
                &mut visited,
                &mut visited_count,
            );
        }
    }
    None
}

fn room_index_containing_global_in_authored(
    point: RoomPoint,
    authored_room: Option<u32>,
) -> Option<RoomIndex> {
    for chunk in ROOM_CHUNKS {
        if authored_room.is_some() && Some(chunk.authored_room) != authored_room {
            continue;
        }
        let Some(record) = ROOMS.get(chunk.room.to_usize()) else {
            continue;
        };
        if chunk_contains_global_point(*chunk, record, point) {
            return Some(chunk.room);
        }
    }
    None
}

fn push_room_search(
    room: RoomIndex,
    queue: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    tail: &mut usize,
    visited: &mut [RoomIndex; MAX_PORTAL_ROOM_BOUNDS],
    visited_count: &mut usize,
) {
    if room == INVALID_ROOM_INDEX || *tail >= queue.len() || *visited_count >= visited.len() {
        return;
    }
    let mut i = 0usize;
    while i < *visited_count {
        if visited[i] == room {
            return;
        }
        i += 1;
    }
    visited[*visited_count] = room;
    *visited_count += 1;
    queue[*tail] = room;
    *tail += 1;
}

fn chunk_neighbours(chunk: LevelChunkRecord) -> [RoomIndex; 4] {
    [
        chunk.neighbours.north,
        chunk.neighbours.east,
        chunk.neighbours.south,
        chunk.neighbours.west,
    ]
}

fn chunk_contains_global_point(
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    if chunk.room.to_usize() >= ROOMS.len() {
        return false;
    }
    match room_visibility_contains_global_point(chunk.room, record, point) {
        Some(contains) => contains,
        None => chunk_bounds_contains_global_point(chunk, record, point),
    }
}

fn chunk_bounds_contains_global_point(
    chunk: LevelChunkRecord,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> bool {
    let (x0, x1, z0, z1) = chunk_global_bounds(chunk, record);
    point.x >= x0 && point.x < x1 && point.z >= z0 && point.z < z1
}

fn room_visibility_contains_global_point(
    room: RoomIndex,
    record: &LevelRoomRecord,
    point: RoomPoint,
) -> Option<bool> {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let local_x = point.x.checked_sub(x0)?;
    let local_z = point.z.checked_sub(z0)?;
    if local_x < 0 || local_z < 0 {
        return Some(false);
    }
    let sx_raw = local_x / sector_size;
    let sz_raw = local_z / sector_size;
    if sx_raw > u16::MAX as i32 || sz_raw > u16::MAX as i32 {
        return Some(false);
    }
    let sx = sx_raw as u16;
    let sz = sz_raw as u16;
    room_visibility_contains_cell(room, sx, sz)
}

fn room_visibility_contains_cell(room: RoomIndex, sx: u16, sz: u16) -> Option<bool> {
    let visibility = ROOM_VISIBILITY
        .iter()
        .find(|visibility| visibility.room == room)?;
    let first = visibility.cell_first.to_usize();
    let count = visibility.cell_count as usize;
    let cells = VISIBILITY_CELLS.get(first..first.checked_add(count)?)?;
    let mut i = 0usize;
    while i < cells.len() {
        let cell = cells[i];
        if cell.room == room && cell.x == sx && cell.z == sz {
            return Some(cell.flags & visibility_cell_flags::HAS_GEOMETRY != 0);
        }
        i += 1;
    }
    Some(false)
}

fn chunk_global_bounds(chunk: LevelChunkRecord, record: &LevelRoomRecord) -> (i32, i32, i32, i32) {
    let sector_size = record.sector_size.max(1);
    let x0 = room_origin_x(record);
    let z0 = room_origin_z(record);
    let x1 = x0.saturating_add((chunk.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((chunk.depth as i32).saturating_mul(sector_size));
    (x0, x1, z0, z1)
}

pub(super) fn local_to_global_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_add(room_origin_x(record)),
        point.y.saturating_add(room_origin_y(record)),
        point.z.saturating_add(room_origin_z(record)),
    )
}

pub(super) fn global_to_local_room_point(room: RoomIndex, point: RoomPoint) -> RoomPoint {
    let Some(record) = ROOMS.get(room.to_usize()) else {
        return point;
    };
    RoomPoint::new(
        point.x.saturating_sub(room_origin_x(record)),
        point.y.saturating_sub(room_origin_y(record)),
        point.z.saturating_sub(room_origin_z(record)),
    )
}

pub(super) fn camera_for_room(camera: WorldCamera, active: ActiveRuntimeRoom) -> WorldCamera {
    WorldCamera::from_basis(
        camera.projection,
        WorldVertex::new(
            camera.position.x.saturating_sub(active.offset_x),
            camera.position.y.saturating_sub(active.offset_y),
            camera.position.z.saturating_sub(active.offset_z),
        ),
        camera.sin_yaw,
        camera.cos_yaw,
        camera.sin_pitch,
        camera.cos_pitch,
    )
}

pub(super) fn active_room_overlaps_collision_window(
    active: ActiveRuntimeRoom,
    anchor: RoomPoint,
    margin: i32,
) -> bool {
    let sector_size = active.sector_size.max(1);
    let x0 = active.offset_x;
    let z0 = active.offset_z;
    let x1 = x0.saturating_add((active.width as i32).saturating_mul(sector_size));
    let z1 = z0.saturating_add((active.depth as i32).saturating_mul(sector_size));
    let margin = margin.max(0);
    anchor.x.saturating_add(margin) >= x0
        && anchor.x.saturating_sub(margin) < x1
        && anchor.z.saturating_add(margin) >= z0
        && anchor.z.saturating_sub(margin) < z1
}
