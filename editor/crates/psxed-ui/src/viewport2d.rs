use super::*;

pub(crate) fn draw_viewport_overlay(
    painter: &egui::Painter,
    rect: Rect,
    project: &ProjectDocument,
    zoom: f32,
    snap_units: u16,
    view: OrthographicView,
) {
    let overlay = Rect::from_min_size(
        rect.left_top() + Vec2::new(12.0, 12.0),
        Vec2::new(132.0, 94.0),
    );
    painter.rect_filled(
        overlay,
        2.0,
        Color32::from_rgba_unmultiplied(14, 20, 26, 224),
    );
    painter.rect_stroke(
        overlay,
        2.0,
        Stroke::new(1.0, STUDIO_BORDER),
        StrokeKind::Inside,
    );
    let lines = [
        format!("{} Orthographic", view.label()),
        format!("Grid: {snap_units} units"),
        format!("{} nodes", project.active_scene().nodes().len()),
        format!("{} resources", project.resources.len()),
        format!("{:.0} px/unit", zoom),
    ];
    for (idx, line) in lines.iter().enumerate() {
        painter.text(
            overlay.left_top() + Vec2::new(10.0, 10.0 + idx as f32 * 15.0),
            Align2::LEFT_TOP,
            line,
            FontId::monospace(11.0),
            STUDIO_TEXT,
        );
    }
}

pub(crate) fn draw_viewport_box_select_marquee(painter: &egui::Painter, rect: Option<Rect>) {
    let Some(rect) = rect else {
        return;
    };
    painter.rect_filled(
        rect,
        0.0,
        Color32::from_rgba_unmultiplied(255, 238, 150, 24),
    );
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(2.0, EDITOR_OUTLINE_GOLD),
        StrokeKind::Outside,
    );
}

pub(crate) fn draw_axes_gizmo(painter: &egui::Painter, rect: Rect, view: OrthographicView) {
    let origin = Pos2::new(rect.left() + 34.0, rect.bottom() - 38.0);
    let x_end = origin + Vec2::new(42.0, 0.0);
    let y_end = origin + Vec2::new(0.0, -42.0);
    let [horizontal_axis, vertical_axis] = view.plane_axes();
    let horizontal_color = axis_color(horizontal_axis);
    let vertical_color = axis_color(vertical_axis);
    let x_stroke = Stroke::new(2.0, horizontal_color);
    let y_stroke = Stroke::new(2.0, vertical_color);

    painter.circle_filled(origin, 3.0, STUDIO_ACCENT);
    painter.line_segment([origin, x_end], x_stroke);
    painter.line_segment([origin, y_end], y_stroke);
    painter.line_segment([x_end, x_end + Vec2::new(-7.0, -4.0)], x_stroke);
    painter.line_segment([x_end, x_end + Vec2::new(-7.0, 4.0)], x_stroke);
    painter.line_segment([y_end, y_end + Vec2::new(-4.0, 7.0)], y_stroke);
    painter.line_segment([y_end, y_end + Vec2::new(4.0, 7.0)], y_stroke);
    painter.text(
        x_end + Vec2::new(8.0, 0.0),
        Align2::LEFT_CENTER,
        axis_label(horizontal_axis),
        FontId::monospace(12.0),
        horizontal_color,
    );
    painter.text(
        y_end + Vec2::new(0.0, -8.0),
        Align2::CENTER_BOTTOM,
        axis_label(vertical_axis),
        FontId::monospace(12.0),
        vertical_color,
    );
}

fn axis_label(axis: usize) -> &'static str {
    match axis {
        0 => "X",
        1 => "Y",
        2 => "Z",
        _ => unreachable!("world axis is always X, Y, or Z"),
    }
}

fn axis_color(axis: usize) -> Color32 {
    match axis {
        0 => Color32::from_rgb(255, 95, 88),
        1 => Color32::from_rgb(140, 255, 128),
        2 => Color32::from_rgb(96, 150, 255),
        _ => unreachable!("world axis is always X, Y, or Z"),
    }
}

/// One of the three axis-aligned 2D authoring planes. All views use the
/// same world-space focus, zoom, grid settings, and editor selection; only
/// the pair of world axes projected into the panel changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrthographicView {
    /// Look down from +Y: horizontal X, vertical Z.
    Top,
    /// Look back from +Z: horizontal X, vertical Y.
    Front,
    /// Look left from +X: horizontal Z, vertical Y.
    Side,
}

impl OrthographicView {
    pub(crate) const ALL: [Self; 3] = [Self::Top, Self::Front, Self::Side];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Front => "Front",
            Self::Side => "Side",
        }
    }

    /// `[horizontal, vertical]` world-axis indices.
    pub(crate) const fn plane_axes(self) -> [usize; 2] {
        match self {
            Self::Top => [0, 2],
            Self::Front => [0, 1],
            Self::Side => [2, 1],
        }
    }

    /// World axis perpendicular to this view. The virtual camera sits on
    /// the positive side of this axis.
    pub(crate) const fn depth_axis(self) -> usize {
        match self {
            Self::Top => 1,
            Self::Front => 2,
            Self::Side => 0,
        }
    }

    pub(crate) fn project_f32(self, world: [f32; 3]) -> [f32; 2] {
        let [horizontal, vertical] = self.plane_axes();
        [world[horizontal], world[vertical]]
    }

    pub(crate) fn project_f64(self, world: [f64; 3]) -> [f64; 2] {
        let [horizontal, vertical] = self.plane_axes();
        [world[horizontal], world[vertical]]
    }

    /// Lift a panel-space world point into 3D while preserving the current
    /// focus coordinate along the hidden/depth axis.
    pub(crate) fn unproject(self, plane: [f32; 2], focus: [f32; 3]) -> [f32; 3] {
        let [horizontal, vertical] = self.plane_axes();
        let mut world = focus;
        world[horizontal] = plane[0];
        world[vertical] = plane[1];
        world
    }

    pub(crate) fn with_projected_focus(self, focus: [f32; 3], plane: [f32; 2]) -> [f32; 3] {
        self.unproject(plane, focus)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewportTransform {
    rect: Rect,
    pan: Vec2,
    zoom: f32,
}

impl ViewportTransform {
    pub(crate) fn new(rect: Rect, pan: Vec2, zoom: f32) -> Self {
        Self { rect, pan, zoom }
    }

    pub(crate) fn from_focus(rect: Rect, focus: [f32; 2], zoom: f32) -> Self {
        Self::new(rect, Vec2::new(-focus[0] * zoom, focus[1] * zoom), zoom)
    }

    pub(crate) fn world_to_screen(self, world: [f32; 2]) -> Pos2 {
        self.rect.center() + self.pan + Vec2::new(world[0] * self.zoom, -world[1] * self.zoom)
    }

    pub(crate) fn screen_to_world(self, screen: Pos2) -> [f32; 2] {
        let delta = screen - self.rect.center() - self.pan;
        [delta.x / self.zoom, -delta.y / self.zoom]
    }

    pub(crate) fn world_rect_to_screen(self, center: [f32; 2], half: [f32; 2]) -> Rect {
        let min = [center[0] - half[0], center[1] - half[1]];
        let max = [center[0] + half[0], center[1] + half[1]];
        let a = self.world_to_screen(min);
        let b = self.world_to_screen(max);
        Rect::from_min_max(
            Pos2::new(a.x.min(b.x), a.y.min(b.y)),
            Pos2::new(a.x.max(b.x), a.y.max(b.y)),
        )
    }

    pub(crate) fn screen_radius(self, radius: f32) -> f32 {
        radius * self.zoom
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ViewportHit {
    pub(crate) id: NodeId,
    shape: HitShape,
}

impl ViewportHit {
    pub(crate) fn rect(
        id: NodeId,
        _name: impl Into<String>,
        center: [f32; 2],
        half: [f32; 2],
    ) -> Self {
        Self {
            id,
            shape: HitShape::Rect { center, half },
        }
    }

    pub(crate) fn circle(
        id: NodeId,
        _name: impl Into<String>,
        center: [f32; 2],
        radius: f32,
    ) -> Self {
        Self {
            id,
            shape: HitShape::Circle { center, radius },
        }
    }

    pub(crate) fn segment(
        id: NodeId,
        _name: impl Into<String>,
        a: [f32; 2],
        b: [f32; 2],
        radius: f32,
    ) -> Self {
        Self {
            id,
            shape: HitShape::Segment { a, b, radius },
        }
    }

    pub(crate) fn contains(&self, world: [f32; 2]) -> bool {
        match self.shape {
            HitShape::Rect { center, half } => {
                world[0] >= center[0] - half[0]
                    && world[0] <= center[0] + half[0]
                    && world[1] >= center[1] - half[1]
                    && world[1] <= center[1] + half[1]
            }
            HitShape::Circle { center, radius } => {
                let dx = world[0] - center[0];
                let dz = world[1] - center[1];
                dx * dx + dz * dz <= radius * radius
            }
            HitShape::Segment { a, b, radius } => {
                point_segment_dist2_2d(world, a, b) <= radius * radius
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HitShape {
    Rect {
        center: [f32; 2],
        half: [f32; 2],
    },
    Circle {
        center: [f32; 2],
        radius: f32,
    },
    Segment {
        a: [f32; 2],
        b: [f32; 2],
        radius: f32,
    },
}

pub(crate) fn draw_world_grid(painter: &egui::Painter, transform: ViewportTransform) {
    let rect = transform.rect;
    let top_left = transform.screen_to_world(rect.left_top());
    let bottom_right = transform.screen_to_world(rect.right_bottom());
    let min_x = top_left[0].min(bottom_right[0]).floor() as i32 - 1;
    let max_x = top_left[0].max(bottom_right[0]).ceil() as i32 + 1;
    let min_z = top_left[1].min(bottom_right[1]).floor() as i32 - 1;
    let max_z = top_left[1].max(bottom_right[1]).ceil() as i32 + 1;

    let minor = Stroke::new(1.0, Color32::from_rgb(20, 43, 52));
    let major = Stroke::new(1.0, Color32::from_rgb(31, 63, 75));
    let axis = Stroke::new(1.0, Color32::from_rgb(58, 91, 103));

    for x in min_x..=max_x {
        let stroke = if x == 0 {
            axis
        } else if x % 4 == 0 {
            major
        } else {
            minor
        };
        let a = transform.world_to_screen([x as f32, min_z as f32]);
        let b = transform.world_to_screen([x as f32, max_z as f32]);
        painter.line_segment([a, b], stroke);
    }

    for z in min_z..=max_z {
        let stroke = if z == 0 {
            axis
        } else if z % 4 == 0 {
            major
        } else {
            minor
        };
        let a = transform.world_to_screen([min_x as f32, z as f32]);
        let b = transform.world_to_screen([max_x as f32, z as f32]);
        painter.line_segment([a, b], stroke);
    }
}

// Selection, validation, and visibility state for one 2D scene-viewport pass.
pub(crate) struct SceneViewportContext<'a> {
    pub(crate) hidden_scene_nodes: &'a HashSet<NodeId>,
    pub(crate) selected: NodeId,
    pub(crate) selected_nodes: &'a HashSet<NodeId>,
    pub(crate) selected_sectors: &'a HashSet<SectorSelection>,
    pub(crate) validation_issue_primitives: &'a [Selection],
    pub(crate) validation_issue_rooms: &'a HashSet<NodeId>,
    pub(crate) show_portals: bool,
    pub(crate) show_lights: bool,
}

pub(crate) fn draw_scene_viewport(
    painter: &egui::Painter,
    transform: ViewportTransform,
    project: &ProjectDocument,
    ctx: SceneViewportContext<'_>,
) -> Vec<ViewportHit> {
    let SceneViewportContext {
        hidden_scene_nodes,
        selected,
        selected_nodes,
        selected_sectors,
        validation_issue_primitives,
        validation_issue_rooms,
        show_portals,
        show_lights,
    } = ctx;
    let scene = project.active_scene();
    let mut hits = Vec::new();

    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        if matches!(node.kind, NodeKind::Section { .. }) {
            draw_room(
                painter,
                transform,
                project,
                node,
                selected_nodes.contains(&node.id)
                    || (selected_nodes.is_empty() && selected == node.id),
                selected_sectors,
                validation_issue_primitives,
                validation_issue_rooms,
                show_portals,
                &mut hits,
            );
        }
    }

    for node in scene.nodes() {
        if scene_node_hidden(scene, hidden_scene_nodes, node.id) {
            continue;
        }
        match &node.kind {
            NodeKind::MeshInstance { .. } => {
                draw_mesh_marker(
                    painter,
                    transform,
                    project,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    &mut hits,
                );
            }
            NodeKind::ImageProp { .. } => {
                draw_simple_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    "I",
                    Color32::from_rgb(210, 170, 120),
                    &mut hits,
                );
            }
            NodeKind::BoxProp { .. } => {
                draw_simple_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    "B",
                    Color32::from_rgb(135, 180, 220),
                    &mut hits,
                );
            }
            NodeKind::CylinderProp { .. } => {
                draw_simple_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    "C",
                    Color32::from_rgb(120, 184, 200),
                    &mut hits,
                );
            }
            NodeKind::SpawnPoint { .. } => {
                draw_spawn_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    &mut hits,
                );
            }
            NodeKind::PointLight {
                color,
                intensity,
                radius,
            } if show_lights => {
                draw_light_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    *color,
                    *intensity,
                    *radius,
                    &mut hits,
                );
            }
            NodeKind::ParticleEmitter { .. } => {
                draw_simple_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    "P",
                    Color32::from_rgb(152, 214, 230),
                    &mut hits,
                );
            }
            NodeKind::Portal { .. } if show_portals => {
                draw_portal_seam_2d(
                    painter,
                    transform,
                    scene,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    &mut hits,
                );
                draw_portal_marker(
                    painter,
                    transform,
                    scene,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    &mut hits,
                );
            }
            NodeKind::Node | NodeKind::Node3D if node.id != scene.root => {
                draw_simple_marker(
                    painter,
                    transform,
                    node,
                    selected_nodes.contains(&node.id)
                        || (selected_nodes.is_empty() && selected == node.id),
                    "N",
                    Color32::from_rgb(110, 124, 150),
                    &mut hits,
                );
            }
            _ => {}
        }
    }

    hits
}

pub(crate) fn draw_room(
    painter: &egui::Painter,
    transform: ViewportTransform,
    project: &ProjectDocument,
    node: &psxed_project::SceneNode,
    selected: bool,
    selected_sectors: &HashSet<SectorSelection>,
    validation_issue_primitives: &[Selection],
    validation_issue_rooms: &HashSet<NodeId>,
    show_portals: bool,
    hits: &mut Vec<ViewportHit>,
) {
    let NodeKind::Section { grid } = &node.kind else {
        return;
    };

    let node_center = node_world(node);
    let Some((local_center, half)) = grid_authored_editor_center_half(grid) else {
        return;
    };
    let center = [
        node_center[0] + local_center[0],
        node_center[1] + local_center[1],
    ];
    let outline = transform.world_rect_to_screen(center, half);
    hits.push(ViewportHit::rect(node.id, node.name.clone(), center, half));
    painter.rect_filled(outline, 0.0, darken(STUDIO_ROOM_FLOOR, 28));

    for x in 0..grid.width {
        for z in 0..grid.depth {
            let Some(sector) = grid.sector(x, z) else {
                continue;
            };
            if !sector.has_geometry() {
                continue;
            }
            let local_tile_center = grid_cell_editor_center(grid, x, z);
            let tile_center = [
                node_center[0] + local_tile_center[0],
                node_center[1] + local_tile_center[1],
            ];
            let screen_rect = transform.world_rect_to_screen(tile_center, [0.5, 0.5]);
            if !screen_rect.intersects(transform.rect) {
                continue;
            }
            if let Some(floor) = &sector.floor {
                let floor_color = material_color(project, floor.material, SurfaceRole::Floor);
                draw_floor_tile(
                    painter,
                    screen_rect,
                    floor_color,
                    x as i32,
                    z as i32,
                    floor.split,
                    floor.dropped_corner,
                );
            }
            if selected_sectors.contains(&(node.id, x, z)) {
                painter.rect_filled(
                    screen_rect.shrink(2.0),
                    0.0,
                    Color32::from_rgba_unmultiplied(255, 238, 150, 58),
                );
                painter.rect_stroke(
                    screen_rect.shrink(2.0),
                    0.0,
                    Stroke::new(EDITOR_SELECTED_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_GOLD),
                    StrokeKind::Inside,
                );
            }
            hits.push(ViewportHit::rect(
                node.id,
                format!("{} sector {},{}", node.name, x, z),
                tile_center,
                [0.5, 0.5],
            ));
            draw_grid_sector_walls(painter, transform, project, tile_center, sector);
        }
    }

    if show_portals {
        draw_portal_room_boundaries_2d(painter, transform, project, node.id, grid, node_center);
    }
    draw_validation_issue_primitives_2d(
        painter,
        transform,
        grid,
        node.id,
        node_center,
        validation_issue_primitives,
    );
    painter.rect_stroke(outline, 0.0, selected_stroke(selected), StrokeKind::Outside);
    if validation_issue_rooms.contains(&node.id) {
        painter.rect_stroke(
            outline.expand(2.0),
            0.0,
            Stroke::new(4.0, Color32::from_rgb(255, 64, 64)),
            StrokeKind::Outside,
        );
    }
    painter.text(
        transform.world_to_screen([center[0] - half[0], center[1] + half[1]])
            + Vec2::new(8.0, -8.0),
        Align2::LEFT_BOTTOM,
        &node.name,
        FontId::monospace(12.0),
        Color32::from_rgb(230, 235, 245),
    );
}

pub(crate) fn draw_portal_room_boundaries_2d(
    painter: &egui::Painter,
    transform: ViewportTransform,
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    node_center: [f32; 2],
) {
    let plan = plan_portal_rooms(
        project.active_scene(),
        room_id,
        grid,
        PortalRoomConfig::default(),
    );
    if plan.room_count() <= 1 {
        return;
    }
    let stroke = Stroke::new(2.0, Color32::from_rgb(96, 255, 196));
    for portal_room in plan.rooms {
        let (local_center, chunk_half) =
            grid_rect_editor_center_half(grid, portal_room.array_origin, portal_room.size);
        let chunk_center = [
            node_center[0] + local_center[0],
            node_center[1] + local_center[1],
        ];
        let rect = transform.world_rect_to_screen(chunk_center, chunk_half);
        if rect.intersects(transform.rect) {
            painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Inside);
        }
    }
}

pub(crate) fn draw_validation_issue_primitives_2d(
    painter: &egui::Painter,
    transform: ViewportTransform,
    grid: &WorldGrid,
    room: NodeId,
    node_center: [f32; 2],
    validation_issue_primitives: &[Selection],
) {
    for selection in validation_issue_primitives {
        let Selection::Face(face) = *selection else {
            continue;
        };
        if face.room != room || face.sx >= grid.width || face.sz >= grid.depth {
            continue;
        }

        let local_tile_center = grid_cell_editor_center(grid, face.sx, face.sz);
        let tile_center = [
            node_center[0] + local_tile_center[0],
            node_center[1] + local_tile_center[1],
        ];
        match face.kind {
            FaceKind::Floor | FaceKind::Ceiling => {
                let rect = transform.world_rect_to_screen(tile_center, [0.5, 0.5]);
                if rect.intersects(transform.rect) {
                    draw_validation_issue_rect(painter, rect);
                }
            }
            FaceKind::Wall { dir, .. } if dir.is_cardinal() => {
                if let Some((wall_center, wall_half)) = wall_band_center_half(tile_center, dir) {
                    let rect = transform.world_rect_to_screen(wall_center, wall_half);
                    if rect.intersects(transform.rect) {
                        draw_validation_issue_rect(painter, rect);
                    }
                }
            }
            FaceKind::Wall { dir, .. } => {
                draw_validation_issue_diagonal(painter, transform, tile_center, dir);
            }
        }
    }
}

pub(crate) fn draw_validation_issue_rect(painter: &egui::Painter, rect: Rect) {
    let fill = Color32::from_rgba_unmultiplied(255, 32, 32, 70);
    let stroke = Stroke::new(4.0, Color32::from_rgb(255, 64, 64));
    painter.rect_filled(rect, 0.0, fill);
    painter.rect_stroke(rect, 0.0, stroke, StrokeKind::Outside);
}

pub(crate) fn draw_validation_issue_diagonal(
    painter: &egui::Painter,
    transform: ViewportTransform,
    tile_center: [f32; 2],
    dir: GridDirection,
) {
    let min_x = tile_center[0] - 0.5;
    let min_z = tile_center[1] - 0.5;
    let (a, b) = match dir {
        GridDirection::NorthWestSouthEast => ([min_x, min_z + 1.0], [min_x + 1.0, min_z]),
        GridDirection::NorthEastSouthWest => ([min_x + 1.0, min_z + 1.0], [min_x, min_z]),
        _ => return,
    };
    let a = transform.world_to_screen(a);
    let b = transform.world_to_screen(b);
    painter.line_segment(
        [a, b],
        Stroke::new(7.0, Color32::from_rgba_unmultiplied(255, 32, 32, 92)),
    );
    painter.line_segment([a, b], Stroke::new(4.0, Color32::from_rgb(255, 64, 64)));
}

pub(crate) fn draw_grid_sector_walls(
    painter: &egui::Painter,
    transform: ViewportTransform,
    project: &ProjectDocument,
    tile_center: [f32; 2],
    sector: &psxed_project::GridSector,
) {
    let min_x = tile_center[0] - 0.5;
    let min_z = tile_center[1] - 0.5;
    for direction in GridDirection::CARDINAL {
        let walls = sector.walls.get(direction);
        if walls.is_empty() {
            continue;
        }
        let material = walls.first().and_then(|wall| wall.material);
        let wall_color = material_color(project, material, SurfaceRole::Wall);
        let Some((wall_center, wall_half)) = wall_band_center_half(tile_center, direction) else {
            continue;
        };
        let screen_rect = transform.world_rect_to_screen(wall_center, wall_half);
        if screen_rect.intersects(transform.rect) {
            draw_wall_band(painter, screen_rect, wall_color);
            painter.rect_stroke(
                screen_rect,
                0.0,
                Stroke::new(1.0, Color32::from_rgb(84, 58, 44)),
                StrokeKind::Inside,
            );
        }
    }

    for (direction, nw_to_se) in [
        (GridDirection::NorthWestSouthEast, true),
        (GridDirection::NorthEastSouthWest, false),
    ] {
        if sector.walls.get(direction).is_empty() {
            continue;
        }
        let a = if nw_to_se {
            transform.world_to_screen([min_x, min_z + 1.0])
        } else {
            transform.world_to_screen([min_x + 1.0, min_z + 1.0])
        };
        let b = if nw_to_se {
            transform.world_to_screen([min_x + 1.0, min_z])
        } else {
            transform.world_to_screen([min_x, min_z])
        };
        painter.line_segment([a, b], Stroke::new(4.0, STUDIO_ROOM_WALL));
        painter.line_segment([a, b], Stroke::new(1.0, Color32::from_rgb(84, 58, 44)));
    }
}

pub(crate) fn wall_band_center_half(
    tile_center: [f32; 2],
    direction: GridDirection,
) -> Option<([f32; 2], [f32; 2])> {
    let wall_thickness = 0.18;
    let min_x = tile_center[0] - 0.5;
    let min_z = tile_center[1] - 0.5;
    match direction {
        GridDirection::North => Some((
            [min_x + 0.5, min_z + 1.0 + wall_thickness * 0.5],
            [0.5, wall_thickness * 0.5],
        )),
        GridDirection::East => Some((
            [min_x + 1.0 + wall_thickness * 0.5, min_z + 0.5],
            [wall_thickness * 0.5, 0.5],
        )),
        GridDirection::South => Some((
            [min_x + 0.5, min_z - wall_thickness * 0.5],
            [0.5, wall_thickness * 0.5],
        )),
        GridDirection::West => Some((
            [min_x - wall_thickness * 0.5, min_z + 0.5],
            [wall_thickness * 0.5, 0.5],
        )),
        _ => None,
    }
}

pub(crate) fn draw_floor_tile(
    painter: &egui::Painter,
    rect: Rect,
    base: Color32,
    ix: i32,
    iz: i32,
    split: GridSplit,
    dropped_corner: Option<Corner>,
) {
    let tint = if (ix + iz) % 2 == 0 {
        lighten(base, 8)
    } else {
        darken(base, 5)
    };
    painter.rect_filled(rect, 0.0, tint);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(44, 56, 65, 168)),
        StrokeKind::Inside,
    );

    if rect.width() < 28.0 || rect.height() < 28.0 {
        return;
    }

    let mid_x = rect.center().x
        + if ix % 2 == 0 {
            -rect.width() * 0.12
        } else {
            rect.width() * 0.10
        };
    let mid_y = rect.center().y
        + if iz % 2 == 0 {
            rect.height() * 0.08
        } else {
            -rect.height() * 0.10
        };
    let crack = Stroke::new(1.0, Color32::from_rgba_unmultiplied(70, 80, 88, 150));
    painter.line_segment(
        [
            Pos2::new(mid_x, rect.top() + 5.0),
            Pos2::new(mid_x, rect.bottom() - 5.0),
        ],
        crack,
    );
    painter.line_segment(
        [
            Pos2::new(rect.left() + 5.0, mid_y),
            Pos2::new(rect.right() - 5.0, mid_y),
        ],
        crack,
    );
    draw_horizontal_split_line(painter, rect, split, dropped_corner);
}

pub(crate) fn draw_horizontal_split_line(
    painter: &egui::Painter,
    rect: Rect,
    split: GridSplit,
    dropped_corner: Option<Corner>,
) {
    if rect.width() < 20.0 || rect.height() < 20.0 {
        return;
    }
    let inset = 4.0;
    let nw = Pos2::new(rect.left() + inset, rect.top() + inset);
    let ne = Pos2::new(rect.right() - inset, rect.top() + inset);
    let se = Pos2::new(rect.right() - inset, rect.bottom() - inset);
    let sw = Pos2::new(rect.left() + inset, rect.bottom() - inset);
    let (a, b) = match split {
        GridSplit::NorthWestSouthEast => (nw, se),
        GridSplit::NorthEastSouthWest => (ne, sw),
    };
    let alpha = if dropped_corner.is_some() { 160 } else { 96 };
    painter.line_segment(
        [a, b],
        Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 238, 150, alpha)),
    );
}

pub(crate) fn draw_wall_band(painter: &egui::Painter, rect: Rect, base: Color32) {
    painter.rect_filled(rect, 0.0, darken(base, 4));
    let highlight = Stroke::new(1.0, Color32::from_rgba_unmultiplied(166, 92, 50, 160));
    let shadow = Stroke::new(1.0, Color32::from_rgba_unmultiplied(72, 42, 30, 180));

    if rect.width() >= rect.height() {
        let rows = (rect.height() / 7.0).max(2.0) as i32;
        for row in 1..rows {
            let y = rect.top() + row as f32 * rect.height() / rows as f32;
            painter.line_segment(
                [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                if row % 2 == 0 { highlight } else { shadow },
            );
        }
        let cols = (rect.width() / 42.0).max(3.0) as i32;
        for col in 1..cols {
            let x = rect.left() + col as f32 * rect.width() / cols as f32;
            painter.line_segment(
                [
                    Pos2::new(x, rect.top() + 3.0),
                    Pos2::new(x, rect.bottom() - 3.0),
                ],
                shadow,
            );
        }
    } else {
        let cols = (rect.width() / 7.0).max(2.0) as i32;
        for col in 1..cols {
            let x = rect.left() + col as f32 * rect.width() / cols as f32;
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                if col % 2 == 0 { highlight } else { shadow },
            );
        }
        let rows = (rect.height() / 42.0).max(3.0) as i32;
        for row in 1..rows {
            let y = rect.top() + row as f32 * rect.height() / rows as f32;
            painter.line_segment(
                [
                    Pos2::new(rect.left() + 3.0, y),
                    Pos2::new(rect.right() - 3.0, y),
                ],
                shadow,
            );
        }
    }
}

pub(crate) fn draw_mesh_marker(
    painter: &egui::Painter,
    transform: ViewportTransform,
    project: &ProjectDocument,
    node: &psxed_project::SceneNode,
    selected: bool,
    hits: &mut Vec<ViewportHit>,
) {
    let NodeKind::MeshInstance { material, .. } = node.kind else {
        return;
    };
    let center = node_world(node);
    let half = [
        node.transform.scale[0].abs().max(0.35) * 0.5,
        node.transform.scale[2].abs().max(0.18) * 0.5,
    ];
    let rect = transform.world_rect_to_screen(center, half);
    let color = material_color(project, material, SurfaceRole::Object);
    let translucent = material_is_translucent(project, material);
    painter.rect_filled(rect, 0.0, color);
    if translucent {
        draw_glass_marker(painter, rect);
    }
    painter.rect_stroke(rect, 0.0, selected_stroke(selected), StrokeKind::Outside);
    painter.text(
        rect.center_top() + Vec2::new(0.0, -6.0),
        Align2::CENTER_BOTTOM,
        &node.name,
        FontId::monospace(11.0),
        Color32::from_rgb(232, 238, 246),
    );
    hits.push(ViewportHit::rect(node.id, node.name.clone(), center, half));
}

pub(crate) fn draw_glass_marker(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(
        rect.shrink(1.0),
        0.0,
        Color32::from_rgba_unmultiplied(190, 230, 232, 34),
    );
    let sheen = Stroke::new(1.0, Color32::from_rgba_unmultiplied(222, 252, 255, 120));
    let step = 18.0;
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        painter.line_segment(
            [
                Pos2::new(x, rect.bottom()),
                Pos2::new((x + rect.height()).min(rect.right()), rect.top()),
            ],
            sheen,
        );
        x += step;
    }
}

pub(crate) fn draw_spawn_marker(
    painter: &egui::Painter,
    transform: ViewportTransform,
    node: &psxed_project::SceneNode,
    selected: bool,
    hits: &mut Vec<ViewportHit>,
) {
    draw_simple_marker(
        painter,
        transform,
        node,
        selected,
        "P",
        Color32::from_rgb(82, 184, 118),
        hits,
    );
}

pub(crate) fn draw_light_marker(
    painter: &egui::Painter,
    transform: ViewportTransform,
    node: &psxed_project::SceneNode,
    selected: bool,
    color: [u8; 3],
    intensity: f32,
    radius: f32,
    hits: &mut Vec<ViewportHit>,
) {
    let center = node_world(node);
    let world_radius = (radius / 4096.0).clamp(0.45, 2.5) * intensity.max(0.25);
    let screen_center = transform.world_to_screen(center);
    painter.circle_filled(
        screen_center,
        transform.screen_radius(world_radius),
        Color32::from_rgba_unmultiplied(color[0], color[1], color[2], 28),
    );
    let fill = Color32::from_rgb(color[0], color[1], color[2]);
    let icon_radius = transform.screen_radius(0.18).max(8.0);
    draw_light_bulb_marker(painter, screen_center, icon_radius, fill, selected);
    painter.text(
        screen_center + Vec2::new(0.0, 16.0),
        Align2::CENTER_TOP,
        &node.name,
        FontId::monospace(10.0),
        Color32::from_rgb(220, 228, 238),
    );
    hits.push(ViewportHit::circle(
        node.id,
        node.name.clone(),
        center,
        0.18_f32.max(8.0 / transform.zoom),
    ));
}

pub(crate) fn draw_light_bulb_marker(
    painter: &egui::Painter,
    center: Pos2,
    radius: f32,
    fill: Color32,
    selected: bool,
) {
    let glass_center = center + Vec2::new(0.0, -radius * 0.25);
    let glass_radius = radius * 0.72;
    let glass_fill = Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 224);
    let stroke = selected_stroke(selected);
    painter.circle_filled(glass_center, glass_radius, glass_fill);
    painter.circle_stroke(glass_center, glass_radius, stroke);

    let base = Rect::from_center_size(
        center + Vec2::new(0.0, radius * 0.52),
        Vec2::new(radius * 0.86, radius * 0.52),
    );
    painter.rect_filled(base, 2.0, darken(fill, 46));
    painter.rect_stroke(base, 2.0, stroke, StrokeKind::Outside);

    let filament = Stroke::new(1.0, Color32::from_rgba_unmultiplied(18, 20, 24, 190));
    let y = glass_center.y + glass_radius * 0.18;
    let left = glass_center.x - glass_radius * 0.38;
    let right = glass_center.x + glass_radius * 0.38;
    let mid = glass_center.x;
    painter.line_segment(
        [Pos2::new(left, y), Pos2::new(mid, y + glass_radius * 0.18)],
        filament,
    );
    painter.line_segment(
        [Pos2::new(mid, y + glass_radius * 0.18), Pos2::new(right, y)],
        filament,
    );
}

pub(crate) fn draw_portal_seam_2d(
    painter: &egui::Painter,
    transform: ViewportTransform,
    scene: &psxed_project::Scene,
    node: &psxed_project::SceneNode,
    selected: bool,
    hits: &mut Vec<ViewportHit>,
) {
    let Some(room_id) = enclosing_room_id(scene, node.id) else {
        return;
    };
    let Some(room) = scene.node(room_id) else {
        return;
    };
    let NodeKind::Section { grid } = &room.kind else {
        return;
    };
    let seam = portal_seam_edges_for_node(grid, node);
    if seam.is_empty() {
        return;
    }
    let room_center = node_world(room);
    let halo = Stroke::new(
        if selected { 9.0 } else { 7.0 },
        Color32::from_rgba_unmultiplied(PORTAL_PINK.r(), PORTAL_PINK.g(), PORTAL_PINK.b(), 54),
    );
    let stroke = Stroke::new(if selected { 4.0 } else { 3.0 }, PORTAL_PINK);
    for edge in seam {
        if let Some((local_a, local_b)) = portal_edge_editor_segment(grid, edge) {
            let a = [room_center[0] + local_a[0], room_center[1] + local_a[1]];
            let b = [room_center[0] + local_b[0], room_center[1] + local_b[1]];
            hits.push(ViewportHit::segment(
                node.id,
                node.name.clone(),
                a,
                b,
                0.12_f32.max(7.0 / transform.zoom),
            ));
        }
        draw_portal_edge_segment_2d(painter, transform, grid, room_center, edge, halo, stroke);
    }
    if let Some(edge) = portal_edge_for_node(grid, node) {
        if let Some((a, b)) = portal_edge_screen_segment(transform, grid, room_center, edge) {
            let mid = Pos2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            painter.circle_filled(mid, 4.5, PORTAL_PINK);
            painter.circle_stroke(mid, 5.5, Stroke::new(1.0, Color32::from_rgb(28, 8, 24)));
        }
    }
}

pub(crate) fn portal_edge_editor_segment(
    grid: &WorldGrid,
    edge: PortalEdge,
) -> Option<([f32; 2], [f32; 2])> {
    portal_edge_editor_segment_for_array(grid, edge.x, edge.z, edge.direction)
}

pub(crate) fn draw_portal_edge_segment_2d(
    painter: &egui::Painter,
    transform: ViewportTransform,
    grid: &WorldGrid,
    room_center: [f32; 2],
    edge: PortalEdge,
    halo: Stroke,
    stroke: Stroke,
) {
    let Some((a, b)) = portal_edge_screen_segment(transform, grid, room_center, edge) else {
        return;
    };
    painter.line_segment([a, b], halo);
    painter.line_segment([a, b], stroke);
}

pub(crate) fn portal_edge_screen_segment(
    transform: ViewportTransform,
    grid: &WorldGrid,
    room_center: [f32; 2],
    edge: PortalEdge,
) -> Option<(Pos2, Pos2)> {
    let (local_a, local_b) = portal_edge_editor_segment(grid, edge)?;
    Some((
        transform.world_to_screen([room_center[0] + local_a[0], room_center[1] + local_a[1]]),
        transform.world_to_screen([room_center[0] + local_b[0], room_center[1] + local_b[1]]),
    ))
}

pub(crate) fn portal_edge_editor_segment_for_array(
    grid: &WorldGrid,
    sx: u16,
    sz: u16,
    dir: GridDirection,
) -> Option<([f32; 2], [f32; 2])> {
    let wcx = grid.origin[0] + sx as i32;
    let wcz = grid.origin[1] + sz as i32;
    portal_edge_editor_segment_for_world_cell(grid, wcx, wcz, dir)
}

pub(crate) fn portal_edge_editor_segment_for_world_cell(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
) -> Option<([f32; 2], [f32; 2])> {
    let world = match dir {
        GridDirection::North => (
            [wcx as f32, wcz as f32 + 1.0],
            [wcx as f32 + 1.0, wcz as f32 + 1.0],
        ),
        GridDirection::East => (
            [wcx as f32 + 1.0, wcz as f32],
            [wcx as f32 + 1.0, wcz as f32 + 1.0],
        ),
        GridDirection::South => ([wcx as f32, wcz as f32], [wcx as f32 + 1.0, wcz as f32]),
        GridDirection::West => ([wcx as f32, wcz as f32], [wcx as f32, wcz as f32 + 1.0]),
        GridDirection::NorthWestSouthEast | GridDirection::NorthEastSouthWest => return None,
    };
    Some((
        grid.world_cells_to_editor(world.0),
        grid.world_cells_to_editor(world.1),
    ))
}

pub(crate) fn draw_portal_marker(
    painter: &egui::Painter,
    transform: ViewportTransform,
    scene: &psxed_project::Scene,
    node: &psxed_project::SceneNode,
    selected: bool,
    hits: &mut Vec<ViewportHit>,
) {
    let center = portal_marker_world_2d(scene, node);
    let screen = transform.world_to_screen(center);
    let radius = transform.screen_radius(0.2).max(9.0);
    let points = vec![
        screen + Vec2::new(0.0, -radius),
        screen + Vec2::new(radius, 0.0),
        screen + Vec2::new(0.0, radius),
        screen + Vec2::new(-radius, 0.0),
    ];
    let fill =
        Color32::from_rgba_unmultiplied(PORTAL_PINK.r(), PORTAL_PINK.g(), PORTAL_PINK.b(), 64);
    painter.add(egui::Shape::convex_polygon(
        points,
        fill,
        Stroke::new(if selected { 3.0 } else { 1.5 }, PORTAL_PINK),
    ));
    painter.text(
        screen,
        Align2::CENTER_CENTER,
        "P",
        FontId::monospace(12.0),
        Color32::WHITE,
    );
    painter.text(
        screen + Vec2::new(0.0, radius + 5.0),
        Align2::CENTER_TOP,
        &node.name,
        FontId::monospace(10.0),
        PORTAL_PINK,
    );
    hits.push(ViewportHit::circle(
        node.id,
        node.name.clone(),
        center,
        0.2_f32.max(radius / transform.zoom),
    ));
}

pub(crate) fn portal_marker_world_2d(
    scene: &psxed_project::Scene,
    node: &psxed_project::SceneNode,
) -> [f32; 2] {
    let local = node_world(node);
    let Some(room_id) = enclosing_room_id(scene, node.id) else {
        return local;
    };
    let Some(room) = scene.node(room_id) else {
        return local;
    };
    let room_center = node_world(room);
    [room_center[0] + local[0], room_center[1] + local[1]]
}

pub(crate) fn draw_simple_marker(
    painter: &egui::Painter,
    transform: ViewportTransform,
    node: &psxed_project::SceneNode,
    selected: bool,
    label: &str,
    fill: Color32,
    hits: &mut Vec<ViewportHit>,
) {
    let center = node_world(node);
    let screen = transform.world_to_screen(center);
    let radius = 0.18;
    painter.circle_filled(screen, transform.screen_radius(radius).max(8.0), fill);
    painter.circle_stroke(
        screen,
        transform.screen_radius(radius).max(8.0),
        selected_stroke(selected),
    );
    painter.text(
        screen,
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(12.0),
        Color32::from_rgb(8, 10, 13),
    );
    painter.text(
        screen + Vec2::new(0.0, 16.0),
        Align2::CENTER_TOP,
        &node.name,
        FontId::monospace(10.0),
        Color32::from_rgb(220, 228, 238),
    );
    hits.push(ViewportHit::circle(
        node.id,
        node.name.clone(),
        center,
        radius.max(8.0 / transform.zoom),
    ));
}
