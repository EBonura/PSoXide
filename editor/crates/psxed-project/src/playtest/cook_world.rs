use super::*;
use crate::{GridDirection, GridVerticalFace};

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

/// Build the flattened stacked-room overlap table and assign each room's
/// contiguous slice. Unlike vertical portals, overlaps are emitted for sealed
/// stacked cells too: a translucent upper floor still needs the lower room
/// resident and drawable so the PS1 blend operation has geometry behind it.
pub(crate) fn assign_floor_stack_overlaps(
    rooms: &mut [PlaytestRoom],
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Vec<u16> {
    let mut overlaps = vec![Vec::<u16>::new(); rooms.len()];
    for chunks in chunks_by_node.values() {
        for left_index in 0..chunks.len() {
            let left = &chunks[left_index];
            for right in &chunks[left_index + 1..] {
                if left.floor_idx == right.floor_idx || !chunks_share_world_cell(left, right) {
                    continue;
                }
                if let Some(list) = overlaps.get_mut(left.room_index as usize) {
                    list.push(right.room_index);
                }
                if let Some(list) = overlaps.get_mut(right.room_index as usize) {
                    list.push(left.room_index);
                }
            }
        }
    }

    let mut flattened = Vec::new();
    for (room_index, room_overlaps) in overlaps.iter_mut().enumerate() {
        room_overlaps.sort_unstable();
        room_overlaps.dedup();
        let first = flattened.len();
        let available = usize::from(u8::MAX).min(room_overlaps.len());
        flattened.extend_from_slice(&room_overlaps[..available]);
        if let Some(room) = rooms.get_mut(room_index) {
            room.overlapped_room_first = u16::try_from(first).unwrap_or(u16::MAX);
            room.overlapped_room_count = u8::try_from(available).unwrap_or(u8::MAX);
        }
    }
    flattened
}

fn chunks_share_world_cell(left: &AuthoredRoomChunk, right: &AuthoredRoomChunk) -> bool {
    let (smaller, larger) = if left.cells.len() <= right.cells.len() {
        (left, right)
    } else {
        (right, left)
    };
    smaller.cells.iter().copied().any(|cell| {
        let world_cell = chunk_cell_world_cell(smaller, cell);
        chunk_contains_world_cell(larger, world_cell)
    })
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
        let Some(NodeKind::Section { grid }) = scene.node(*node_id).map(|n| &n.kind) else {
            continue;
        };
        let s = grid.sector_size;
        if s <= 0 {
            continue;
        }
        let room_origin_y_base = scene
            .node(*node_id)
            .map(|node| (f64::from(node.transform.translation[1]) * f64::from(s)) as i32)
            .unwrap_or(0);
        let floor_elevation_offset = room_origin_y_base.saturating_sub(grid.elevation);
        let mut openings: BTreeMap<(u16, u16, i32), BTreeSet<(i32, i32)>> = BTreeMap::new();
        for chunk in chunks {
            // Only emit from the lower side of each boundary, so each
            // shared cell yields exactly one reciprocal pair.
            let upper_floor = chunk.floor_idx + 1;
            let (Some(lower_grid), Some(upper_grid)) =
                (grid.floor(chunk.floor_idx), grid.floor(upper_floor))
            else {
                continue;
            };
            let boundary_y = floor_elevation_offset.saturating_add(upper_grid.elevation);
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
                openings
                    .entry((below_room, above_room, boundary_y))
                    .or_default()
                    .insert((world_cell[0], world_cell[1]));
            }
        }
        for ((below_room, above_room, boundary_y), cells) in openings {
            for [x0_cell, z0_cell, x1_cell, z1_cell] in exact_cell_rectangles(cells) {
                let x0 = x0_cell.saturating_mul(s);
                let x1 = x1_cell.saturating_mul(s);
                let z0 = z0_cell.saturating_mul(s);
                let z1 = z1_cell.saturating_mul(s);
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

/// Cover grid cells with deterministic, non-overlapping rectangles. A
/// rectangle never bridges a missing cell, so the combined portal quads cover
/// exactly the same opening as the original per-cell quads.
fn exact_cell_rectangles(mut cells: BTreeSet<(i32, i32)>) -> Vec<[i32; 4]> {
    let mut rectangles = Vec::new();
    while let Some(&(x0, z0)) = cells.first() {
        let mut x1 = x0 + 1;
        while cells.contains(&(x1, z0)) {
            x1 += 1;
        }
        let mut z1 = z0 + 1;
        while (x0..x1).all(|x| cells.contains(&(x, z1))) {
            z1 += 1;
        }
        for z in z0..z1 {
            for x in x0..x1 {
                cells.remove(&(x, z));
            }
        }
        rectangles.push([x0, z0, x1, z1]);
    }
    rectangles
}

#[cfg(test)]
mod stack_portal_compaction_tests {
    use super::exact_cell_rectangles;
    use std::collections::BTreeSet;

    fn covered_cells(rectangles: &[[i32; 4]]) -> BTreeSet<(i32, i32)> {
        let mut covered = BTreeSet::new();
        for &[x0, z0, x1, z1] in rectangles {
            assert!(x1 > x0 && z1 > z0);
            for z in z0..z1 {
                for x in x0..x1 {
                    assert!(covered.insert((x, z)), "rectangles overlap at ({x}, {z})");
                }
            }
        }
        covered
    }

    #[test]
    fn exact_rectangles_compact_runs_without_filling_holes() {
        let cells: BTreeSet<_> = [
            (0, 0),
            (1, 0),
            (2, 0),
            (0, 1),
            (2, 1),
            (0, 2),
            (1, 2),
            (2, 2),
            (5, 4),
            (6, 4),
            (5, 5),
            (6, 5),
        ]
        .into_iter()
        .collect();
        let rectangles = exact_cell_rectangles(cells.clone());
        assert_eq!(covered_cells(&rectangles), cells);
        assert!(rectangles.len() < cells.len());
    }

    #[test]
    fn exact_rectangles_merge_a_full_opening() {
        let cells: BTreeSet<_> = (10..16).map(|z| (3, z)).collect();
        assert_eq!(exact_cell_rectangles(cells), vec![[3, 10, 4, 16]]);
    }
}

/// Emit lateral portal pairs where two consecutive authored layers meet along
/// a cardinal edge instead of overlapping in X/Z.
///
/// A terraced floor (a shallow pit, stair landing, water basin, etc.) is often
/// easiest to author by painting its lower cells on the layer below. Those
/// cells cook into a distinct runtime room because their `origin_y` differs.
/// Previously only same-layer chunks received horizontal portals and only
/// overlapping cells received vertical portals, leaving an adjacent terrace
/// as two isolated runtime rooms. The current room could neither stream/query
/// the other side's collision nor keep it in the visible set.
///
/// Low solid riser walls are deliberately allowed: if their top does not rise
/// above the higher floor edge, the character motor can step over them. A wall
/// extending into the opening still seals the seam and suppresses the portal.
pub(crate) fn auto_wire_floor_terrace_portals(
    scene: &crate::Scene,
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> Vec<PlaytestRoomPortal> {
    let mut out = Vec::new();
    for (node_id, chunks) in chunks_by_node {
        let Some(room_node) = scene.node(*node_id) else {
            continue;
        };
        let NodeKind::Section { grid } = &room_node.kind else {
            continue;
        };
        let s = grid.sector_size;
        if s <= 0 || grid.floor_count() < 2 {
            continue;
        }
        let room_origin_y_base =
            (f64::from(room_node.transform.translation[1]) * f64::from(s)) as i32;
        let floor_elevation_offset = room_origin_y_base.saturating_sub(grid.elevation);
        let mut seams = Vec::new();

        for source in chunks {
            let Some(source_grid) = grid.floor(source.floor_idx) else {
                continue;
            };
            let source_origin_y = floor_elevation_offset.saturating_add(source_grid.elevation);
            for &cell in &source.cells {
                let world_cell = chunk_cell_world_cell(source, cell);
                for direction in GridDirection::CARDINAL {
                    let Some(opposite) = direction.opposite_cardinal() else {
                        continue;
                    };
                    let neighbour_cell = terrace_neighbour_world_cell(world_cell, direction);
                    let Some(destination) = chunks.iter().find(|candidate| {
                        candidate.room_index != source.room_index
                            && candidate.floor_idx.abs_diff(source.floor_idx) == 1
                            && chunk_contains_world_cell(candidate, neighbour_cell)
                    }) else {
                        continue;
                    };
                    // One physical seam produces one reciprocal pair.
                    if source.room_index > destination.room_index {
                        continue;
                    }
                    let Some(destination_grid) = grid.floor(destination.floor_idx) else {
                        continue;
                    };
                    let Some((source_x, source_z)) =
                        source_grid.world_cell_to_array(world_cell[0], world_cell[1])
                    else {
                        continue;
                    };
                    let Some((destination_x, destination_z)) =
                        destination_grid.world_cell_to_array(neighbour_cell[0], neighbour_cell[1])
                    else {
                        continue;
                    };
                    let Some(source_sector) = source_grid.sector(source_x, source_z) else {
                        continue;
                    };
                    let Some(destination_sector) =
                        destination_grid.sector(destination_x, destination_z)
                    else {
                        continue;
                    };
                    if source_sector.floor.is_none() || destination_sector.floor.is_none() {
                        continue;
                    }

                    let destination_origin_y =
                        floor_elevation_offset.saturating_add(destination_grid.elevation);
                    let source_heights = source_grid
                        .wall_heights_aligned_to_surfaces_for_world_cell(
                            world_cell[0],
                            world_cell[1],
                            direction,
                        )
                        .map(|height| source_origin_y.saturating_add(height));
                    let destination_heights = destination_grid
                        .wall_heights_aligned_to_surfaces_for_world_cell(
                            neighbour_cell[0],
                            neighbour_cell[1],
                            opposite,
                        )
                        .map(|height| destination_origin_y.saturating_add(height));
                    let bottom = source_heights[0]
                        .max(source_heights[1])
                        .max(destination_heights[0])
                        .max(destination_heights[1]);
                    let top = source_heights[2]
                        .min(source_heights[3])
                        .min(destination_heights[2])
                        .min(destination_heights[3]);
                    if top <= bottom
                        || terrace_wall_seals_opening(
                            source_sector.walls.get(direction),
                            source_origin_y,
                            bottom,
                        )
                        || terrace_wall_seals_opening(
                            destination_sector.walls.get(opposite),
                            destination_origin_y,
                            bottom,
                        )
                    {
                        continue;
                    }

                    seams.push(TerracePortalSeam {
                        source_room: source.room_index,
                        destination_room: destination.room_index,
                        direction,
                        first_cell: world_cell,
                        last_cell: world_cell,
                        bottom,
                        top,
                    });
                }
            }
        }

        seams.sort_by_key(terrace_portal_seam_sort_key);
        let mut index = 0usize;
        while index < seams.len() {
            let mut merged = seams[index];
            index += 1;
            while index < seams.len() && terrace_portal_seams_can_merge(merged, seams[index]) {
                merged.last_cell = seams[index].last_cell;
                index += 1;
            }
            let Some(opposite) = merged.direction.opposite_cardinal() else {
                continue;
            };
            let vertices = terrace_portal_span_vertices(merged, s);
            out.push(PlaytestRoomPortal {
                source_room: merged.source_room,
                destination_room: merged.destination_room,
                kind: 0,
                normal: terrace_portal_source_normal(merged.direction),
                vertices,
            });
            out.push(PlaytestRoomPortal {
                source_room: merged.destination_room,
                destination_room: merged.source_room,
                kind: 0,
                normal: terrace_portal_source_normal(opposite),
                vertices,
            });
        }
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct TerracePortalSeam {
    source_room: u16,
    destination_room: u16,
    direction: GridDirection,
    first_cell: [i32; 2],
    last_cell: [i32; 2],
    bottom: i32,
    top: i32,
}

fn terrace_portal_seam_sort_key(seam: &TerracePortalSeam) -> (u16, u16, u8, i32, i32, i32, i32) {
    let (line, span) = terrace_portal_seam_line_and_span(*seam);
    (
        seam.source_room,
        seam.destination_room,
        terrace_direction_slot(seam.direction),
        line,
        span,
        seam.bottom,
        seam.top,
    )
}

fn terrace_portal_seams_can_merge(left: TerracePortalSeam, right: TerracePortalSeam) -> bool {
    if left.source_room != right.source_room
        || left.destination_room != right.destination_room
        || left.direction != right.direction
        || left.bottom != right.bottom
        || left.top != right.top
    {
        return false;
    }
    let (left_line, _) = terrace_portal_seam_line_and_span(left);
    let (right_line, right_span) = terrace_portal_seam_line_and_span(right);
    let left_span = match left.direction {
        GridDirection::North | GridDirection::South => left.last_cell[0],
        GridDirection::East | GridDirection::West => left.last_cell[1],
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => i32::MIN,
    };
    left_line == right_line && left_span.saturating_add(1) == right_span
}

fn terrace_portal_seam_line_and_span(seam: TerracePortalSeam) -> (i32, i32) {
    match seam.direction {
        GridDirection::North => (seam.first_cell[1].saturating_add(1), seam.first_cell[0]),
        GridDirection::East => (seam.first_cell[0].saturating_add(1), seam.first_cell[1]),
        GridDirection::South => (seam.first_cell[1], seam.first_cell[0]),
        GridDirection::West => (seam.first_cell[0], seam.first_cell[1]),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => {
            (i32::MIN, i32::MIN)
        }
    }
}

fn terrace_direction_slot(direction: GridDirection) -> u8 {
    match direction {
        GridDirection::North => 0,
        GridDirection::East => 1,
        GridDirection::South => 2,
        GridDirection::West => 3,
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => u8::MAX,
    }
}

fn terrace_neighbour_world_cell(cell: [i32; 2], direction: GridDirection) -> [i32; 2] {
    match direction {
        GridDirection::North => [cell[0], cell[1].saturating_add(1)],
        GridDirection::East => [cell[0].saturating_add(1), cell[1]],
        GridDirection::South => [cell[0], cell[1].saturating_sub(1)],
        GridDirection::West => [cell[0].saturating_sub(1), cell[1]],
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => cell,
    }
}

fn terrace_wall_seals_opening(
    walls: &[GridVerticalFace],
    room_origin_y: i32,
    opening_bottom: i32,
) -> bool {
    walls.iter().any(|wall| {
        wall.solid
            && room_origin_y.saturating_add(wall.heights[2].max(wall.heights[3])) > opening_bottom
    })
}

fn terrace_portal_span_vertices(seam: TerracePortalSeam, sector_size: i32) -> [[i32; 3]; 4] {
    let x0 = seam.first_cell[0].saturating_mul(sector_size);
    let x1 = seam.last_cell[0]
        .saturating_add(1)
        .saturating_mul(sector_size);
    let z0 = seam.first_cell[1].saturating_mul(sector_size);
    let z1 = seam.last_cell[1]
        .saturating_add(1)
        .saturating_mul(sector_size);
    let (a, b) = match seam.direction {
        GridDirection::North => ([x0, z1], [x1, z1]),
        GridDirection::East => ([x1, z1], [x1, z0]),
        GridDirection::South => ([x0, z0], [x1, z0]),
        GridDirection::West => ([x0, z1], [x0, z0]),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => {
            ([x0, z0], [x0, z0])
        }
    };
    [
        [a[0], seam.bottom, a[1]],
        [b[0], seam.bottom, b[1]],
        [b[0], seam.top, b[1]],
        [a[0], seam.top, a[1]],
    ]
}

fn terrace_portal_source_normal(direction: GridDirection) -> [i16; 3] {
    match direction {
        GridDirection::North => [0, 0, -1],
        GridDirection::East => [-1, 0, 0],
        GridDirection::South => [0, 0, 1],
        GridDirection::West => [1, 0, 0],
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => [0, 0, 0],
    }
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
        if matches!(parent.kind, NodeKind::Section { .. }) {
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
    format!("Section '{name}' failed cook: {err}")
}

/// Wire authored portals that join two different Sections.
///
/// The intra-section planner deliberately only cuts seams inside one grid
/// (`portal_rooms.rs` filters markers to descendants of the section node), so
/// before this every Section cooked as its own island: no shared portal, no PVS
/// relationship, no traversal edge. `room_connections.rs` derived the
/// `Section A <-> Section B` view years ahead of anything consuming it; this is
/// where that view finally reaches the runtime.
///
/// Sections share one world-cell space through `WorldGrid::origin` (a Section's
/// own X/Z transform never moves its geometry), so a cross-section portal is
/// expressed in exactly the same coordinates as an intra-section one and needs
/// no rebasing.
///
/// Returns the portal pairs plus diagnostics for connections that cannot be
/// wired, so the caller can fail the cook rather than ship a door to nowhere.
pub(crate) fn cross_section_portals(
    scene: &crate::Scene,
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
) -> (Vec<PlaytestRoomPortal>, Vec<CrossSectionIssue>) {
    let mut out = Vec::new();
    let mut issues = Vec::new();

    for connection in crate::room_connections::derive_room_connections(scene) {
        let a = &connection.a;
        let Some(b) = connection.b.as_ref() else {
            // Unassigned / missing target: the editor panel already flags these;
            // the cook now refuses them rather than silently dropping the link.
            issues.push(CrossSectionIssue {
                portal: a.portal,
                room: a.room,
                status: connection.status,
            });
            continue;
        };
        if a.room == b.room {
            // Same section: the intra-section planner already cut this seam.
            continue;
        }
        if connection.status.needs_repair() {
            issues.push(CrossSectionIssue {
                portal: a.portal,
                room: a.room,
                status: connection.status,
            });
            continue;
        }

        let (Some(source), Some(destination)) = (
            portal_runtime_room(scene, chunks_by_node, a),
            portal_runtime_room(scene, chunks_by_node, b),
        ) else {
            issues.push(CrossSectionIssue {
                portal: a.portal,
                room: a.room,
                status: connection.status,
            });
            continue;
        };
        let Some((quad, normal)) = cross_section_quad(scene, a) else {
            issues.push(CrossSectionIssue {
                portal: a.portal,
                room: a.room,
                status: connection.status,
            });
            continue;
        };

        out.push(PlaytestRoomPortal {
            source_room: source,
            destination_room: destination,
            kind: 0,
            normal,
            vertices: quad,
        });
        // The reciprocal record, so visibility and traversal work both ways.
        out.push(PlaytestRoomPortal {
            source_room: destination,
            destination_room: source,
            kind: 0,
            normal: [-normal[0], -normal[1], -normal[2]],
            vertices: quad,
        });
    }

    (out, issues)
}

/// A cross-section connection the cook could not wire.
pub(crate) struct CrossSectionIssue {
    pub portal: NodeId,
    pub room: NodeId,
    pub status: crate::room_connections::RoomConnectionStatus,
}

/// Runtime room index for the chunk holding this portal endpoint's cell.
fn portal_runtime_room(
    scene: &crate::Scene,
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
    endpoint: &crate::room_connections::RoomConnectionEndpoint,
) -> Option<u16> {
    let Some(NodeKind::Section { grid }) = scene.node(endpoint.room).map(|n| &n.kind) else {
        return None;
    };
    let edge = endpoint.edge?;
    let world_cell = [
        grid.origin[0].saturating_add(i32::from(edge.x)),
        grid.origin[1].saturating_add(i32::from(edge.z)),
    ];
    chunks_by_node
        .get(&endpoint.room)?
        .iter()
        .find(|chunk| chunk_contains_world_cell(chunk, world_cell))
        .map(|chunk| chunk.room_index)
}

/// The portal rectangle in shared world units, plus its source-facing normal.
fn cross_section_quad(
    scene: &crate::Scene,
    endpoint: &crate::room_connections::RoomConnectionEndpoint,
) -> Option<([[i32; 3]; 4], [i16; 3])> {
    let Some(NodeKind::Section { grid }) = scene.node(endpoint.room).map(|n| &n.kind) else {
        return None;
    };
    let edge = endpoint.edge?;
    let s = grid.sector_size;
    if s <= 0 {
        return None;
    }
    let base_y = scene
        .node(endpoint.room)
        .map(|node| (f64::from(node.transform.translation[1]) * f64::from(s)) as i32)
        .unwrap_or(0);
    let top = base_y.saturating_add(s.saturating_mul(2));

    let x = grid.origin[0].saturating_add(i32::from(edge.x));
    let z = grid.origin[1].saturating_add(i32::from(edge.z));
    // Canonical edges are north or east only, matching the intra-section planner.
    let (a, b, normal) = match edge.direction {
        GridDirection::North => (
            [x.saturating_mul(s), z.saturating_add(1).saturating_mul(s)],
            [
                x.saturating_add(1).saturating_mul(s),
                z.saturating_add(1).saturating_mul(s),
            ],
            [0i16, 0, -1],
        ),
        GridDirection::East => (
            [
                x.saturating_add(1).saturating_mul(s),
                z.saturating_add(1).saturating_mul(s),
            ],
            [x.saturating_add(1).saturating_mul(s), z.saturating_mul(s)],
            [-1i16, 0, 0],
        ),
        _ => return None,
    };
    Some((
        [
            [a[0], base_y, a[1]],
            [b[0], base_y, b[1]],
            [b[0], top, b[1]],
            [a[0], top, a[1]],
        ],
        normal,
    ))
}

/// Emit portals where two different Sections meet on a shared cell edge that
/// both sides leave open.
///
/// This is the socket contract from the prefab kit applied between Sections: a
/// socket is the absence of a perimeter wall, so two pieces stamped edge to
/// edge with facing sockets already describe a doorway, and authoring a Portal
/// marker for every such join is busywork the stamp implied.
///
/// It does NOT contradict the planner's rule that it "must not invent chunk
/// boundaries for size, walls, or streaming". That rule stops the cook slicing
/// one authored grid behind the author's back. Joining two Sections the author
/// deliberately placed against each other honours intent rather than inventing
/// it. Edges already covered by an authored portal are skipped, so hand-wiring
/// always wins.
pub(crate) fn auto_adjacent_section_portals(
    scene: &crate::Scene,
    chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
    already_wired: &HashSet<(i32, i32, char)>,
) -> Vec<PlaytestRoomPortal> {
    // (world x, world z, floor elevation) -> (section, runtime room, floor)
    let mut occupancy: HashMap<(i32, i32, i32), (NodeId, u16, usize)> = HashMap::new();
    for (node_id, chunks) in chunks_by_node {
        let Some(NodeKind::Section { grid: base }) = scene.node(*node_id).map(|n| &n.kind) else {
            continue;
        };
        let base_y = scene
            .node(*node_id)
            .map(|n| (f64::from(n.transform.translation[1]) * f64::from(base.sector_size)) as i32)
            .unwrap_or(0);
        for chunk in chunks {
            let Some(floor) = base.floor(chunk.floor_idx) else {
                continue;
            };
            let elevation = base_y
                .saturating_add(floor.elevation)
                .saturating_sub(base.elevation);
            for &cell in &chunk.cells {
                let wc = chunk_cell_world_cell(chunk, cell);
                occupancy.insert((wc[0], wc[1], elevation), (*node_id, chunk.room_index, chunk.floor_idx));
            }
        }
    }

    let mut out = Vec::new();
    let mut emitted: HashSet<(i32, i32, char)> = HashSet::new();
    let mut keys: Vec<_> = occupancy.keys().copied().collect();
    keys.sort_unstable();
    for key in keys {
        let (x, z, elevation) = key;
        let (section, room, floor_idx) = occupancy[&key];
        // East and north only, so each shared edge is considered once.
        for (direction, axis, nx, nz) in [
            (GridDirection::East, 'X', x + 1, z),
            (GridDirection::North, 'Z', x, z + 1),
        ] {
            let Some(&(other_section, other_room, other_floor)) =
                occupancy.get(&(nx, nz, elevation))
            else {
                continue;
            };
            if other_section == section {
                continue; // the intra-section planner owns this edge
            }
            if already_wired.contains(&(x, z, axis)) || !emitted.insert((x, z, axis)) {
                continue;
            }
            // Both sides must leave the edge open. One wall is a wall.
            let back = direction.opposite_cardinal().unwrap_or(direction);
            if !section_edge_is_open(scene, section, floor_idx, [x, z], direction)
                || !section_edge_is_open(scene, other_section, other_floor, [nx, nz], back)
            {
                continue;
            }
            let Some(grid) = section_floor(scene, section, floor_idx) else {
                continue;
            };
            let s = grid.sector_size;
            if s <= 0 {
                continue;
            }
            let top = elevation.saturating_add(s.saturating_mul(2));
            let (a, b, normal) = match direction {
                GridDirection::East => (
                    [(x + 1).saturating_mul(s), (z + 1).saturating_mul(s)],
                    [(x + 1).saturating_mul(s), z.saturating_mul(s)],
                    [-1i16, 0, 0],
                ),
                _ => (
                    [x.saturating_mul(s), (z + 1).saturating_mul(s)],
                    [(x + 1).saturating_mul(s), (z + 1).saturating_mul(s)],
                    [0i16, 0, -1],
                ),
            };
            let quad = [
                [a[0], elevation, a[1]],
                [b[0], elevation, b[1]],
                [b[0], top, b[1]],
                [a[0], top, a[1]],
            ];
            out.push(PlaytestRoomPortal {
                source_room: room,
                destination_room: other_room,
                kind: 0,
                normal,
                vertices: quad,
            });
            out.push(PlaytestRoomPortal {
                source_room: other_room,
                destination_room: room,
                kind: 0,
                normal: [-normal[0], -normal[1], -normal[2]],
                vertices: quad,
            });
        }
    }
    out
}

fn section_floor<'a>(
    scene: &'a crate::Scene,
    section: NodeId,
    floor_idx: usize,
) -> Option<&'a WorldGrid> {
    let Some(NodeKind::Section { grid }) = scene.node(section).map(|n| &n.kind) else {
        return None;
    };
    grid.floor(floor_idx)
}

/// True when the cell has floor and carries no wall on `direction`.
fn section_edge_is_open(
    scene: &crate::Scene,
    section: NodeId,
    floor_idx: usize,
    world_cell: [i32; 2],
    direction: GridDirection,
) -> bool {
    let Some(grid) = section_floor(scene, section, floor_idx) else {
        return false;
    };
    grid.world_cell_to_array(world_cell[0], world_cell[1])
        .and_then(|(sx, sz)| grid.sector(sx, sz))
        .is_some_and(|sector| sector.floor.is_some() && sector.walls.get(direction).is_empty())
}
