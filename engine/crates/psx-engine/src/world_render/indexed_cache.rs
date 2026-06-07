use super::*;

#[cfg(feature = "room-surface-profile")]
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct RoomSurfaceMicroProfile {
    submit_detail: TexturedGouraudSubmitMicroProfile,
    material_cycles: u32,
    projected_cycles: u32,
    screen_cycles: u32,
    kind_cycles: u32,
    backface_cycles: u32,
    lighting_cycles: u32,
    submit_cycles: u32,
    profiled: u32,
    material_misses: u32,
    projected_rejects: u32,
    screen_culled: u32,
    backface_culled: u32,
    floors: u32,
    ceilings: u32,
    walls: u32,
    whole_quads: u32,
    split_tris: u32,
    lighting_rejects: u32,
}

#[cfg(not(feature = "room-surface-profile"))]
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct RoomSurfaceMicroProfile;

impl RoomSurfaceMicroProfile {
    #[inline(always)]
    fn new() -> Self {
        #[cfg(feature = "room-surface-profile")]
        {
            Self::default()
        }
        #[cfg(not(feature = "room-surface-profile"))]
        {
            Self
        }
    }

    #[inline(always)]
    fn cycle() -> u32 {
        #[cfg(feature = "room-surface-profile")]
        {
            crate::telemetry::cycle_counter()
        }
        #[cfg(not(feature = "room-surface-profile"))]
        {
            0
        }
    }

    #[inline(always)]
    fn elapsed(start: u32) -> u32 {
        Self::cycle().wrapping_sub(start)
    }

    #[inline(always)]
    fn add_material(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.material_cycles = self.material_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_projected(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.projected_cycles = self.projected_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_screen(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.screen_cycles = self.screen_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_kind(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.kind_cycles = self.kind_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_backface(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.backface_cycles = self.backface_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_lighting(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.lighting_cycles = self.lighting_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_submit(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.submit_cycles = self.submit_cycles.saturating_add(_cycles);
        }
    }

    #[cfg(feature = "room-surface-profile")]
    #[inline(always)]
    pub(super) fn submit_profile(&mut self) -> &mut TexturedGouraudSubmitMicroProfile {
        &mut self.submit_detail
    }

    #[inline(always)]
    fn count_profiled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.profiled = self.profiled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_material_miss(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.material_misses = self.material_misses.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_projected_reject(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.projected_rejects = self.projected_rejects.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_screen_culled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.screen_culled = self.screen_culled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_backface_culled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.backface_culled = self.backface_culled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_lighting_reject(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.lighting_rejects = self.lighting_rejects.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_kind(&mut self, _kind: WorldSurfaceKind) {
        #[cfg(feature = "room-surface-profile")]
        {
            match _kind {
                WorldSurfaceKind::Floor => self.floors = self.floors.saturating_add(1),
                WorldSurfaceKind::Ceiling => self.ceilings = self.ceilings.saturating_add(1),
                WorldSurfaceKind::Wall { .. } => self.walls = self.walls.saturating_add(1),
            }
        }
    }

    #[inline(always)]
    fn count_shape(&mut self, _triangle_index: u8) {
        #[cfg(feature = "room-surface-profile")]
        {
            if _triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                self.split_tris = self.split_tris.saturating_add(1);
            } else {
                self.whole_quads = self.whole_quads.saturating_add(1);
            }
        }
    }

    #[inline(always)]
    fn emit(self) {
        #[cfg(feature = "room-surface-profile")]
        {
            use crate::telemetry;
            telemetry::counter(
                telemetry::counter::ROOM_SURF_MATERIAL_CYCLES,
                self.material_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_PROJECTED_CYCLES,
                self.projected_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SCREEN_CYCLES,
                self.screen_cycles,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_KIND_CYCLES, self.kind_cycles);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_BACKFACE_CYCLES,
                self.backface_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_LIGHTING_CYCLES,
                self.lighting_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SUBMIT_CYCLES,
                self.submit_cycles,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_PROFILED, self.profiled);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_MATERIAL_MISSES,
                self.material_misses,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_PROJECTED_REJECTS,
                self.projected_rejects,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SCREEN_CULLED,
                self.screen_culled,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_BACKFACE_CULLED,
                self.backface_culled,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_FLOORS, self.floors);
            telemetry::counter(telemetry::counter::ROOM_SURF_CEILINGS, self.ceilings);
            telemetry::counter(telemetry::counter::ROOM_SURF_WALLS, self.walls);
            telemetry::counter(telemetry::counter::ROOM_SURF_WHOLE_QUADS, self.whole_quads);
            telemetry::counter(telemetry::counter::ROOM_SURF_SPLIT_TRIS, self.split_tris);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_LIGHTING_REJECTS,
                self.lighting_rejects,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES,
                self.submit_detail.hw_safe_test_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PACKET_FILL_CYCLES,
                self.submit_detail.packet_fill_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES,
                self.submit_detail.primitive_push_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_DEPTH_CYCLES,
                self.submit_detail.depth_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_COMMAND_CYCLES,
                self.submit_detail.command_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_FALLBACK_CYCLES,
                self.submit_detail.fallback_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_HW_SAFE_CALLS,
                self.submit_detail.hw_safe_calls,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_FALLBACK_CALLS,
                self.submit_detail.fallback_calls,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_COMMAND_OVERFLOWS,
                self.submit_detail.command_overflows,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS,
                self.submit_detail.primitive_overflows,
            );
        }
    }
}

/// Draw a cached vertex-lit room using a deduplicated cached vertex
/// stream. The projected scratch slices must be at least as long as
/// `cached_vertices`.
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed_cached_room_vertex_lit_visible_cells<
    const OT: usize,
    L: WorldSurfaceLighting,
>(
    cached_cells: &[CachedRoomCell],
    cached_cell_vertices: &[u16],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_indices: &mut [u16],
    projected_vertices: &mut [crate::render3d::ProjectedVertex],
    projected_ready: &mut [bool],
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    _room_depth: u16,
    _sector_size: i32,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    visible_cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_ready.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < visible_cells.len()
        || accepted_cell_depths.len() < visible_cells.len()
    {
        return stats;
    }
    if visible_cells.is_empty() {
        return stats;
    }

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;
    let mut accepted_depths_need_sort = false;
    // Per-cell depth/cull transforms run on the GTE (MVMVA) via the loaded
    // camera instead of redoing the camera rotation in CPU fixed-point for
    // every candidate cell; matches the rounding of the GTE vertex projection.
    let loaded_camera = LoadedWorldCameraGte::load(*camera);

    for visible in visible_cells.iter().copied() {
        let Some(cell_index) = cached_room_cell_index_for_visible(cached_cells, visible) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index).copied() else {
            continue;
        };

        stats.cells_considered = stats.cells_considered.wrapping_add(1);
        let cell_depth = if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_PRECULLED {
            let visibility_center = WorldVertex::new(
                cell.visibility_center[0],
                cell.visibility_center[1],
                cell.visibility_center[2],
            );
            loaded_camera.view_vertex(visibility_center).z
        } else if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_UNKNOWN {
            let visibility_center = WorldVertex::new(
                cell.visibility_center[0],
                cell.visibility_center[1],
                cell.visibility_center[2],
            );
            let visibility_view = loaded_camera.view_vertex(visibility_center);
            if !cell_visibility_view_visible_to_camera(
                camera,
                options,
                visibility_view,
                cell.visibility_radius,
                screen_margin,
            ) {
                stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                continue;
            }
            accepted_depths_need_sort = true;
            visibility_view.z
        } else {
            visible.camera_depth as i32
        };

        stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
        accepted_cell_indices[accepted_cell_count] = cell_index as u16;
        accepted_cell_depths[accepted_cell_count] = cell_depth;
        accepted_cell_count += 1;
        projected_index_count = collect_cached_cell_vertex_indices(
            cell,
            cached_cell_vertices,
            cached_surfaces,
            projected_ready,
            projected_indices,
            projected_index_count,
        );
    }
    if accepted_depths_need_sort {
        sort_cached_room_cell_indices_by_depth(
            &mut accepted_cell_indices[..accepted_cell_count],
            &mut accepted_cell_depths[..accepted_cell_count],
        );
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_CELL_SELECT);

    let projected_indices = &projected_indices[..projected_index_count];
    stats.projected_vertices = projected_index_count.min(u16::MAX as usize) as u16;
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_PROJECT);
    project_world_vertex_indices_gte(
        *camera,
        cached_vertices,
        projected_indices,
        projected_vertices,
    );
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_PROJECT);
    if use_vertex_depths {
        crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_DEPTH_PREP);
        for raw_index in projected_indices {
            let index = *raw_index as usize;
            projected_depths[index] = lighting.prepare_vertex_depth(projected_vertices[index].sz);
        }
        crate::telemetry::stage_end(crate::telemetry::stage::ROOM_DEPTH_PREP);
    }

    let mut surface_profile = RoomSurfaceMicroProfile::new();
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    for accepted_index in 0..accepted_cell_count {
        let Some(&cell_index) = accepted_cell_indices.get(accepted_index) else {
            continue;
        };
        let Some(&cell_depth) = accepted_cell_depths.get(accepted_index) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .wrapping_add(draw_indexed_cached_room_surface(
                        cached_surfaces[i],
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        screen_bounds,
                        materials,
                        lighting,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    for raw_index in projected_indices {
        if let Some(ready) = projected_ready.get_mut(*raw_index as usize) {
            *ready = false;
        }
    }
    stats
}

/// Draw every populated cell from a cached vertex-lit room.
///
/// This bypasses cooked visible-cell/PVS filtering after the caller has
/// already selected an active chunk. Cells are still depth-sorted for the
/// ordering-table painter path, and surfaces still run the usual projection,
/// screen, near-plane, and backface checks.
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed_cached_room_vertex_lit_all_cells<const OT: usize, L: WorldSurfaceLighting>(
    cached_cells: &[CachedRoomCell],
    cached_cell_vertices: &[u16],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_indices: &mut [u16],
    projected_vertices: &mut [ProjectedVertex],
    projected_ready: &mut [bool],
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    screen_margin: i32,
    cull_cells_laterally: bool,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_ready.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < cached_cells.len()
        || accepted_cell_depths.len() < cached_cells.len()
    {
        return stats;
    }
    if cached_cells.is_empty() || cached_surfaces.is_empty() {
        return stats;
    }

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;
    let loaded_camera = LoadedWorldCameraGte::load(*camera);

    for (cell_index, cell) in cached_cells.iter().copied().enumerate() {
        if cell.surface_count == 0 || cell_index > u16::MAX as usize {
            continue;
        }
        stats.cells_considered = stats.cells_considered.wrapping_add(1);
        let visibility_center = WorldVertex::new(
            cell.visibility_center[0],
            cell.visibility_center[1],
            cell.visibility_center[2],
        );
        let visibility_view = loaded_camera.view_vertex(visibility_center);
        if cull_cells_laterally
            && !cell_visibility_view_in_lateral_frustum(
                camera,
                visibility_view,
                cell.visibility_radius,
                screen_margin,
            )
        {
            stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
            continue;
        }
        stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
        accepted_cell_indices[accepted_cell_count] = cell_index as u16;
        accepted_cell_depths[accepted_cell_count] = visibility_view.z;
        accepted_cell_count += 1;
    }
    sort_cached_room_cell_indices_by_depth(
        &mut accepted_cell_indices[..accepted_cell_count],
        &mut accepted_cell_depths[..accepted_cell_count],
    );
    for &cell_index in &accepted_cell_indices[..accepted_cell_count] {
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        projected_index_count = collect_cached_cell_vertex_indices(
            cell,
            cached_cell_vertices,
            cached_surfaces,
            projected_ready,
            projected_indices,
            projected_index_count,
        );
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_CELL_SELECT);

    let projected_indices = &projected_indices[..projected_index_count];
    stats.projected_vertices = projected_index_count.min(u16::MAX as usize) as u16;
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_PROJECT);
    project_world_vertex_indices_gte(
        *camera,
        cached_vertices,
        projected_indices,
        projected_vertices,
    );
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_PROJECT);
    if use_vertex_depths {
        crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_DEPTH_PREP);
        for raw_index in projected_indices {
            let index = *raw_index as usize;
            projected_depths[index] = lighting.prepare_vertex_depth(projected_vertices[index].sz);
        }
        crate::telemetry::stage_end(crate::telemetry::stage::ROOM_DEPTH_PREP);
    }

    let mut surface_profile = RoomSurfaceMicroProfile::new();
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    for accepted_index in 0..accepted_cell_count {
        let Some(&cell_index) = accepted_cell_indices.get(accepted_index) else {
            continue;
        };
        let Some(&cell_depth) = accepted_cell_depths.get(accepted_index) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .wrapping_add(draw_indexed_cached_room_surface(
                        cached_surfaces[i],
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        screen_bounds,
                        materials,
                        lighting,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    for raw_index in projected_indices {
        if let Some(ready) = projected_ready.get_mut(*raw_index as usize) {
            *ready = false;
        }
    }
    stats
}

fn sort_cached_room_cell_indices_by_depth(indices: &mut [u16], depths: &mut [i32]) {
    if indices.len() > depths.len() {
        return;
    }
    let mut gap = indices.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < indices.len() {
            let index = indices[i];
            let depth = depths[i];
            let mut j = i;
            while j >= gap && depths[j - gap] < depth {
                indices[j] = indices[j - gap];
                depths[j] = depths[j - gap];
                j -= gap;
            }
            indices[j] = index;
            depths[j] = depth;
            i += 1;
        }
        gap /= 2;
    }
}

fn cached_room_cell_index(cells: &[CachedRoomCell], x: u16, z: u16) -> Option<usize> {
    let key = cached_room_cell_key(x, z);
    let mut low = 0usize;
    let mut high = cells.len();
    while low < high {
        let mid = (low + high) / 2;
        let cell = cells[mid];
        let cell_key = cached_room_cell_key(cell.x, cell.z);
        if cell_key < key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let cell = cells.get(low).copied()?;
    (cached_room_cell_key(cell.x, cell.z) == key && cell.surface_count != 0).then_some(low)
}

#[inline(always)]
fn cached_room_cell_index_for_visible(
    cells: &[CachedRoomCell],
    visible: GridVisibleCell,
) -> Option<usize> {
    if visible.cache_cell_index != GridVisibleCell::CACHE_CELL_INDEX_UNKNOWN {
        let index = visible.cache_cell_index as usize;
        let cell = *cells.get(index)?;
        if cell.x == visible.x && cell.z == visible.z && cell.surface_count != 0 {
            return Some(index);
        }
    }
    cached_room_cell_index(cells, visible.x, visible.z)
}

fn collect_cached_cell_vertex_indices(
    cell: CachedRoomCell,
    cached_cell_vertices: &[u16],
    cached_surfaces: &[CachedRoomSurface],
    projected_ready: &mut [bool],
    projected_indices: &mut [u16],
    mut projected_index_count: usize,
) -> usize {
    if cell.vertex_count == 0 {
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut surface_index = first;
        while surface_index < end {
            for raw_index in cached_surfaces[surface_index].vertex_indices {
                projected_index_count = push_unique_projected_index(
                    raw_index,
                    projected_ready,
                    projected_indices,
                    projected_index_count,
                );
            }
            surface_index += 1;
        }
        return projected_index_count;
    }
    let first = cell.vertex_first as usize;
    let end = first
        .saturating_add(cell.vertex_count as usize)
        .min(cached_cell_vertices.len());
    let mut i = first;
    while i < end {
        projected_index_count = push_unique_projected_index(
            cached_cell_vertices[i],
            projected_ready,
            projected_indices,
            projected_index_count,
        );
        i += 1;
    }
    projected_index_count
}

fn push_unique_projected_index(
    raw_index: u16,
    projected_ready: &mut [bool],
    projected_indices: &mut [u16],
    projected_index_count: usize,
) -> usize {
    let vertex_index = raw_index as usize;
    if vertex_index < projected_ready.len()
        && !projected_ready[vertex_index]
        && projected_index_count < projected_indices.len()
    {
        projected_ready[vertex_index] = true;
        projected_indices[projected_index_count] = raw_index;
        projected_index_count + 1
    } else {
        projected_index_count
    }
}

const fn cached_room_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn draw_indexed_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    projected_vertices: &[ProjectedVertex],
    projected_depths: &[i32],
    use_vertex_depths: bool,
    use_direct_baked_rgb: bool,
    screen_bounds: ProjectedScreenBounds,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    options: WorldSurfaceOptions,
    submit_depths: CachedRoomSubmitDepths,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
    profile: &mut RoomSurfaceMicroProfile,
) -> u16 {
    profile.count_profiled();
    profile.count_shape(surface.triangle_index);
    let projected_start = RoomSurfaceMicroProfile::cycle();
    let ids = surface.vertex_indices;
    let (projected, vertex_depths) = if use_vertex_depths {
        let Some((projected, depths)) =
            indexed_projected_quad_with_depths(projected_vertices, projected_depths, ids)
        else {
            profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
            profile.count_projected_reject();
            return 0;
        };
        (projected, Some(depths))
    } else {
        let Some(projected) = indexed_projected_quad(projected_vertices, ids) else {
            profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
            profile.count_projected_reject();
            return 0;
        };
        (projected, None)
    };
    profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
    let screen_start = RoomSurfaceMicroProfile::cycle();
    if projected_quad_outside_screen(projected, screen_bounds) {
        profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
        profile.count_screen_culled();
        return 1;
    }
    profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
    let kind_start = RoomSurfaceMicroProfile::cycle();
    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    profile.add_kind(RoomSurfaceMicroProfile::elapsed(kind_start));
    profile.count_kind(kind);
    let material_start = RoomSurfaceMicroProfile::cycle();
    let Some(&material) = materials.get(surface.material_slot as usize) else {
        profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
        profile.count_material_miss();
        return 0;
    };
    let material = cached_uv_material(material);
    profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            let is_ceiling = matches!(kind, WorldSurfaceKind::Ceiling);
            let use_triangle_depth =
                cached_surface_uses_triangle_depth(depth_mode, kind, surface, projected);
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (horizontal_depth_options(options), submit_depths.horizontal)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                use_triangle_depth,
                kind,
                surface,
                projected,
            );
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_split_triangle_backface_culled(
                    projected,
                    material,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    is_ceiling,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    ids,
                    vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    surface.uv_words,
                    colors,
                    material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    is_ceiling,
                    triangles,
                    world,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            } else {
                let projected_for_cull = if is_ceiling {
                    reverse_quad_winding(projected)
                } else {
                    projected
                };
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_quad_backface_culled(
                    projected_for_cull,
                    material,
                    CullMode::Back,
                    split_triangles_runtime(surface.split),
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    ids,
                    vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let (projected, uv_words, colors) = if is_ceiling {
                    (
                        reverse_quad_winding(projected),
                        reverse_quad_winding(surface.uv_words),
                        reverse_quad_winding(colors),
                    )
                } else {
                    (projected, surface.uv_words, colors)
                };
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_sided_projected_gouraud_quad_cached_uv_words(
                    world,
                    triangles,
                    projected,
                    uv_words,
                    colors,
                    material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    surface.split,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
        WorldSurfaceKind::Wall { direction } => {
            let wall_material = wall_material_for_direction(material, direction);
            let use_triangle_depth =
                cached_surface_uses_triangle_depth(depth_mode, kind, surface, projected);
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (options, submit_depths.vertical)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                use_triangle_depth,
                kind,
                surface,
                projected,
            );
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_split_triangle_backface_culled(
                    projected,
                    wall_material,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    ids,
                    vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    surface.uv_words,
                    colors,
                    wall_material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                    triangles,
                    world,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            } else {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_quad_backface_culled(
                    projected,
                    wall_material,
                    CullMode::Back,
                    SPLIT_NW_SE_TRIANGLES,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    ids,
                    vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_sided_projected_gouraud_quad_cached_uv_words(
                    world,
                    triangles,
                    projected,
                    surface.uv_words,
                    colors,
                    wall_material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    SPLIT_NW_SE,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
    }
    1
}

#[inline(always)]
fn indexed_world_quad(vertices: &[WorldVertex], ids: [u16; 4]) -> Option<[WorldVertex; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= vertices.len() {
        return None;
    }
    // SAFETY: `max_index < vertices.len()` proves every id is in range.
    unsafe {
        Some([
            *vertices.get_unchecked(a),
            *vertices.get_unchecked(b),
            *vertices.get_unchecked(c),
            *vertices.get_unchecked(d),
        ])
    }
}

pub(super) fn cached_surface_uses_triangle_depth(
    mode: CachedRoomDepthMode,
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    match mode {
        CachedRoomDepthMode::FixedCell => false,
        CachedRoomDepthMode::PerTriangle => true,
        CachedRoomDepthMode::Hybrid => match kind {
            WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
                cached_horizontal_surface_is_risky(surface, projected)
            }
            WorldSurfaceKind::Wall { .. } => false,
        },
        CachedRoomDepthMode::HybridWalls => cached_surface_is_risky(kind, surface, projected),
    }
}

fn cached_surface_subdivision_options(
    options: WorldSurfaceOptions,
    mode: CachedRoomSubdivisionMode,
    use_triangle_depth: bool,
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> WorldSurfaceOptions {
    let allow_visual_subdivision = match mode {
        CachedRoomSubdivisionMode::All => true,
        CachedRoomSubdivisionMode::DepthSorted => use_triangle_depth,
        CachedRoomSubdivisionMode::Risky => cached_surface_is_risky(kind, surface, projected),
    };
    if allow_visual_subdivision {
        options
    } else {
        options.with_textured_triangle_max_edge(0)
    }
}

fn cached_surface_is_risky(
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            cached_horizontal_surface_is_risky(surface, projected)
        }
        WorldSurfaceKind::Wall { .. } => {
            cached_surface_projected_depth_span(surface, projected) >= HYBRID_HORIZONTAL_DEPTH_SPAN
        }
    }
}

fn cached_horizontal_surface_is_risky(
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    if surface.kind_flags & CACHED_SURFACE_HORIZONTAL_NON_FLAT != 0 {
        return true;
    }
    cached_surface_projected_depth_span(surface, projected) >= HYBRID_HORIZONTAL_DEPTH_SPAN
}

fn cached_surface_projected_depth_span(
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> i32 {
    if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        let (a, b, c) = split_triangles_runtime(surface.split)[surface.triangle_index as usize];
        let min_z = projected[a].sz.min(projected[b].sz).min(projected[c].sz);
        let max_z = projected[a].sz.max(projected[b].sz).max(projected[c].sz);
        return max_z.saturating_sub(min_z);
    }
    let min_z = projected[0]
        .sz
        .min(projected[1].sz)
        .min(projected[2].sz)
        .min(projected[3].sz);
    let max_z = projected[0]
        .sz
        .max(projected[1].sz)
        .max(projected[2].sz)
        .max(projected[3].sz);
    max_z.saturating_sub(min_z)
}

pub(super) fn cached_surface_center(
    vertices: [WorldVertex; 4],
    split: u8,
    triangle_index: u8,
) -> RoomPoint {
    if triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        let (a, b, c) = split_triangles_runtime(split)[triangle_index as usize];
        return RoomPoint::new(
            (vertices[a].x + vertices[b].x + vertices[c].x) / 3,
            (vertices[a].y + vertices[b].y + vertices[c].y) / 3,
            (vertices[a].z + vertices[b].z + vertices[c].z) / 3,
        );
    }
    RoomPoint::new(
        average4_i32(vertices[0].x, vertices[1].x, vertices[2].x, vertices[3].x),
        average4_i32(vertices[0].y, vertices[1].y, vertices[2].y, vertices[3].y),
        average4_i32(vertices[0].z, vertices[1].z, vertices[2].z, vertices[3].z),
    )
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ProjectedScreenBounds {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

fn projected_screen_bounds(camera: &WorldCamera, margin: i32) -> ProjectedScreenBounds {
    let margin = margin.max(0);
    ProjectedScreenBounds {
        left: -margin,
        right: (camera.projection.screen_x as i32)
            .saturating_mul(2)
            .saturating_add(margin),
        top: -margin,
        bottom: (camera.projection.screen_y as i32)
            .saturating_mul(2)
            .saturating_add(margin),
    }
}

#[inline(always)]
fn projected_quad_outside_screen(
    projected: [ProjectedVertex; 4],
    bounds: ProjectedScreenBounds,
) -> bool {
    let min_x = projected[0]
        .sx
        .min(projected[1].sx)
        .min(projected[2].sx)
        .min(projected[3].sx) as i32;
    let max_x = projected[0]
        .sx
        .max(projected[1].sx)
        .max(projected[2].sx)
        .max(projected[3].sx) as i32;
    let min_y = projected[0]
        .sy
        .min(projected[1].sy)
        .min(projected[2].sy)
        .min(projected[3].sy) as i32;
    let max_y = projected[0]
        .sy
        .max(projected[1].sy)
        .max(projected[2].sy)
        .max(projected[3].sy) as i32;
    max_x < bounds.left || min_x > bounds.right || max_y < bounds.top || min_y > bounds.bottom
}

#[inline(always)]
fn indexed_projected_quad(
    projected_vertices: &[ProjectedVertex],
    ids: [u16; 4],
) -> Option<[ProjectedVertex; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= projected_vertices.len() {
        return None;
    }
    // SAFETY: `max_index < projected_vertices.len()` proves every id is in range.
    let projected = unsafe {
        [
            *projected_vertices.get_unchecked(a),
            *projected_vertices.get_unchecked(b),
            *projected_vertices.get_unchecked(c),
            *projected_vertices.get_unchecked(d),
        ]
    };
    if !projected[0].is_valid()
        || !projected[1].is_valid()
        || !projected[2].is_valid()
        || !projected[3].is_valid()
    {
        return None;
    }
    Some(projected)
}

#[inline(always)]
fn indexed_projected_quad_with_depths(
    projected_vertices: &[ProjectedVertex],
    depths: &[i32],
    ids: [u16; 4],
) -> Option<([ProjectedVertex; 4], [i32; 4])> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= projected_vertices.len() || max_index >= depths.len() {
        return None;
    }
    // SAFETY: the max-index checks prove every id is in range for both slices.
    let (projected, depths) = unsafe {
        (
            [
                *projected_vertices.get_unchecked(a),
                *projected_vertices.get_unchecked(b),
                *projected_vertices.get_unchecked(c),
                *projected_vertices.get_unchecked(d),
            ],
            [
                *depths.get_unchecked(a),
                *depths.get_unchecked(b),
                *depths.get_unchecked(c),
                *depths.get_unchecked(d),
            ],
        )
    };
    if !projected[0].is_valid()
        || !projected[1].is_valid()
        || !projected[2].is_valid()
        || !projected[3].is_valid()
    {
        return None;
    }
    Some((projected, depths))
}

#[inline(always)]
fn indexed_vertex_lighting_colors<L: WorldSurfaceLighting>(
    lighting: &L,
    surface: CachedRoomSurface,
    material: WorldRenderMaterial,
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    vertex_depths: Option<[i32; 4]>,
    use_direct_baked_rgb: bool,
) -> Option<[(u8, u8, u8); 4]> {
    let has_baked_rgb = surface.has_baked_rgb();
    if use_direct_baked_rgb && has_baked_rgb {
        return Some(surface.baked_vertex_rgb);
    }
    if has_baked_rgb {
        let sample = surface.sample_without_center();
        if let Some(colors) = lighting.shade_cached_baked_vertices(sample, vertex_depths, material)
        {
            return Some(colors);
        }
    }

    let vertices = indexed_world_quad(cached_vertices, ids)?;
    let sample = surface.sample_with_center(
        vertices,
        lighting.needs_surface_sample_center(has_baked_rgb),
    );
    if let Some(depths) = vertex_depths {
        return Some(vertex_lighting_colors_with_depths(
            lighting, sample, material, vertices, depths,
        ));
    }
    Some(vertex_lighting_colors(lighting, sample, material, vertices))
}

#[inline(always)]
fn projected_split_triangle_backface_culled(
    projected: [ProjectedVertex; 4],
    material: WorldRenderMaterial,
    base_cull: CullMode,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
) -> bool {
    if cull_for_sidedness(material.sidedness, base_cull) != CullMode::Back {
        return false;
    }
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    projected_quad_triangle_back_facing(projected, tri)
}

#[inline(always)]
fn projected_quad_backface_culled(
    projected: [ProjectedVertex; 4],
    material: WorldRenderMaterial,
    base_cull: CullMode,
    split_triangles: [(usize, usize, usize); 2],
) -> bool {
    if cull_for_sidedness(material.sidedness, base_cull) != CullMode::Back {
        return false;
    }
    let projected = if material.sidedness == SurfaceSidedness::Back {
        reverse_quad_winding(projected)
    } else {
        projected
    };
    let [(a, b, c), (d, e, f)] = split_triangles;
    projected_quad_triangle_back_facing(projected, (a, b, c))
        && projected_quad_triangle_back_facing(projected, (d, e, f))
}

#[inline(always)]
fn projected_quad_triangle_back_facing(
    projected: [ProjectedVertex; 4],
    tri: (usize, usize, usize),
) -> bool {
    let (a, b, c) = tri;
    projected_triangle_back_facing([projected[a], projected[b], projected[c]])
}

#[inline(always)]
fn projected_triangle_back_facing(verts: [ProjectedVertex; 3]) -> bool {
    psx_gte::scene::screen_triangle_back_facing([
        (verts[0].sx, verts[0].sy),
        (verts[1].sx, verts[1].sy),
        (verts[2].sx, verts[2].sy),
    ])
}

const fn cached_uv_material(mut material: WorldRenderMaterial) -> WorldRenderMaterial {
    material.texture_width = ROOM_TEXTURE_UV_SIZE;
    material.texture_height = ROOM_TEXTURE_UV_SIZE;
    material
}
