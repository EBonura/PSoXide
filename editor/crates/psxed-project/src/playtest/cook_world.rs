use super::*;

pub(crate) fn cooked_sector(
    cooked: &CookedWorldGrid,
    x: u16,
    z: u16,
) -> Option<&crate::world_cook::CookedGridSector> {
    let index = (x as usize)
        .checked_mul(cooked.depth as usize)?
        .checked_add(z as usize)?;
    cooked.sectors.get(index)?.as_ref()
}

pub(crate) fn cooked_sector_y_bounds(
    sector: &crate::world_cook::CookedGridSector,
    sector_size: i32,
) -> (i32, i32) {
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    let mut any = false;
    if let Some(face) = sector.floor {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, face.heights);
    }
    if let Some(face) = sector.ceiling {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, face.heights);
    }
    for wall in sector
        .walls
        .north
        .iter()
        .chain(sector.walls.east.iter())
        .chain(sector.walls.south.iter())
        .chain(sector.walls.west.iter())
    {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, wall.heights);
    }
    if any {
        (min_y, max_y)
    } else {
        (0, sector_size)
    }
}

pub(crate) fn include_cooked_heights(
    min_y: &mut i32,
    max_y: &mut i32,
    any: &mut bool,
    heights: [i32; 4],
) {
    for height in heights {
        *min_y = (*min_y).min(height);
        *max_y = (*max_y).max(height);
        *any = true;
    }
}

pub(crate) fn blocker_mask_for_sector(
    sector: &crate::world_cook::CookedGridSector,
    sector_size: i32,
) -> u8 {
    let mut mask = 0u8;
    if has_full_height_solid_wall(&sector.walls.north, sector_size) {
        mask |= visibility_edge_flags::NORTH;
    }
    if has_full_height_solid_wall(&sector.walls.east, sector_size) {
        mask |= visibility_edge_flags::EAST;
    }
    if has_full_height_solid_wall(&sector.walls.south, sector_size) {
        mask |= visibility_edge_flags::SOUTH;
    }
    if has_full_height_solid_wall(&sector.walls.west, sector_size) {
        mask |= visibility_edge_flags::WEST;
    }
    mask
}

pub(crate) fn has_full_height_solid_wall(
    walls: &[crate::world_cook::CookedGridVerticalFace],
    sector_size: i32,
) -> bool {
    walls.iter().any(|wall| {
        if !wall.solid {
            return false;
        }
        let bottom = wall.heights[0].min(wall.heights[1]);
        let top = wall.heights[2].max(wall.heights[3]);
        top.saturating_sub(bottom)
            >= sector_size
                .saturating_sub(FULL_HEIGHT_BLOCKER_TOLERANCE)
                .max(sector_size / 2)
    })
}

pub(crate) fn collect_pending_floor_links(
    room: u16,
    grid: &WorldGrid,
    out: &mut Vec<PendingRoomFloorLink>,
) {
    let mut x = 0u16;
    while x < grid.width {
        let mut z = 0u16;
        while z < grid.depth {
            if let Some(sector) = grid.sector(x, z) {
                let above_room = sector.floor_above.and_then(|link| link.target_room);
                let below_room = sector.floor_below.and_then(|link| link.target_room);
                if above_room.is_some() || below_room.is_some() {
                    out.push(PendingRoomFloorLink {
                        room,
                        x,
                        z,
                        world_cell: [
                            grid.origin[0].saturating_add(x as i32),
                            grid.origin[1].saturating_add(z as i32),
                        ],
                        above_room,
                        below_room,
                    });
                }
            }
            z = z.saturating_add(1);
        }
        x = x.saturating_add(1);
    }
}

pub(crate) fn resolve_room_floor_links(
    pending: &[PendingRoomFloorLink],
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Vec<PlaytestRoomFloorLink> {
    let mut out = Vec::new();
    for link in pending {
        let above_room =
            resolve_floor_link_target(link.above_room, link.world_cell, chunks_by_node);
        let below_room =
            resolve_floor_link_target(link.below_room, link.world_cell, chunks_by_node);
        if above_room.is_none() && below_room.is_none() {
            continue;
        }
        out.push(PlaytestRoomFloorLink {
            room: link.room,
            x: link.x,
            z: link.z,
            above_room,
            below_room,
        });
    }
    out
}

/// Auto-wire vertical links between consecutive floors of each room.
/// Every floor of one room lives under a single node id at the same
/// world cells, so the `(node id, cell)` resolver used for authored
/// links can't tell two floors apart. Here we link directly by chunk
/// `room_index`, using each chunk's `floor_idx`: floor N's sector links
/// down to floor N-1's chunk and up to floor N+1's chunk at the same
/// world cell. Single-floor rooms produce nothing.
pub(crate) fn auto_wire_floor_stack_links(
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Vec<PlaytestRoomFloorLink> {
    let mut out = Vec::new();
    for chunks in chunks_by_node.values() {
        let max_floor = chunks
            .iter()
            .map(|chunk| chunk.floor_idx)
            .max()
            .unwrap_or(0);
        if max_floor == 0 {
            continue;
        }
        for chunk in chunks {
            for &cell in &chunk.cells {
                let world_cell = chunk_cell_world_cell(chunk, cell);
                let find_on = |floor: usize| -> Option<u16> {
                    chunks
                        .iter()
                        .find(|other| {
                            other.floor_idx == floor && chunk_contains_world_cell(other, world_cell)
                        })
                        .map(|other| other.room_index)
                };
                let below_room = chunk.floor_idx.checked_sub(1).and_then(find_on);
                let above_room = find_on(chunk.floor_idx + 1);
                if above_room.is_none() && below_room.is_none() {
                    continue;
                }
                out.push(PlaytestRoomFloorLink {
                    room: chunk.room_index,
                    x: cell[0].saturating_sub(chunk.array_origin[0]),
                    z: cell[1].saturating_sub(chunk.array_origin[1]),
                    above_room,
                    below_room,
                });
            }
        }
    }
    out
}

/// Emit vertical portal quads between consecutive floors, mirroring the
/// floor links. For each cell shared by floor N (below) and floor N+1
/// (above) the runtime gets a reciprocal pair: an up-portal owned by the
/// lower room (normal +Y) and a down-portal owned by the upper room
/// (normal -Y), both lying in the horizontal plane at the boundary
/// elevation (floor N+1's elevation). Without these, `room_floor_links`
/// connect the rooms for streaming/walking but the portal clipper and
/// portal-view overlay have no quad to draw or cull against. Coords match
/// the wall-portal convention: world units = `(grid.origin + cell) *
/// sector_size`, vertices `[BL, BR, TR, TL]`.
pub(crate) fn auto_wire_floor_stack_portals(
    scene: &crate::Scene,
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Vec<PlaytestRoomPortal> {
    let mut out = Vec::new();
    for (node_id, chunks) in chunks_by_node {
        let max_floor = chunks
            .iter()
            .map(|chunk| chunk.floor_idx)
            .max()
            .unwrap_or(0);
        if max_floor == 0 {
            continue;
        }
        let Some(NodeKind::Room { grid }) = scene.node(*node_id).map(|n| &n.kind) else {
            continue;
        };
        let s = grid.sector_size;
        if s <= 0 {
            continue;
        }
        for chunk in chunks {
            // Only emit from the lower side of each boundary, so each
            // shared cell yields exactly one reciprocal pair.
            let upper_floor = chunk.floor_idx + 1;
            let (Some(lower_grid), Some(upper_grid)) =
                (grid.floor(chunk.floor_idx), grid.floor(upper_floor))
            else {
                continue;
            };
            let boundary_y = upper_grid.elevation;
            for &cell in &chunk.cells {
                let world_cell = chunk_cell_world_cell(chunk, cell);
                let Some(above) = chunks.iter().find(|other| {
                    other.floor_idx == upper_floor && chunk_contains_world_cell(other, world_cell)
                }) else {
                    continue;
                };
                // A vertical portal only exists where there is an actual
                // GAP between the floors: the upper floor has no floor
                // face AND the lower floor has no ceiling face at this
                // cell. A sealed cell (floor above / ceiling below) is a
                // solid slab you can neither see nor walk through, so it
                // gets no portal. Cells are addressed in each floor grid's
                // own array space via its origin.
                let upper_sealed = upper_grid
                    .world_cell_to_array(world_cell[0], world_cell[1])
                    .and_then(|(sx, sz)| upper_grid.sector(sx, sz))
                    .is_some_and(|sector| sector.floor.is_some());
                let lower_sealed = lower_grid
                    .world_cell_to_array(world_cell[0], world_cell[1])
                    .and_then(|(sx, sz)| lower_grid.sector(sx, sz))
                    .is_some_and(|sector| sector.ceiling.is_some());
                if upper_sealed || lower_sealed {
                    continue;
                }
                let below_room = u16::try_from(chunk.room_index as usize).unwrap_or(u16::MAX);
                let above_room = above.room_index;
                // Cell footprint in world units at the boundary plane.
                let x0 = world_cell[0].saturating_mul(s);
                let x1 = world_cell[0].saturating_add(1).saturating_mul(s);
                let z0 = world_cell[1].saturating_mul(s);
                let z1 = world_cell[1].saturating_add(1).saturating_mul(s);
                let quad = [
                    [x0, boundary_y, z0],
                    [x1, boundary_y, z0],
                    [x1, boundary_y, z1],
                    [x0, boundary_y, z1],
                ];
                // Up-portal: owned by the lower room, looks up into the
                // upper room (source-facing normal points back down).
                out.push(PlaytestRoomPortal {
                    source_room: below_room,
                    destination_room: above_room,
                    kind: 1,
                    normal: [0, -1, 0],
                    vertices: quad,
                });
                // Down-portal: owned by the upper room, looks down.
                out.push(PlaytestRoomPortal {
                    source_room: above_room,
                    destination_room: below_room,
                    kind: 1,
                    normal: [0, 1, 0],
                    vertices: quad,
                });
            }
        }
    }
    out
}

/// Sort the portal table by `source_room` and rebuild every room's
/// `[portal_first, portal_count)` range so the runtime BFS scans all of a
/// room's portals (horizontal + vertical) as one contiguous slice.
/// Stable sort preserves each room's internal portal order. Rooms with no
/// portals get `portal_first=0, portal_count=0`.
pub(crate) fn regroup_room_portals(
    rooms: &mut [PlaytestRoom],
    room_portals: &mut [PlaytestRoomPortal],
) {
    room_portals.sort_by_key(|portal| portal.source_room);
    for (index, room) in rooms.iter_mut().enumerate() {
        let room_index = u16::try_from(index).unwrap_or(u16::MAX);
        let first = room_portals
            .iter()
            .position(|portal| portal.source_room == room_index);
        match first {
            Some(first) => {
                let count = room_portals[first..]
                    .iter()
                    .take_while(|portal| portal.source_room == room_index)
                    .count();
                room.portal_first = u16::try_from(first).unwrap_or(u16::MAX);
                room.portal_count = u8::try_from(count).unwrap_or(u8::MAX);
            }
            None => {
                room.portal_first = 0;
                room.portal_count = 0;
            }
        }
    }
}

pub(crate) fn resolve_floor_link_target(
    target_room: Option<NodeId>,
    world_cell: [i32; 2],
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Option<u16> {
    let chunks = chunks_by_node.get(&target_room?)?;
    chunks
        .iter()
        .find(|chunk| chunk_contains_world_cell(chunk, world_cell))
        .or_else(|| chunks.first())
        .map(|chunk| chunk.room_index)
}

pub(crate) fn chunk_contains_world_cell(chunk: &AuthoredRoomChunk, world_cell: [i32; 2]) -> bool {
    chunk
        .cells
        .iter()
        .any(|cell| chunk_cell_world_cell(chunk, *cell) == world_cell)
}

pub(crate) fn chunk_cell_world_cell(chunk: &AuthoredRoomChunk, cell: [u16; 2]) -> [i32; 2] {
    [
        chunk.world_origin[0]
            .saturating_add((cell[0] as i32).saturating_sub(chunk.array_origin[0] as i32)),
        chunk.world_origin[1]
            .saturating_add((cell[1] as i32).saturating_sub(chunk.array_origin[1] as i32)),
    ]
}

pub(crate) fn runtime_room_name(room_name: &str, room_count: usize, room_index: usize) -> String {
    if room_count <= 1 {
        room_name.to_string()
    } else {
        format!("{room_name} / Portal Room {room_index}")
    }
}

pub(crate) fn enclosing_room<'a>(
    scene: &'a crate::Scene,
    node: &'a SceneNode,
) -> Option<&'a SceneNode> {
    let mut current = node.parent;
    while let Some(parent_id) = current {
        let parent = scene.node(parent_id)?;
        if matches!(parent.kind, NodeKind::Room { .. }) {
            return Some(parent);
        }
        current = parent.parent;
    }
    None
}

pub(crate) fn chunk_for_node<'a>(
    node: &SceneNode,
    grid: &WorldGrid,
    chunks: &'a [AuthoredRoomChunk],
) -> Option<&'a AuthoredRoomChunk> {
    let world_cells =
        grid.editor_to_world_cells([node.transform.translation[0], node.transform.translation[2]]);
    let wcx = world_cells[0].floor() as i32;
    let wcz = world_cells[1].floor() as i32;
    let (sx, sz) = grid.world_cell_to_array(wcx, wcz)?;
    // A node's XZ cell can exist on several stacked floors. Bind to the
    // floor the node was AUTHORED on (`node.floor`, explicit), not by
    // inferring from Y -- the authored Y is a placement default (e.g. the
    // 2.89-sector standing height shared by every project) and can land
    // above the wrong floor. Floor 0 is the ground; clamp so a stale
    // index can't miss. Single-floor rooms have one candidate and the
    // clamp collapses to it.
    let target_floor = node.floor.min(grid.floor_count().saturating_sub(1));
    let mut fallback = None;
    for chunk in chunks {
        let on_cell = chunk
            .cells
            .iter()
            .any(|cell| cell[0] == sx && cell[1] == sz);
        if !on_cell {
            continue;
        }
        if chunk.floor_idx == target_floor {
            return Some(chunk);
        }
        fallback.get_or_insert(chunk);
    }
    fallback
}

pub(crate) fn build_playtest_chunks(
    room_chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
    room_count: usize,
) -> Vec<PlaytestChunk> {
    let mut chunks = vec![
        PlaytestChunk {
            room: 0,
            authored_room: 0,
            chunk_index: 0,
            origin_x: 0,
            origin_z: 0,
            width: 0,
            depth: 0,
            neighbours: [None; 4],
            triangles: 0,
            psxw_bytes: 0,
            static_lit_bytes: 0,
            populated_cells: 0,
            flags: 0,
        };
        room_count
    ];

    for node_chunks in room_chunks_by_node.values() {
        for chunk in node_chunks {
            let Some(out) = chunks.get_mut(chunk.room_index as usize) else {
                continue;
            };
            *out = PlaytestChunk {
                room: chunk.room_index,
                authored_room: chunk.authored_room,
                chunk_index: chunk.chunk_index,
                origin_x: chunk.world_origin[0],
                origin_z: chunk.world_origin[1],
                width: chunk.size[0],
                depth: chunk.size[1],
                neighbours: chunk.neighbours,
                triangles: chunk.triangles,
                psxw_bytes: chunk.psxw_bytes,
                static_lit_bytes: chunk.static_lit_bytes,
                populated_cells: chunk.populated_cells,
                flags: 0,
            };
        }
    }

    chunks
}

/// Convert a node's editor-space transform to its generated
/// runtime chunk-local coordinates. The authored Room may be
/// arbitrary-size; the cooked `.psxw` for one chunk is still
/// array-rooted at that chunk's origin.
pub(crate) fn node_chunk_local_position(
    node: &SceneNode,
    grid: &WorldGrid,
    chunk: &AuthoredRoomChunk,
) -> [i32; 3] {
    let world_cells =
        grid.editor_to_world_cells([node.transform.translation[0], node.transform.translation[2]]);
    let s = grid.sector_size as f32;
    [
        ((world_cells[0] - chunk.world_origin[0] as f32) * s) as i32,
        (node.transform.translation[1] * s) as i32,
        ((world_cells[1] - chunk.world_origin[1] as f32) * s) as i32,
    ]
}

/// Like [`node_chunk_local_position`], but treats the node as a
/// floor anchor: X/Z come from the authored transform and Y is
/// sampled from the floor directly underneath when possible.
pub(crate) fn floor_anchored_node_chunk_local_position(
    node: &SceneNode,
    grid: &WorldGrid,
    chunk: &AuthoredRoomChunk,
) -> [i32; 3] {
    let mut pos = node_chunk_local_position(node, grid, chunk);
    let world =
        grid.editor_to_room_local([node.transform.translation[0], node.transform.translation[2]]);
    if let Some(floor_y) = grid.floor_height_at_room_local(world[0] as i32, world[2] as i32) {
        pos[1] = floor_y;
    }
    pos
}

/// Convert an editor euler-degrees-Y rotation to a PSX angle
/// unit (`0..4096`).
pub(crate) fn yaw_from_degrees(degrees: f32) -> i16 {
    angle_from_degrees(degrees)
}

/// Convert editor Euler degrees to PSX angle units (`0..4096`), stored as the
/// signed value the compact records use. The math is shared with the editor
/// preview via [`crate::spatial::euler_degrees_to_q12`] so authored facing
/// can't drift between preview and cooked output.
pub(crate) fn angle_from_degrees(degrees: f32) -> i16 {
    crate::spatial::euler_degrees_to_q12(degrees) as i16
}

pub(crate) fn cook_error_for_node(name: &str, err: WorldGridCookError) -> String {
    format!("Room '{name}' failed cook: {err}")
}
