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
    tr_subdivision_candidates: u32,
    tr_subdivision_submitted: u32,
    /// E2: cycles spent rebuilding the per-surface `WorldSurfaceOptions`
    /// variants. This work sits between the timed lighting and submit
    /// sections, so it was inside the unattributed ~48% of the stage.
    options_cycles: u32,
    /// E2: per-cell setup ahead of the surface loop (tile depth options plus
    /// the submit-depth table). Charged once per accepted cell.
    cell_setup_cycles: u32,
    /// E2: the whole `draw_indexed_cached_room_surface` call. Subtracting the
    /// inner timed sections leaves the part of the surface body that no
    /// counter reaches; subtracting this and `cell_setup` from the stage
    /// leaves the loop's own overhead.
    surface_call_cycles: u32,
    /// Warp probe: predicted affine texture error over the surfaces the
    /// depth-band rule subdivided, and over the ones it left alone. Count,
    /// sum and max (1/16 texel units) rather than buckets, so the mean and
    /// the worst case both survive.
    warp_subdivided: WarpStats,
    warp_untouched: WarpStats,
}

/// Predicted-warp accumulator for one side of the subdivide/skip split.
#[cfg(feature = "room-surface-profile")]
#[derive(Copy, Clone, Debug, Default)]
pub(super) struct WarpStats {
    /// Surfaces observed.
    count: u32,
    /// Sum of predicted error, 1/16 texel units.
    sum: u32,
    /// Worst predicted error, 1/16 texel units.
    max: u32,
    /// Surfaces predicted to warp under one texel.
    under_1tx: u32,
}

/// Predicted affine texture error for a surface, in 1/16 texel units.
///
/// This is the closed form measured in `docs/texture-warping-2026-07-27.md`:
/// for an edge spanning `du` texels between depths `za` and `zb`, affine
/// interpolation lands the screen midpoint off by
/// `du * |zb - za| / (2 * (za + zb))` texels. That bench found the true
/// worst-case error over the polygon runs ~2.4x the edge-midpoint value, so
/// the result is scaled by 12/5 to be a bound rather than a sample.
///
/// Integer throughout, and ordered to stay inside i32: `dz <= zsum` always
/// holds for positive depths, so the `dz << 8` ratio is bounded by 256.
#[cfg(feature = "room-surface-profile")]
fn predicted_warp_16ths(projected: [ProjectedVertex; 4], uv_words: [u16; 4]) -> u32 {
    // Quad layout is v0,v1,v2,v3 -> triangles (0,1,2) and (1,2,3), i.e. a
    // tl/tr/bl/br lattice. Check both axes on both sides; the surface's warp
    // is the worst edge.
    const EDGES: [(usize, usize); 4] = [(0, 1), (2, 3), (0, 2), (1, 3)];
    let mut worst = 0u32;
    for (a, b) in EDGES {
        let (za, zb) = (projected[a].sz, projected[b].sz);
        if za <= 0 || zb <= 0 {
            continue; // behind the near plane; projection is meaningless here
        }
        let zsum = (za + zb) as u32;
        let dz = (za - zb).unsigned_abs();
        let (ua, va) = (uv_words[a] as u8, (uv_words[a] >> 8) as u8);
        let (ub, vb) = (uv_words[b] as u8, (uv_words[b] >> 8) as u8);
        let du = (ua.abs_diff(ub)).max(va.abs_diff(vb)) as u32;
        // err * 16 = 8 * du * dz / zsum = du * ((dz << 8) / zsum) / 32
        let ratio = (dz << 8) / zsum.max(1);
        let err = (du * ratio) >> 5;
        worst = worst.max(err * 12 / 5);
    }
    worst
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
    fn add_cell_setup(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.cell_setup_cycles = self.cell_setup_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_surface_call(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.surface_call_cycles = self.surface_call_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_options(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.options_cycles = self.options_cycles.saturating_add(_cycles);
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
    fn count_tr_subdivision_candidate(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.tr_subdivision_candidates = self.tr_subdivision_candidates.saturating_add(1);
        }
    }

    /// Warp probe: record what the closed form would have decided for this
    /// surface, alongside what the depth-band rule actually did. Read-only;
    /// it changes no geometry. The point is to find out, on real content,
    /// how often the two disagree in each direction.
    #[inline(always)]
    fn count_warp(
        &mut self,
        _projected: [ProjectedVertex; 4],
        _uv_words: [u16; 4],
        _subdivided: bool,
    ) {
        #[cfg(feature = "room-surface-profile")]
        {
            let err = predicted_warp_16ths(_projected, _uv_words);
            let stats = if _subdivided {
                &mut self.warp_subdivided
            } else {
                &mut self.warp_untouched
            };
            stats.count = stats.count.saturating_add(1);
            stats.sum = stats.sum.saturating_add(err);
            stats.max = stats.max.max(err);
            if err < 16 {
                stats.under_1tx = stats.under_1tx.saturating_add(1);
            }
        }
    }

    #[inline(always)]
    fn count_tr_subdivision_submitted(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.tr_subdivision_submitted = self.tr_subdivision_submitted.saturating_add(1);
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
            telemetry::counter(
                telemetry::counter::ROOM_SURF_OPTIONS_CYCLES,
                self.options_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_CELL_SETUP_CYCLES,
                self.cell_setup_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_CALL_CYCLES,
                self.surface_call_cycles,
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
                telemetry::counter::ROOM_SURF_TR_SUBDIVISION_CANDIDATES,
                self.tr_subdivision_candidates,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_TR_SUBDIVISION_SUBMITTED,
                self.tr_subdivision_submitted,
            );
            for (base, s) in [
                (
                    telemetry::counter::ROOM_SURF_WARP_SUBDIVIDED_COUNT,
                    self.warp_subdivided,
                ),
                (
                    telemetry::counter::ROOM_SURF_WARP_UNTOUCHED_COUNT,
                    self.warp_untouched,
                ),
            ] {
                telemetry::counter(base, s.count);
                telemetry::counter(base + 1, s.sum);
                telemetry::counter(base + 2, s.max);
                telemetry::counter(base + 3, s.under_1tx);
            }
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
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    _room_depth: u16,
    sector_size: i32,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    shade_prewarmed_packets: bool,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    visible_cells: &[GridVisibleCell],
    screen_margin: i32,
    portal_window: Option<PortalCellWindow>,
    mut prebuilt_pool: Option<(&mut [QuadTexturedGouraud], &mut [u8])>,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < visible_cells.len()
        || accepted_cell_depths.len() < visible_cells.len()
    {
        return stats;
    }
    if visible_cells.is_empty() {
        return stats;
    }
    // Reuse the depth scratch's small prefix as one bit per cached vertex
    // while collecting. This is self-initializing on every draw, costs one
    // word clear per 32 vertices, and removes the old per-vertex bool arena
    // plus its end-of-draw clearing pass.
    let projected_seen_word_count = cached_vertices.len().div_ceil(32);
    let projected_indices = &mut projected_indices[..cached_vertices.len()];

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;
    let mut accepted_vertex_references = 0usize;
    let mut accepted_depths_need_sort = false;
    // Per-cell depth/cull transforms run on the GTE (MVMVA) via the loaded
    // camera instead of redoing the camera rotation in CPU fixed-point for
    // every candidate cell; matches the rounding of the GTE vertex projection.
    let loaded_camera = LoadedWorldCameraGte::load(*camera);
    let cell_frustum = CellFrustum::new(camera, options, screen_margin);
    let cell_half_xz = sector_size.max(1).saturating_add(1) >> 1;

    for visible in visible_cells.iter().copied() {
        cell_stage_begin(crate::telemetry::stage::CELL_LOOKUP);
        let looked_up = cached_room_cell_index_for_visible(cached_cells, visible);
        cell_stage_end(crate::telemetry::stage::CELL_LOOKUP);
        let Some(cell_index) = looked_up else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index) else {
            continue;
        };

        stats.cells_considered = stats.cells_considered.wrapping_add(1);
        cell_stage_begin(crate::telemetry::stage::CELL_DEPTH);
        let visibility_center = WorldVertex::new(
            cell.visibility_center[0],
            cell.visibility_center[1],
            cell.visibility_center[2],
        );
        let portal_view = portal_window.map(|_| loaded_camera.view_vertex(visibility_center));
        let mut visibility_half_y = None;
        let cell_depth = if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_PRECULLED {
            portal_view
                .unwrap_or_else(|| loaded_camera.view_vertex(visibility_center))
                .z
        } else if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_UNKNOWN {
            let visibility_view =
                portal_view.unwrap_or_else(|| loaded_camera.view_vertex(visibility_center));
            let half_y = cell.visibility_center[1]
                .saturating_sub(cell.min_y)
                .abs()
                .max(cell.max_y.saturating_sub(cell.visibility_center[1]).abs());
            visibility_half_y = Some(half_y);
            if !cell_frustum.cell_aabb_visible(visibility_view, cell_half_xz, half_y, cell_half_xz)
            {
                stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                cell_stage_end(crate::telemetry::stage::CELL_DEPTH);
                continue;
            }
            accepted_depths_need_sort = true;
            visibility_view.z
        } else {
            visible.camera_depth as i32
        };
        if let Some(window) = portal_window {
            let visibility_view =
                portal_view.unwrap_or_else(|| loaded_camera.view_vertex(visibility_center));
            let half_y = visibility_half_y.unwrap_or_else(|| {
                cell.visibility_center[1]
                    .saturating_sub(cell.min_y)
                    .abs()
                    .max(cell.max_y.saturating_sub(cell.visibility_center[1]).abs())
            });
            if !cell_frustum.cell_aabb_intersects_portal_window(
                visibility_view,
                cell_half_xz,
                half_y,
                cell_half_xz,
                window,
            ) {
                stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                cell_stage_end(crate::telemetry::stage::CELL_DEPTH);
                continue;
            }
        }
        cell_stage_end(crate::telemetry::stage::CELL_DEPTH);

        stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
        // SAFETY: both accepted arrays were validated to hold at least
        // `visible_cells.len()` entries at function entry, and the count
        // grows at most once per visible cell, so it is always in range.
        unsafe {
            *accepted_cell_indices.get_unchecked_mut(accepted_cell_count) = cell_index as u16;
            *accepted_cell_depths.get_unchecked_mut(accepted_cell_count) = cell_depth;
        }
        accepted_cell_count += 1;
        accepted_vertex_references =
            accepted_vertex_references.saturating_add(if cell.vertex_count == 0 {
                cell.surface_count.saturating_mul(4)
            } else {
                cell.vertex_count
            } as usize);
    }
    if accepted_depths_need_sort {
        sort_cached_room_cell_indices_by_depth(
            &mut accepted_cell_indices[..accepted_cell_count],
            &mut accepted_cell_depths[..accepted_cell_count],
            projected_vertices,
            projected_depths,
        );
    }
    // When accepted cells collectively reference at least the room cache's
    // vertex count, deduplicating their indices is more CPU work than simply
    // projecting the contiguous cache. Sparse portal views keep the selective
    // indexed path so large rooms do not regress.
    let project_dense_cache = accepted_vertex_references >= cached_vertices.len();
    if project_dense_cache {
        // Keep the projected-index scratch coherent for callers that inspect
        // it and, on MIPS, preserve the tighter loop layout measured for the
        // dense path.
        for (index, slot) in projected_indices.iter_mut().enumerate() {
            *slot = index as u16;
        }
    } else {
        projected_depths[..projected_seen_word_count].fill(0);
        cell_stage_begin(crate::telemetry::stage::CELL_COLLECT);
        for &cell_index in &accepted_cell_indices[..accepted_cell_count] {
            let Some(cell) = cached_cells.get(cell_index as usize) else {
                continue;
            };
            projected_index_count = collect_cached_cell_vertex_indices(
                cell,
                cached_cell_vertices,
                cached_surfaces,
                &mut projected_depths[..projected_seen_word_count],
                projected_indices,
                projected_index_count,
            );
        }
        cell_stage_end(crate::telemetry::stage::CELL_COLLECT);
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_CELL_SELECT);

    stats.projected_vertices = if project_dense_cache {
        cached_vertices.len().min(u16::MAX as usize) as u16
    } else {
        projected_index_count.min(u16::MAX as usize) as u16
    };
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_PROJECT);
    if project_dense_cache {
        project_world_vertices_gte(*camera, cached_vertices, projected_vertices);
    } else {
        project_world_vertex_indices_gte(
            *camera,
            cached_vertices,
            &projected_indices[..projected_index_count],
            projected_vertices,
        );
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_PROJECT);
    if use_vertex_depths {
        crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_DEPTH_PREP);
        if project_dense_cache {
            for (index, projected) in projected_vertices[..cached_vertices.len()]
                .iter()
                .enumerate()
            {
                projected_depths[index] = lighting.prepare_vertex_depth(projected.sz);
            }
        } else {
            for raw_index in &projected_indices[..projected_index_count] {
                let index = *raw_index as usize;
                projected_depths[index] =
                    lighting.prepare_vertex_depth(projected_vertices[index].sz);
            }
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
        let cell_setup_start = RoomSurfaceMicroProfile::cycle();
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
        surface_profile.add_cell_setup(RoomSurfaceMicroProfile::elapsed(cell_setup_start));
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            // Per-surface prebuilt pool entry: the packet and its
            // validity byte share the surface's index in the room's
            // surface slice. Out-of-pool surfaces fall back to the
            // per-frame arena path.
            let surface_prebuilt = match prebuilt_pool.as_mut() {
                Some((pool, valid)) => match (pool.get_mut(i), valid.get_mut(i)) {
                    (Some(quad), Some(valid)) => Some((quad, valid)),
                    _ => None,
                },
                None => None,
            };
            let surface_call_start = RoomSurfaceMicroProfile::cycle();
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .wrapping_add(draw_indexed_cached_room_surface(
                        &cached_surfaces[i],
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        shade_prewarmed_packets,
                        screen_bounds,
                        materials,
                        lighting,
                        camera,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        surface_prebuilt,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            surface_profile.add_surface_call(RoomSurfaceMicroProfile::elapsed(surface_call_start));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    stats
}

/// Draw every populated cell from a cached vertex-lit room.
///
/// This bypasses cooked visible-cell/PVS filtering after the caller has
/// already selected an active chunk. Cells are still depth-sorted for the
/// ordering-table painter path, and surfaces still run the usual projection,
/// screen, near-plane, and backface checks.
pub fn draw_indexed_cached_room_vertex_lit_all_cells<const OT: usize, L: WorldSurfaceLighting>(
    cached_cells: &[CachedRoomCell],
    cached_cell_vertices: &[u16],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_indices: &mut [u16],
    projected_vertices: &mut [ProjectedVertex],
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    materials: &[WorldRenderMaterial],
    lighting: &L,
    shade_prewarmed_packets: bool,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    screen_margin: i32,
    sector_size: i32,
    cull_cells_laterally: bool,
    mut prebuilt_pool: Option<(&mut [QuadTexturedGouraud], &mut [u8])>,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < cached_cells.len()
        || accepted_cell_depths.len() < cached_cells.len()
    {
        return stats;
    }
    if cached_cells.is_empty() || cached_surfaces.is_empty() {
        return stats;
    }
    let projected_seen_word_count = cached_vertices.len().div_ceil(32);
    projected_depths[..projected_seen_word_count].fill(0);
    let projected_indices = &mut projected_indices[..cached_vertices.len()];

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;
    let loaded_camera = LoadedWorldCameraGte::load(*camera);

    let mut cell_frustum = CellFrustum::new(camera, options, screen_margin);
    // The all-cells fallback intentionally has no far-plane cull. Keep that
    // policy while using the tighter cached-cell AABB for lateral rejection.
    cell_frustum.far = i32::MAX;
    let cell_half_xz = sector_size.max(1).saturating_add(1) >> 1;
    cell_stage_begin(crate::telemetry::stage::CELL_DEPTH);
    // Cap the scan slice up front instead of testing `cell_index >
    // u16::MAX` on every iteration (everything past the cap would be
    // skipped one by one anyway).
    let scan_cells = &cached_cells[..cached_cells.len().min(u16::MAX as usize + 1)];
    for (cell_index, cell) in scan_cells.iter().enumerate() {
        if cell.surface_count == 0 {
            continue;
        }
        stats.cells_considered = stats.cells_considered.wrapping_add(1);
        let visibility_center = WorldVertex::new(
            cell.visibility_center[0],
            cell.visibility_center[1],
            cell.visibility_center[2],
        );
        let visibility_view = loaded_camera.view_vertex(visibility_center);
        if cull_cells_laterally {
            let half_y = cell.visibility_center[1]
                .saturating_sub(cell.min_y)
                .abs()
                .max(cell.max_y.saturating_sub(cell.visibility_center[1]).abs());
            if !cell_frustum.cell_aabb_visible(visibility_view, cell_half_xz, half_y, cell_half_xz)
            {
                stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                continue;
            }
        }
        stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
        // SAFETY: both accepted arrays were validated to hold at least
        // `cached_cells.len()` entries at function entry, and the count
        // grows at most once per scanned cell, so it is always in range.
        unsafe {
            *accepted_cell_indices.get_unchecked_mut(accepted_cell_count) = cell_index as u16;
            *accepted_cell_depths.get_unchecked_mut(accepted_cell_count) = visibility_view.z;
        }
        accepted_cell_count += 1;
    }
    cell_stage_end(crate::telemetry::stage::CELL_DEPTH);
    sort_cached_room_cell_indices_by_depth(
        &mut accepted_cell_indices[..accepted_cell_count],
        &mut accepted_cell_depths[..accepted_cell_count],
        projected_vertices,
        projected_depths,
    );
    // The bucket sorter borrows this scratch after selection. Restore the
    // vertex-seen bitset before the sorted cell walk starts collecting.
    projected_depths[..projected_seen_word_count].fill(0);
    cell_stage_begin(crate::telemetry::stage::CELL_COLLECT);
    for &cell_index in &accepted_cell_indices[..accepted_cell_count] {
        let Some(cell) = cached_cells.get(cell_index as usize) else {
            continue;
        };
        projected_index_count = collect_cached_cell_vertex_indices(
            cell,
            cached_cell_vertices,
            cached_surfaces,
            &mut projected_depths[..projected_seen_word_count],
            projected_indices,
            projected_index_count,
        );
    }
    cell_stage_end(crate::telemetry::stage::CELL_COLLECT);
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
        let cell_setup_start = RoomSurfaceMicroProfile::cycle();
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
        surface_profile.add_cell_setup(RoomSurfaceMicroProfile::elapsed(cell_setup_start));
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            // Per-surface prebuilt pool entry: the packet and its
            // validity byte share the surface's index in the room's
            // surface slice. Out-of-pool surfaces fall back to the
            // per-frame arena path.
            let surface_prebuilt = match prebuilt_pool.as_mut() {
                Some((pool, valid)) => match (pool.get_mut(i), valid.get_mut(i)) {
                    (Some(quad), Some(valid)) => Some((quad, valid)),
                    _ => None,
                },
                None => None,
            };
            let surface_call_start = RoomSurfaceMicroProfile::cycle();
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .wrapping_add(draw_indexed_cached_room_surface(
                        &cached_surfaces[i],
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        shade_prewarmed_packets,
                        screen_bounds,
                        materials,
                        lighting,
                        camera,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        surface_prebuilt,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            surface_profile.add_surface_call(RoomSurfaceMicroProfile::elapsed(surface_call_start));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    stats
}

/// Stage marks for the per-candidate phases inside ROOM_CELL_SELECT
/// (cell_lookup / cell_depth / cell_collect). Compiled out by default:
/// the per-cell telemetry writes cost ~4k cycles/frame on the benchmark
/// tape, so they only ride in `cell-select-profile` builds.
#[inline(always)]
fn cell_stage_begin(id: u16) {
    #[cfg(feature = "cell-select-profile")]
    crate::telemetry::stage_begin(id);
    #[cfg(not(feature = "cell-select-profile"))]
    let _ = id;
}

#[inline(always)]
fn cell_stage_end(id: u16) {
    #[cfg(feature = "cell-select-profile")]
    crate::telemetry::stage_end(id);
    #[cfg(not(feature = "cell-select-profile"))]
    let _ = id;
}

fn sort_cached_room_cell_indices_by_depth(
    indices: &mut [u16],
    depths: &mut [i32],
    pair_scratch: &mut [ProjectedVertex],
    bucket_scratch: &mut [i32],
) {
    if indices.len() > depths.len() {
        return;
    }
    let len = indices.len();
    if len < 2 {
        return;
    }
    // Tiny room lists are cheaper in-place; larger lists use only enough
    // coarse buckets to keep the per-bucket insertion tails short. Avoid
    // clearing the old fixed 128 counters for every small active room.
    let (bucket_count, bucket_shift) = if len < 24 {
        sort_cached_room_cell_indices_by_depth_shell(indices, depths);
        return;
    } else if len < 48 && bucket_scratch.len() >= 32 {
        (32usize, 10u32)
    } else if len < 96 && bucket_scratch.len() >= 64 {
        (64usize, 9u32)
    } else if bucket_scratch.len() >= 128 {
        (128usize, 8u32)
    } else {
        sort_cached_room_cell_indices_by_depth_shell(indices, depths);
        return;
    };
    if pair_scratch.len() < len {
        sort_cached_room_cell_indices_by_depth_shell(indices, depths);
        return;
    }

    let buckets = &mut bucket_scratch[..bucket_count];
    buckets.fill(0);
    let bucket_for =
        |depth: i32| ((depth.clamp(0, 32_767) as usize) >> bucket_shift).min(bucket_count - 1);
    for &depth in &depths[..len] {
        let bucket = bucket_for(depth);
        buckets[bucket] += 1;
    }

    let mut running = 0i32;
    let mut bucket = bucket_count;
    while bucket != 0 {
        bucket -= 1;
        let count = buckets[bucket];
        buckets[bucket] = running;
        running += count;
    }
    let mut i = 0usize;
    while i < len {
        let bucket = bucket_for(depths[i]);
        let out = buckets[bucket] as usize;
        pair_scratch[out] = ProjectedVertex::new(indices[i] as i16, 0, depths[i]);
        buckets[bucket] += 1;
        i += 1;
    }

    // Counting places broad far-to-near depth bands. Stable insertion only
    // refines records that landed in the same band, preserving the exact raw
    // depth order (and equal-depth submission order) of the old full sort.
    let mut start = 0usize;
    bucket = bucket_count;
    while bucket != 0 {
        bucket -= 1;
        let end = buckets[bucket] as usize;
        let mut item_index = start + 1;
        while item_index < end {
            let item = pair_scratch[item_index];
            let mut position = item_index;
            while position > start && pair_scratch[position - 1].sz < item.sz {
                pair_scratch[position] = pair_scratch[position - 1];
                position -= 1;
            }
            pair_scratch[position] = item;
            item_index += 1;
        }
        start = end;
    }

    i = 0;
    while i < len {
        indices[i] = pair_scratch[i].sx as u16;
        depths[i] = pair_scratch[i].sz;
        i += 1;
    }
}

fn sort_cached_room_cell_indices_by_depth_shell(indices: &mut [u16], depths: &mut [i32]) {
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
    cell: &CachedRoomCell,
    cached_cell_vertices: &[u16],
    cached_surfaces: &[CachedRoomSurface],
    projected_seen_words: &mut [i32],
    projected_indices: &mut [u16],
    mut projected_index_count: usize,
) -> usize {
    if cell.vertex_count == 0 {
        let first = (cell.surface_first as usize).min(cached_surfaces.len());
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        for surface in &cached_surfaces[first..end] {
            for raw_index in surface.vertex_indices {
                projected_index_count = push_unique_projected_index(
                    raw_index,
                    projected_seen_words,
                    projected_indices,
                    projected_index_count,
                );
            }
        }
        return projected_index_count;
    }
    let first = (cell.vertex_first as usize).min(cached_cell_vertices.len());
    let end = first
        .saturating_add(cell.vertex_count as usize)
        .min(cached_cell_vertices.len());
    for &raw_index in &cached_cell_vertices[first..end] {
        projected_index_count = push_unique_projected_index(
            raw_index,
            projected_seen_words,
            projected_indices,
            projected_index_count,
        );
    }
    projected_index_count
}

/// Callers provide one cleared seen bit for every room vertex and slice
/// `projected_indices` to the room's vertex count. An index whose bit word
/// exists is therefore a valid vertex slot, and the write cursor (one push
/// per distinct vertex) can never reach the indices slice end.
#[inline(always)]
fn push_unique_projected_index(
    raw_index: u16,
    projected_seen_words: &mut [i32],
    projected_indices: &mut [u16],
    projected_index_count: usize,
) -> usize {
    let vertex_index = raw_index as usize;
    let word_index = vertex_index >> 5;
    let mask = 1u32 << (vertex_index & 31);
    if let Some(seen_word) = projected_seen_words.get_mut(word_index) {
        let seen_bits = *seen_word as u32;
        if seen_bits & mask == 0 {
            *seen_word = (seen_bits | mask) as i32;
            debug_assert!(projected_index_count < projected_indices.len());
            // SAFETY: see the function doc; distinct pushes are bounded
            // by the represented vertex count and indices slice length.
            unsafe {
                *projected_indices.get_unchecked_mut(projected_index_count) = raw_index;
            }
            return projected_index_count + 1;
        }
    }
    projected_index_count
}

const fn cached_room_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

#[inline(always)]
fn draw_indexed_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: &CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    projected_vertices: &[ProjectedVertex],
    projected_depths: &[i32],
    use_vertex_depths: bool,
    use_direct_baked_rgb: bool,
    shade_prewarmed_packets: bool,
    screen_bounds: ProjectedScreenBounds,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    submit_depths: CachedRoomSubmitDepths,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    mut prebuilt: Option<(&mut QuadTexturedGouraud, &mut u8)>,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
    profile: &mut RoomSurfaceMicroProfile,
) -> u16 {
    profile.count_profiled();
    profile.count_shape(surface.triangle_index);
    let projected_start = RoomSurfaceMicroProfile::cycle();
    let ids = surface.vertex_indices;
    let projected_with_depths = if use_vertex_depths {
        indexed_projected_quad_with_depths(projected_vertices, projected_depths, ids)
    } else {
        indexed_projected_quad(projected_vertices, ids).map(|projected| (projected, [0; 4]))
    };
    let Some((projected, raw_vertex_depths)) = projected_with_depths else {
        profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
        profile.count_projected_reject();
        return draw_near_clipped_cached_room_surface(
            surface,
            cached_vertices,
            ids,
            materials,
            lighting,
            camera,
            options,
            use_vertex_depths,
            use_direct_baked_rgb,
            triangles,
            world,
        );
    };
    let vertex_depths = use_vertex_depths.then_some(raw_vertex_depths);
    profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
    let screen_start = RoomSurfaceMicroProfile::cycle();
    let projected_metrics = ProjectedQuadMetrics::new(projected);
    if projected_metrics.outside_screen(screen_bounds) {
        profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
        profile.count_screen_culled();
        return 1;
    }
    profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
    #[cfg(not(feature = "room-surface-profile"))]
    if shade_prewarmed_packets
        && !use_direct_baked_rgb
        && surface.has_baked_rgb()
        && surface.triangle_index >= WHOLE_QUAD_TRIANGLE_INDEX
    {
        if let Some((quad, valid)) = prebuilt.as_mut() {
            let ready = **valid;
            if ready & WARMED_ROOM_QUAD_READY != 0
                && try_submit_shaded_encoded_warmed_room_quad(
                    surface,
                    cached_vertices,
                    ids,
                    projected,
                    projected_metrics,
                    vertex_depths,
                    materials,
                    lighting,
                    camera,
                    &options,
                    submit_depths,
                    depth_mode,
                    subdivision_mode,
                    quad,
                    ready,
                    triangles,
                    world,
                )
            {
                return 1;
            }
        }
    }
    #[cfg(not(feature = "room-surface-profile"))]
    if use_direct_baked_rgb
        && surface.has_baked_rgb()
        && surface.triangle_index >= WHOLE_QUAD_TRIANGLE_INDEX
    {
        if let Some((quad, valid)) = prebuilt.as_mut() {
            let ready = **valid;
            if ready & WARMED_ROOM_QUAD_READY != 0
                && try_submit_encoded_warmed_room_quad(
                    surface,
                    cached_vertices,
                    ids,
                    projected,
                    projected_metrics,
                    materials,
                    camera,
                    &options,
                    submit_depths,
                    depth_mode,
                    subdivision_mode,
                    quad,
                    ready,
                    triangles,
                    world,
                )
            {
                return 1;
            }
        }
    }
    let kind_start = RoomSurfaceMicroProfile::cycle();
    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    profile.add_kind(RoomSurfaceMicroProfile::elapsed(kind_start));
    profile.count_kind(kind);
    let material_start = RoomSurfaceMicroProfile::cycle();
    let Some(&base_material) = materials.get(surface.material_slot as usize) else {
        profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
        profile.count_material_miss();
        return 0;
    };
    let uv_words = if base_material.animation.is_animated() {
        prebuilt = None;
        animated_cached_uv_words(base_material, options, surface.uv_words)
    } else {
        surface.uv_words
    };
    // The encoded warm path runs before material lookup and therefore cannot
    // select the transparent tie-break layer. Keep translucent surfaces on
    // the normal packet path where `with_material_layer` is applied.
    if base_material.texture.is_translucent() {
        prebuilt = None;
    }
    let material = cached_uv_material(base_material);
    profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
    // A valid prebuilt packet already owns the immutable baked colours. Once
    // its first frame has populated them, avoid reconstructing and shuffling
    // the same four RGB triples on every subsequent draw. Positions and OT
    // depth remain frame-dependent and are still patched below.
    let prebuilt_static_colors_ready = use_direct_baked_rgb
        && surface.has_baked_rgb()
        && prebuilt.as_ref().is_some_and(|entry| *entry.1 != 0);
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            let is_ceiling = matches!(kind, WorldSurfaceKind::Ceiling);
            let surface_risky = cached_surface_risk_for_modes(
                depth_mode,
                subdivision_mode,
                kind,
                surface,
                projected,
                projected_metrics.depth_span(),
            );
            let use_triangle_depth =
                cached_surface_uses_triangle_depth_with_risk(depth_mode, kind, surface_risky);
            let options_start = RoomSurfaceMicroProfile::cycle();
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (horizontal_depth_options(options), submit_depths.horizontal)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                kind,
                use_triangle_depth,
                surface_risky,
            );
            profile.add_options(RoomSurfaceMicroProfile::elapsed(options_start));
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
                let colors = adaptive_debug_root_colors(surface_options, colors);
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                let adaptive_subdivision = surface_options.adaptive_subdivision
                    && adaptive_projected_triangle_needs_subdivision(
                        projected,
                        surface_options.adaptive_subdivision_profile,
                        surface.split,
                        surface.triangle_index as usize,
                        is_ceiling,
                        material.sidedness,
                    );
                profile.count_warp(projected, uv_words, adaptive_subdivision);
                if adaptive_subdivision {
                    profile.count_tr_subdivision_candidate();
                }
                if adaptive_subdivision
                    && submit_adaptive_cached_room_triangle(
                        cached_vertices,
                        ids,
                        camera,
                        uv_words,
                        colors,
                        material,
                        &surface_options,
                        surface.split,
                        surface.triangle_index as usize,
                        is_ceiling,
                        triangles,
                        world,
                    )
                {
                    profile.count_tr_subdivision_submitted();
                    profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
                    return 1;
                }
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    uv_words,
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
                let adaptive_subdivision = surface_options.adaptive_subdivision
                    && adaptive_projected_quad_needs_subdivision(
                        projected,
                        surface_options.adaptive_subdivision_profile,
                    );
                profile.count_warp(projected, uv_words, adaptive_subdivision);
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
                #[cfg(not(feature = "room-surface-profile"))]
                if prebuilt_static_colors_ready
                    && !adaptive_subdivision
                    && !surface_options.adaptive_debug_subdivision_levels
                {
                    let prepared_depth = prepared_depth.unwrap_or_else(|| {
                        PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
                    });
                    let packet_verts = warmed_room_quad_packet_vertices(
                        projected,
                        material.sidedness,
                        surface.split,
                        is_ceiling,
                    );
                    if let Some((quad, _)) = prebuilt.as_mut() {
                        if world
                            .try_submit_warmed_textured_gouraud_quad(
                                quad,
                                packet_verts,
                                projected_metrics.hardware_extent_safe()
                                    && surface_options.textured_split_max_edge == 0,
                                &surface_options,
                                prepared_depth,
                            )
                            .is_some()
                        {
                            return 1;
                        }
                    }
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let colors = if prebuilt_static_colors_ready
                    && !adaptive_subdivision
                    && !surface_options.adaptive_debug_subdivision_levels
                {
                    [(0, 0, 0); 4]
                } else {
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
                    colors
                };
                let colors = adaptive_debug_root_colors(surface_options, colors);
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                if adaptive_subdivision {
                    profile.count_tr_subdivision_candidate();
                }
                if adaptive_subdivision
                    && submit_adaptive_cached_room_quad(
                        cached_vertices,
                        ids,
                        projected,
                        projected_metrics.hardware_extent_safe(),
                        camera,
                        uv_words,
                        colors,
                        material,
                        &surface_options,
                        surface.split,
                        is_ceiling,
                        None,
                        triangles,
                        world,
                    )
                {
                    profile.count_tr_subdivision_submitted();
                    profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
                    return 1;
                }
                let (projected, uv_words, colors) = if is_ceiling {
                    (
                        reverse_quad_winding(projected),
                        reverse_quad_winding(uv_words),
                        reverse_quad_winding(colors),
                    )
                } else {
                    (projected, uv_words, colors)
                };
                // Risky whole-quads keep the single-packet quad path
                // with their own averaged depth (the key their two
                // leaves would approximate) instead of splitting into
                // two triangle packets: measured -55 packets/frame on
                // the benchmark tape.
                let prepared_depth = Some(prepared_depth.unwrap_or_else(|| {
                    PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
                }));
                let prebuilt_colors_static = use_direct_baked_rgb && surface.has_baked_rgb();
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
                    prebuilt,
                    prebuilt_colors_static,
                    warmed_room_quad_ready_value(material.sidedness, surface.split, is_ceiling),
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
        WorldSurfaceKind::Wall { direction } => {
            let wall_material =
                wall_material_for_direction(material, direction, surface.wall_faces_owner());
            let surface_risky = cached_surface_risk_for_modes(
                depth_mode,
                subdivision_mode,
                kind,
                surface,
                projected,
                projected_metrics.depth_span(),
            );
            let use_triangle_depth =
                cached_surface_uses_triangle_depth_with_risk(depth_mode, kind, surface_risky);
            let options_start = RoomSurfaceMicroProfile::cycle();
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (options, submit_depths.vertical)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                kind,
                use_triangle_depth,
                surface_risky,
            );
            profile.add_options(RoomSurfaceMicroProfile::elapsed(options_start));
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
                let colors = adaptive_debug_root_colors(surface_options, colors);
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                let adaptive_subdivision = surface_options.adaptive_subdivision
                    && adaptive_projected_triangle_needs_subdivision(
                        projected,
                        surface_options.adaptive_subdivision_profile,
                        surface.split,
                        surface.triangle_index as usize,
                        false,
                        wall_material.sidedness,
                    );
                profile.count_warp(projected, uv_words, adaptive_subdivision);
                if adaptive_subdivision {
                    profile.count_tr_subdivision_candidate();
                }
                if adaptive_subdivision
                    && submit_adaptive_cached_room_triangle(
                        cached_vertices,
                        ids,
                        camera,
                        uv_words,
                        colors,
                        wall_material,
                        &surface_options,
                        surface.split,
                        surface.triangle_index as usize,
                        false,
                        triangles,
                        world,
                    )
                {
                    profile.count_tr_subdivision_submitted();
                    profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
                    return 1;
                }
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    uv_words,
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
                let adaptive_subdivision = surface_options.adaptive_subdivision
                    && adaptive_projected_quad_needs_subdivision(
                        projected,
                        surface_options.adaptive_subdivision_profile,
                    );
                profile.count_warp(projected, uv_words, adaptive_subdivision);
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
                #[cfg(not(feature = "room-surface-profile"))]
                if prebuilt_static_colors_ready
                    && !adaptive_subdivision
                    && !surface_options.adaptive_debug_subdivision_levels
                {
                    let prepared_depth = prepared_depth.unwrap_or_else(|| {
                        PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
                    });
                    let packet_verts = warmed_room_quad_packet_vertices(
                        projected,
                        wall_material.sidedness,
                        SPLIT_NW_SE,
                        false,
                    );
                    if let Some((quad, _)) = prebuilt.as_mut() {
                        if world
                            .try_submit_warmed_textured_gouraud_quad(
                                quad,
                                packet_verts,
                                projected_metrics.hardware_extent_safe()
                                    && surface_options.textured_split_max_edge == 0,
                                &surface_options,
                                prepared_depth,
                            )
                            .is_some()
                        {
                            return 1;
                        }
                    }
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let colors = if prebuilt_static_colors_ready
                    && !adaptive_subdivision
                    && !surface_options.adaptive_debug_subdivision_levels
                {
                    [(0, 0, 0); 4]
                } else {
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
                    colors
                };
                let colors = adaptive_debug_root_colors(surface_options, colors);
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                if adaptive_subdivision {
                    profile.count_tr_subdivision_candidate();
                }
                if adaptive_subdivision
                    && submit_adaptive_cached_room_quad(
                        cached_vertices,
                        ids,
                        projected,
                        projected_metrics.hardware_extent_safe(),
                        camera,
                        uv_words,
                        colors,
                        wall_material,
                        &surface_options,
                        SPLIT_NW_SE,
                        false,
                        None,
                        triangles,
                        world,
                    )
                {
                    profile.count_tr_subdivision_submitted();
                    profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
                    return 1;
                }
                // Same single-packet upgrade for risky whole-quad walls.
                let prepared_depth = Some(prepared_depth.unwrap_or_else(|| {
                    PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
                }));
                let prebuilt_colors_static = use_direct_baked_rgb && surface.has_baked_rgb();
                submit_sided_projected_gouraud_quad_cached_uv_words(
                    world,
                    triangles,
                    projected,
                    uv_words,
                    colors,
                    wall_material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    SPLIT_NW_SE,
                    prebuilt,
                    prebuilt_colors_static,
                    warmed_room_quad_ready_value(wall_material.sidedness, SPLIT_NW_SE, false),
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
    }
    1
}

const WARMED_ROOM_QUAD_READY: u8 = 0x80;
const WARMED_ROOM_QUAD_REVERSE: u8 = 0x01;
const WARMED_ROOM_QUAD_SPLIT_NE_SW: u8 = 0x02;
const WARMED_ROOM_QUAD_DOUBLE_SIDED: u8 = 0x04;
/// Packet-corner reversal, carrying ONLY `reverse_front` (ceilings).
///
/// Kept apart from [`WARMED_ROOM_QUAD_REVERSE`], which also folds in a `Back`
/// sidedness and drives the CULL test. Letting sidedness reach the corner order
/// swaps which of the quad's two triangles is submitted first; see
/// `quad_packet_order`.
const WARMED_ROOM_QUAD_REVERSE_FRONT: u8 = 0x08;

/// Build the immutable payload of baked whole-quad room packets before the
/// room reaches the render loop.
///
/// Positions and OT links remain frame-dependent and are patched when the
/// surface is drawn. Materials, UVs, baked vertex colours, winding, and split
/// order are static for a resident room, so preparing them during the streamed
/// room/material lifecycle removes the first-visible-frame packet spike.
pub fn prewarm_indexed_cached_room_quads(
    surfaces: &[CachedRoomSurface],
    materials: &[WorldRenderMaterial],
    quads: &mut [QuadTexturedGouraud],
    valid: &mut [u8],
) -> usize {
    let count = surfaces.len().min(quads.len()).min(valid.len());
    let mut warmed = 0usize;
    let mut index = 0usize;
    while index < count {
        valid[index] = 0;
        let surface = &surfaces[index];
        if surface.has_baked_rgb() && surface.triangle_index >= WHOLE_QUAD_TRIANGLE_INDEX {
            if let Some(&base_material) = materials.get(surface.material_slot as usize) {
                if base_material.animation.is_animated() || base_material.texture.is_translucent() {
                    index += 1;
                    continue;
                }
                let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
                let (material, split, reverse_front) = match kind {
                    WorldSurfaceKind::Floor => {
                        (cached_uv_material(base_material), surface.split, false)
                    }
                    WorldSurfaceKind::Ceiling => {
                        (cached_uv_material(base_material), surface.split, true)
                    }
                    WorldSurfaceKind::Wall { direction } => (
                        wall_material_for_direction(
                            cached_uv_material(base_material),
                            direction,
                            surface.wall_faces_owner(),
                        ),
                        SPLIT_NW_SE,
                        false,
                    ),
                };
                let uv_words = warmed_room_quad_packet_values(
                    surface.uv_words,
                    material.sidedness,
                    split,
                    reverse_front,
                );
                let colors = warmed_room_quad_packet_values(
                    surface.baked_vertex_rgb,
                    material.sidedness,
                    split,
                    reverse_front,
                );
                quads[index] = QuadTexturedGouraud::with_packet_material_packed_uv_words(
                    [(0, 0); 4],
                    uv_words,
                    colors,
                    material.gouraud_packet,
                );
                valid[index] =
                    warmed_room_quad_ready_value(material.sidedness, split, reverse_front);
                warmed += 1;
            }
        }
        index += 1;
    }
    warmed
}

#[inline(always)]
const fn warmed_room_quad_ready_value(
    sidedness: SurfaceSidedness,
    split: u8,
    reverse_front: bool,
) -> u8 {
    let reverse = reverse_front ^ matches!(sidedness, SurfaceSidedness::Back);
    WARMED_ROOM_QUAD_READY
        | if reverse { WARMED_ROOM_QUAD_REVERSE } else { 0 }
        | if reverse_front {
            WARMED_ROOM_QUAD_REVERSE_FRONT
        } else {
            0
        }
        | if split == SPLIT_NE_SW {
            WARMED_ROOM_QUAD_SPLIT_NE_SW
        } else {
            0
        }
        | if matches!(sidedness, SurfaceSidedness::Both) {
            WARMED_ROOM_QUAD_DOUBLE_SIDED
        } else {
            0
        }
}

#[inline(always)]
fn warmed_room_quad_packet_vertices_from_ready(
    mut verts: [ProjectedVertex; 4],
    ready: u8,
) -> [ProjectedVertex; 4] {
    if ready & WARMED_ROOM_QUAD_REVERSE_FRONT != 0 {
        verts = reverse_quad_winding(verts);
    }
    if ready & WARMED_ROOM_QUAD_SPLIT_NE_SW != 0 {
        [verts[0], verts[1], verts[3], verts[2]]
    } else {
        [verts[1], verts[0], verts[2], verts[3]]
    }
}

#[inline(always)]
fn warmed_room_quad_packet_colors_from_ready(
    mut colors: [(u8, u8, u8); 4],
    ready: u8,
) -> [(u8, u8, u8); 4] {
    if ready & WARMED_ROOM_QUAD_REVERSE_FRONT != 0 {
        colors = reverse_quad_winding(colors);
    }
    if ready & WARMED_ROOM_QUAD_SPLIT_NE_SW != 0 {
        [colors[0], colors[1], colors[3], colors[2]]
    } else {
        [colors[1], colors[0], colors[2], colors[3]]
    }
}

#[inline(always)]
fn try_submit_encoded_warmed_room_quad<const OT: usize>(
    surface: &CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    projected: [ProjectedVertex; 4],
    projected_metrics: ProjectedQuadMetrics,
    materials: &[WorldRenderMaterial],
    camera: &WorldCamera,
    options: &WorldSurfaceOptions,
    submit_depths: CachedRoomSubmitDepths,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    quad: &mut QuadTexturedGouraud,
    ready: u8,
    primitives: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> bool {
    if encoded_warmed_room_quad_backface_culled(projected, ready) {
        return true;
    }
    let packet_verts = warmed_room_quad_packet_vertices_from_ready(projected, ready);

    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    let surface_risky = cached_surface_risk_for_modes(
        depth_mode,
        subdivision_mode,
        kind,
        surface,
        projected,
        projected_metrics.depth_span(),
    );
    let use_triangle_depth =
        cached_surface_uses_triangle_depth_with_risk(depth_mode, kind, surface_risky);
    let (surface_options, prepared_depth) = if use_triangle_depth {
        (triangle_depth_options(*options), None)
    } else if matches!(kind, WorldSurfaceKind::Wall { .. }) {
        (*options, submit_depths.vertical)
    } else {
        (horizontal_depth_options(*options), submit_depths.horizontal)
    };
    let surface_options = cached_surface_subdivision_options(
        surface_options,
        subdivision_mode,
        kind,
        use_triangle_depth,
        surface_risky,
    );
    if adaptive_warmed_quad_requires_dynamic_submit(&surface_options, projected) {
        let Some(&base_material) = materials.get(surface.material_slot as usize) else {
            return false;
        };
        let material = match kind {
            WorldSurfaceKind::Wall { direction } => wall_material_for_direction(
                cached_uv_material(base_material),
                direction,
                surface.wall_faces_owner(),
            ),
            WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
                cached_uv_material(base_material)
            }
        };
        return submit_adaptive_cached_room_quad(
            cached_vertices,
            ids,
            projected,
            projected_metrics.hardware_extent_safe(),
            camera,
            surface.uv_words,
            surface.baked_vertex_rgb,
            material,
            &surface_options,
            match kind {
                WorldSurfaceKind::Wall { .. } => SPLIT_NW_SE,
                WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => surface.split,
            },
            matches!(kind, WorldSurfaceKind::Ceiling),
            Some(quad),
            primitives,
            world,
        );
    }
    let prepared_depth = prepared_depth.unwrap_or_else(|| {
        PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
    });
    world
        .try_submit_warmed_textured_gouraud_quad(
            quad,
            packet_verts,
            projected_metrics.hardware_extent_safe()
                && surface_options.textured_split_max_edge == 0,
            &surface_options,
            prepared_depth,
        )
        .is_some()
}

#[allow(unused_variables)]
#[inline(always)]
fn try_submit_shaded_encoded_warmed_room_quad<const OT: usize, L: WorldSurfaceLighting>(
    surface: &CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    projected: [ProjectedVertex; 4],
    projected_metrics: ProjectedQuadMetrics,
    vertex_depths: Option<[i32; 4]>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: &WorldSurfaceOptions,
    submit_depths: CachedRoomSubmitDepths,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    quad: &mut QuadTexturedGouraud,
    ready: u8,
    primitives: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> bool {
    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    let surface_risky = cached_surface_risk_for_modes(
        depth_mode,
        subdivision_mode,
        kind,
        surface,
        projected,
        projected_metrics.depth_span(),
    );
    let use_triangle_depth =
        cached_surface_uses_triangle_depth_with_risk(depth_mode, kind, surface_risky);
    let (surface_options, prepared_depth) = if use_triangle_depth {
        (triangle_depth_options(*options), None)
    } else if matches!(kind, WorldSurfaceKind::Wall { .. }) {
        (*options, submit_depths.vertical)
    } else {
        (horizontal_depth_options(*options), submit_depths.horizontal)
    };
    let surface_options = cached_surface_subdivision_options(
        surface_options,
        subdivision_mode,
        kind,
        use_triangle_depth,
        surface_risky,
    );
    if adaptive_warmed_quad_requires_dynamic_submit(&surface_options, projected) {
        // Fogged TR candidates cannot patch the authored root packet directly,
        // but their generated lighting adapter can still shade baked RGB
        // without the general surface-lighting path. Keep the canonical TR
        // emitter so its camera-space depth checks, lattice, underdraw, and
        // packet order remain the single source of truth.
        #[cfg(feature = "tr-subdivision-lattice")]
        if surface_options.adaptive_subdivision_profile.max_levels == 1
            && !surface_options.adaptive_debug_subdivision_levels
            && projected_metrics.hardware_extent_safe()
        {
            let Some(colors) = lighting
                .shade_prewarmed_baked_vertices(surface.sample_without_center(), vertex_depths)
            else {
                return false;
            };
            let Some(&base_material) = materials.get(surface.material_slot as usize) else {
                return false;
            };
            let (material, split, reverse_front) = match kind {
                WorldSurfaceKind::Wall { direction } => (
                    wall_material_for_direction(
                        cached_uv_material(base_material),
                        direction,
                        surface.wall_faces_owner(),
                    ),
                    SPLIT_NW_SE,
                    false,
                ),
                WorldSurfaceKind::Floor => {
                    (cached_uv_material(base_material), surface.split, false)
                }
                WorldSurfaceKind::Ceiling => {
                    (cached_uv_material(base_material), surface.split, true)
                }
            };
            let projected_for_cull = if reverse_front {
                reverse_quad_winding(projected)
            } else {
                projected
            };
            if projected_quad_backface_culled(
                projected_for_cull,
                material,
                CullMode::Back,
                split_triangles_runtime(split),
            ) {
                return true;
            }
            if submit_adaptive_cached_room_quad(
                cached_vertices,
                ids,
                projected,
                true,
                camera,
                surface.uv_words,
                colors,
                material,
                &surface_options,
                split,
                reverse_front,
                None,
                primitives,
                world,
            ) {
                return true;
            }
        }
        return false;
    }
    if encoded_warmed_room_quad_backface_culled(projected, ready) {
        return true;
    }
    let Some(colors) =
        lighting.shade_prewarmed_baked_vertices(surface.sample_without_center(), vertex_depths)
    else {
        return false;
    };
    let packet_verts = warmed_room_quad_packet_vertices_from_ready(projected, ready);
    let packet_colors = warmed_room_quad_packet_colors_from_ready(colors, ready);
    let prepared_depth = prepared_depth.unwrap_or_else(|| {
        PreparedTriangleDepth::from_quad_average::<OT>(surface_options, projected)
    });
    world
        .try_submit_warmed_textured_gouraud_quad_with_colors(
            quad,
            packet_verts,
            packet_colors,
            projected_metrics.hardware_extent_safe()
                && surface_options.textured_split_max_edge == 0,
            &surface_options,
            prepared_depth,
        )
        .is_some()
}

#[inline(always)]
pub(super) fn encoded_warmed_room_quad_backface_culled(
    mut projected: [ProjectedVertex; 4],
    ready: u8,
) -> bool {
    if ready & WARMED_ROOM_QUAD_DOUBLE_SIDED != 0 {
        return false;
    }
    if ready & WARMED_ROOM_QUAD_REVERSE != 0 {
        projected = reverse_quad_winding(projected);
    }
    let split = if ready & WARMED_ROOM_QUAD_SPLIT_NE_SW != 0 {
        SPLIT_NE_SW
    } else {
        SPLIT_NW_SE
    };
    let [(a, b, c), (d, e, f)] = split_triangles_runtime(split);
    projected_triangle_back_facing([projected[a], projected[b], projected[c]])
        && projected_triangle_back_facing([projected[d], projected[e], projected[f]])
}

#[inline(always)]
fn warmed_room_quad_packet_vertices(
    verts: [ProjectedVertex; 4],
    sidedness: SurfaceSidedness,
    split: u8,
    reverse_front: bool,
) -> [ProjectedVertex; 4] {
    warmed_room_quad_packet_values(verts, sidedness, split, reverse_front)
}

#[inline(always)]
fn warmed_room_quad_packet_values<T: Copy>(
    mut values: [T; 4],
    sidedness: SurfaceSidedness,
    split: u8,
    reverse_front: bool,
) -> [T; 4] {
    // `sidedness` deliberately does not reach the corner order; see
    // `quad_packet_order` in `world_render.rs`.
    let _ = sidedness;
    if reverse_front {
        values = reverse_quad_winding(values);
    }
    if split == SPLIT_NE_SW {
        [values[0], values[1], values[3], values[2]]
    } else {
        [values[1], values[0], values[2], values[3]]
    }
}

/// Rare near-plane fallback for the projected room cache.
///
/// Reconstructing four view-space vertices only when a cached projection is
/// invalid avoids both whole-surface popping and a permanent per-room view
/// arena. The clipped polygon is emitted as ordinary triangles, preserving
/// the PS1 extent splitter and interpolating UV/light values at the new edge.
fn draw_near_clipped_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: &CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    use_vertex_depths: bool,
    use_direct_baked_rgb: bool,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> u16 {
    let Some(vertices) = indexed_world_quad(cached_vertices, ids) else {
        return 0;
    };
    let views = [
        camera.view_vertex(vertices[0]),
        camera.view_vertex(vertices[1]),
        camera.view_vertex(vertices[2]),
        camera.view_vertex(vertices[3]),
    ];
    if views.iter().all(|view| view.z < camera.projection.near_z) {
        return 1;
    }

    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    let Some(&base_material) = materials.get(surface.material_slot as usize) else {
        return 0;
    };
    let uv_words = if base_material.animation.is_animated() {
        animated_cached_uv_words(base_material, options, surface.uv_words)
    } else {
        surface.uv_words
    };
    let base_material = cached_uv_material(base_material);
    let vertex_depths = use_vertex_depths.then(|| {
        [
            lighting.prepare_vertex_depth(views[0].z),
            lighting.prepare_vertex_depth(views[1].z),
            lighting.prepare_vertex_depth(views[2].z),
            lighting.prepare_vertex_depth(views[3].z),
        ]
    });
    let Some(colors) = indexed_vertex_lighting_colors(
        lighting,
        surface,
        base_material,
        cached_vertices,
        ids,
        vertex_depths,
        use_direct_baked_rgb,
    ) else {
        return 0;
    };

    let (material, reverse_front) = match kind {
        WorldSurfaceKind::Floor => (base_material, false),
        WorldSurfaceKind::Ceiling => (base_material, true),
        WorldSurfaceKind::Wall { direction } => (
            wall_material_for_direction(base_material, direction, surface.wall_faces_owner()),
            false,
        ),
    };
    let opts = triangle_depth_options(options)
        .with_cull_mode(cull_for_sidedness(material.sidedness, CullMode::Back))
        .with_material_layer(material.texture);
    let split = split_triangles_runtime(surface.split);
    let count = if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        1
    } else {
        2
    };
    let mut triangle_index = 0usize;
    while triangle_index < count {
        let selected = if count == 1 {
            surface.triangle_index as usize
        } else {
            triangle_index
        };
        let (a, mut b, mut c) = split[selected.min(1)];
        if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
            core::mem::swap(&mut b, &mut c);
        }
        let stats = world.submit_textured_gouraud_view_triangle_uv_words(
            triangles,
            [views[a], views[b], views[c]],
            [uv_words[a], uv_words[b], uv_words[c]],
            [colors[a], colors[b], colors[c]],
            camera.projection,
            material.texture,
            opts,
        );
        if stats.primitive_overflow || stats.command_overflow {
            break;
        }
        triangle_index += 1;
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

#[inline(always)]
fn adaptive_projected_quad_needs_subdivision(
    projected: [ProjectedVertex; 4],
    profile: AdaptiveSubdivisionProfile,
) -> bool {
    projected[0]
        .sz
        .max(projected[1].sz)
        .max(projected[2].sz)
        .max(projected[3].sz)
        < profile.far_depth
}

#[inline(always)]
fn adaptive_debug_root_colors<const N: usize>(
    options: WorldSurfaceOptions,
    colors: [(u8, u8, u8); N],
) -> [(u8, u8, u8); N] {
    if options.adaptive_debug_subdivision_levels {
        [(255, 0, 0); N]
    } else {
        colors
    }
}

/// Warmed authored quads can only take the direct packet path when TR
/// subdivision will not replace them with dynamically generated children.
#[inline(always)]
pub(super) fn adaptive_warmed_quad_requires_dynamic_submit(
    options: &WorldSurfaceOptions,
    projected: [ProjectedVertex; 4],
) -> bool {
    options.adaptive_debug_subdivision_levels
        || (options.adaptive_subdivision
            && adaptive_projected_quad_needs_subdivision(
                projected,
                options.adaptive_subdivision_profile,
            ))
}

#[inline(always)]
fn adaptive_projected_triangle_needs_subdivision(
    projected: [ProjectedVertex; 4],
    profile: AdaptiveSubdivisionProfile,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    sidedness: SurfaceSidedness,
) -> bool {
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    projected[tri.0]
        .sz
        .max(projected[tri.1].sz)
        .max(projected[tri.2].sz)
        < profile.far_depth
}

fn submit_adaptive_cached_room_triangle<const OT: usize>(
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    camera: &WorldCamera,
    uv_words: [u16; 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: &WorldSurfaceOptions,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    triangles: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> bool {
    let Some(vertices) = indexed_world_quad(cached_vertices, ids) else {
        return false;
    };
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    let views = [
        camera.view_vertex(vertices[tri.0]),
        camera.view_vertex(vertices[tri.1]),
        camera.view_vertex(vertices[tri.2]),
    ];
    let options = (*options)
        .with_cull_mode(cull_for_sidedness(material.sidedness, CullMode::Back))
        .with_material_layer(material.texture);
    let _ = world.submit_adaptive_textured_gouraud_view_triangle_uv_words(
        triangles,
        views,
        [uv_words[tri.0], uv_words[tri.1], uv_words[tri.2]],
        [colors[tri.0], colors[tri.1], colors[tri.2]],
        camera.projection,
        material.texture,
        &options,
    );
    true
}

fn submit_adaptive_cached_room_quad<const OT: usize>(
    cached_vertices: &[WorldVertex],
    ids: [u16; 4],
    projected: [ProjectedVertex; 4],
    root_extent_safe: bool,
    camera: &WorldCamera,
    uv_words: [u16; 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: &WorldSurfaceOptions,
    split: u8,
    reverse_front: bool,
    warmed_root: Option<&mut QuadTexturedGouraud>,
    primitives: &mut impl RoomSurfaceSink,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> bool {
    let Some(vertices) = indexed_world_quad(cached_vertices, ids) else {
        return false;
    };
    let views = [
        camera.view_vertex(vertices[0]),
        camera.view_vertex(vertices[1]),
        camera.view_vertex(vertices[2]),
        camera.view_vertex(vertices[3]),
    ];
    let packet_views =
        warmed_room_quad_packet_values(views, material.sidedness, split, reverse_front);
    let packet_projected =
        warmed_room_quad_packet_values(projected, material.sidedness, split, reverse_front);
    let packet_uv_words =
        warmed_room_quad_packet_values(uv_words, material.sidedness, split, reverse_front);
    let packet_colors =
        warmed_room_quad_packet_values(colors, material.sidedness, split, reverse_front);
    let options = (*options)
        .with_cull_mode(cull_for_sidedness(material.sidedness, CullMode::Back))
        .with_material_layer(material.texture);
    let stats = world.submit_adaptive_textured_gouraud_view_quad_uv_words(
        primitives,
        packet_views,
        Some(packet_projected),
        root_extent_safe,
        warmed_root,
        packet_uv_words,
        packet_colors,
        camera.projection,
        material.texture,
        &options,
    );
    // The caller treats `true` as "this surface is drawn" and skips its own
    // whole-quad submit. Discarding these stats and returning `true`
    // unconditionally therefore turned any subdivision that emitted nothing
    // into a silent hole with no counter recording it -- the cortex_v1 rooms
    // 6/7 floor report. Report success only when geometry actually reached the
    // sink, so a failed subdivision falls back to the authored quad.
    stats.submitted_triangles > 0 && !stats.primitive_overflow && !stats.command_overflow
}

#[cfg(test)]
pub(super) fn cached_surface_uses_triangle_depth(
    mode: CachedRoomDepthMode,
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    let depth_span = cached_surface_projected_depth_span(&surface, projected);
    cached_surface_uses_triangle_depth_with_risk(
        mode,
        kind,
        cached_surface_is_risky(kind, &surface, projected, depth_span),
    )
}

#[inline(always)]
fn cached_surface_uses_triangle_depth_with_risk(
    mode: CachedRoomDepthMode,
    kind: WorldSurfaceKind,
    surface_risky: bool,
) -> bool {
    match mode {
        CachedRoomDepthMode::FixedCell => false,
        CachedRoomDepthMode::PerTriangle => true,
        CachedRoomDepthMode::Hybrid => match kind {
            WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => surface_risky,
            WorldSurfaceKind::Wall { .. } => false,
        },
        CachedRoomDepthMode::HybridWalls => surface_risky,
    }
}

#[inline(always)]
fn cached_surface_risk_for_modes(
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    kind: WorldSurfaceKind,
    surface: &CachedRoomSurface,
    projected: [ProjectedVertex; 4],
    depth_span: i32,
) -> bool {
    if matches!(
        depth_mode,
        CachedRoomDepthMode::Hybrid | CachedRoomDepthMode::HybridWalls
    ) || matches!(subdivision_mode, CachedRoomSubdivisionMode::Risky)
    {
        cached_surface_is_risky(kind, surface, projected, depth_span)
    } else {
        false
    }
}

pub(super) fn cached_surface_subdivision_options(
    options: WorldSurfaceOptions,
    mode: CachedRoomSubdivisionMode,
    kind: WorldSurfaceKind,
    use_triangle_depth: bool,
    surface_risky: bool,
) -> WorldSurfaceOptions {
    let kind_mask = match kind {
        WorldSurfaceKind::Floor => AdaptiveSubdivisionKindMask::FLOOR,
        WorldSurfaceKind::Ceiling => AdaptiveSubdivisionKindMask::CEILING,
        WorldSurfaceKind::Wall { .. } => AdaptiveSubdivisionKindMask::WALL,
    };
    let allow_visual_subdivision = match mode {
        CachedRoomSubdivisionMode::All => true,
        CachedRoomSubdivisionMode::DepthSorted => use_triangle_depth,
        CachedRoomSubdivisionMode::Risky => surface_risky,
    } && options.adaptive_subdivision_kinds.contains(kind_mask);
    if allow_visual_subdivision {
        options
    } else {
        options
            .with_textured_triangle_max_edge(0)
            .with_adaptive_subdivision(false)
    }
}

fn cached_surface_is_risky(
    kind: WorldSurfaceKind,
    surface: &CachedRoomSurface,
    projected: [ProjectedVertex; 4],
    depth_span: i32,
) -> bool {
    let depth_span = if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        cached_surface_projected_depth_span(surface, projected)
    } else {
        depth_span
    };
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            cached_horizontal_surface_is_risky(surface, depth_span)
        }
        WorldSurfaceKind::Wall { .. } => depth_span >= HYBRID_HORIZONTAL_DEPTH_SPAN,
    }
}

fn cached_horizontal_surface_is_risky(surface: &CachedRoomSurface, depth_span: i32) -> bool {
    if surface.kind_flags & CACHED_SURFACE_HORIZONTAL_NON_FLAT != 0 {
        return true;
    }
    depth_span >= HYBRID_HORIZONTAL_DEPTH_SPAN
}

fn cached_surface_projected_depth_span(
    surface: &CachedRoomSurface,
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

#[derive(Copy, Clone)]
struct ProjectedQuadMetrics {
    min_x: i16,
    max_x: i16,
    min_y: i16,
    max_y: i16,
    min_z: i32,
    max_z: i32,
}

impl ProjectedQuadMetrics {
    #[inline(always)]
    fn new(projected: [ProjectedVertex; 4]) -> Self {
        let min_x = projected[0]
            .sx
            .min(projected[1].sx)
            .min(projected[2].sx)
            .min(projected[3].sx);
        let max_x = projected[0]
            .sx
            .max(projected[1].sx)
            .max(projected[2].sx)
            .max(projected[3].sx);
        let min_y = projected[0]
            .sy
            .min(projected[1].sy)
            .min(projected[2].sy)
            .min(projected[3].sy);
        let max_y = projected[0]
            .sy
            .max(projected[1].sy)
            .max(projected[2].sy)
            .max(projected[3].sy);
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
        Self {
            min_x,
            max_x,
            min_y,
            max_y,
            min_z,
            max_z,
        }
    }

    #[inline(always)]
    fn outside_screen(self, bounds: ProjectedScreenBounds) -> bool {
        i32::from(self.max_x) < bounds.left
            || i32::from(self.min_x) > bounds.right
            || i32::from(self.max_y) < bounds.top
            || i32::from(self.min_y) > bounds.bottom
    }

    #[inline(always)]
    fn depth_span(self) -> i32 {
        self.max_z.saturating_sub(self.min_z)
    }

    #[inline(always)]
    fn hardware_extent_safe(self) -> bool {
        crate::render3d::projected_model_bounds_hw_extent_safe(
            self.min_x, self.max_x, self.min_y, self.max_y,
        )
    }
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
    surface: &CachedRoomSurface,
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
    // Room vertices are already projected and the GTE is idle during the
    // surface walk. Use the hardware NCLIP path with the silicon-measured
    // input/result gaps instead of paying two serialized CPU multiplies for
    // every candidate face.
    psx_gte::scene::screen_area_mac0_scheduled([
        (verts[0].sx, verts[0].sy),
        (verts[1].sx, verts[1].sy),
        (verts[2].sx, verts[2].sy),
    ]) <= 0
}

const fn cached_uv_material(mut material: WorldRenderMaterial) -> WorldRenderMaterial {
    material.texture_width = ROOM_TEXTURE_UV_SIZE;
    material.texture_height = ROOM_TEXTURE_UV_SIZE;
    material
}

#[inline(always)]
fn animated_cached_uv_words(
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    uv_words: [u16; 4],
) -> [u16; 4] {
    let (offset_u, offset_v) = material.animation.uv_offset(
        options.material_animation_tick,
        options.material_animation_hz,
        material.texture_width,
        material.texture_height,
    );
    if offset_u == 0 && offset_v == 0 {
        return uv_words;
    }
    uv_words.map(|word| {
        let u = (word as u8).wrapping_add(offset_u);
        let v = ((word >> 8) as u8).wrapping_add(offset_v);
        u16::from(u) | (u16::from(v) << 8)
    })
}

#[cfg(all(test, feature = "room-surface-profile"))]
mod warp_probe_tests {
    use super::*;

    /// Build a quad lattice with the given per-vertex depths and UV span.
    fn quad(depths: [i32; 4], uv: [(u8, u8); 4]) -> ([ProjectedVertex; 4], [u16; 4]) {
        let mut p = [ProjectedVertex {
            sx: 0,
            sy: 0,
            sz: 0,
        }; 4];
        let mut w = [0u16; 4];
        for i in 0..4 {
            p[i].sz = depths[i];
            w[i] = uv[i].0 as u16 | ((uv[i].1 as u16) << 8);
        }
        (p, w)
    }

    /// The guest runs integer fixed-point; the bench that produced the rule
    /// runs f64. Check the port agrees with the reference to within the
    /// truncation we accepted, on a hard case (5:1 depth ratio, full UV span).
    #[test]
    fn integer_port_matches_the_float_closed_form() {
        // tl/tr at depth 200, bl/br at 1000: depth varies down the surface.
        let (p, w) = quad([200, 200, 1000, 1000], [(0, 0), (63, 0), (0, 63), (63, 63)]);
        let got = predicted_warp_16ths(p, w);

        // Reference: du * |zb-za| / (2*(za+zb)), calibrated x2.4, in 1/16ths.
        let want = 63.0 * 800.0 / (2.0 * 1200.0) * 2.4 * 16.0;
        let err = (got as f64 - want).abs() / want;
        assert!(
            err < 0.02,
            "got {got}, want {want:.1} ({:.1}% off)",
            err * 100.0
        );
    }

    #[test]
    fn constant_depth_surface_cannot_warp() {
        let (p, w) = quad([500; 4], [(0, 0), (63, 0), (0, 63), (63, 63)]);
        assert_eq!(predicted_warp_16ths(p, w), 0);
    }

    #[test]
    fn error_is_proportional_to_uv_span() {
        let full = quad([200, 200, 1000, 1000], [(0, 0), (63, 0), (0, 63), (63, 63)]);
        let half = quad([200, 200, 1000, 1000], [(0, 0), (31, 0), (0, 31), (31, 31)]);
        let (a, b) = (
            predicted_warp_16ths(full.0, full.1),
            predicted_warp_16ths(half.0, half.1),
        );
        // Halving the texture span on the surface halves the warp. This is the
        // `uvhalf` control from the bench, which measured exactly that.
        assert!(
            (a as i32 - 2 * b as i32).abs() <= 16,
            "full {a}, half {b}: not proportional"
        );
    }

    #[test]
    fn vertices_behind_the_near_plane_are_skipped_not_counted() {
        let (p, w) = quad([-10, -10, 1000, 1000], [(0, 0), (63, 0), (0, 63), (63, 63)]);
        // Only the two far vertices are valid, and they share a depth, so
        // there is no usable edge and the probe must report nothing rather
        // than a garbage ratio from a negative denominator.
        assert_eq!(predicted_warp_16ths(p, w), 0);
    }
}
