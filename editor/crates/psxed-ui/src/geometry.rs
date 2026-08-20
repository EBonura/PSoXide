use super::*;

/// Outcome of a primitive delete. `Removed` => the face is gone
/// (user selected a face or edge). `Triangulated` => one corner
/// was dropped and the face is still alive as a triangle.
/// `Missing` => the face / sector wasn't where the selection
/// thought it was (stale state) -- caller should leave things
/// alone.
pub(crate) enum DeleteOutcome {
    Removed(&'static str),
    Triangulated(&'static str),
    Missing,
}

/// Short label for a face kind, used in delete status messages.
pub(crate) const fn describe_face_kind(kind: FaceKind) -> &'static str {
    match kind {
        FaceKind::Floor => "floor",
        FaceKind::Ceiling => "ceiling",
        FaceKind::Wall { .. } => "wall",
    }
}

/// One-line human description of a `FaceRef` for status messages
/// and the inspector header. Walls include their cardinal direction
/// + stack index since a single edge can carry several stacked
///   walls (windows / arches).
pub(crate) fn describe_face(face: FaceRef) -> String {
    match face.kind {
        FaceKind::Floor => format!("Floor at {},{}", face.sx, face.sz),
        FaceKind::Ceiling => format!("Ceiling at {},{}", face.sx, face.sz),
        FaceKind::Wall { dir, stack } => {
            format!(
                "{} wall #{stack} at {},{}",
                direction_label(dir),
                face.sx,
                face.sz
            )
        }
    }
}

/// Status-line / breadcrumb text for any `Selection`. Falls
/// through to `describe_face` for face selections so existing
/// face copy stays identical.
pub(crate) fn describe_selection(selection: Selection) -> String {
    match selection {
        Selection::Face(face) => describe_face(face),
        Selection::Triangle(triangle) => describe_triangle(triangle),
        Selection::Edge(edge) => describe_edge(edge),
        Selection::Vertex(vertex) => describe_vertex(vertex),
    }
}

pub(crate) fn describe_triangle(triangle: HorizontalTriangleRef) -> String {
    let surface = match triangle.surface {
        HorizontalSurfaceKind::Floor => "Floor",
        HorizontalSurfaceKind::Ceiling => "Ceiling",
    };
    format!(
        "{surface} triangle {} at {},{}",
        triangle.index.label(),
        triangle.sx,
        triangle.sz
    )
}

pub(crate) fn describe_material_target(target: MaterialTarget) -> String {
    match target {
        MaterialTarget::Face(face) => describe_face(face),
        MaterialTarget::Triangle(triangle) => describe_triangle(triangle),
        MaterialTarget::BrushFace { brush, face } => {
            format!("brush {} face {}", brush + 1, face + 1)
        }
    }
}

pub(crate) fn selection_wall_face(selection: Selection) -> Option<FaceRef> {
    let Selection::Face(face) = selection else {
        return None;
    };
    matches!(face.kind, FaceKind::Wall { .. }).then_some(face)
}

pub(crate) fn selection_horizontal_face(selection: Selection, kind: FaceKind) -> Option<FaceRef> {
    let Selection::Face(face) = selection else {
        return None;
    };
    (face.kind == kind && matches!(face.kind, FaceKind::Floor | FaceKind::Ceiling)).then_some(face)
}

pub(crate) const fn horizontal_face_plural(kind: FaceKind) -> &'static str {
    match kind {
        FaceKind::Floor => "floors",
        FaceKind::Ceiling => "ceilings",
        FaceKind::Wall { .. } => "faces",
    }
}

pub(crate) fn selection_edge(selection: Selection) -> Option<EdgeRef> {
    let Selection::Edge(edge) = selection else {
        return None;
    };
    Some(edge)
}

pub(crate) fn wall_span_bounds(
    anchor: FaceRef,
    current: FaceRef,
    dir: GridDirection,
) -> Option<(u16, u16, u16, u16)> {
    match dir {
        GridDirection::North | GridDirection::South if anchor.sz == current.sz => Some((
            anchor.sx.min(current.sx),
            anchor.sx.max(current.sx),
            anchor.sz,
            anchor.sz,
        )),
        GridDirection::East | GridDirection::West if anchor.sx == current.sx => Some((
            anchor.sx,
            anchor.sx,
            anchor.sz.min(current.sz),
            anchor.sz.max(current.sz),
        )),
        _ => None,
    }
}

pub(crate) fn selection_sector(selection: Selection) -> (NodeId, u16, u16) {
    match selection {
        Selection::Face(face) => (face.room, face.sx, face.sz),
        Selection::Triangle(triangle) => (triangle.room, triangle.sx, triangle.sz),
        Selection::Edge(edge) => match edge.anchor {
            EdgeAnchor::Floor { sx, sz, .. }
            | EdgeAnchor::Ceiling { sx, sz, .. }
            | EdgeAnchor::Wall { sx, sz, .. } => (edge.room, sx, sz),
        },
        Selection::Vertex(vertex) => match vertex.anchor {
            VertexAnchor::Floor { sx, sz, .. }
            | VertexAnchor::Ceiling { sx, sz, .. }
            | VertexAnchor::Wall { sx, sz, .. } => (vertex.room, sx, sz),
        },
    }
}

pub(crate) fn describe_edge(edge: EdgeRef) -> String {
    match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => {
            format!("Floor {} edge at {sx},{sz}", direction_label(dir))
        }
        EdgeAnchor::Ceiling { sx, sz, dir } => {
            format!("Ceiling {} edge at {sx},{sz}", direction_label(dir))
        }
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge,
        } => format!(
            "{} wall #{stack} {} edge at {sx},{sz}",
            direction_label(dir),
            wall_edge_label(edge),
        ),
    }
}

pub(crate) fn describe_vertex(vertex: VertexRef) -> String {
    match vertex.anchor {
        VertexAnchor::Floor { sx, sz, corner } => {
            format!("Floor {} vertex at {sx},{sz}", corner_label(corner))
        }
        VertexAnchor::Ceiling { sx, sz, corner } => {
            format!("Ceiling {} vertex at {sx},{sz}", corner_label(corner))
        }
        VertexAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            corner,
        } => format!(
            "{} wall #{stack} {} vertex at {sx},{sz}",
            direction_label(dir),
            wall_corner_label(corner),
        ),
    }
}

pub(crate) fn push_unique_selection(selections: &mut Vec<Selection>, selection: Selection) {
    if !selections.contains(&selection) {
        selections.push(selection);
    }
}

pub(crate) fn push_unique_material_target(
    targets: &mut Vec<MaterialTarget>,
    target: MaterialTarget,
) {
    if !targets.contains(&target) {
        targets.push(target);
    }
}

pub(crate) fn autotile_selection_status(
    selected_tiles: usize,
    visited_walls: usize,
    updated_walls: usize,
    clamped_walls: usize,
) -> String {
    if visited_walls == 0 {
        return "No selected tiles have walls to autotile".to_string();
    }
    if updated_walls == 0 {
        return format!("Selected walls already autotiled across {selected_tiles} tile(s)");
    }
    let mut status =
        format!("Autotiled {updated_walls} wall(s) across {selected_tiles} selected tile(s)");
    if clamped_walls > 0 {
        status.push_str(&format!("; {clamped_walls} V span(s) clamped"));
    }
    status
}

pub(crate) fn face_edges(face: FaceRef) -> Vec<EdgeRef> {
    match face.kind {
        FaceKind::Floor => floor_edges(face.room, face.sx, face.sz, false),
        FaceKind::Ceiling => floor_edges(face.room, face.sx, face.sz, true),
        FaceKind::Wall { dir, stack } => [
            WallEdge::Bottom,
            WallEdge::Right,
            WallEdge::Top,
            WallEdge::Left,
        ]
        .into_iter()
        .map(|edge| EdgeRef {
            room: face.room,
            anchor: EdgeAnchor::Wall {
                sx: face.sx,
                sz: face.sz,
                dir,
                stack,
                edge,
            },
        })
        .collect(),
    }
}

pub(crate) fn triangle_edges(triangle: HorizontalTriangleRef) -> Vec<EdgeRef> {
    let mut edges = Vec::new();
    for idx in 0..3 {
        let a = triangle.corners[idx];
        let b = triangle.corners[(idx + 1) % 3];
        let Some(dir) = horizontal_edge_dir_from_corners(a, b) else {
            continue;
        };
        let anchor = match triangle.surface {
            HorizontalSurfaceKind::Floor => EdgeAnchor::Floor {
                sx: triangle.sx,
                sz: triangle.sz,
                dir,
            },
            HorizontalSurfaceKind::Ceiling => EdgeAnchor::Ceiling {
                sx: triangle.sx,
                sz: triangle.sz,
                dir,
            },
        };
        edges.push(EdgeRef {
            room: triangle.room,
            anchor,
        });
    }
    edges
}

pub(crate) fn floor_edges(room: NodeId, sx: u16, sz: u16, ceiling: bool) -> Vec<EdgeRef> {
    [
        GridDirection::North,
        GridDirection::East,
        GridDirection::South,
        GridDirection::West,
    ]
    .into_iter()
    .map(|dir| EdgeRef {
        room,
        anchor: if ceiling {
            EdgeAnchor::Ceiling { sx, sz, dir }
        } else {
            EdgeAnchor::Floor { sx, sz, dir }
        },
    })
    .collect()
}

pub(crate) fn face_vertices(face: FaceRef) -> Vec<VertexRef> {
    match face.kind {
        FaceKind::Floor => floor_vertices(face.room, face.sx, face.sz, false),
        FaceKind::Ceiling => floor_vertices(face.room, face.sx, face.sz, true),
        FaceKind::Wall { dir, stack } => [
            WallCorner::BL,
            WallCorner::BR,
            WallCorner::TR,
            WallCorner::TL,
        ]
        .into_iter()
        .map(|corner| VertexRef {
            room: face.room,
            anchor: VertexAnchor::Wall {
                sx: face.sx,
                sz: face.sz,
                dir,
                stack,
                corner,
            },
        })
        .collect(),
    }
}

pub(crate) fn triangle_vertices(triangle: HorizontalTriangleRef) -> Vec<VertexRef> {
    triangle
        .corners
        .into_iter()
        .map(|corner| {
            let anchor = match triangle.surface {
                HorizontalSurfaceKind::Floor => VertexAnchor::Floor {
                    sx: triangle.sx,
                    sz: triangle.sz,
                    corner,
                },
                HorizontalSurfaceKind::Ceiling => VertexAnchor::Ceiling {
                    sx: triangle.sx,
                    sz: triangle.sz,
                    corner,
                },
            };
            VertexRef {
                room: triangle.room,
                anchor,
            }
        })
        .collect()
}

pub(crate) fn floor_vertices(room: NodeId, sx: u16, sz: u16, ceiling: bool) -> Vec<VertexRef> {
    [Corner::NW, Corner::NE, Corner::SE, Corner::SW]
        .into_iter()
        .map(|corner| VertexRef {
            room,
            anchor: if ceiling {
                VertexAnchor::Ceiling { sx, sz, corner }
            } else {
                VertexAnchor::Floor { sx, sz, corner }
            },
        })
        .collect()
}

/// Adapter: face → its first edge (north for floor / ceiling,
/// bottom for walls). Used by mode-switch logic so a face
/// selection naturally promotes into one of its edges.
pub(crate) fn face_first_edge(face: FaceRef) -> EdgeRef {
    let anchor = match face.kind {
        FaceKind::Floor => EdgeAnchor::Floor {
            sx: face.sx,
            sz: face.sz,
            dir: GridDirection::North,
        },
        FaceKind::Ceiling => EdgeAnchor::Ceiling {
            sx: face.sx,
            sz: face.sz,
            dir: GridDirection::North,
        },
        FaceKind::Wall { dir, stack } => EdgeAnchor::Wall {
            sx: face.sx,
            sz: face.sz,
            dir,
            stack,
            edge: WallEdge::Bottom,
        },
    };
    EdgeRef {
        room: face.room,
        anchor,
    }
}

pub(crate) fn triangle_first_edge(triangle: HorizontalTriangleRef) -> EdgeRef {
    triangle_edges(triangle)
        .into_iter()
        .next()
        .unwrap_or_else(|| face_first_edge(triangle.parent_face()))
}

/// Adapter: face → its first vertex (NW for floor / ceiling,
/// BL for walls).
pub(crate) fn face_first_vertex(face: FaceRef) -> VertexRef {
    let anchor = match face.kind {
        FaceKind::Floor => VertexAnchor::Floor {
            sx: face.sx,
            sz: face.sz,
            corner: Corner::NW,
        },
        FaceKind::Ceiling => VertexAnchor::Ceiling {
            sx: face.sx,
            sz: face.sz,
            corner: Corner::NW,
        },
        FaceKind::Wall { dir, stack } => VertexAnchor::Wall {
            sx: face.sx,
            sz: face.sz,
            dir,
            stack,
            corner: WallCorner::BL,
        },
    };
    VertexRef {
        room: face.room,
        anchor,
    }
}

pub(crate) fn triangle_first_vertex(triangle: HorizontalTriangleRef) -> VertexRef {
    triangle_vertices(triangle)
        .into_iter()
        .next()
        .unwrap_or_else(|| face_first_vertex(triangle.parent_face()))
}

/// Adapter: edge → the face that owns it. Used when the user
/// switches from Edge mode back to Face mode and we don't want
/// to lose context.
pub(crate) fn edge_owning_face_ref(edge: EdgeRef) -> Option<FaceRef> {
    let kind = match edge.anchor {
        EdgeAnchor::Floor { .. } => FaceKind::Floor,
        EdgeAnchor::Ceiling { .. } => FaceKind::Ceiling,
        EdgeAnchor::Wall { dir, stack, .. } => FaceKind::Wall { dir, stack },
    };
    let (sx, sz) = match edge.anchor {
        EdgeAnchor::Floor { sx, sz, .. }
        | EdgeAnchor::Ceiling { sx, sz, .. }
        | EdgeAnchor::Wall { sx, sz, .. } => (sx, sz),
    };
    Some(FaceRef {
        room: edge.room,
        sx,
        sz,
        kind,
    })
}

/// Adapter: edge → its first endpoint vertex. Edge perimeter
/// convention: floor / ceiling north = NW-NE, east = NE-SE,
/// south = SE-SW, west = SW-NW. Wall bottom = BL-BR, right =
/// BR-TR, top = TR-TL, left = TL-BL. The "first" vertex is
/// the leading corner of that walk.
pub(crate) fn edge_first_vertex(edge: EdgeRef) -> VertexRef {
    let anchor = match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => VertexAnchor::Floor {
            sx,
            sz,
            corner: edge_first_floor_corner(dir),
        },
        EdgeAnchor::Ceiling { sx, sz, dir } => VertexAnchor::Ceiling {
            sx,
            sz,
            corner: edge_first_floor_corner(dir),
        },
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge,
        } => VertexAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            corner: edge_first_wall_corner(edge),
        },
    };
    VertexRef {
        room: edge.room,
        anchor,
    }
}

pub(crate) const fn edge_first_floor_corner(dir: GridDirection) -> Corner {
    match dir {
        GridDirection::North => Corner::NW,
        GridDirection::East => Corner::NE,
        GridDirection::South => Corner::SE,
        GridDirection::West => Corner::SW,
        GridDirection::NorthWestSouthEast => Corner::NW,
        GridDirection::NorthEastSouthWest => Corner::NE,
    }
}

pub(crate) const fn edge_first_wall_corner(edge: WallEdge) -> WallCorner {
    match edge {
        WallEdge::Bottom => WallCorner::BL,
        WallEdge::Right => WallCorner::BR,
        WallEdge::Top => WallCorner::TR,
        WallEdge::Left => WallCorner::TL,
    }
}

/// Adapter: vertex → owning face.
pub(crate) fn vertex_owning_face_ref(vertex: VertexRef) -> Option<FaceRef> {
    let kind = match vertex.anchor {
        VertexAnchor::Floor { .. } => FaceKind::Floor,
        VertexAnchor::Ceiling { .. } => FaceKind::Ceiling,
        VertexAnchor::Wall { dir, stack, .. } => FaceKind::Wall { dir, stack },
    };
    let (sx, sz) = match vertex.anchor {
        VertexAnchor::Floor { sx, sz, .. }
        | VertexAnchor::Ceiling { sx, sz, .. }
        | VertexAnchor::Wall { sx, sz, .. } => (sx, sz),
    };
    Some(FaceRef {
        room: vertex.room,
        sx,
        sz,
        kind,
    })
}

/// Adapter: vertex → one of the two edges it sits on. Picks
/// the first walking the perimeter from this corner.
pub(crate) fn vertex_first_edge(vertex: VertexRef) -> EdgeRef {
    let anchor = match vertex.anchor {
        VertexAnchor::Floor { sx, sz, corner } => EdgeAnchor::Floor {
            sx,
            sz,
            dir: floor_corner_first_edge(corner),
        },
        VertexAnchor::Ceiling { sx, sz, corner } => EdgeAnchor::Ceiling {
            sx,
            sz,
            dir: floor_corner_first_edge(corner),
        },
        VertexAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            corner,
        } => EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge: wall_corner_first_edge(corner),
        },
    };
    EdgeRef {
        room: vertex.room,
        anchor,
    }
}

pub(crate) const fn floor_corner_first_edge(corner: Corner) -> GridDirection {
    match corner {
        Corner::NW => GridDirection::North,
        Corner::NE => GridDirection::East,
        Corner::SE => GridDirection::South,
        Corner::SW => GridDirection::West,
    }
}

pub(crate) const fn wall_corner_first_edge(corner: WallCorner) -> WallEdge {
    match corner {
        WallCorner::BL => WallEdge::Bottom,
        WallCorner::BR => WallEdge::Right,
        WallCorner::TR => WallEdge::Top,
        WallCorner::TL => WallEdge::Left,
    }
}

pub(crate) const fn corner_label(corner: Corner) -> &'static str {
    match corner {
        Corner::NW => "NW",
        Corner::NE => "NE",
        Corner::SE => "SE",
        Corner::SW => "SW",
    }
}

pub(crate) const fn wall_corner_label(corner: WallCorner) -> &'static str {
    match corner {
        WallCorner::BL => "bottom-left",
        WallCorner::BR => "bottom-right",
        WallCorner::TR => "top-right",
        WallCorner::TL => "top-left",
    }
}

pub(crate) const fn wall_edge_label(edge: WallEdge) -> &'static str {
    match edge {
        WallEdge::Bottom => "bottom",
        WallEdge::Right => "right",
        WallEdge::Top => "top",
        WallEdge::Left => "left",
    }
}

pub(crate) fn direction_label(dir: GridDirection) -> &'static str {
    match dir {
        GridDirection::North => "North",
        GridDirection::East => "East",
        GridDirection::South => "South",
        GridDirection::West => "West",
        GridDirection::NorthWestSouthEast => "NW-SE",
        GridDirection::NorthEastSouthWest => "NE-SW",
    }
}

pub(crate) const fn next_cardinal_direction(dir: GridDirection) -> GridDirection {
    match dir {
        GridDirection::North => GridDirection::East,
        GridDirection::East => GridDirection::South,
        GridDirection::South => GridDirection::West,
        GridDirection::West
        | GridDirection::NorthWestSouthEast
        | GridDirection::NorthEastSouthWest => GridDirection::North,
    }
}

pub(crate) fn portal_edge_midpoint_editor(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    dir: GridDirection,
) -> [f32; 2] {
    let wcx = grid.origin[0] + sx as i32;
    let wcz = grid.origin[1] + sz as i32;
    let world = match dir {
        GridDirection::North => [wcx as f32 + 0.5, wcz as f32 + 1.0],
        GridDirection::East => [wcx as f32 + 1.0, wcz as f32 + 0.5],
        GridDirection::South => [wcx as f32 + 0.5, wcz as f32],
        GridDirection::West => [wcx as f32, wcz as f32 + 0.5],
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => {
            [wcx as f32 + 0.5, wcz as f32 + 0.5]
        }
    };
    grid.world_cells_to_editor(world)
}

pub(crate) fn portal_edge_valid_for_world_cell(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
) -> bool {
    let Some((sx, sz)) = grid.world_cell_to_array(wcx, wcz) else {
        return false;
    };
    portal_edge_valid_for_array_cell(grid, sx, sz, dir)
}

pub(crate) fn portal_edge_valid_for_array_cell(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    dir: GridDirection,
) -> bool {
    let Some((nx, nz)) = portal_edge_neighbour(sx, sz, dir) else {
        return false;
    };
    nx < grid.width
        && nz < grid.depth
        && grid.sector(sx, sz).is_some_and(GridSector::has_geometry)
        && grid.sector(nx, nz).is_some_and(GridSector::has_geometry)
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

pub(crate) fn portal_edge_neighbour(sx: u16, sz: u16, dir: GridDirection) -> Option<(u16, u16)> {
    match dir {
        GridDirection::North => Some((sx, sz.checked_add(1)?)),
        GridDirection::East => Some((sx.checked_add(1)?, sz)),
        GridDirection::South => Some((sx, sz.checked_sub(1)?)),
        GridDirection::West => Some((sx.checked_sub(1)?, sz)),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => None,
    }
}

/// Pick the cardinal `GridDirection` for a wall edge given the
/// click offset from the cell's world-space centre. Mirrors the
/// renderer's `WallEdge` mapping in `editor_preview`:
/// `North = +Z`, `East = +X`, `South = -Z`, `West = -X`. The
/// dominant axis decides; ties favour the X axis.
pub(crate) fn edge_from_world_offset(dx: f32, dz: f32) -> GridDirection {
    psxed_project::spatial::editor_wall_direction_from_offset(dx, dz)
}

/// World-space integer position of `corner` in the room
/// described by `grid`. Returns `None` if the addressed face
/// no longer exists (cell out of bounds, geometry missing).
pub fn face_corner_world(grid: &WorldGrid, corner: FaceCornerRef) -> Option<[i32; 3]> {
    match corner {
        FaceCornerRef::Floor { sx, sz, corner } => {
            if sx >= grid.width || sz >= grid.depth {
                return None;
            }
            let face = grid.sector(sx, sz)?.floor.as_ref()?;
            Some(floor_corner_world(grid, sx, sz, corner, face.heights))
        }
        FaceCornerRef::FloorTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => {
            if sx >= grid.width || sz >= grid.depth {
                return None;
            }
            let face = grid.sector(sx, sz)?.floor.as_ref()?;
            triangle_corner_world(grid, sx, sz, face, triangle, corner)
        }
        FaceCornerRef::Ceiling { sx, sz, corner } => {
            if sx >= grid.width || sz >= grid.depth {
                return None;
            }
            let face = grid.sector(sx, sz)?.ceiling.as_ref()?;
            Some(floor_corner_world(grid, sx, sz, corner, face.heights))
        }
        FaceCornerRef::CeilingTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => {
            if sx >= grid.width || sz >= grid.depth {
                return None;
            }
            let face = grid.sector(sx, sz)?.ceiling.as_ref()?;
            triangle_corner_world(grid, sx, sz, face, triangle, corner)
        }
        FaceCornerRef::Wall {
            sx,
            sz,
            dir,
            stack,
            corner,
        } => {
            if sx >= grid.width || sz >= grid.depth {
                return None;
            }
            let walls = grid.sector(sx, sz)?.walls.get(dir);
            let wall = walls.get(stack as usize)?;
            wall_corner_world(grid, sx, sz, dir, corner, wall.heights)
        }
    }
}

pub(crate) fn triangle_corner_world(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    face: &GridHorizontalFace,
    triangle: HorizontalTriangleIndex,
    corner: Corner,
) -> Option<[i32; 3]> {
    let corners = horizontal_triangle_corners(face.split, triangle);
    let slot = corners.iter().position(|candidate| *candidate == corner)?;
    let heights = face.triangle_heights(triangle.idx());
    let [x, z] = grid.cell_bounds_world(sx, sz).horizontal_corner_xz(corner);
    Some([x, heights[slot], z])
}

pub(crate) fn floor_corner_world(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    corner: Corner,
    heights: [i32; 4],
) -> [i32; 3] {
    let [x, z] = grid.cell_bounds_world(sx, sz).horizontal_corner_xz(corner);
    [x, heights[corner.idx()], z]
}

pub(crate) fn horizontal_face_world_corners(
    bounds: GridCellBounds,
    heights: [i32; 4],
) -> [[f32; 3]; 4] {
    let nw = bounds.horizontal_corner_xz(Corner::NW);
    let ne = bounds.horizontal_corner_xz(Corner::NE);
    let se = bounds.horizontal_corner_xz(Corner::SE);
    let sw = bounds.horizontal_corner_xz(Corner::SW);
    [
        [nw[0] as f32, heights[Corner::NW.idx()] as f32, nw[1] as f32],
        [ne[0] as f32, heights[Corner::NE.idx()] as f32, ne[1] as f32],
        [se[0] as f32, heights[Corner::SE.idx()] as f32, se[1] as f32],
        [sw[0] as f32, heights[Corner::SW.idx()] as f32, sw[1] as f32],
    ]
}

pub(crate) fn horizontal_triangle_world_corners(
    bounds: GridCellBounds,
    corners: [Corner; 3],
    heights: [i32; 3],
) -> [[f32; 3]; 3] {
    let point = |corner: Corner, height: i32| {
        let [x, z] = bounds.horizontal_corner_xz(corner);
        [x as f32, height as f32, z as f32]
    };
    [
        point(corners[0], heights[0]),
        point(corners[1], heights[1]),
        point(corners[2], heights[2]),
    ]
}

pub(crate) fn wall_corner_world(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    dir: GridDirection,
    corner: WallCorner,
    heights: [i32; 4],
) -> Option<[i32; 3]> {
    let (bl, br) = grid.cell_bounds_world(sx, sz).wall_endpoints_xz(dir)?;
    let [x, z] = match corner {
        // BL / TL share the BL endpoint; BR / TR share BR.
        WallCorner::BL | WallCorner::TL => bl,
        WallCorner::BR | WallCorner::TR => br,
    };
    Some([x, heights[corner.idx()], z])
}

pub(crate) fn wall_face_world_corners(
    bounds: GridCellBounds,
    dir: GridDirection,
    heights: [i32; 4],
) -> Option<[[f32; 3]; 4]> {
    let (bl, br) = bounds.wall_endpoints_xz(dir)?;
    Some([
        [
            bl[0] as f32,
            heights[WallCorner::BL.idx()] as f32,
            bl[1] as f32,
        ],
        [
            br[0] as f32,
            heights[WallCorner::BR.idx()] as f32,
            br[1] as f32,
        ],
        [
            br[0] as f32,
            heights[WallCorner::TR.idx()] as f32,
            br[1] as f32,
        ],
        [
            bl[0] as f32,
            heights[WallCorner::TL.idx()] as f32,
            bl[1] as f32,
        ],
    ])
}

/// Universal coincidence resolver. Returns the physical vertex
/// containing `seed` -- every face-corner whose current world
/// position equals the seed's world position. Walks every
/// floor / ceiling / wall corner in the grid (`O(faces × 4)`,
/// runs in microseconds for 32×32 rooms).
pub(crate) fn physical_vertex(grid: &WorldGrid, seed: FaceCornerRef) -> Option<PhysicalVertex> {
    let world = face_corner_world(grid, seed)?;
    let mut members = Vec::new();
    for sx in 0..grid.width {
        for sz in 0..grid.depth {
            let Some(sector) = grid.sector(sx, sz) else {
                continue;
            };
            if sector.floor.is_some() {
                for c in [Corner::NW, Corner::NE, Corner::SE, Corner::SW] {
                    let r = FaceCornerRef::Floor { sx, sz, corner: c };
                    if face_corner_world(grid, r) == Some(world) {
                        members.push(r);
                    }
                }
            }
            if sector.ceiling.is_some() {
                for c in [Corner::NW, Corner::NE, Corner::SE, Corner::SW] {
                    let r = FaceCornerRef::Ceiling { sx, sz, corner: c };
                    if face_corner_world(grid, r) == Some(world) {
                        members.push(r);
                    }
                }
            }
            for dir in GridDirection::ALL {
                for (stack_idx, _) in sector.walls.get(dir).iter().enumerate() {
                    for c in [
                        WallCorner::BL,
                        WallCorner::BR,
                        WallCorner::TR,
                        WallCorner::TL,
                    ] {
                        let r = FaceCornerRef::Wall {
                            sx,
                            sz,
                            dir,
                            stack: stack_idx as u8,
                            corner: c,
                        };
                        if face_corner_world(grid, r) == Some(world) {
                            members.push(r);
                        }
                    }
                }
            }
        }
    }
    Some(PhysicalVertex { world, members })
}

pub(crate) fn detached_vertex(grid: &WorldGrid, seed: FaceCornerRef) -> Option<PhysicalVertex> {
    Some(PhysicalVertex {
        world: face_corner_world(grid, seed)?,
        members: vec![seed],
    })
}

pub(crate) fn vertex_for_seed(
    grid: &WorldGrid,
    seed: FaceCornerRef,
    connectivity: VertexConnectivity,
) -> Option<PhysicalVertex> {
    if matches!(
        seed,
        FaceCornerRef::FloorTriangle { .. } | FaceCornerRef::CeilingTriangle { .. }
    ) {
        return detached_vertex(grid, seed);
    }
    match connectivity {
        VertexConnectivity::Welded => physical_vertex(grid, seed),
        VertexConnectivity::Detached => detached_vertex(grid, seed),
    }
}

/// Face-corner seeds for a drag-translate stroke. The drag
/// engine resolves each seed through the active vertex
/// connectivity mode before applying height edits.
///
/// - Face: 4 corners of the face (preserves slope; same Δ on
///   each).
/// - Edge: 2 endpoint corners.
/// - Vertex: 1 corner.
pub(crate) fn drag_corner_seeds(selection: Selection) -> Option<Vec<FaceCornerRef>> {
    Some(match selection {
        Selection::Face(face) => match face.kind {
            FaceKind::Floor => [Corner::NW, Corner::NE, Corner::SE, Corner::SW]
                .iter()
                .map(|c| FaceCornerRef::Floor {
                    sx: face.sx,
                    sz: face.sz,
                    corner: *c,
                })
                .collect(),
            FaceKind::Ceiling => [Corner::NW, Corner::NE, Corner::SE, Corner::SW]
                .iter()
                .map(|c| FaceCornerRef::Ceiling {
                    sx: face.sx,
                    sz: face.sz,
                    corner: *c,
                })
                .collect(),
            FaceKind::Wall { dir, stack } => [
                WallCorner::BL,
                WallCorner::BR,
                WallCorner::TR,
                WallCorner::TL,
            ]
            .iter()
            .map(|c| FaceCornerRef::Wall {
                sx: face.sx,
                sz: face.sz,
                dir,
                stack,
                corner: *c,
            })
            .collect(),
        },
        Selection::Triangle(triangle) => triangle
            .corners
            .into_iter()
            .map(|corner| match triangle.surface {
                HorizontalSurfaceKind::Floor => FaceCornerRef::FloorTriangle {
                    sx: triangle.sx,
                    sz: triangle.sz,
                    triangle: triangle.index,
                    corner,
                },
                HorizontalSurfaceKind::Ceiling => FaceCornerRef::CeilingTriangle {
                    sx: triangle.sx,
                    sz: triangle.sz,
                    triangle: triangle.index,
                    corner,
                },
            })
            .collect(),
        Selection::Edge(edge) => {
            let (a, b) = edge_endpoint_corners(edge);
            vec![a, b]
        }
        Selection::Vertex(vertex) => vec![vertex.anchor.as_face_corner()],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EdgePathKind {
    Floor,
    Ceiling,
    Wall { stack: u8, edge: WallEdge },
}

pub(crate) fn edge_path_kind(edge: EdgeRef) -> EdgePathKind {
    match edge.anchor {
        EdgeAnchor::Floor { .. } => EdgePathKind::Floor,
        EdgeAnchor::Ceiling { .. } => EdgePathKind::Ceiling,
        EdgeAnchor::Wall { stack, edge, .. } => EdgePathKind::Wall { stack, edge },
    }
}

pub(crate) fn edge_world_segment(grid: &WorldGrid, edge: EdgeRef) -> Option<([i32; 3], [i32; 3])> {
    let (a, b) = edge_endpoint_corners(edge);
    Some((face_corner_world(grid, a)?, face_corner_world(grid, b)?))
}

pub(crate) fn edge_segments_touch(a: ([i32; 3], [i32; 3]), b: ([i32; 3], [i32; 3])) -> bool {
    a.0 == b.0 || a.0 == b.1 || a.1 == b.0 || a.1 == b.1
}

pub(crate) fn edge_endpoints_with_connectivity(
    grid: &WorldGrid,
    edge: EdgeRef,
    connectivity: VertexConnectivity,
) -> Option<(PhysicalVertex, PhysicalVertex)> {
    let (a, b) = edge_endpoint_corners(edge);
    let pa = vertex_for_seed(grid, a, connectivity)?;
    let pb = vertex_for_seed(grid, b, connectivity)?;
    Some((pa, pb))
}

/// Endpoint face-corners of `edge` as `(start, end)`. Order
/// matches the perimeter walk used elsewhere -- north = NW→NE,
/// east = NE→SE, etc.
pub(crate) fn edge_endpoint_corners(edge: EdgeRef) -> (FaceCornerRef, FaceCornerRef) {
    match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => {
            let (ca, cb) = floor_edge_endpoints(dir);
            (
                FaceCornerRef::Floor { sx, sz, corner: ca },
                FaceCornerRef::Floor { sx, sz, corner: cb },
            )
        }
        EdgeAnchor::Ceiling { sx, sz, dir } => {
            let (ca, cb) = floor_edge_endpoints(dir);
            (
                FaceCornerRef::Ceiling { sx, sz, corner: ca },
                FaceCornerRef::Ceiling { sx, sz, corner: cb },
            )
        }
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge,
        } => {
            let (ca, cb) = wall_edge_endpoints(edge);
            (
                FaceCornerRef::Wall {
                    sx,
                    sz,
                    dir,
                    stack,
                    corner: ca,
                },
                FaceCornerRef::Wall {
                    sx,
                    sz,
                    dir,
                    stack,
                    corner: cb,
                },
            )
        }
    }
}

pub(crate) const fn floor_edge_endpoints(dir: GridDirection) -> (Corner, Corner) {
    match dir {
        GridDirection::North => (Corner::NW, Corner::NE),
        GridDirection::East => (Corner::NE, Corner::SE),
        GridDirection::South => (Corner::SE, Corner::SW),
        GridDirection::West => (Corner::SW, Corner::NW),
        GridDirection::NorthWestSouthEast => (Corner::NW, Corner::SE),
        GridDirection::NorthEastSouthWest => (Corner::NE, Corner::SW),
    }
}

pub(crate) fn horizontal_edge_dir_from_corners(a: Corner, b: Corner) -> Option<GridDirection> {
    match (a, b) {
        (Corner::NW, Corner::NE) | (Corner::NE, Corner::NW) => Some(GridDirection::North),
        (Corner::NE, Corner::SE) | (Corner::SE, Corner::NE) => Some(GridDirection::East),
        (Corner::SE, Corner::SW) | (Corner::SW, Corner::SE) => Some(GridDirection::South),
        (Corner::SW, Corner::NW) | (Corner::NW, Corner::SW) => Some(GridDirection::West),
        (Corner::NW, Corner::SE) | (Corner::SE, Corner::NW) => {
            Some(GridDirection::NorthWestSouthEast)
        }
        (Corner::NE, Corner::SW) | (Corner::SW, Corner::NE) => {
            Some(GridDirection::NorthEastSouthWest)
        }
        _ => None,
    }
}

pub(crate) const fn wall_edge_endpoints(edge: WallEdge) -> (WallCorner, WallCorner) {
    match edge {
        WallEdge::Bottom => (WallCorner::BL, WallCorner::BR),
        WallEdge::Right => (WallCorner::BR, WallCorner::TR),
        WallEdge::Top => (WallCorner::TR, WallCorner::TL),
        WallEdge::Left => (WallCorner::TL, WallCorner::BL),
    }
}

/// Inspector member-list label.
pub(crate) fn face_corner_label(corner: FaceCornerRef) -> String {
    match corner {
        FaceCornerRef::Floor { sx, sz, corner } => {
            format!("Floor ({sx},{sz}) {}", corner_label(corner))
        }
        FaceCornerRef::FloorTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => format!(
            "Floor triangle {} ({sx},{sz}) {}",
            triangle.label(),
            corner_label(corner)
        ),
        FaceCornerRef::Ceiling { sx, sz, corner } => {
            format!("Ceiling ({sx},{sz}) {}", corner_label(corner))
        }
        FaceCornerRef::CeilingTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => format!(
            "Ceiling triangle {} ({sx},{sz}) {}",
            triangle.label(),
            corner_label(corner)
        ),
        FaceCornerRef::Wall {
            sx,
            sz,
            dir,
            stack,
            corner,
        } => format!(
            "{} wall #{stack} ({sx},{sz}) {}",
            direction_label(dir),
            wall_corner_label(corner)
        ),
    }
}

/// Apply a new Y to every member of `vertex`. X / Z are
/// preserved by construction -- `face_corner_world` returns the
/// current `(X, Y, Z)` and we only ever rewrite the corner's
/// height array entry.
pub(crate) fn apply_vertex_height(grid: &mut WorldGrid, vertex: &PhysicalVertex, new_y: i32) {
    let new_y = snap_height(new_y);
    for member in &vertex.members {
        write_face_corner_height(grid, *member, new_y);
    }
}

pub(crate) fn write_face_corner_height(grid: &mut WorldGrid, corner: FaceCornerRef, new_y: i32) {
    match corner {
        FaceCornerRef::Floor { sx, sz, corner } => {
            if let Some(sector) = grid.sector_mut(sx, sz) {
                if let Some(face) = sector.floor.as_mut() {
                    face.heights[corner.idx()] = new_y;
                }
            }
        }
        FaceCornerRef::FloorTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => {
            if let Some(sector) = grid.sector_mut(sx, sz) {
                if let Some(face) = sector.floor.as_mut() {
                    write_triangle_corner_height(face, triangle, corner, new_y);
                }
            }
        }
        FaceCornerRef::Ceiling { sx, sz, corner } => {
            if let Some(sector) = grid.sector_mut(sx, sz) {
                if let Some(face) = sector.ceiling.as_mut() {
                    face.heights[corner.idx()] = new_y;
                }
            }
        }
        FaceCornerRef::CeilingTriangle {
            sx,
            sz,
            triangle,
            corner,
        } => {
            if let Some(sector) = grid.sector_mut(sx, sz) {
                if let Some(face) = sector.ceiling.as_mut() {
                    write_triangle_corner_height(face, triangle, corner, new_y);
                }
            }
        }
        FaceCornerRef::Wall {
            sx,
            sz,
            dir,
            stack,
            corner,
        } => {
            if let Some(sector) = grid.sector_mut(sx, sz) {
                if let Some(wall) = sector.walls.get_mut(dir).get_mut(stack as usize) {
                    wall.heights[corner.idx()] = new_y;
                }
            }
        }
    }
}

pub(crate) fn write_triangle_corner_height(
    face: &mut GridHorizontalFace,
    triangle: HorizontalTriangleIndex,
    corner: Corner,
    new_y: i32,
) {
    let corners = horizontal_triangle_corners(face.split, triangle);
    let Some(slot) = corners.iter().position(|candidate| *candidate == corner) else {
        return;
    };
    face.triangle_heights_mut(triangle.idx())[slot] = new_y;
}

pub(crate) fn selection_copy_face(selection: Selection) -> Option<FaceRef> {
    selection.as_face().or_else(|| match selection {
        Selection::Edge(edge) => edge_owning_face_ref(edge),
        Selection::Vertex(vertex) => vertex_owning_face_ref(vertex),
        Selection::Face(_) | Selection::Triangle(_) => None,
    })
}

pub(crate) fn sector_fragment_for_selection(
    grid: &WorldGrid,
    selection: Selection,
) -> Option<GridSector> {
    let face = selection_copy_face(selection)?;
    let sector = grid.sector(face.sx, face.sz)?;
    let mut fragment = GridSector::empty();
    match face.kind {
        FaceKind::Floor => {
            fragment.floor = sector.floor.clone();
        }
        FaceKind::Ceiling => {
            fragment.ceiling = sector.ceiling.clone();
        }
        FaceKind::Wall { dir, stack } => {
            let wall = sector.walls.get(dir).get(stack as usize)?;
            fragment.walls.get_mut(dir).push(wall.clone());
        }
    }
    fragment.has_geometry().then_some(fragment)
}

pub(crate) fn merge_clipboard_fragment(target: &mut GridSector, fragment: GridSector) {
    if fragment.floor.is_some() {
        target.floor = fragment.floor;
    }
    if fragment.ceiling.is_some() {
        target.ceiling = fragment.ceiling;
    }
    for direction in GridDirection::ALL {
        for wall in fragment.walls.get(direction) {
            target.walls.get_mut(direction).push(wall.clone());
        }
    }
}

pub(crate) fn merge_primitive_fragment(
    target: &mut GridSector,
    fragment: GridSector,
    room: NodeId,
    sx: u16,
    sz: u16,
    selected: &mut Vec<Selection>,
) {
    if let Some(floor) = fragment.floor {
        target.floor = Some(floor);
        push_unique_selection(
            selected,
            Selection::Face(FaceRef {
                room,
                sx,
                sz,
                kind: FaceKind::Floor,
            }),
        );
    }
    if let Some(ceiling) = fragment.ceiling {
        target.ceiling = Some(ceiling);
        push_unique_selection(
            selected,
            Selection::Face(FaceRef {
                room,
                sx,
                sz,
                kind: FaceKind::Ceiling,
            }),
        );
    }
    for direction in GridDirection::ALL {
        for wall in fragment.walls.get(direction) {
            let Ok(stack) = u8::try_from(target.walls.get(direction).len()) else {
                continue;
            };
            target.walls.get_mut(direction).push(wall.clone());
            push_unique_selection(
                selected,
                Selection::Face(FaceRef {
                    room,
                    sx,
                    sz,
                    kind: FaceKind::Wall {
                        dir: direction,
                        stack,
                    },
                }),
            );
        }
    }
}

pub(crate) fn remove_primitive_faces_from_project(
    project: &mut ProjectDocument,
    targets: &[Selection],
    active_floor: usize,
) {
    let mut faces = Vec::new();
    for &target in targets {
        let Some(face) = selection_copy_face(target) else {
            continue;
        };
        if !faces.contains(&face) {
            faces.push(face);
        }
    }
    faces.sort_by_key(primitive_remove_sort_key);
    for face in faces {
        let Some(node) = project.active_scene_mut().node_mut(face.room) else {
            continue;
        };
        let NodeKind::Section { grid } = &mut node.kind else {
            continue;
        };
        let idx = active_floor.min(grid.floor_count().saturating_sub(1));
        let Some(grid) = grid.floor_mut(idx) else {
            continue;
        };
        remove_face_from_grid(grid, face);
    }
}

pub(crate) fn primitive_remove_sort_key(face: &FaceRef) -> (u64, u16, u16, u8, u8, u8) {
    let (kind, direction, stack) = match face.kind {
        FaceKind::Floor => (0, 0, 0),
        FaceKind::Ceiling => (1, 0, 0),
        FaceKind::Wall { dir, stack } => (2, direction_sort_index(dir), u8::MAX - stack),
    };
    (face.room.raw(), face.sx, face.sz, kind, direction, stack)
}

pub(crate) const fn direction_sort_index(direction: GridDirection) -> u8 {
    match direction {
        GridDirection::North => 0,
        GridDirection::East => 1,
        GridDirection::South => 2,
        GridDirection::West => 3,
        GridDirection::NorthWestSouthEast => 4,
        GridDirection::NorthEastSouthWest => 5,
    }
}

pub(crate) fn remove_face_from_grid(grid: &mut WorldGrid, face: FaceRef) {
    let Some(sector) = grid.sector_mut(face.sx, face.sz) else {
        return;
    };
    match face.kind {
        FaceKind::Floor => {
            sector.floor = None;
        }
        FaceKind::Ceiling => {
            sector.ceiling = None;
        }
        FaceKind::Wall { dir, stack } => {
            let walls = sector.walls.get_mut(dir);
            let index = stack as usize;
            if index < walls.len() {
                walls.remove(index);
            }
        }
    }
}

pub(crate) fn transformed_geometry_cells(
    cells: &[GeometryClipboardCell],
    width: i32,
    height: i32,
    rotation_quarters: u8,
    flip_x: bool,
    flip_z: bool,
) -> Vec<([i32; 2], Option<GridSector>)> {
    let (rotated_width, rotated_height) =
        rotated_geometry_dimensions(width, height, rotation_quarters);
    cells
        .iter()
        .map(|cell| {
            let mut offset = rotate_cell_offset_cw(cell.offset, width, height, rotation_quarters);
            let mut sector = cell.sector.clone();
            if let Some(sector) = sector.as_mut() {
                for _ in 0..rotation_quarters % 4 {
                    *sector = rotate_sector_cw(sector);
                }
                if flip_x {
                    *sector = flip_sector_x(sector);
                }
                if flip_z {
                    *sector = flip_sector_z(sector);
                }
            }
            if flip_x {
                offset[0] = rotated_width - 1 - offset[0];
            }
            if flip_z {
                offset[1] = rotated_height - 1 - offset[1];
            }
            (offset, sector)
        })
        .collect()
}

pub(crate) fn rotated_geometry_dimensions(
    mut width: i32,
    mut height: i32,
    rotation_quarters: u8,
) -> (i32, i32) {
    if rotation_quarters % 2 == 1 {
        std::mem::swap(&mut width, &mut height);
    }
    (width, height)
}

pub(crate) fn rotate_cell_offset_cw(
    mut offset: [i32; 2],
    mut width: i32,
    mut height: i32,
    rotation_quarters: u8,
) -> [i32; 2] {
    for _ in 0..rotation_quarters % 4 {
        offset = [offset[1], width - 1 - offset[0]];
        std::mem::swap(&mut width, &mut height);
    }
    offset
}

pub(crate) fn rotate_sector_cw(sector: &GridSector) -> GridSector {
    let mut rotated = GridSector::empty();
    rotated.floor = sector.floor.clone().map(rotate_horizontal_face_cw);
    rotated.ceiling = sector.ceiling.clone().map(rotate_horizontal_face_cw);
    rotated.floor_above = sector.floor_above;
    rotated.floor_below = sector.floor_below;
    for direction in GridDirection::ALL {
        let rotated_direction = rotate_direction_cw(direction);
        for wall in sector.walls.get(direction) {
            rotated
                .walls
                .get_mut(rotated_direction)
                .push(rotate_vertical_face_cw(direction, wall.clone()));
        }
    }
    rotated
}

pub(crate) fn flip_sector_x(sector: &GridSector) -> GridSector {
    let mut flipped = GridSector::empty();
    flipped.floor = sector.floor.clone().map(flip_horizontal_face_x);
    flipped.ceiling = sector.ceiling.clone().map(flip_horizontal_face_x);
    flipped.floor_above = sector.floor_above;
    flipped.floor_below = sector.floor_below;
    for direction in GridDirection::ALL {
        let flipped_direction = flip_direction_x(direction);
        for wall in sector.walls.get(direction) {
            flipped
                .walls
                .get_mut(flipped_direction)
                .push(flip_vertical_face_x(direction, wall.clone()));
        }
    }
    flipped
}

pub(crate) fn flip_sector_z(sector: &GridSector) -> GridSector {
    let mut flipped = GridSector::empty();
    flipped.floor = sector.floor.clone().map(flip_horizontal_face_z);
    flipped.ceiling = sector.ceiling.clone().map(flip_horizontal_face_z);
    flipped.floor_above = sector.floor_above;
    flipped.floor_below = sector.floor_below;
    for direction in GridDirection::ALL {
        let flipped_direction = flip_direction_z(direction);
        for wall in sector.walls.get(direction) {
            flipped
                .walls
                .get_mut(flipped_direction)
                .push(flip_vertical_face_z(direction, wall.clone()));
        }
    }
    flipped
}

pub(crate) fn rotate_horizontal_face_cw(mut face: GridHorizontalFace) -> GridHorizontalFace {
    let old_heights = face.heights;
    let old_split = face.split;
    let old_overrides = face.triangle_overrides;
    face.heights = [
        old_heights[Corner::SW.idx()],
        old_heights[Corner::NW.idx()],
        old_heights[Corner::NE.idx()],
        old_heights[Corner::SE.idx()],
    ];
    face.split = rotate_split_cw(old_split);
    face.dropped_corner = face.dropped_corner.map(rotate_corner_cw);

    for new_index in 0..2 {
        let old_index = rotated_horizontal_triangle_source_index(old_split, face.split, new_index);
        let mut triangle_override = *old_overrides.get(old_index);
        if let Some(heights) = triangle_override.heights {
            triangle_override.heights = Some(rotate_triangle_heights_cw(
                old_split, old_index, face.split, new_index, heights,
            ));
        }
        *face.triangle_override_mut(new_index) = triangle_override;
    }
    face
}

pub(crate) fn flip_horizontal_face_x(mut face: GridHorizontalFace) -> GridHorizontalFace {
    let old_heights = face.heights;
    face.heights = [
        old_heights[Corner::NE.idx()],
        old_heights[Corner::NW.idx()],
        old_heights[Corner::SW.idx()],
        old_heights[Corner::SE.idx()],
    ];
    flip_horizontal_face(face, flip_corner_x, flip_uv_transform_u)
}

pub(crate) fn flip_horizontal_face_z(mut face: GridHorizontalFace) -> GridHorizontalFace {
    let old_heights = face.heights;
    face.heights = [
        old_heights[Corner::SW.idx()],
        old_heights[Corner::SE.idx()],
        old_heights[Corner::NE.idx()],
        old_heights[Corner::NW.idx()],
    ];
    flip_horizontal_face(face, flip_corner_z, flip_uv_transform_v)
}

pub(crate) fn flip_horizontal_face(
    mut face: GridHorizontalFace,
    flip_corner: fn(Corner) -> Corner,
    flip_uv: fn(GridUvTransform) -> GridUvTransform,
) -> GridHorizontalFace {
    let old_split = face.split;
    let old_overrides = face.triangle_overrides;
    face.split = flip_split(old_split);
    face.dropped_corner = face.dropped_corner.map(flip_corner);
    face.uv = flip_uv(face.uv);

    for new_index in 0..2 {
        let old_index = transformed_horizontal_triangle_source_index(
            old_split,
            face.split,
            new_index,
            flip_corner,
        );
        let mut triangle_override = *old_overrides.get(old_index);
        if let Some(uv) = triangle_override.uv.as_mut() {
            *uv = flip_uv(*uv);
        }
        if let Some(heights) = triangle_override.heights {
            triangle_override.heights = Some(transform_triangle_heights(
                old_split,
                old_index,
                face.split,
                new_index,
                heights,
                flip_corner,
            ));
        }
        *face.triangle_override_mut(new_index) = triangle_override;
    }
    face
}

pub(crate) fn rotated_horizontal_triangle_source_index(
    old_split: GridSplit,
    new_split: GridSplit,
    new_index: usize,
) -> usize {
    transformed_horizontal_triangle_source_index(old_split, new_split, new_index, rotate_corner_cw)
}

pub(crate) fn transformed_horizontal_triangle_source_index(
    old_split: GridSplit,
    new_split: GridSplit,
    new_index: usize,
    transform_corner: fn(Corner) -> Corner,
) -> usize {
    let new_corners = psxed_project::horizontal_triangle_corners(new_split, new_index);
    for old_index in 0..2 {
        let old_corners =
            psxed_project::horizontal_triangle_corners(old_split, old_index).map(transform_corner);
        if same_corner_set(old_corners, new_corners) {
            return old_index;
        }
    }
    new_index
}

pub(crate) fn same_corner_set(a: [Corner; 3], b: [Corner; 3]) -> bool {
    a.iter().all(|corner| b.contains(corner))
}

pub(crate) fn rotate_triangle_heights_cw(
    old_split: GridSplit,
    old_index: usize,
    new_split: GridSplit,
    new_index: usize,
    old_heights: [i32; 3],
) -> [i32; 3] {
    transform_triangle_heights(
        old_split,
        old_index,
        new_split,
        new_index,
        old_heights,
        rotate_corner_cw,
    )
}

pub(crate) fn transform_triangle_heights(
    old_split: GridSplit,
    old_index: usize,
    new_split: GridSplit,
    new_index: usize,
    old_heights: [i32; 3],
    transform_corner: fn(Corner) -> Corner,
) -> [i32; 3] {
    let old_corners = psxed_project::horizontal_triangle_corners(old_split, old_index);
    let new_corners = psxed_project::horizontal_triangle_corners(new_split, new_index);
    let mut new_heights = old_heights;
    for (old_corner, height) in old_corners.into_iter().zip(old_heights) {
        let rotated_corner = transform_corner(old_corner);
        if let Some(slot) = new_corners
            .iter()
            .position(|corner| *corner == rotated_corner)
        {
            new_heights[slot] = height;
        }
    }
    new_heights
}

pub(crate) fn rotate_vertical_face_cw(
    direction: GridDirection,
    mut wall: GridVerticalFace,
) -> GridVerticalFace {
    let old_heights = wall.heights;
    let mut heights = [0; 4];
    for corner in [
        WallCorner::BL,
        WallCorner::BR,
        WallCorner::TR,
        WallCorner::TL,
    ] {
        let rotated_corner = rotate_wall_corner_cw(direction, corner);
        heights[rotated_corner.idx()] = old_heights[corner.idx()];
    }
    wall.heights = heights;
    wall.dropped_corner = wall
        .dropped_corner
        .map(|corner| rotate_wall_corner_cw(direction, corner));
    wall
}

pub(crate) fn flip_vertical_face_x(
    direction: GridDirection,
    wall: GridVerticalFace,
) -> GridVerticalFace {
    flip_vertical_face(direction, wall, flip_wall_corner_x)
}

pub(crate) fn flip_vertical_face_z(
    direction: GridDirection,
    wall: GridVerticalFace,
) -> GridVerticalFace {
    flip_vertical_face(direction, wall, flip_wall_corner_z)
}

pub(crate) fn flip_vertical_face(
    direction: GridDirection,
    mut wall: GridVerticalFace,
    flip_corner: fn(GridDirection, WallCorner) -> WallCorner,
) -> GridVerticalFace {
    let old_heights = wall.heights;
    let mut heights = [0; 4];
    for corner in [
        WallCorner::BL,
        WallCorner::BR,
        WallCorner::TR,
        WallCorner::TL,
    ] {
        let flipped_corner = flip_corner(direction, corner);
        heights[flipped_corner.idx()] = old_heights[corner.idx()];
    }
    wall.heights = heights;
    wall.dropped_corner = wall
        .dropped_corner
        .map(|corner| flip_corner(direction, corner));
    wall.uv = flip_uv_transform_u(wall.uv);
    wall
}

pub(crate) const fn rotate_direction_cw(direction: GridDirection) -> GridDirection {
    match direction {
        GridDirection::North => GridDirection::East,
        GridDirection::East => GridDirection::South,
        GridDirection::South => GridDirection::West,
        GridDirection::West => GridDirection::North,
        GridDirection::NorthWestSouthEast => GridDirection::NorthEastSouthWest,
        GridDirection::NorthEastSouthWest => GridDirection::NorthWestSouthEast,
    }
}

pub(crate) const fn flip_direction_x(direction: GridDirection) -> GridDirection {
    match direction {
        GridDirection::North => GridDirection::North,
        GridDirection::East => GridDirection::West,
        GridDirection::South => GridDirection::South,
        GridDirection::West => GridDirection::East,
        GridDirection::NorthWestSouthEast => GridDirection::NorthEastSouthWest,
        GridDirection::NorthEastSouthWest => GridDirection::NorthWestSouthEast,
    }
}

pub(crate) const fn flip_direction_z(direction: GridDirection) -> GridDirection {
    match direction {
        GridDirection::North => GridDirection::South,
        GridDirection::East => GridDirection::East,
        GridDirection::South => GridDirection::North,
        GridDirection::West => GridDirection::West,
        GridDirection::NorthWestSouthEast => GridDirection::NorthEastSouthWest,
        GridDirection::NorthEastSouthWest => GridDirection::NorthWestSouthEast,
    }
}

pub(crate) const fn rotate_split_cw(split: GridSplit) -> GridSplit {
    flip_split(split)
}

pub(crate) const fn flip_split(split: GridSplit) -> GridSplit {
    match split {
        GridSplit::NorthWestSouthEast => GridSplit::NorthEastSouthWest,
        GridSplit::NorthEastSouthWest => GridSplit::NorthWestSouthEast,
    }
}

pub(crate) const fn rotate_corner_cw(corner: Corner) -> Corner {
    match corner {
        Corner::NW => Corner::NE,
        Corner::NE => Corner::SE,
        Corner::SE => Corner::SW,
        Corner::SW => Corner::NW,
    }
}

pub(crate) const fn flip_corner_x(corner: Corner) -> Corner {
    match corner {
        Corner::NW => Corner::NE,
        Corner::NE => Corner::NW,
        Corner::SE => Corner::SW,
        Corner::SW => Corner::SE,
    }
}

pub(crate) const fn flip_corner_z(corner: Corner) -> Corner {
    match corner {
        Corner::NW => Corner::SW,
        Corner::NE => Corner::SE,
        Corner::SE => Corner::NE,
        Corner::SW => Corner::NW,
    }
}

pub(crate) fn rotate_wall_corner_cw(direction: GridDirection, corner: WallCorner) -> WallCorner {
    let rotated_direction = rotate_direction_cw(direction);
    let (horizontal_corner, top) = wall_corner_horizontal_endpoint(direction, corner);
    let rotated_corner = rotate_corner_cw(horizontal_corner);
    wall_corner_from_horizontal_endpoint(rotated_direction, rotated_corner, top).unwrap_or(corner)
}

pub(crate) fn flip_wall_corner_x(direction: GridDirection, corner: WallCorner) -> WallCorner {
    let flipped_direction = flip_direction_x(direction);
    let (horizontal_corner, top) = wall_corner_horizontal_endpoint(direction, corner);
    let flipped_corner = flip_corner_x(horizontal_corner);
    wall_corner_from_horizontal_endpoint(flipped_direction, flipped_corner, top).unwrap_or(corner)
}

pub(crate) fn flip_wall_corner_z(direction: GridDirection, corner: WallCorner) -> WallCorner {
    let flipped_direction = flip_direction_z(direction);
    let (horizontal_corner, top) = wall_corner_horizontal_endpoint(direction, corner);
    let flipped_corner = flip_corner_z(horizontal_corner);
    wall_corner_from_horizontal_endpoint(flipped_direction, flipped_corner, top).unwrap_or(corner)
}

pub(crate) const fn wall_corner_horizontal_endpoint(
    direction: GridDirection,
    corner: WallCorner,
) -> (Corner, bool) {
    let (bl, br) = wall_direction_endpoint_corners(direction);
    match corner {
        WallCorner::BL => (bl, false),
        WallCorner::BR => (br, false),
        WallCorner::TR => (br, true),
        WallCorner::TL => (bl, true),
    }
}

pub(crate) const fn wall_direction_endpoint_corners(direction: GridDirection) -> (Corner, Corner) {
    match direction {
        GridDirection::North => (Corner::NW, Corner::NE),
        GridDirection::East => (Corner::NE, Corner::SE),
        GridDirection::South => (Corner::SE, Corner::SW),
        GridDirection::West => (Corner::SW, Corner::NW),
        GridDirection::NorthWestSouthEast => (Corner::NW, Corner::SE),
        GridDirection::NorthEastSouthWest => (Corner::NE, Corner::SW),
    }
}

pub(crate) fn wall_corner_from_horizontal_endpoint(
    direction: GridDirection,
    corner: Corner,
    top: bool,
) -> Option<WallCorner> {
    let (bl, br) = wall_direction_endpoint_corners(direction);
    if corner == bl {
        Some(if top { WallCorner::TL } else { WallCorner::BL })
    } else if corner == br {
        Some(if top { WallCorner::TR } else { WallCorner::BR })
    } else {
        None
    }
}

pub(crate) fn flip_uv_transform_u(mut uv: GridUvTransform) -> GridUvTransform {
    uv.flip_u = !uv.flip_u;
    uv
}

pub(crate) fn flip_uv_transform_v(mut uv: GridUvTransform) -> GridUvTransform {
    uv.flip_v = !uv.flip_v;
    uv
}

pub(crate) fn horizontal_triangle_index_at_local(
    local_x: f32,
    local_z: f32,
    sector_size: i32,
    split: GridSplit,
) -> HorizontalTriangleIndex {
    let size = sector_size.max(1) as f32;
    match split {
        GridSplit::NorthWestSouthEast => {
            if local_x + local_z >= size {
                HorizontalTriangleIndex::A
            } else {
                HorizontalTriangleIndex::B
            }
        }
        GridSplit::NorthEastSouthWest => {
            if local_z >= local_x {
                HorizontalTriangleIndex::A
            } else {
                HorizontalTriangleIndex::B
            }
        }
    }
}

pub(crate) const fn horizontal_triangle_other(
    index: HorizontalTriangleIndex,
) -> HorizontalTriangleIndex {
    match index {
        HorizontalTriangleIndex::A => HorizontalTriangleIndex::B,
        HorizontalTriangleIndex::B => HorizontalTriangleIndex::A,
    }
}

pub(crate) const fn horizontal_triangle_corners(
    split: GridSplit,
    index: HorizontalTriangleIndex,
) -> [Corner; 3] {
    psxed_project::horizontal_triangle_corners(split, horizontal_triangle_index_usize(index))
}

pub(crate) const fn horizontal_triangle_index_usize(index: HorizontalTriangleIndex) -> usize {
    match index {
        HorizontalTriangleIndex::A => 0,
        HorizontalTriangleIndex::B => 1,
    }
}

pub(crate) const fn horizontal_triangle_delete_corner(
    split: GridSplit,
    index: HorizontalTriangleIndex,
) -> Corner {
    match (split, index) {
        (GridSplit::NorthWestSouthEast, HorizontalTriangleIndex::A) => Corner::NE,
        (GridSplit::NorthWestSouthEast, HorizontalTriangleIndex::B) => Corner::SW,
        (GridSplit::NorthEastSouthWest, HorizontalTriangleIndex::A) => Corner::NW,
        (GridSplit::NorthEastSouthWest, HorizontalTriangleIndex::B) => Corner::SE,
    }
}

/// Wall-quad triangle decomposition. The diagonal flips when
/// the dropped corner sits on the BL-TR line -- `BL` / `TR`
/// trigger the BR-TL diagonal.
pub(crate) type WallTri = ([f32; 3], [f32; 3], [f32; 3], [WallCorner; 3]);
pub(crate) fn wall_triangles(
    bl: [f32; 3],
    br: [f32; 3],
    tr: [f32; 3],
    tl: [f32; 3],
    dropped: Option<WallCorner>,
) -> [WallTri; 2] {
    let use_br_tl = matches!(dropped, Some(WallCorner::BL) | Some(WallCorner::TR));
    if use_br_tl {
        [
            (bl, br, tl, [WallCorner::BL, WallCorner::BR, WallCorner::TL]),
            (br, tr, tl, [WallCorner::BR, WallCorner::TR, WallCorner::TL]),
        ]
    } else {
        [
            (bl, br, tr, [WallCorner::BL, WallCorner::BR, WallCorner::TR]),
            (bl, tr, tl, [WallCorner::BL, WallCorner::TR, WallCorner::TL]),
        ]
    }
}

pub(crate) fn material_sidedness(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> MaterialFaceSidedness {
    material
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Material(material) => Some(material.sidedness()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(crate) fn wall_material_sidedness(sidedness: MaterialFaceSidedness) -> MaterialFaceSidedness {
    match sidedness {
        MaterialFaceSidedness::Front => MaterialFaceSidedness::Back,
        MaterialFaceSidedness::Back => MaterialFaceSidedness::Front,
        MaterialFaceSidedness::Both => MaterialFaceSidedness::Both,
    }
}

pub(crate) fn wall_side_visible_from_camera(
    sidedness: MaterialFaceSidedness,
    bounds: GridCellBounds,
    direction: GridDirection,
    camera_position: [f32; 3],
) -> bool {
    let sidedness = wall_material_sidedness(sidedness);
    let cam_x = camera_position[0];
    let cam_z = camera_position[2];
    let inside_distance = match direction {
        GridDirection::North => bounds.z1 as f32 - cam_z,
        GridDirection::East => bounds.x1 as f32 - cam_x,
        GridDirection::South => cam_z - bounds.z0 as f32,
        GridDirection::West => cam_x - bounds.x0 as f32,
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return true,
    };
    match sidedness {
        MaterialFaceSidedness::Both => true,
        MaterialFaceSidedness::Back => inside_distance >= 0.0,
        MaterialFaceSidedness::Front => inside_distance <= 0.0,
    }
}

/// Index of the corner closest (3D distance) to `hit` among
/// `corners`. Caller is responsible for the corner ordering
/// convention -- `[NW, NE, SE, SW]` for floors / ceilings,
/// `[BL, BR, TR, TL]` for walls, or triangle perimeter order.
pub(crate) fn closest_corner_idx<const N: usize>(corners: &[[f32; 3]; N], hit: [f32; 3]) -> usize {
    let mut best = 0usize;
    let mut best_d2 = f32::INFINITY;
    for (i, c) in corners.iter().enumerate() {
        let d2 = dist2_3d(*c, hit);
        if d2 < best_d2 {
            best = i;
            best_d2 = d2;
        }
    }
    best
}

/// Index of the edge closest to `hit`. Edge `i` runs
/// `corners[i] → corners[(i+1) % N]`.
pub(crate) fn closest_edge_idx<const N: usize>(corners: &[[f32; 3]; N], hit: [f32; 3]) -> usize {
    let mut best = 0usize;
    let mut best_d2 = f32::INFINITY;
    for i in 0..N {
        let a = corners[i];
        let b = corners[(i + 1) % N];
        let d2 = point_segment_dist2(hit, a, b);
        if d2 < best_d2 {
            best = i;
            best_d2 = d2;
        }
    }
    best
}

pub(crate) fn dist2_3d(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx * dx + dy * dy + dz * dz
}

/// Squared distance from point `p` to the segment `a-b`.
pub(crate) fn point_segment_dist2(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2];
    let t = if len2 > 0.0 {
        ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
    dist2_3d(q, p)
}

/// Floor / ceiling corner index `0..3` → `Corner` (NW, NE,
/// SE, SW in perimeter order).
pub(crate) const fn floor_corner_idx(idx: usize) -> Corner {
    match idx {
        0 => Corner::NW,
        1 => Corner::NE,
        2 => Corner::SE,
        _ => Corner::SW,
    }
}

pub(crate) const fn wall_corner_idx(idx: usize) -> WallCorner {
    match idx {
        0 => WallCorner::BL,
        1 => WallCorner::BR,
        2 => WallCorner::TR,
        _ => WallCorner::TL,
    }
}

/// Floor / ceiling edge index `0..3` → cardinal `GridDirection`.
pub(crate) const fn floor_edge_dir(idx: usize) -> GridDirection {
    match idx {
        0 => GridDirection::North,
        1 => GridDirection::East,
        2 => GridDirection::South,
        _ => GridDirection::West,
    }
}

pub(crate) const fn wall_edge_idx(idx: usize) -> WallEdge {
    match idx {
        0 => WallEdge::Bottom,
        1 => WallEdge::Right,
        2 => WallEdge::Top,
        _ => WallEdge::Left,
    }
}

/// Möller-Trumbore ray-triangle intersection. Returns the ray
/// parameter `t`, or `None` for misses / degenerate triangles.
pub(crate) fn ray_triangle(
    orig: [f32; 3],
    dir: [f32; 3],
    v0: [f32; 3],
    v1: [f32; 3],
    v2: [f32; 3],
) -> Option<f32> {
    let edge1 = sub3(v1, v0);
    let edge2 = sub3(v2, v0);
    let h = cross3(dir, edge2);
    let a = dot3(edge1, h);
    if a.abs() < 1e-6 {
        return None;
    }
    let f = 1.0 / a;
    let s = sub3(orig, v0);
    let u = f * dot3(s, h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross3(s, edge1);
    let v = f * dot3(dir, q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * dot3(edge2, q);
    if t > 1e-3 {
        Some(t)
    } else {
        None
    }
}

/// Same ray test, but applying the material side rule before the
/// expensive barycentric checks. Horizontal grid faces use this so
/// the Select tool passes through a face whose rendered side is
/// currently culled.
pub(crate) fn ray_triangle_sided(
    orig: [f32; 3],
    dir: [f32; 3],
    v0: [f32; 3],
    v1: [f32; 3],
    v2: [f32; 3],
    sidedness: MaterialFaceSidedness,
) -> Option<f32> {
    let edge1 = sub3(v1, v0);
    let edge2 = sub3(v2, v0);
    let h = cross3(dir, edge2);
    let a = dot3(edge1, h);
    if !ray_triangle_side_visible(sidedness, a) {
        return None;
    }
    let f = 1.0 / a;
    let s = sub3(orig, v0);
    let u = f * dot3(s, h);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross3(s, edge1);
    let v = f * dot3(dir, q);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = f * dot3(edge2, q);
    if t > 1e-3 {
        Some(t)
    } else {
        None
    }
}

pub(crate) fn ray_triangle_side_visible(
    sidedness: MaterialFaceSidedness,
    signed_area: f32,
) -> bool {
    const EPS: f32 = 1e-6;
    match sidedness {
        MaterialFaceSidedness::Front => signed_area < -EPS,
        MaterialFaceSidedness::Back => signed_area > EPS,
        MaterialFaceSidedness::Both => signed_area.abs() >= EPS,
    }
}
