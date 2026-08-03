use super::*;

/// Walk every populated sector of `room`, emitting one textured
/// quad per floor / ceiling face plus one per wall.
///
/// `materials` is indexed by the slot ids returned from
/// [`SectorRender::floor_material`], [`SectorRender::ceiling_material`]
/// and [`WallRender::material`]. A face whose slot points past the
/// table is dropped silently -- friendlier than a panic while the
/// author is mid-iteration with partially-assigned materials.
///
/// Cells are corner-rooted at world `(0, 0)`: cell `(sx, sz)`
/// occupies `x ∈ [sx*S, (sx+1)*S]`, `z ∈ [sz*S, (sz+1)*S]`.
/// Position the camera target at the room's centre -- typically
/// `(W*S/2, 0, D*S/2)` -- so the orbit lands on the geometry.
///
/// `options` carries the depth band + range. Per-material
/// [`SurfaceSidedness`] selects front-only, back-only, or
/// double-sided emission; front-sided faces use [`CullMode::Back`].
///
/// # Quad corner conventions
///
/// All four-corner inputs to [`WorldRenderPass::submit_textured_quad`]
/// are emitted in perimeter order. The renderer splits along the
/// `0`–`2` diagonal (see `TEXTURED_QUAD_TRIANGLES` in `render3d.rs`),
/// so corner positions and UVs must agree on what `0`, `1`, `2`,
/// `3` mean.
///
/// * **Floors / ceilings** -- records store `[NW, NE, SE, SW]`.
///   Floors keep that top-facing winding; ceilings flip to the
///   inward underside winding. UVs are transformed with the vertices.
/// * **Walls** -- runtime records store `[bottom-left, bottom-right,
///   top-right, top-left]` for an owning cell edge. That physical corner
///   order makes the wall back side face the owning cell/interior. Wall
///   emission swaps Front/Back material intent so authors can use a
///   front-sided material for the common one-sided interior wall case.
///
/// [`SectorRender::floor_material`]: crate::SectorRender::floor_material
/// [`SectorRender::ceiling_material`]: crate::SectorRender::ceiling_material
/// [`WallRender::material`]: crate::WallRender::material
pub fn draw_room<const OT: usize>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    draw_room_lit(
        room,
        materials,
        &NoWorldSurfaceLighting,
        camera,
        options,
        triangles,
        world,
    );
}

/// Draw a room while giving the caller one material-shading hook per
/// emitted floor, ceiling, and wall surface.
pub fn draw_room_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    for sx in 0..room.width() {
        for sz in 0..room.depth() {
            let Some(sector) = room.sector(sx, sz) else {
                continue;
            };
            let _ = draw_sector_lit(
                room, sx, sz, sector, materials, lighting, camera, options, triangles, world,
            );
        }
    }
}

/// Draw a room through a coarse grid visibility pass.
///
/// Traversal is ring-ordered from farthest to nearest around
/// `visibility.anchor`, which gives bucketed ordering a stable coarse
/// back-to-front submission order before the PS1 ordering table handles
/// per-triangle depth buckets.
pub fn draw_room_lit_grid_visible<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visibility: GridVisibility,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let width = room.width();
    let depth = room.depth();
    if width == 0 || depth == 0 {
        return stats;
    }

    let sector_size = room.sector_size().max(1);
    let anchor_x = grid_cell_for_world(visibility.anchor.x, sector_size).clamp(0, width as i32 - 1);
    let anchor_z = grid_cell_for_world(visibility.anchor.z, sector_size).clamp(0, depth as i32 - 1);
    let radius = visibility.radius_cells as i32;
    let min_x = (anchor_x - radius).max(0) as u16;
    let max_x = (anchor_x + radius).min(width as i32 - 1) as u16;
    let min_z = (anchor_z - radius).max(0) as u16;
    let max_z = (anchor_z + radius).min(depth as i32 - 1) as u16;

    let max_ring_x = (anchor_x - min_x as i32).max(max_x as i32 - anchor_x);
    let max_ring_z = (anchor_z - min_z as i32).max(max_z as i32 - anchor_z);
    let mut ring = max_ring_x.max(max_ring_z);
    loop {
        let mut sx = min_x;
        while sx <= max_x {
            let mut sz = min_z;
            while sz <= max_z {
                let dx = ((sx as i32) - anchor_x).abs();
                let dz = ((sz as i32) - anchor_z).abs();
                if dx.max(dz) == ring {
                    if let Some(sector) = room.sector(sx, sz) {
                        stats.cells_considered = stats.cells_considered.wrapping_add(1);
                        let (min_y, max_y) = sector_y_bounds(room, sector);
                        if !cell_visible_to_camera(
                            camera,
                            options,
                            sx,
                            sz,
                            sector_size,
                            min_y,
                            max_y,
                            visibility.screen_margin,
                        ) {
                            stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                        } else {
                            stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
                            let cell_options = tile_depth_options(
                                options,
                                camera,
                                GridVisibleCell::new(sx, sz, min_y, max_y),
                                sector_size,
                            );
                            stats.surfaces_considered =
                                stats.surfaces_considered.wrapping_add(draw_sector_lit(
                                    room,
                                    sx,
                                    sz,
                                    sector,
                                    materials,
                                    lighting,
                                    camera,
                                    cell_options,
                                    triangles,
                                    world,
                                ));
                        }
                    }
                }
                if sz == max_z {
                    break;
                }
                sz += 1;
            }
            if sx == max_x {
                break;
            }
            sx += 1;
        }
        if ring == 0 {
            break;
        }
        ring -= 1;
    }

    stats
}

/// Draw a room using one textured Gouraud triangle per emitted
/// triangle. The lighting hook is evaluated at every surface corner,
/// which gives static point lights a smooth per-vertex falloff while
/// preserving authored texture windows/UV tiling.
pub fn draw_room_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    for sx in 0..room.width() {
        for sz in 0..room.depth() {
            let Some(sector) = room.sector(sx, sz) else {
                continue;
            };
            let _ = draw_sector_vertex_lit(
                room, sx, sz, sector, materials, lighting, camera, options, triangles, world,
            );
        }
    }
}

/// Draw a vertex-lit room through the same coarse grid visibility pass
/// used by [`draw_room_lit_grid_visible`].
pub fn draw_room_vertex_lit_grid_visible<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visibility: GridVisibility,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let width = room.width();
    let depth = room.depth();
    if width == 0 || depth == 0 {
        return stats;
    }

    let sector_size = room.sector_size().max(1);
    let anchor_x = grid_cell_for_world(visibility.anchor.x, sector_size).clamp(0, width as i32 - 1);
    let anchor_z = grid_cell_for_world(visibility.anchor.z, sector_size).clamp(0, depth as i32 - 1);
    let radius = visibility.radius_cells as i32;
    let min_x = (anchor_x - radius).max(0) as u16;
    let max_x = (anchor_x + radius).min(width as i32 - 1) as u16;
    let min_z = (anchor_z - radius).max(0) as u16;
    let max_z = (anchor_z + radius).min(depth as i32 - 1) as u16;

    let max_ring_x = (anchor_x - min_x as i32).max(max_x as i32 - anchor_x);
    let max_ring_z = (anchor_z - min_z as i32).max(max_z as i32 - anchor_z);
    let mut ring = max_ring_x.max(max_ring_z);
    loop {
        let mut sx = min_x;
        while sx <= max_x {
            let mut sz = min_z;
            while sz <= max_z {
                let dx = ((sx as i32) - anchor_x).abs();
                let dz = ((sz as i32) - anchor_z).abs();
                if dx.max(dz) == ring {
                    if let Some(sector) = room.sector(sx, sz) {
                        stats.cells_considered = stats.cells_considered.wrapping_add(1);
                        let (min_y, max_y) = sector_y_bounds(room, sector);
                        if !cell_visible_to_camera(
                            camera,
                            options,
                            sx,
                            sz,
                            sector_size,
                            min_y,
                            max_y,
                            visibility.screen_margin,
                        ) {
                            stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
                        } else {
                            stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
                            let cell_options = tile_depth_options(
                                options,
                                camera,
                                GridVisibleCell::new(sx, sz, min_y, max_y),
                                sector_size,
                            );
                            stats.surfaces_considered =
                                stats
                                    .surfaces_considered
                                    .wrapping_add(draw_sector_vertex_lit(
                                        room,
                                        sx,
                                        sz,
                                        sector,
                                        materials,
                                        lighting,
                                        camera,
                                        cell_options,
                                        triangles,
                                        world,
                                    ));
                        }
                    }
                }
                if sz == max_z {
                    break;
                }
                sz += 1;
            }
            if sx == max_x {
                break;
            }
            sx += 1;
        }
        if ring == 0 {
            break;
        }
        ring -= 1;
    }

    stats
}

/// Draw a vertex-lit room from a cooked far-to-near visible-cell
/// list. This avoids rebuilding the same ring traversal and cell
/// bounds every frame; the caller supplies PVS/portal-filtered cells
/// generated by the editor cook.
pub fn draw_room_vertex_lit_visible_cells<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let sector_size = room.sector_size().max(1);
    for cell in cells {
        let Some(sector) = room.sector(cell.x, cell.z) else {
            continue;
        };
        stats.cells_considered = stats.cells_considered.wrapping_add(1);
        if !cell_visible_to_camera(
            camera,
            options,
            cell.x,
            cell.z,
            sector_size.max(1),
            cell.min_y,
            cell.max_y,
            screen_margin,
        ) {
            stats.cells_frustum_culled = stats.cells_frustum_culled.wrapping_add(1);
            continue;
        }
        stats.cells_drawn = stats.cells_drawn.wrapping_add(1);
        let cell_options = tile_depth_options(options, camera, *cell, sector_size);
        stats.surfaces_considered = stats
            .surfaces_considered
            .wrapping_add(draw_sector_vertex_lit(
                room,
                cell.x,
                cell.z,
                sector,
                materials,
                lighting,
                camera,
                cell_options,
                triangles,
                world,
            ));
    }
    stats
}
