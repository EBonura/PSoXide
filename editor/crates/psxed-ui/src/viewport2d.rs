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
        format_viewport_zoom(zoom),
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

pub(crate) fn format_viewport_zoom(zoom: f32) -> String {
    if zoom >= 10.0 {
        format!("{zoom:.0} px/unit")
    } else if zoom >= 1.0 {
        format!("{zoom:.1} px/unit")
    } else {
        format!("{zoom:.3} px/unit")
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
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum HitShape {
    Rect { center: [f32; 2], half: [f32; 2] },
    Circle { center: [f32; 2], radius: f32 },
}

pub(crate) fn readable_grid_step(base_step: f32, zoom: f32) -> f32 {
    let mut step = base_step.max(f32::EPSILON);
    while step * zoom < 10.0 && step <= (i32::MAX as f32) * 0.5 {
        step *= 2.0;
    }
    step
}

pub(crate) fn draw_world_grid(
    painter: &egui::Painter,
    transform: ViewportTransform,
    base_step: f32,
) {
    let rect = transform.rect;
    let top_left = transform.screen_to_world(rect.left_top());
    let bottom_right = transform.screen_to_world(rect.right_bottom());
    let step = readable_grid_step(base_step, transform.zoom);
    let min_x = (top_left[0].min(bottom_right[0]) / step).floor() as i32 - 1;
    let max_x = (top_left[0].max(bottom_right[0]) / step).ceil() as i32 + 1;
    let min_z = (top_left[1].min(bottom_right[1]) / step).floor() as i32 - 1;
    let max_z = (top_left[1].max(bottom_right[1]) / step).ceil() as i32 + 1;

    for x in min_x..=max_x {
        let a = transform.world_to_screen([x as f32 * step, min_z as f32 * step]);
        let b = transform.world_to_screen([x as f32 * step, max_z as f32 * step]);
        painter.line_segment([a, b], world_grid_stroke(x));
    }

    for z in min_z..=max_z {
        let a = transform.world_to_screen([min_x as f32 * step, z as f32 * step]);
        let b = transform.world_to_screen([max_x as f32 * step, z as f32 * step]);
        painter.line_segment([a, b], world_grid_stroke(z));
    }
}

fn world_grid_stroke(index: i32) -> Stroke {
    let color = if index == 0 {
        Color32::from_rgb(58, 91, 103)
    } else if index % 4 == 0 {
        Color32::from_rgb(31, 63, 75)
    } else {
        Color32::from_rgb(20, 43, 52)
    };
    Stroke::new(1.0, color)
}

/// Reapply the active world grid inside one projected brush face.
///
/// TrenchBroom renders its background grid first, then evaluates the same
/// world-space grid in the brush-face shader so texture/fill pixels cannot
/// hide it. PSoXide's orthographic brush pass is host-drawn, so the equivalent
/// is to clip the existing global grid to each convex face after face fills and
/// before outlines and edit handles. Coordinates remain anchored at world zero;
/// this is deliberately not a face-local grid.
pub(crate) fn draw_world_grid_on_convex_polygon(
    painter: &egui::Painter,
    transform: ViewportTransform,
    base_step: f32,
    polygon: &[[f64; 2]],
) {
    if polygon.len() < 3 || projected_polygon_area_f64(polygon).abs() <= f64::EPSILON {
        return;
    }
    let step = f64::from(readable_grid_step(base_step, transform.zoom));
    let (min_x, max_x) = polygon
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point[0]), max.max(point[0]))
        });
    let (min_y, max_y) = polygon
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), point| {
            (min.min(point[1]), max.max(point[1]))
        });
    let view_a = transform.screen_to_world(transform.rect.left_top());
    let view_b = transform.screen_to_world(transform.rect.right_bottom());
    let min_x = min_x.max(f64::from(view_a[0].min(view_b[0])));
    let max_x = max_x.min(f64::from(view_a[0].max(view_b[0])));
    let min_y = min_y.max(f64::from(view_a[1].min(view_b[1])));
    let max_y = max_y.min(f64::from(view_a[1].max(view_b[1])));
    if ![min_x, max_x, min_y, max_y, step]
        .into_iter()
        .all(f64::is_finite)
        || step <= 0.0
    {
        return;
    }

    let x_first = (min_x / step).ceil() as i32;
    let x_last = (max_x / step).floor() as i32;
    for index in x_first..=x_last {
        let coordinate = f64::from(index) * step;
        if let Some((a, b)) = clip_axis_line_to_convex_polygon(polygon, 0, coordinate) {
            painter.line_segment(
                [
                    transform.world_to_screen(a.map(|value| value as f32)),
                    transform.world_to_screen(b.map(|value| value as f32)),
                ],
                world_grid_stroke(index),
            );
        }
    }

    let y_first = (min_y / step).ceil() as i32;
    let y_last = (max_y / step).floor() as i32;
    for index in y_first..=y_last {
        let coordinate = f64::from(index) * step;
        if let Some((a, b)) = clip_axis_line_to_convex_polygon(polygon, 1, coordinate) {
            painter.line_segment(
                [
                    transform.world_to_screen(a.map(|value| value as f32)),
                    transform.world_to_screen(b.map(|value| value as f32)),
                ],
                world_grid_stroke(index),
            );
        }
    }
}

fn projected_polygon_area_f64(polygon: &[[f64; 2]]) -> f64 {
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .take(polygon.len())
        .map(|(a, b)| a[0] * b[1] - a[1] * b[0])
        .sum::<f64>()
        * 0.5
}

fn clip_axis_line_to_convex_polygon(
    polygon: &[[f64; 2]],
    axis: usize,
    coordinate: f64,
) -> Option<([f64; 2], [f64; 2])> {
    const EPSILON: f64 = 1.0 / 4096.0;
    let mut intersections = Vec::with_capacity(polygon.len().min(8));
    let mut push_unique = |candidate: [f64; 2]| {
        if intersections.iter().all(|point: &[f64; 2]| {
            (point[0] - candidate[0]).powi(2) + (point[1] - candidate[1]).powi(2)
                > EPSILON * EPSILON
        }) {
            intersections.push(candidate);
        }
    };
    for edge in 0..polygon.len() {
        let a = polygon[edge];
        let b = polygon[(edge + 1) % polygon.len()];
        let da = a[axis] - coordinate;
        let db = b[axis] - coordinate;
        if da.abs() <= EPSILON {
            push_unique(a);
        }
        if db.abs() <= EPSILON {
            push_unique(b);
        }
        if (da < -EPSILON && db > EPSILON) || (da > EPSILON && db < -EPSILON) {
            let t = da / (da - db);
            push_unique([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
        }
    }
    if intersections.len() < 2 {
        return None;
    }
    let mut farthest = (0, 1);
    let mut farthest_distance = 0.0;
    for a in 0..intersections.len() {
        for b in (a + 1)..intersections.len() {
            let distance = (intersections[a][0] - intersections[b][0]).powi(2)
                + (intersections[a][1] - intersections[b][1]).powi(2);
            if distance > farthest_distance {
                farthest = (a, b);
                farthest_distance = distance;
            }
        }
    }
    (farthest_distance > EPSILON * EPSILON)
        .then(|| (intersections[farthest.0], intersections[farthest.1]))
}

// Selection and visibility state for one 2D BSP scene-viewport pass.
pub(crate) struct SceneViewportContext<'a> {
    pub(crate) hidden_scene_nodes: &'a HashSet<NodeId>,
    pub(crate) selected: NodeId,
    pub(crate) selected_nodes: &'a HashSet<NodeId>,
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
        show_lights,
    } = ctx;
    let scene = project.active_scene();
    let mut hits = Vec::new();

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

pub(crate) fn portal_edge_editor_segment(
    grid: &WorldGrid,
    edge: PortalEdge,
) -> Option<([f32; 2], [f32; 2])> {
    portal_edge_editor_segment_for_array(grid, edge.x, edge.z, edge.direction)
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

#[cfg(test)]
mod brush_surface_grid_tests {
    use super::*;

    #[test]
    fn surface_grid_line_is_clipped_to_the_projected_face() {
        let polygon = [[5.0, 7.0], [37.0, 7.0], [37.0, 39.0], [5.0, 39.0]];
        let (a, b) = clip_axis_line_to_convex_polygon(&polygon, 0, 16.0)
            .expect("world-aligned line crosses the brush face");
        assert_eq!(a[0], 16.0);
        assert_eq!(b[0], 16.0);
        assert_eq!([a[1].min(b[1]), a[1].max(b[1])], [7.0, 39.0]);
    }

    #[test]
    fn surface_grid_handles_a_line_coincident_with_a_face_edge() {
        let polygon = [[5.0, 7.0], [37.0, 7.0], [37.0, 39.0], [5.0, 39.0]];
        let (a, b) = clip_axis_line_to_convex_polygon(&polygon, 1, 7.0)
            .expect("face edge is also a valid global grid segment");
        assert_eq!(a[1], 7.0);
        assert_eq!(b[1], 7.0);
        assert_eq!([a[0].min(b[0]), a[0].max(b[0])], [5.0, 37.0]);
    }

    #[test]
    fn surface_grid_uses_the_same_readable_interval_as_the_background() {
        assert_eq!(readable_grid_step(16.0, 1.0), 16.0);
        assert_eq!(readable_grid_step(16.0, 0.25), 64.0);
        let polygon_min = 5.0_f64;
        let first_global_line = (polygon_min / 16.0).ceil() * 16.0;
        assert_eq!(first_global_line, 16.0, "grid must not restart at the face");
    }
}
