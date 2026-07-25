use super::*;

/// Predecode all renderable floor, ceiling, and wall surfaces in a
/// room into caller-owned fixed arrays.
///
/// Cell headers are written in `(x, z)` order only for populated cells.
/// Surface records are only written for populated sectors that
/// reference a material slot and have valid geometry.
/// If either output slice is too small, `overflow` is set and callers
/// should fall back to the uncached room renderer for that room.
pub fn cache_room_vertex_lit_surfaces(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    cells_out: &mut [CachedRoomCell],
    vertices_out: &mut [WorldVertex],
    surfaces_out: &mut [CachedRoomSurface],
) -> CachedRoomSurfaceCacheStats {
    let width = room.width();
    let depth = room.depth();

    let sector_size = room.sector_size();
    let mut cell_count = 0usize;
    let mut vertex_count = 0usize;
    let mut surface_count = 0usize;
    let mut sx = 0u16;
    while sx < width {
        let mut sz = 0u16;
        while sz < depth {
            let surface_first = surface_count;

            let Some(sector) = room.sector(sx, sz) else {
                sz += 1;
                continue;
            };

            if sector.has_floor() {
                let heights = sector.floor_heights();
                let split = sector.floor_split();
                if let Some((slot, uvs)) = merged_floor_surface(sector) {
                    let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                    let Some(vertex_indices) =
                        cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                    else {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    };
                    let sample = WorldSurfaceSample {
                        kind: WorldSurfaceKind::Floor,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                        ordinal: 0,
                    };
                    if !cache_room_surface(
                        surfaces_out,
                        &mut surface_count,
                        CachedRoomSurface::new(
                            slot,
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        )
                        .with_horizontal_non_flat(horizontal_heights_non_flat4(heights)),
                    ) {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    }
                } else {
                    for triangle_index in 0..2 {
                        if !sector.floor_triangle_present(triangle_index) {
                            continue;
                        }
                        let Some(slot) = sector.floor_triangle_material(triangle_index) else {
                            continue;
                        };
                        let triangle_heights = sector.floor_triangle_heights(triangle_index);
                        let vertices = horizontal_triangle_vertices(
                            sx,
                            sz,
                            sector_size,
                            split,
                            triangle_index,
                            triangle_heights,
                            heights,
                        );
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Floor,
                            sx,
                            sz,
                            center: horizontal_triangle_center(
                                sx,
                                sz,
                                sector_size,
                                triangle_heights_to_quad(
                                    heights,
                                    split,
                                    triangle_index,
                                    triangle_heights,
                                ),
                                split,
                                triangle_index,
                            ),
                            baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                            ordinal: triangle_index as u16,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                slot,
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.floor_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            )
                            .with_horizontal_non_flat(
                                horizontal_heights_non_flat3(triangle_heights),
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
            }

            if sector.has_ceiling() {
                let heights = sector.ceiling_heights();
                let split = sector.ceiling_split();
                if let Some((slot, uvs)) = merged_ceiling_surface(sector) {
                    let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                    let Some(vertex_indices) =
                        cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                    else {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    };
                    let sample = WorldSurfaceSample {
                        kind: WorldSurfaceKind::Ceiling,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                        ordinal: 0,
                    };
                    if !cache_room_surface(
                        surfaces_out,
                        &mut surface_count,
                        CachedRoomSurface::new(
                            slot,
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        )
                        .with_horizontal_non_flat(horizontal_heights_non_flat4(heights)),
                    ) {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    }
                } else {
                    for triangle_index in 0..2 {
                        if !sector.ceiling_triangle_present(triangle_index) {
                            continue;
                        }
                        let Some(slot) = sector.ceiling_triangle_material(triangle_index) else {
                            continue;
                        };
                        let triangle_heights = sector.ceiling_triangle_heights(triangle_index);
                        let vertices = horizontal_triangle_vertices(
                            sx,
                            sz,
                            sector_size,
                            split,
                            triangle_index,
                            triangle_heights,
                            heights,
                        );
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Ceiling,
                            sx,
                            sz,
                            center: horizontal_triangle_center(
                                sx,
                                sz,
                                sector_size,
                                triangle_heights_to_quad(
                                    heights,
                                    split,
                                    triangle_index,
                                    triangle_heights,
                                ),
                                split,
                                triangle_index,
                            ),
                            baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                            ordinal: triangle_index as u16,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                slot,
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.ceiling_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            )
                            .with_horizontal_non_flat(
                                horizontal_heights_non_flat3(triangle_heights),
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
            }

            let mut i = 0;
            while i < sector.wall_count() {
                if let Some(wall) = room.sector_wall(sector, i) {
                    if let Some(vertices) =
                        wall_vertices(sx, sz, sector_size, wall.direction(), wall.heights())
                    {
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let (split, triangle_index) = wall_shape_triangle(wall.shape())
                            .unwrap_or((SPLIT_NW_SE, WHOLE_QUAD_TRIANGLE_INDEX));
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Wall {
                                direction: wall.direction(),
                            },
                            sx,
                            sz,
                            center: wall_shape_center(vertices, wall.shape()),
                            baked_vertex_rgb: baked_vertex_rgb(room.wall_light(sector, i)),
                            ordinal: i,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                wall.material(),
                                vertex_indices,
                                cached_material_uvs(materials, wall.material(), wall.uvs()),
                                sample,
                                split,
                                triangle_index,
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
                i += 1;
            }

            let surface_len = surface_count.saturating_sub(surface_first);
            if surface_len > u16::MAX as usize
                || surface_first > u16::MAX as usize
                || cell_count > u16::MAX as usize
            {
                return CachedRoomSurfaceCacheStats {
                    cell_count,
                    surface_count,
                    vertex_count,
                    overflow: true,
                };
            }
            if surface_len > 0 {
                if cell_count >= cells_out.len() {
                    return CachedRoomSurfaceCacheStats {
                        cell_count,
                        surface_count,
                        vertex_count,
                        overflow: true,
                    };
                }
                let (min_y, max_y) = sector_y_bounds(room, sector);
                cells_out[cell_count] = CachedRoomCell::new(
                    sx,
                    sz,
                    sector_size,
                    min_y,
                    max_y,
                    surface_first as u16,
                    surface_len as u16,
                    0,
                    0,
                );
                cell_count += 1;
            }

            sz += 1;
        }
        sx += 1;
    }

    CachedRoomSurfaceCacheStats {
        cell_count,
        surface_count,
        vertex_count,
        overflow: false,
    }
}
