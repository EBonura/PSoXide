//! Portal-seam outline rendering for the editor 3D viewport.

use super::*;

pub(crate) fn walk_portal_seams(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: NodeId,
    hovered: Option<NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for node in scene.nodes() {
        if !matches!(node.kind, NodeKind::Portal { .. })
            || scene_node_hidden(scene, hidden_scene_nodes, node.id)
            || !is_descendant_of_room(scene, node.id, room_id)
        {
            continue;
        }
        let mut style = PORTAL_SEAM_STYLE;
        if node.id == selected || hovered == Some(node.id) {
            style.thickness_px = 4.0;
        }
        push_portal_seam_edges(grid, portal_seam_edges_for_node(grid, node), style, scratch);
    }
}

pub(crate) fn push_portal_seam_edges(
    grid: &WorldGrid,
    edges: Vec<PortalEdge>,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    for run in portal_seam_runs(edges) {
        push_portal_seam_run(grid, run, style, scratch);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PortalSeamRun {
    start: PortalEdge,
    len: u16,
}

pub(crate) fn portal_seam_runs(mut edges: Vec<PortalEdge>) -> Vec<PortalSeamRun> {
    edges.retain(|edge| matches!(edge.direction, GridDirection::North | GridDirection::East));
    edges.sort_by_key(|edge| match edge.direction {
        GridDirection::North => (0, edge.z, edge.x),
        GridDirection::East => (1, edge.x, edge.z),
        _ => (2, edge.z, edge.x),
    });
    let mut runs = Vec::new();
    for edge in edges {
        if let Some(run) = runs.last_mut() {
            if portal_edge_continues_run(run, edge) {
                run.len = run.len.saturating_add(1);
                continue;
            }
        }
        runs.push(PortalSeamRun {
            start: edge,
            len: 1,
        });
    }
    runs
}

pub(crate) fn portal_edge_continues_run(run: &PortalSeamRun, edge: PortalEdge) -> bool {
    if edge.direction != run.start.direction {
        return false;
    }
    match edge.direction {
        GridDirection::North => {
            edge.z == run.start.z && run.start.x.checked_add(run.len) == Some(edge.x)
        }
        GridDirection::East => {
            edge.x == run.start.x && run.start.z.checked_add(run.len) == Some(edge.z)
        }
        _ => false,
    }
}

pub(crate) fn push_portal_seam_run(
    grid: &WorldGrid,
    run: PortalSeamRun,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    if run.len == 0 {
        return;
    }
    let mut first: Option<[spatial::RoomPoint; 4]> = None;
    let mut last: Option<[spatial::RoomPoint; 4]> = None;
    for offset in 0..run.len {
        let edge = match run.start.direction {
            GridDirection::North => {
                let Some(x) = run.start.x.checked_add(offset) else {
                    return;
                };
                PortalEdge { x, ..run.start }
            }
            GridDirection::East => {
                let Some(z) = run.start.z.checked_add(offset) else {
                    return;
                };
                PortalEdge { z, ..run.start }
            }
            _ => return,
        };
        let Some(corners) = portal_edge_wall_corners(grid, edge) else {
            continue;
        };
        push_portal_segment(scratch, corners[0], corners[1], style);
        push_portal_segment(scratch, corners[3], corners[2], style);
        if first.is_none() {
            first = Some(corners);
        }
        last = Some(corners);
    }

    let Some(first) = first else {
        return;
    };
    let Some(last) = last else {
        return;
    };
    match run.start.direction {
        GridDirection::North => {
            push_portal_segment(scratch, first[0], first[3], style);
            push_portal_segment(scratch, last[1], last[2], style);
        }
        GridDirection::East => {
            push_portal_segment(scratch, first[1], first[2], style);
            push_portal_segment(scratch, last[0], last[3], style);
        }
        _ => {}
    }
}

pub(crate) fn push_portal_edge_wall_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some(corners) = portal_edge_wall_corners_for_world_cell(grid, wcx, wcz, dir) else {
        return;
    };
    push_portal_segment(scratch, corners[0], corners[1], style);
    push_portal_segment(scratch, corners[1], corners[2], style);
    push_portal_segment(scratch, corners[2], corners[3], style);
    push_portal_segment(scratch, corners[3], corners[0], style);
}

pub(crate) fn portal_edge_wall_corners(grid: &WorldGrid, edge: PortalEdge) -> Option<[spatial::RoomPoint; 4]> {
    portal_edge_wall_corners_for_world_cell(
        grid,
        grid.origin[0] + edge.x as i32,
        grid.origin[1] + edge.z as i32,
        edge.direction,
    )
}

pub(crate) fn portal_edge_wall_corners_for_world_cell(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
) -> Option<[spatial::RoomPoint; 4]> {
    const BOTTOM_LIFT: i32 = 24;
    let bounds = spatial::cell_bounds_from_world_cell(wcx, wcz, grid.sector_size);
    let heights = portal_edge_height_span_for_world_cell(grid, wcx, wcz, dir);
    let mut corners = spatial::editor_wall_outline_corners(bounds, dir, heights, 0)?;
    corners[0][1] = corners[0][1].saturating_add(BOTTOM_LIFT);
    corners[1][1] = corners[1][1].saturating_add(BOTTOM_LIFT);
    Some(corners)
}

pub(crate) fn portal_edge_height_span_for_world_cell(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
) -> [i32; 4] {
    let mut bottom: [Option<i32>; 2] = [None, None];
    let mut top: [Option<i32>; 2] = [None, None];
    let mut fallback_bottom: Option<i32> = None;
    let mut fallback_top: Option<i32> = None;

    if let Some(sector) = sector_for_world_cell(grid, wcx, wcz) {
        sample_sector_edge_span(sector, dir, false, &mut bottom, &mut top);
        sample_sector_vertical_bounds(sector, &mut fallback_bottom, &mut fallback_top);
    }
    if let Some((nwcx, nwcz, opposite)) = portal_neighbour_world_cell(wcx, wcz, dir) {
        if let Some(sector) = sector_for_world_cell(grid, nwcx, nwcz) {
            sample_sector_edge_span(sector, opposite, true, &mut bottom, &mut top);
            sample_sector_vertical_bounds(sector, &mut fallback_bottom, &mut fallback_top);
        }
    }

    let fallback_bottom = fallback_bottom.unwrap_or(0);
    let bottom = [
        bottom[0].unwrap_or(fallback_bottom).min(fallback_bottom),
        bottom[1].unwrap_or(fallback_bottom).min(fallback_bottom),
    ];
    let fallback_top = fallback_top.unwrap_or_else(|| {
        bottom[0]
            .max(bottom[1])
            .saturating_add(grid.sector_size.max(1))
    });
    let mut top = [
        top[0].unwrap_or(fallback_top).max(fallback_top),
        top[1].unwrap_or(fallback_top).max(fallback_top),
    ];
    for i in 0..2 {
        if top[i] <= bottom[i] {
            top[i] = bottom[i].saturating_add(grid.sector_size.max(1));
        }
    }
    [bottom[0], bottom[1], top[1], top[0]]
}

pub(crate) fn sector_for_world_cell(grid: &WorldGrid, wcx: i32, wcz: i32) -> Option<&GridSector> {
    let (sx, sz) = grid.world_cell_to_array(wcx, wcz)?;
    grid.sector(sx, sz)
}

pub(crate) fn portal_neighbour_world_cell(
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
) -> Option<(i32, i32, GridDirection)> {
    let opposite = dir.opposite_cardinal()?;
    match dir {
        GridDirection::North => Some((wcx, wcz.checked_add(1)?, opposite)),
        GridDirection::East => Some((wcx.checked_add(1)?, wcz, opposite)),
        GridDirection::South => Some((wcx, wcz.checked_sub(1)?, opposite)),
        GridDirection::West => Some((wcx.checked_sub(1)?, wcz, opposite)),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

pub(crate) fn sample_sector_edge_span(
    sector: &GridSector,
    dir: GridDirection,
    reverse: bool,
    bottom: &mut [Option<i32>; 2],
    top: &mut [Option<i32>; 2],
) {
    if let Some(edge) = sector
        .floor
        .as_ref()
        .and_then(|floor| horizontal_edge_heights_for_portal(floor.heights, dir))
    {
        include_min_edge(bottom, maybe_reverse_edge(edge, reverse));
    }
    if let Some(edge) = sector
        .ceiling
        .as_ref()
        .and_then(|ceiling| horizontal_edge_heights_for_portal(ceiling.heights, dir))
    {
        include_max_edge(top, maybe_reverse_edge(edge, reverse));
    }
    for wall in sector.walls.get(dir) {
        let wall_bottom = [
            wall.heights[WallCorner::BL.idx()],
            wall.heights[WallCorner::BR.idx()],
        ];
        let wall_top = [
            wall.heights[WallCorner::TL.idx()],
            wall.heights[WallCorner::TR.idx()],
        ];
        include_min_edge(bottom, maybe_reverse_edge(wall_bottom, reverse));
        include_max_edge(top, maybe_reverse_edge(wall_top, reverse));
    }
}

pub(crate) fn sample_sector_vertical_bounds(
    sector: &GridSector,
    bottom: &mut Option<i32>,
    top: &mut Option<i32>,
) {
    if let Some(floor) = &sector.floor {
        for height in floor.heights {
            include_min_value(bottom, height);
        }
    }
    if let Some(ceiling) = &sector.ceiling {
        for height in ceiling.heights {
            include_max_value(top, height);
        }
    }
    for dir in GridDirection::ALL {
        for wall in sector.walls.get(dir) {
            for height in wall.heights {
                include_min_value(bottom, height);
                include_max_value(top, height);
            }
        }
    }
}

pub(crate) fn horizontal_edge_heights_for_portal(heights: [i32; 4], dir: GridDirection) -> Option<[i32; 2]> {
    match dir {
        GridDirection::North => Some([heights[Corner::NW.idx()], heights[Corner::NE.idx()]]),
        GridDirection::East => Some([heights[Corner::NE.idx()], heights[Corner::SE.idx()]]),
        GridDirection::South => Some([heights[Corner::SE.idx()], heights[Corner::SW.idx()]]),
        GridDirection::West => Some([heights[Corner::SW.idx()], heights[Corner::NW.idx()]]),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

pub(crate) fn maybe_reverse_edge(mut edge: [i32; 2], reverse: bool) -> [i32; 2] {
    if reverse {
        edge.swap(0, 1);
    }
    edge
}

pub(crate) fn include_min_edge(target: &mut [Option<i32>; 2], edge: [i32; 2]) {
    for i in 0..2 {
        include_min_value(&mut target[i], edge[i]);
    }
}

pub(crate) fn include_max_edge(target: &mut [Option<i32>; 2], edge: [i32; 2]) {
    for i in 0..2 {
        include_max_value(&mut target[i], edge[i]);
    }
}

pub(crate) fn include_min_value(target: &mut Option<i32>, value: i32) {
    *target = Some(target.map_or(value, |current| current.min(value)));
}

pub(crate) fn include_max_value(target: &mut Option<i32>, value: i32) {
    *target = Some(target.map_or(value, |current| current.max(value)));
}

pub(crate) fn push_portal_segment(
    scratch: &mut PreviewScratch,
    a: spatial::RoomPoint,
    b: spatial::RoomPoint,
    style: FaceOutlineStyle,
) {
    let pa = gte_scene::project_vertex(world_to_view(a));
    let pb = gte_scene::project_vertex(world_to_view(b));
    if pa.sz == 0 || pb.sz == 0 {
        return;
    }
    push_screen_line(scratch, pa, pb, style);
}

pub(crate) fn canonical_portal_edge_for_array_cell(
    sx: u16,
    sz: u16,
    dir: GridDirection,
) -> Option<PortalEdge> {
    match dir {
        GridDirection::North => Some(PortalEdge {
            x: sx,
            z: sz,
            direction: GridDirection::North,
        }),
        GridDirection::East => Some(PortalEdge {
            x: sx,
            z: sz,
            direction: GridDirection::East,
        }),
        GridDirection::South => Some(PortalEdge {
            x: sx,
            z: sz.checked_sub(1)?,
            direction: GridDirection::North,
        }),
        GridDirection::West => Some(PortalEdge {
            x: sx.checked_sub(1)?,
            z: sz,
            direction: GridDirection::East,
        }),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}
