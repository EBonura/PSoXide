use super::*;
use crate::MaterialAnimationMode;

pub(crate) fn append_room_visibility(
    room_index: u16,
    cooked: &CookedWorldGrid,
    visibility_radius: u16,
    room_visibility: &mut Vec<PlaytestRoomVisibility>,
    visibility_cells: &mut Vec<PlaytestVisibilityCell>,
    visibility_pvs: &mut Vec<PlaytestVisibilityPvs>,
    visibility_pvs_bits: &mut Vec<u8>,
) {
    let cell_first = u16::try_from(visibility_cells.len()).unwrap_or(u16::MAX);
    let mut local_cells = build_visibility_cells(room_index, cooked);
    let cell_count = u16::try_from(local_cells.len()).unwrap_or(u16::MAX);
    let index_by_coord = visibility_index_by_coord(cooked.width, cooked.depth, &local_cells);
    assign_visibility_portals(
        cooked.width,
        cooked.depth,
        &index_by_coord,
        &mut local_cells,
    );
    let pvs_first = u32::try_from(visibility_pvs.len()).unwrap_or(u32::MAX);
    append_visibility_pvs(
        cooked.width,
        cooked.depth,
        &local_cells,
        &index_by_coord,
        visibility_radius,
        visibility_pvs,
        visibility_pvs_bits,
    );
    let pvs_count =
        u16::try_from(visibility_pvs.len().saturating_sub(pvs_first as usize)).unwrap_or(u16::MAX);

    visibility_cells.extend(local_cells);
    room_visibility.push(PlaytestRoomVisibility {
        room: room_index,
        cell_first,
        cell_count,
        pvs_first,
        pvs_count,
    });
}

/// Rebuild every room-local PVS after the complete portal graph is known.
///
/// The first visibility pass runs while rooms are still being cooked, before
/// vertical and terrace portals exist.  A room-local flood therefore cannot
/// see a cell on the far side of a shallow layered recess when the only open
/// path briefly crosses into the lower runtime room and back out.  This pass
/// walks the final, global cell graph but still writes the compact room-local
/// bitsets expected by the runtime.  There is no runtime graph walk and no
/// increase in the on-console PVS format.
pub(crate) fn rebuild_portal_connected_visibility_pvs(
    rooms: &[PlaytestRoom],
    chunks: &[PlaytestChunk],
    room_portals: &[PlaytestRoomPortal],
    room_visibility: &mut [PlaytestRoomVisibility],
    visibility_cells: &[PlaytestVisibilityCell],
    visibility_pvs: &mut Vec<PlaytestVisibilityPvs>,
    visibility_pvs_bits: &mut Vec<u8>,
) {
    if visibility_cells.is_empty() || room_visibility.is_empty() {
        return;
    }

    let mut graph = vec![Vec::<usize>::new(); visibility_cells.len()];
    let mut by_room_coord = std::collections::HashMap::new();
    let mut by_room_world = std::collections::HashMap::new();
    let mut world_coords = vec![[0i32; 2]; visibility_cells.len()];
    for (global_index, cell) in visibility_cells.iter().enumerate() {
        by_room_coord.insert((cell.room, cell.x, cell.z), global_index);
        if let Some(chunk) = chunks.get(cell.room as usize) {
            world_coords[global_index] = [
                chunk.origin_x.saturating_add(cell.x as i32),
                chunk.origin_z.saturating_add(cell.z as i32),
            ];
            by_room_world.insert(
                (
                    cell.room,
                    world_coords[global_index][0],
                    world_coords[global_index][1],
                ),
                global_index,
            );
        }
    }

    // Preserve the existing open-edge graph inside each runtime room.
    for (global_index, cell) in visibility_cells.iter().enumerate() {
        for edge in VISIBILITY_EDGES {
            if cell.portal_mask & edge.bit == 0 {
                continue;
            }
            let nx = cell.x as i32 + edge.dx;
            let nz = cell.z as i32 + edge.dz;
            if nx < 0 || nz < 0 {
                continue;
            }
            if let Some(&neighbour) = by_room_coord.get(&(cell.room, nx as u16, nz as u16)) {
                push_unique_visibility_edge(&mut graph, global_index, neighbour);
            }
        }
    }

    // Join cells on opposite sides of the final cooked portals.  Testing the
    // portal rectangle midpoint prevents two rooms that share several sealed
    // boundaries from becoming over-connected.
    for portal in room_portals {
        let source = visibility_room_cell_range(room_visibility, portal.source_room);
        let destination = visibility_room_cell_range(room_visibility, portal.destination_room);
        let (Some(source), Some(_destination)) = (source, destination) else {
            continue;
        };
        for source_index in source {
            let source_world = world_coords[source_index];
            let candidate_coords: &[[i32; 2]] = if portal.kind == 1 {
                std::slice::from_ref(&source_world)
            } else {
                &[
                    [source_world[0], source_world[1] - 1],
                    [source_world[0] + 1, source_world[1]],
                    [source_world[0], source_world[1] + 1],
                    [source_world[0] - 1, source_world[1]],
                ]
            };
            for candidate in candidate_coords {
                let Some(&destination_index) =
                    by_room_world.get(&(portal.destination_room, candidate[0], candidate[1]))
                else {
                    continue;
                };
                if portal_connects_visibility_cells(
                    portal,
                    rooms,
                    &world_coords,
                    source_index,
                    destination_index,
                ) {
                    push_unique_visibility_edge(&mut graph, source_index, destination_index);
                }
            }
        }
    }

    visibility_pvs.clear();
    visibility_pvs_bits.clear();
    for visibility in room_visibility.iter_mut() {
        let Some(local_range) =
            visibility_room_cell_range(std::slice::from_ref(visibility), visibility.room)
        else {
            visibility.pvs_first = visibility_pvs.len() as u32;
            visibility.pvs_count = 0;
            continue;
        };
        let local_count = local_range.len();
        let bitset_bytes = visibility_pvs_bitset_bytes(local_count);
        let radius = rooms
            .get(visibility.room as usize)
            .map(|room| room.visibility_radius)
            .unwrap_or_default();
        visibility.pvs_first = u32::try_from(visibility_pvs.len()).unwrap_or(u32::MAX);

        for anchor in local_range.clone() {
            let mut visited = vec![false; visibility_cells.len()];
            let mut queue = vec![(anchor, 0u16)];
            visited[anchor] = true;
            let mut cursor = 0usize;
            while let Some(&(cell_index, distance)) = queue.get(cursor) {
                cursor += 1;
                if distance >= radius {
                    continue;
                }
                for &neighbour in &graph[cell_index] {
                    if !visited[neighbour] {
                        visited[neighbour] = true;
                        queue.push((neighbour, distance.saturating_add(1)));
                    }
                }
            }

            // Match the original conservative one-cell shell.  It hides tiny
            // cracks at a PVS boundary without opening another traversal step.
            let selected = visited.clone();
            for (cell_index, is_selected) in selected.iter().copied().enumerate() {
                if !is_selected {
                    continue;
                }
                let cell = visibility_cells[cell_index];
                for edge in VISIBILITY_EDGES {
                    let nx = cell.x as i32 + edge.dx;
                    let nz = cell.z as i32 + edge.dz;
                    if nx < 0 || nz < 0 {
                        continue;
                    }
                    if let Some(&neighbour) = by_room_coord.get(&(cell.room, nx as u16, nz as u16))
                    {
                        visited[neighbour] = true;
                    }
                }
            }

            let mut bits = vec![0u8; bitset_bytes];
            for (local_index, global_index) in local_range.clone().enumerate() {
                if visited[global_index] {
                    set_visibility_pvs_bit(&mut bits, local_index);
                }
            }
            let byte_first =
                find_existing_visibility_pvs_bits(visibility_pvs, visibility_pvs_bits, &bits)
                    .unwrap_or_else(|| {
                        let first = u32::try_from(visibility_pvs_bits.len()).unwrap_or(u32::MAX);
                        visibility_pvs_bits.extend_from_slice(&bits);
                        first
                    });
            visibility_pvs.push(PlaytestVisibilityPvs {
                byte_first,
                byte_count: u16::try_from(bitset_bytes).unwrap_or(u16::MAX),
            });
        }
        visibility.pvs_count = u16::try_from(local_count).unwrap_or(u16::MAX);
    }
}

fn visibility_room_cell_range(
    room_visibility: &[PlaytestRoomVisibility],
    room: u16,
) -> Option<std::ops::Range<usize>> {
    let visibility = room_visibility.iter().find(|entry| entry.room == room)?;
    let first = visibility.cell_first as usize;
    Some(first..first.saturating_add(visibility.cell_count as usize))
}

fn push_unique_visibility_edge(graph: &mut [Vec<usize>], a: usize, b: usize) {
    if a == b || a >= graph.len() || b >= graph.len() {
        return;
    }
    if !graph[a].contains(&b) {
        graph[a].push(b);
    }
}

fn portal_connects_visibility_cells(
    portal: &PlaytestRoomPortal,
    rooms: &[PlaytestRoom],
    world_coords: &[[i32; 2]],
    source_index: usize,
    destination_index: usize,
) -> bool {
    let source = world_coords[source_index];
    let destination = world_coords[destination_index];
    let dx = (source[0] - destination[0]).abs();
    let dz = (source[1] - destination[1]).abs();
    if (portal.kind == 1 && (dx != 0 || dz != 0))
        || (portal.kind != 1 && dx.saturating_add(dz) != 1)
    {
        return false;
    }
    let sector_size = rooms
        .get(portal.source_room as usize)
        .map(|room| room.sector_size.max(1))
        .unwrap_or(1) as i64;
    let midpoint_x2 = (source[0] as i64)
        .saturating_add(destination[0] as i64)
        .saturating_add(1)
        .saturating_mul(sector_size);
    let midpoint_z2 = (source[1] as i64)
        .saturating_add(destination[1] as i64)
        .saturating_add(1)
        .saturating_mul(sector_size);
    let min_x2 = portal
        .vertices
        .iter()
        .map(|vertex| i64::from(vertex[0]).saturating_mul(2))
        .min()
        .unwrap_or_default();
    let max_x2 = portal
        .vertices
        .iter()
        .map(|vertex| i64::from(vertex[0]).saturating_mul(2))
        .max()
        .unwrap_or_default();
    let min_z2 = portal
        .vertices
        .iter()
        .map(|vertex| i64::from(vertex[2]).saturating_mul(2))
        .min()
        .unwrap_or_default();
    let max_z2 = portal
        .vertices
        .iter()
        .map(|vertex| i64::from(vertex[2]).saturating_mul(2))
        .max()
        .unwrap_or_default();
    midpoint_x2 >= min_x2 && midpoint_x2 <= max_x2 && midpoint_z2 >= min_z2 && midpoint_z2 <= max_z2
}

pub(crate) fn append_room_surface_cache(
    room_index: u16,
    room_bytes: &[u8],
    materials: &[PlaytestMaterial],
    assets: &[PlaytestAsset],
    room_surface_caches: &mut Vec<PlaytestRoomSurfaceCache>,
    room_cache_cells: &mut Vec<PlaytestCachedRoomCell>,
    room_cache_cell_vertices: &mut Vec<u16>,
    room_cache_vertices: &mut Vec<PlaytestCachedRoomVertex>,
    room_cache_surfaces: &mut Vec<PlaytestCachedRoomSurface>,
) -> Result<(), String> {
    let room = RuntimeRoom::from_bytes(room_bytes)
        .map_err(|e| format!("Room #{room_index} generated cache parse failed: {e:?}"))?;
    let cache_materials = cache_materials_for_room(room_index, materials, assets)?;
    let surface_capacity = (room.width() as usize)
        .saturating_mul(room.depth() as usize)
        .saturating_mul(4)
        .saturating_add(room.world().wall_count() as usize)
        .max(1);
    let cell_capacity = (room.width() as usize)
        .saturating_mul(room.depth() as usize)
        .max(1);
    let vertex_capacity = surface_capacity.saturating_mul(4).max(1);
    let mut cells = vec![CachedRoomCell::EMPTY; cell_capacity];
    let mut vertices = vec![WorldVertex::ZERO; vertex_capacity];
    let mut surfaces = vec![CachedRoomSurface::EMPTY; surface_capacity];
    let stats = cache_room_vertex_lit_surfaces(
        room.render(),
        &cache_materials,
        &mut cells,
        &mut vertices,
        &mut surfaces,
    );
    if stats.overflow {
        return Err(format!(
            "Room #{room_index} generated surface cache overflowed its computed capacity"
        ));
    }
    let cell_first = checked_u32(room_cache_cells.len(), "room cache cell start")?;
    let vertex_first = checked_u32(room_cache_vertices.len(), "room cache vertex start")?;
    let surface_first = checked_u32(room_cache_surfaces.len(), "room cache surface start")?;
    let cell_vertex_first = checked_u32(
        room_cache_cell_vertices.len(),
        "room cache cell vertex start",
    )?;
    let cell_count = checked_u16(stats.cell_count, "room cache cell count")?;
    let vertex_count = checked_u16(stats.vertex_count, "room cache vertex count")?;
    let surface_count = checked_u16(stats.surface_count, "room cache surface count")?;
    let mut local_cell_vertices = Vec::new();
    let mut playtest_cells = Vec::with_capacity(stats.cell_count);
    for cell in &cells[..stats.cell_count] {
        let local_vertex_first = checked_u16(
            local_cell_vertices.len(),
            "room cache local cell vertex start",
        )?;
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(stats.surface_count);
        let mut unique = Vec::new();
        for surface in &surfaces[first..end] {
            for vertex_index in surface.vertex_indices {
                if (vertex_index as usize) < stats.vertex_count && !unique.contains(&vertex_index) {
                    unique.push(vertex_index);
                }
            }
        }
        let local_vertex_count = checked_u16(unique.len(), "room cache local cell vertex count")?;
        local_cell_vertices.extend(unique);
        playtest_cells.push(playtest_cached_room_cell(
            *cell,
            local_vertex_first,
            local_vertex_count,
        ));
    }
    let cell_vertex_count = checked_u16(local_cell_vertices.len(), "room cache cell vertex count")?;

    room_cache_cells.extend(playtest_cells);
    room_cache_cell_vertices.extend(local_cell_vertices);
    room_cache_vertices.extend(
        vertices[..stats.vertex_count]
            .iter()
            .copied()
            .map(playtest_cached_room_vertex),
    );
    room_cache_surfaces.extend(
        surfaces[..stats.surface_count]
            .iter()
            .copied()
            .map(playtest_cached_room_surface),
    );
    room_surface_caches.push(PlaytestRoomSurfaceCache {
        room: room_index,
        cell_first,
        cell_count,
        cell_vertex_first,
        cell_vertex_count,
        vertex_first,
        vertex_count,
        surface_first,
        surface_count,
    });
    Ok(())
}

pub(crate) fn assign_visibility_cache_cell_indices(
    room_index: u16,
    room_visibility: &[PlaytestRoomVisibility],
    visibility_cells: &mut [PlaytestVisibilityCell],
    room_surface_caches: &[PlaytestRoomSurfaceCache],
    room_cache_cells: &[PlaytestCachedRoomCell],
) {
    let Some(visibility) = room_visibility
        .iter()
        .find(|visibility| visibility.room == room_index)
    else {
        return;
    };
    let Some(cache) = room_surface_caches
        .iter()
        .find(|cache| cache.room == room_index)
    else {
        return;
    };
    let visible_first = visibility.cell_first as usize;
    let visible_end = visible_first.saturating_add(visibility.cell_count as usize);
    let cache_first = cache.cell_first as usize;
    let cache_end = cache_first.saturating_add(cache.cell_count as usize);
    let Some(visible_cells) = visibility_cells.get_mut(visible_first..visible_end) else {
        return;
    };
    let Some(cache_cells) = room_cache_cells.get(cache_first..cache_end) else {
        return;
    };
    for cell in visible_cells {
        cell.cache_cell_index =
            cached_room_cell_index_for_coord(cache_cells, cell.x, cell.z).unwrap_or(u16::MAX);
    }
}

pub(crate) fn cached_room_cell_index_for_coord(
    cells: &[PlaytestCachedRoomCell],
    x: u16,
    z: u16,
) -> Option<u16> {
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
    let cell = cells.get(low)?;
    (cached_room_cell_key(cell.x, cell.z) == key)
        .then(|| u16::try_from(low).ok())
        .flatten()
}

pub(crate) const fn cached_room_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

pub(crate) fn cache_materials_for_room(
    room_index: u16,
    materials: &[PlaytestMaterial],
    assets: &[PlaytestAsset],
) -> Result<Vec<WorldRenderMaterial>, String> {
    let mut out = Vec::new();
    for material in materials
        .iter()
        .filter(|material| material.room == room_index)
    {
        let slot = material.local_slot as usize;
        if out.len() <= slot {
            out.resize(slot + 1, WorldRenderMaterial::cache_only(64, 64));
        }
        let texture_asset = assets.get(material.texture_asset_index).ok_or_else(|| {
            format!(
                "Room #{room_index} material slot {} references missing texture asset {}",
                material.local_slot, material.texture_asset_index
            )
        })?;
        let texture = psx_asset::Texture::from_bytes(&texture_asset.bytes).map_err(|e| {
            format!(
                "Room #{room_index} material slot {} texture '{}' parse failed while building generated cache: {e:?}",
                material.local_slot, texture_asset.source_label
            )
        })?;
        let mut width = room_cache_texture_size(texture.width());
        let mut height = room_cache_texture_size(texture.height());
        if material.animation.mode == MaterialAnimationMode::Flipbook {
            let flipbook = material.animation.flipbook.normalized();
            if texture.width() % u16::from(flipbook.columns) != 0
                || texture.height() % u16::from(flipbook.rows) != 0
            {
                return Err(format!(
                    "Room #{room_index} material slot {} flipbook grid {}x{} does not divide texture {}x{}",
                    material.local_slot,
                    flipbook.columns,
                    flipbook.rows,
                    texture.width(),
                    texture.height()
                ));
            }
            let frame_width = texture.width() / u16::from(flipbook.columns);
            let frame_height = texture.height() / u16::from(flipbook.rows);
            if !valid_room_texture_size(frame_width) || !valid_room_texture_size(frame_height) {
                return Err(format!(
                    "Room #{room_index} material slot {} flipbook frames are {}x{}; each axis must be a power of two from 8 to 64 texels",
                    material.local_slot, frame_width, frame_height
                ));
            }
            width = frame_width as u8;
            height = frame_height as u8;
        }
        out[slot] = WorldRenderMaterial::cache_only(width, height);
    }
    Ok(out)
}

pub(crate) fn room_cache_texture_size(size: u16) -> u8 {
    if !valid_room_texture_size(size) {
        64
    } else {
        size as u8
    }
}

const fn valid_room_texture_size(size: u16) -> bool {
    size >= 8 && size <= 64 && size.is_power_of_two() && size.is_multiple_of(8)
}

pub(crate) fn checked_u32(value: usize, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} {value} exceeds u32::MAX"))
}

pub(crate) fn checked_u16(value: usize, what: &str) -> Result<u16, String> {
    u16::try_from(value).map_err(|_| format!("{what} {value} exceeds u16::MAX"))
}

pub(crate) fn playtest_cached_room_cell(
    cell: CachedRoomCell,
    vertex_first: u16,
    vertex_count: u16,
) -> PlaytestCachedRoomCell {
    PlaytestCachedRoomCell {
        x: cell.x,
        z: cell.z,
        min_y: cell.min_y,
        max_y: cell.max_y,
        visibility_center: cell.visibility_center,
        visibility_radius: cell.visibility_radius,
        surface_first: cell.surface_first,
        surface_count: cell.surface_count,
        vertex_first,
        vertex_count,
    }
}

pub(crate) fn playtest_cached_room_vertex(vertex: WorldVertex) -> PlaytestCachedRoomVertex {
    PlaytestCachedRoomVertex {
        x: vertex.x,
        y: vertex.y,
        z: vertex.z,
    }
}

pub(crate) fn playtest_cached_room_surface(
    surface: CachedRoomSurface,
) -> PlaytestCachedRoomSurface {
    PlaytestCachedRoomSurface {
        material_slot: surface.material_slot,
        vertex_indices: surface.vertex_indices,
        sample_sx: surface.sample_sx,
        sample_sz: surface.sample_sz,
        sample_ordinal: surface.sample_ordinal,
        uv_words: surface.uv_words,
        baked_vertex_rgb: surface.baked_vertex_rgb,
        kind_flags: surface.kind_flags,
        wall_direction: surface.wall_direction,
        split: surface.split,
        triangle_index: surface.triangle_index,
    }
}

pub(crate) fn build_visibility_cells(
    room_index: u16,
    cooked: &CookedWorldGrid,
) -> Vec<PlaytestVisibilityCell> {
    let mut out = Vec::new();
    for x in 0..cooked.width {
        for z in 0..cooked.depth {
            let Some(sector) = cooked_sector(cooked, x, z) else {
                continue;
            };
            let (min_y, max_y) = cooked_sector_y_bounds(sector, cooked.sector_size);
            out.push(PlaytestVisibilityCell {
                room: room_index,
                x,
                z,
                min_y,
                max_y,
                portal_mask: 0,
                blocker_mask: blocker_mask_for_sector(sector, cooked.sector_size),
                cache_cell_index: u16::MAX,
                flags: visibility_cell_flags::HAS_GEOMETRY,
            });
        }
    }
    out
}

pub(crate) fn visibility_index_by_coord(
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
) -> Vec<Option<usize>> {
    let mut out = vec![None; (width as usize).saturating_mul(depth as usize)];
    for (index, cell) in cells.iter().enumerate() {
        if let Some(flat) = visibility_flat_index(depth, cell.x, cell.z) {
            if let Some(slot) = out.get_mut(flat) {
                *slot = Some(index);
            }
        }
    }
    out
}

pub(crate) fn assign_visibility_portals(
    width: u16,
    depth: u16,
    index_by_coord: &[Option<usize>],
    cells: &mut [PlaytestVisibilityCell],
) {
    for index in 0..cells.len() {
        let x = cells[index].x;
        let z = cells[index].z;
        let mut mask = 0u8;
        for edge in VISIBILITY_EDGES {
            let Some((nx, nz)) = neighbour_cell(width, depth, x, z, edge.dx, edge.dz) else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            let this_blocked = cells[index].blocker_mask & edge.bit != 0;
            let neighbour_blocked = cells[neighbour_index].blocker_mask & edge.opposite_bit != 0;
            if !this_blocked && !neighbour_blocked {
                mask |= edge.bit;
            }
        }
        cells[index].portal_mask = mask;
    }
}

pub(crate) fn append_visibility_pvs(
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
    visibility_radius: u16,
    visibility_pvs: &mut Vec<PlaytestVisibilityPvs>,
    visibility_pvs_bits: &mut Vec<u8>,
) {
    let bitset_bytes = visibility_pvs_bitset_bytes(cells.len());
    let mut bits = vec![0u8; bitset_bytes];
    for anchor_index in 0..cells.len() {
        bits.fill(0);
        fill_visibility_pvs_bits(
            anchor_index,
            width,
            depth,
            cells,
            index_by_coord,
            visibility_radius,
            &mut bits,
        );
        let byte_first =
            find_existing_visibility_pvs_bits(visibility_pvs, visibility_pvs_bits, &bits)
                .unwrap_or_else(|| {
                    let byte_first = u32::try_from(visibility_pvs_bits.len()).unwrap_or(u32::MAX);
                    visibility_pvs_bits.extend_from_slice(&bits);
                    byte_first
                });
        visibility_pvs.push(PlaytestVisibilityPvs {
            byte_first,
            byte_count: u16::try_from(bitset_bytes).unwrap_or(u16::MAX),
        });
    }
}

pub(crate) fn find_existing_visibility_pvs_bits(
    visibility_pvs: &[PlaytestVisibilityPvs],
    visibility_pvs_bits: &[u8],
    bits: &[u8],
) -> Option<u32> {
    for pvs in visibility_pvs {
        if pvs.byte_count as usize != bits.len() {
            continue;
        }
        let start = pvs.byte_first as usize;
        let Some(end) = start.checked_add(bits.len()) else {
            continue;
        };
        if visibility_pvs_bits.get(start..end) == Some(bits) {
            return Some(pvs.byte_first);
        }
    }
    None
}

pub(crate) fn visibility_pvs_bitset_bytes(cell_count: usize) -> usize {
    cell_count.saturating_add(7) / 8
}

pub(crate) fn fill_visibility_pvs_bits(
    anchor_index: usize,
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
    visibility_radius: u16,
    bits: &mut [u8],
) -> Vec<usize> {
    let visible = visibility_indices_for_anchor(
        anchor_index,
        width,
        depth,
        cells,
        index_by_coord,
        visibility_radius,
    );
    for &index in &visible {
        set_visibility_pvs_bit(bits, index);
    }
    visible
}

pub(crate) fn visibility_indices_for_anchor(
    anchor_index: usize,
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
    visibility_radius: u16,
) -> Vec<usize> {
    if anchor_index >= cells.len() {
        return Vec::new();
    }
    let anchor = cells[anchor_index];
    let mut visible = Vec::new();
    let mut visited = vec![false; cells.len()];
    let mut selected = vec![false; cells.len()];
    let mut queue = Vec::new();
    visited[anchor_index] = true;
    queue.push((anchor_index, 0u16));

    let mut cursor = 0usize;
    while let Some(&(cell_index, distance)) = queue.get(cursor) {
        cursor += 1;
        visible.push(cell_index);
        if distance >= visibility_radius {
            continue;
        }

        let cell = cells[cell_index];
        for edge in VISIBILITY_EDGES {
            if cell.portal_mask & edge.bit == 0 {
                continue;
            }
            let Some((nx, nz)) = neighbour_cell(width, depth, cell.x, cell.z, edge.dx, edge.dz)
            else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            if visited[neighbour_index] {
                continue;
            }
            visited[neighbour_index] = true;
            queue.push((neighbour_index, distance + 1));
        }
    }

    for &(index, _) in &queue {
        selected[index] = true;
    }
    let mut i = queue.len();
    while i != 0 {
        i -= 1;
        let cell = cells[queue[i].0];
        for edge in VISIBILITY_EDGES {
            let Some((nx, nz)) = neighbour_cell(width, depth, cell.x, cell.z, edge.dx, edge.dz)
            else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            if !selected[neighbour_index] {
                selected[neighbour_index] = true;
                visible.push(neighbour_index);
            }
        }
    }

    visible.sort_by(|&a, &b| {
        let ca = cells[a];
        let cb = cells[b];
        let da = chebyshev_distance(anchor, ca);
        let db = chebyshev_distance(anchor, cb);
        db.cmp(&da).then(ca.x.cmp(&cb.x)).then(ca.z.cmp(&cb.z))
    });
    visible
}

pub(crate) fn set_visibility_pvs_bit(bits: &mut [u8], index: usize) {
    let byte = index / 8;
    let bit = index % 8;
    if let Some(slot) = bits.get_mut(byte) {
        *slot |= 1 << bit;
    }
}

#[derive(Clone, Copy)]
pub(crate) struct VisibilityEdge {
    bit: u8,
    opposite_bit: u8,
    dx: i32,
    dz: i32,
}

pub(crate) const VISIBILITY_EDGES: [VisibilityEdge; 4] = [
    VisibilityEdge {
        bit: visibility_edge_flags::NORTH,
        opposite_bit: visibility_edge_flags::SOUTH,
        dx: 0,
        dz: -1,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::EAST,
        opposite_bit: visibility_edge_flags::WEST,
        dx: 1,
        dz: 0,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::SOUTH,
        opposite_bit: visibility_edge_flags::NORTH,
        dx: 0,
        dz: 1,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::WEST,
        opposite_bit: visibility_edge_flags::EAST,
        dx: -1,
        dz: 0,
    },
];

pub(crate) fn chebyshev_distance(
    anchor: PlaytestVisibilityCell,
    cell: PlaytestVisibilityCell,
) -> i32 {
    (cell.x as i32 - anchor.x as i32)
        .abs()
        .max((cell.z as i32 - anchor.z as i32).abs())
}

pub(crate) fn visibility_cell_index(
    index_by_coord: &[Option<usize>],
    depth: u16,
    x: u16,
    z: u16,
) -> Option<usize> {
    let flat = visibility_flat_index(depth, x, z)?;
    index_by_coord.get(flat).copied().flatten()
}

pub(crate) fn visibility_flat_index(depth: u16, x: u16, z: u16) -> Option<usize> {
    (x as usize)
        .checked_mul(depth as usize)?
        .checked_add(z as usize)
}

pub(crate) fn neighbour_cell(
    width: u16,
    depth: u16,
    x: u16,
    z: u16,
    dx: i32,
    dz: i32,
) -> Option<(u16, u16)> {
    let nx = x as i32 + dx;
    let nz = z as i32 + dz;
    if nx < 0 || nz < 0 || nx > u16::MAX as i32 || nz > u16::MAX as i32 {
        return None;
    }
    let nx = nx as u16;
    let nz = nz as u16;
    if nx >= width || nz >= depth {
        return None;
    }
    Some((nx, nz))
}
