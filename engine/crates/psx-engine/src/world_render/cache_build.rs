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
                            )
                            .with_wall_faces_owner(
                                wall_faces_owning_cell(room, sx, sz, wall.direction()),
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

/// Whether a cardinal wall's only reachable side is the cell that owns it.
///
/// Cardinal wall windings put the owning cell's interior on the back face, so a
/// wall is culled for a player standing inside its own cell. Making every wall
/// double-sided fixes that but doubles its raster work. Grid adjacency settles
/// most of them for free: if the cell across the wall has no floor -- or is
/// outside the room, which is the room's outer shell -- then nobody can stand
/// on the far side, the owning cell is the only viewpoint, and one face is
/// enough.
///
/// Only `true` is a claim. `false` means "not proven", and the renderer falls
/// back to both faces, so a wrong answer here costs cycles rather than
/// geometry. Diagonals are excluded: they cut through a cell rather than
/// bounding it, so they have no single neighbour.
fn wall_faces_owning_cell(room: RoomRender<'_, '_>, sx: u16, sz: u16, direction: u8) -> bool {
    let Some((nx, nz)) = wall_neighbour_cell(sx, sz, direction) else {
        // Diagonal, or off the near edge of the room.
        return matches!(direction, DIR_NORTH | DIR_SOUTH | DIR_WEST | DIR_EAST);
    };
    if nx >= room.width() || nz >= room.depth() {
        return true;
    }
    room.sector(nx, nz).is_none_or(|sector| !sector.has_floor())
}

/// Cell on the far side of a cardinal wall, or `None` for a diagonal or a wall
/// on the room's `0` edge.
///
/// The offsets follow `wall_vertices`: north sits on the cell's low-z edge and
/// west on its low-x edge, so their neighbours are the lower cells. Getting a
/// sign wrong here reintroduces the disappearing-wall bug by proving the wrong
/// cell empty, which is why this is separated out and tested.
const fn wall_neighbour_cell(sx: u16, sz: u16, direction: u8) -> Option<(u16, u16)> {
    match direction {
        DIR_NORTH => match sz.checked_sub(1) {
            Some(nz) => Some((sx, nz)),
            None => None,
        },
        DIR_SOUTH => Some((sx, sz + 1)),
        DIR_WEST => match sx.checked_sub(1) {
            Some(nx) => Some((nx, sz)),
            None => None,
        },
        DIR_EAST => Some((sx + 1, sz)),
        _ => None,
    }
}

#[cfg(test)]
mod wall_orientation_tests {
    use super::*;

    #[test]
    fn cardinal_neighbours_match_wall_vertex_edges() {
        // North/west walls sit on the low edge, so they face the lower cell.
        assert_eq!(wall_neighbour_cell(4, 4, DIR_NORTH), Some((4, 3)));
        assert_eq!(wall_neighbour_cell(4, 4, DIR_SOUTH), Some((4, 5)));
        assert_eq!(wall_neighbour_cell(4, 4, DIR_WEST), Some((3, 4)));
        assert_eq!(wall_neighbour_cell(4, 4, DIR_EAST), Some((5, 4)));
    }

    #[test]
    fn room_edge_and_diagonals_have_no_single_neighbour() {
        assert_eq!(wall_neighbour_cell(0, 0, DIR_NORTH), None);
        assert_eq!(wall_neighbour_cell(0, 0, DIR_WEST), None);
        assert_eq!(wall_neighbour_cell(4, 4, DIR_NORTH_WEST_SOUTH_EAST), None);
        assert_eq!(wall_neighbour_cell(4, 4, DIR_NORTH_EAST_SOUTH_WEST), None);
    }

    #[test]
    fn owner_facing_wall_keeps_one_face_and_others_keep_both() {
        let base = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)));
        // Proven owner-only: no front/back swap, so the visible face is the one
        // the player can reach, and it stays single-sided.
        // Same swap as the both-faces path, one face instead of two. The
        // orientation must match what renders correctly there, not the
        // authored front.
        let owned = wall_material_for_direction(base, DIR_NORTH, true);
        assert_eq!(owned.sidedness, SurfaceSidedness::Back);
        assert_ne!(owned.sidedness, SurfaceSidedness::Both, "one face, not two");
        // Unproven: the conservative both-faces answer this replaced.
        assert_eq!(
            wall_material_for_direction(base, DIR_NORTH, false).sidedness,
            SurfaceSidedness::Both
        );
        // A diagonal cuts through a cell and is used from both sides even when
        // the flag is set.
        assert_eq!(
            wall_material_for_direction(base, DIR_NORTH_WEST_SOUTH_EAST, true).sidedness,
            SurfaceSidedness::Both
        );
    }
}
