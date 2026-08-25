use super::*;

#[derive(Debug, Clone, Copy)]
pub(crate) enum SurfaceRole {
    Object,
}

pub(crate) fn material_color(
    project: &ProjectDocument,
    material: Option<ResourceId>,
    role: SurfaceRole,
) -> Color32 {
    let Some(id) = material else {
        return match role {
            SurfaceRole::Object => Color32::from_rgb(125, 155, 190),
        };
    };
    let Some(resource) = project.resource(id) else {
        return Color32::from_rgb(150, 80, 120);
    };

    let name = resource.name.to_ascii_lowercase();
    let mut color = if name.contains("brick") {
        Color32::from_rgb(126, 72, 43)
    } else if name.contains("floor") || name.contains("stone") {
        STUDIO_ROOM_FLOOR
    } else if name.contains("glass") {
        Color32::from_rgba_unmultiplied(122, 176, 198, 118)
    } else {
        match role {
            SurfaceRole::Object => Color32::from_rgb(125, 155, 190),
        }
    };

    if let ResourceData::Material(material) = &resource.data {
        if material.blend_mode != PsxBlendMode::Opaque {
            color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 132);
        }
    }

    color
}

pub(crate) fn material_is_translucent(
    project: &ProjectDocument,
    material: Option<ResourceId>,
) -> bool {
    material
        .and_then(|id| project.resource(id))
        .is_some_and(|resource| match &resource.data {
            ResourceData::Material(material) => material.blend_mode != PsxBlendMode::Opaque,
            _ => false,
        })
}

pub(crate) fn selected_stroke(selected: bool) -> Stroke {
    if selected {
        Stroke::new(EDITOR_SELECTED_OUTLINE_STROKE_WIDTH, EDITOR_OUTLINE_GOLD)
    } else {
        Stroke::new(1.0, Color32::from_rgb(70, 84, 108))
    }
}

pub(crate) fn node_world(node: &psxed_project::SceneNode) -> [f32; 2] {
    [node.transform.translation[0], node.transform.translation[2]]
}

pub(crate) fn room_grid_center_cells(
    scene: &psxed_project::Scene,
    room: NodeId,
) -> Option<[f32; 2]> {
    let node = scene.node(room)?;
    let NodeKind::Section { grid } = &node.kind else {
        return None;
    };
    Some(grid.grid_center_cells())
}

pub(crate) fn node_kind_uses_room_editor_position(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Entity
            | NodeKind::MeshInstance { .. }
            | NodeKind::ImageProp { .. }
            | NodeKind::BoxProp { .. }
            | NodeKind::CylinderProp { .. }
            | NodeKind::SpawnPoint { .. }
            | NodeKind::PointLight { .. }
            | NodeKind::Portal { .. }
    )
}

pub(crate) fn recenter_room_spatial_descendants(
    scene: &mut psxed_project::Scene,
    room: NodeId,
    old_center: [f32; 2],
) {
    let Some(new_center) = room_grid_center_cells(scene, room) else {
        return;
    };
    let delta = [old_center[0] - new_center[0], old_center[1] - new_center[1]];
    if delta[0] == 0.0 && delta[1] == 0.0 {
        return;
    }
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|node| node.id != room)
        .filter(|node| scene.is_descendant_of(node.id, room))
        .filter(|node| node_kind_uses_room_editor_position(&node.kind))
        .map(|node| node.id)
        .collect();
    for id in ids {
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation[0] += delta[0];
            node.transform.translation[2] += delta[1];
        }
    }
}

pub(crate) fn extend_room_grid_to_include_preserving_child_positions(
    scene: &mut psxed_project::Scene,
    room: NodeId,
    wcx: i32,
    wcz: i32,
    active_floor: usize,
) -> Option<(u16, u16)> {
    let old_center = room_grid_center_cells(scene, room)?;
    let cell = {
        let node = scene.node_mut(room)?;
        let NodeKind::Section { grid } = &mut node.kind else {
            return None;
        };
        let idx = active_floor.min(grid.floor_count().saturating_sub(1));
        grid.floor_mut(idx)?.extend_to_include(wcx, wcz)
    };
    recenter_room_spatial_descendants(scene, room, old_center);
    Some(cell)
}

pub(crate) fn resize_room_grid_preserving_child_positions(
    scene: &mut psxed_project::Scene,
    room: NodeId,
    width: u16,
    depth: u16,
    active_floor: usize,
) -> bool {
    let Some(old_center) = room_grid_center_cells(scene, room) else {
        return false;
    };
    let resized = {
        let Some(node) = scene.node_mut(room) else {
            return false;
        };
        let NodeKind::Section { grid } = &mut node.kind else {
            return false;
        };
        let idx = active_floor.min(grid.floor_count().saturating_sub(1));
        let Some(grid) = grid.floor_mut(idx) else {
            return false;
        };
        if grid.width == width && grid.depth == depth {
            return false;
        }
        grid.resize(width, depth);
        true
    };
    if resized {
        recenter_room_spatial_descendants(scene, room, old_center);
    }
    resized
}

pub(crate) fn grid_cell_editor_center(grid: &WorldGrid, sx: u16, sz: u16) -> [f32; 2] {
    [
        sx as f32 + 0.5 - grid.width as f32 * 0.5,
        sz as f32 + 0.5 - grid.depth as f32 * 0.5,
    ]
}

pub(crate) fn grid_rect_editor_center_half(
    grid: &WorldGrid,
    array_origin: [u16; 2],
    size: [u16; 2],
) -> ([f32; 2], [f32; 2]) {
    let half = [size[0] as f32 * 0.5, size[1] as f32 * 0.5];
    (
        [
            array_origin[0] as f32 + half[0] - grid.width as f32 * 0.5,
            array_origin[1] as f32 + half[1] - grid.depth as f32 * 0.5,
        ],
        half,
    )
}

pub(crate) fn grid_authored_editor_center_half(grid: &WorldGrid) -> Option<([f32; 2], [f32; 2])> {
    let footprint = grid.authored_footprint()?;
    Some(grid_rect_editor_center_half(
        grid,
        [footprint.x, footprint.z],
        [footprint.width, footprint.depth],
    ))
}

pub(crate) fn merge_bounds(
    bounds: &mut Option<(f32, f32, f32, f32)>,
    center: [f32; 2],
    half: [f32; 2],
) {
    let next = (
        center[0] - half[0],
        center[1] - half[1],
        center[0] + half[0],
        center[1] + half[1],
    );
    *bounds = Some(match *bounds {
        Some((min_x, min_z, max_x, max_z)) => (
            min_x.min(next.0),
            min_z.min(next.1),
            max_x.max(next.2),
            max_z.max(next.3),
        ),
        None => next,
    });
}

pub(crate) fn bounds_to_center_half(bounds: (f32, f32, f32, f32)) -> ([f32; 2], [f32; 2]) {
    let (min_x, min_z, max_x, max_z) = bounds;
    (
        [(min_x + max_x) * 0.5, (min_z + max_z) * 0.5],
        [(max_x - min_x) * 0.5, (max_z - min_z) * 0.5],
    )
}

pub(crate) fn merge_bounds_3d(
    bounds: &mut Option<(f32, f32, f32, f32, f32, f32)>,
    center: [f32; 3],
    half: [f32; 3],
) {
    let next = (
        center[0] - half[0],
        center[1] - half[1],
        center[2] - half[2],
        center[0] + half[0],
        center[1] + half[1],
        center[2] + half[2],
    );
    *bounds = Some(match *bounds {
        Some((min_x, min_y, min_z, max_x, max_y, max_z)) => (
            min_x.min(next.0),
            min_y.min(next.1),
            min_z.min(next.2),
            max_x.max(next.3),
            max_y.max(next.4),
            max_z.max(next.5),
        ),
        None => next,
    });
}

pub(crate) fn bounds_3d_to_center_half(
    bounds: (f32, f32, f32, f32, f32, f32),
) -> ([f32; 3], [f32; 3]) {
    let (min_x, min_y, min_z, max_x, max_y, max_z) = bounds;
    (
        [
            (min_x + max_x) * 0.5,
            (min_y + max_y) * 0.5,
            (min_z + max_z) * 0.5,
        ],
        [
            (max_x - min_x) * 0.5,
            (max_y - min_y) * 0.5,
            (max_z - min_z) * 0.5,
        ],
    )
}

pub(crate) fn command_shortcut(key: egui::Key) -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key)
}

pub(crate) fn command_shift_shortcut(key: egui::Key) -> egui::KeyboardShortcut {
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT), key)
}

pub(crate) fn consume_command_shortcut(ctx: &egui::Context, key: egui::Key) -> bool {
    let shortcut = command_shortcut(key);
    ctx.input_mut(|input| consume_shortcut_once(input, &shortcut))
}

/// Consume Copy through either a synthetic key event or the platform event
/// emitted by egui-winit. Native integrations translate Cmd/Ctrl+C into
/// `Event::Copy` before editor code sees it, so listening for `Key::C` alone
/// only works in tests that bypass the real window event path.
pub(crate) fn consume_copy_shortcut(ctx: &egui::Context) -> bool {
    consume_command_shortcut(ctx, egui::Key::C)
        || ctx.input_mut(|input| consume_platform_clipboard_event(input, true))
}

/// Consume Paste through either a synthetic key event or egui-winit's native
/// `Event::Paste`. The event text belongs to the OS clipboard; PSoXide keeps
/// its geometry payload internally and uses the event only as shortcut intent.
pub(crate) fn consume_paste_shortcut(ctx: &egui::Context) -> bool {
    consume_command_shortcut(ctx, egui::Key::V)
        || ctx.input_mut(|input| consume_platform_clipboard_event(input, false))
}

fn consume_platform_clipboard_event(input: &mut egui::InputState, copy: bool) -> bool {
    let mut triggered = false;
    input.events.retain(|event| {
        let matches = if copy {
            matches!(event, egui::Event::Copy)
        } else {
            matches!(event, egui::Event::Paste(_))
        };
        triggered |= matches;
        !matches
    });
    triggered
}

pub(crate) fn consume_command_shift_shortcut(ctx: &egui::Context, key: egui::Key) -> bool {
    let shortcut = command_shift_shortcut(key);
    ctx.input_mut(|input| consume_shortcut_once(input, &shortcut))
}

pub(crate) fn consume_shortcut_once(
    input: &mut egui::InputState,
    shortcut: &egui::KeyboardShortcut,
) -> bool {
    let egui::KeyboardShortcut {
        modifiers,
        logical_key,
    } = *shortcut;
    let mut triggered = false;
    input.events.retain(|event| {
        let is_match = matches!(
            event,
            egui::Event::Key {
                key,
                modifiers: event_modifiers,
                pressed: true,
                ..
            } if *key == logical_key && event_modifiers.matches_logically(modifiers)
        );
        if !is_match {
            return true;
        }
        if matches!(event, egui::Event::Key { repeat: false, .. }) {
            triggered = true;
        }
        false
    });
    triggered
}

pub(crate) fn consume_command_cycle_shortcut(ctx: &egui::Context, key: egui::Key) -> Option<bool> {
    if consume_command_shift_shortcut(ctx, key) {
        Some(true)
    } else if consume_command_shortcut(ctx, key) {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn command_shortcut_text(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("Cmd+{key}")
    } else {
        format!("Ctrl+{key}")
    }
}

pub(crate) fn command_shift_shortcut_text(key: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("Shift+Cmd+{key}")
    } else {
        format!("Shift+Ctrl+{key}")
    }
}

pub(crate) fn menu_label(label: &str, shortcut: &str) -> String {
    format!("{label}    {shortcut}")
}

pub(crate) fn bare_shortcuts_available(focus_taken: bool, modifiers: egui::Modifiers) -> bool {
    !focus_taken && !modifiers.command && !modifiers.ctrl
}

/// Whether a widget or selectable label owns keyboard shortcuts this frame.
///
/// Selectable labels intentionally do not take egui keyboard focus, but their
/// selection state still needs to receive the platform `Copy` event.
pub(crate) fn widget_owns_keyboard_shortcuts(ctx: &egui::Context) -> bool {
    ctx.memory(|memory| memory.focused().is_some())
        || ctx.wants_keyboard_input()
        || egui::text_selection::LabelSelectionState::load(ctx).has_selection()
}

/// A text field can keep egui keyboard focus after the user has returned to
/// editing the world. Clear that stale focus on a viewport press so the next
/// frame's editor shortcuts are routed to the selected geometry.
pub(crate) fn surrender_stale_focus_on_viewport_pointer(
    ctx: &egui::Context,
    response: &egui::Response,
) {
    if !response.hovered() || !ctx.input(|input| input.pointer.any_pressed()) {
        return;
    }
    if let Some(focused) = ctx.memory(|memory| memory.focused()) {
        ctx.memory_mut(|memory| memory.surrender_focus(focused));
    }
}

pub(crate) fn cycle_value<T: Copy + PartialEq>(values: &[T], current: T, reverse: bool) -> T {
    let Some(index) = values.iter().position(|value| *value == current) else {
        return values.first().copied().unwrap_or(current);
    };
    if values.is_empty() {
        return current;
    }
    let next = if reverse {
        (index + values.len() - 1) % values.len()
    } else {
        (index + 1) % values.len()
    };
    values[next]
}

pub(crate) fn frame_radius_for_3d_bounds(half: [f32; 3]) -> i32 {
    let extent = half[0].max(half[1]).max(half[2]).max(128.0);
    (extent * 3.2).clamp(512.0, 262_144.0) as i32
}

pub(crate) fn orbit_camera_position_f32(
    yaw_q12: u16,
    pitch_q12: u16,
    radius: i32,
    target: [i32; 3],
) -> [f32; 3] {
    let radius = radius as f32;
    let cos_p = cos_q12_turn_f32(pitch_q12);
    let sin_p = sin_q12_turn_f32(pitch_q12);
    let cos_y = cos_q12_turn_f32(yaw_q12);
    let sin_y = sin_q12_turn_f32(yaw_q12);
    [
        target[0] as f32 + radius * cos_p * sin_y,
        target[1] as f32 - radius * sin_p,
        target[2] as f32 + radius * cos_p * cos_y,
    ]
}

pub(crate) fn orbit_camera_position_i32(
    yaw_q12: u16,
    pitch_q12: u16,
    radius: i32,
    target: [i32; 3],
) -> [i32; 3] {
    orbit_camera_position_f32(yaw_q12, pitch_q12, radius, target).map(round_to_i32)
}

pub(crate) fn camera_forward_from_angles(yaw_q12: u16, pitch_q12: u16) -> [f32; 3] {
    let cos_p = cos_q12_turn_f32(pitch_q12);
    let sin_p = sin_q12_turn_f32(pitch_q12);
    let cos_y = cos_q12_turn_f32(yaw_q12);
    let sin_y = sin_q12_turn_f32(yaw_q12);
    normalize3([-cos_p * sin_y, sin_p, -cos_p * cos_y])
}

pub(crate) fn camera_pitch_q12_from_vertical_distance(vertical: i32, distance: i32) -> i16 {
    if vertical == 0 {
        return 0;
    }
    let ay = vertical.saturating_abs();
    let ax = distance.saturating_abs().max(1);
    let base = if ay <= ax {
        ay.saturating_mul(512) / ax
    } else {
        1024 - (ax.saturating_mul(512) / ay.max(1))
    }
    .min(1024);
    let signed = if vertical < 0 { -base } else { base };
    signed.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub(crate) fn sin_q12_turn_f32(angle_q12: u16) -> f32 {
    psx_engine::Angle::from_q12(angle_q12).sin().raw() as f32 / 4096.0
}

pub(crate) fn cos_q12_turn_f32(angle_q12: u16) -> f32 {
    psx_engine::Angle::from_q12(angle_q12).cos().raw() as f32 / 4096.0
}

pub(crate) fn distance3_f32(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    dx.mul_add(dx, dy.mul_add(dy, dz * dz)).sqrt()
}

pub(crate) fn camera_angles_to_look_at(position: [i32; 3], target: [i32; 3]) -> Option<(u16, u16)> {
    let dx = (target[0] - position[0]) as f32;
    let dy = (target[1] - position[1]) as f32;
    let dz = (target[2] - position[2]) as f32;
    let len = dx.mul_add(dx, dy.mul_add(dy, dz * dz)).sqrt();
    if !len.is_finite() || len <= f32::EPSILON {
        return None;
    }

    let dir = [dx / len, dy / len, dz / len];
    let yaw = q12_from_radians((-dir[0]).atan2(-dir[2]));
    let pitch =
        signed_to_q12(q12_signed_from_radians(dir[1].clamp(-1.0, 1.0).asin()).clamp(-960, 960));
    Some((yaw, pitch))
}

pub(crate) fn q12_from_radians(radians: f32) -> u16 {
    ((radians * (4096.0 / std::f32::consts::TAU)).round() as i32).rem_euclid(4096) as u16
}

pub(crate) fn q12_signed_from_radians(radians: f32) -> i32 {
    (radians * (4096.0 / std::f32::consts::TAU)).round() as i32
}

pub(crate) fn viewport_3d_pan_delta(
    camera: ViewportCameraState,
    panel_size: Vec2,
    pointer_delta: Vec2,
) -> [f32; 3] {
    let basis = camera.basis();
    let width = panel_size.x.max(1.0);
    let height = panel_size.y.max(1.0);
    let radius = (camera.radius as f32).max(1.0);
    let right = -pointer_delta.x * radius / width;
    let up = pointer_delta.y * radius * 0.75 / height;
    [
        basis.right[0] * right + basis.up[0] * up,
        basis.right[1] * right + basis.up[1] * up,
        basis.right[2] * right + basis.up[2] * up,
    ]
}

pub(crate) fn project_world_to_viewport_screen(
    camera: ViewportCameraState,
    viewport: Rect,
    world: [f32; 3],
) -> Option<Pos2> {
    let basis = camera.basis();
    let rel = sub3(world, basis.position);
    let depth = dot3(rel, basis.forward);
    if !depth.is_finite() || depth <= 1.0 {
        return None;
    }

    let half_fov_x: f32 = 0.5;
    let half_fov_y: f32 = 0.5 * 240.0 / 320.0;
    let nx = dot3(rel, basis.right) / (depth * half_fov_x);
    let ny = -dot3(rel, basis.up) / (depth * half_fov_y);
    if !nx.is_finite() || !ny.is_finite() {
        return None;
    }

    Some(Pos2::new(
        viewport.center().x + nx * viewport.width() * 0.5,
        viewport.center().y + ny * viewport.height() * 0.5,
    ))
}

pub(crate) fn distance_to_segment_2d(point: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let len_sq = ab.length_sq();
    if len_sq <= f32::EPSILON {
        return (point - a).length();
    }
    let t = ((point - a).dot(ab) / len_sq).clamp(0.0, 1.0);
    let closest = a + ab * t;
    (point - closest).length()
}

pub(crate) fn polygon_area_2d(points: &[Pos2]) -> f32 {
    if points.len() < 3 {
        return 0.0;
    }
    let mut area = 0.0;
    for i in 0..points.len() {
        let a = points[i];
        let b = points[(i + 1) % points.len()];
        area += a.x * b.y - b.x * a.y;
    }
    area * 0.5
}

/// Smallest distance from `point` to any edge of `polygon`, in pixels.
/// Used to give the move-plane pick the same screen-space forgiveness
/// the axis pick already has.
pub(crate) fn distance_to_polygon_edges_2d(point: Pos2, polygon: &[Pos2]) -> f32 {
    if polygon.len() < 2 {
        return f32::INFINITY;
    }
    let mut best = f32::INFINITY;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        best = best.min(distance_to_segment_2d(point, polygon[j], polygon[i]));
        j = i;
    }
    best
}

pub(crate) fn point_in_polygon_2d(point: Pos2, polygon: &[Pos2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let a = polygon[i];
        let b = polygon[j];
        if (a.y > point.y) != (b.y > point.y) {
            let x = (b.x - a.x) * (point.y - a.y) / (b.y - a.y) + a.x;
            if point.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

pub(crate) fn vec3_nearly_equal(a: [f32; 3], b: [f32; 3]) -> bool {
    (a[0] - b[0]).abs() < 0.001 && (a[1] - b[1]).abs() < 0.001 && (a[2] - b[2]).abs() < 0.001
}

pub(crate) fn round_to_i32(value: f32) -> i32 {
    value.round().clamp(i32::MIN as f32, i32::MAX as f32) as i32
}

pub(crate) fn add_q12_signed_clamped(value: u16, delta: i32, min: i32, max: i32) -> u16 {
    signed_to_q12((q12_to_signed(value) + delta).clamp(min, max))
}

pub(crate) fn q12_to_signed(value: u16) -> i32 {
    let raw = value as i32;
    if raw >= 2048 {
        raw - 4096
    } else {
        raw
    }
}

pub(crate) fn signed_to_q12(value: i32) -> u16 {
    value.rem_euclid(4096) as u16
}

pub(crate) fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub(crate) fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len_sq = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if len_sq <= f32::EPSILON {
        return [0.0, 0.0, 1.0];
    }
    let inv = len_sq.sqrt().recip();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

pub(crate) fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Walk the active scene and collect every Room node as an
/// `(id, display name)` pair, used by Portal pickers.
/// Walk parent links until a `NodeKind::Section` is found.
/// Returns its `NodeId` or `None` if `node_id` lives outside
/// any Room.
pub(crate) fn enclosing_room_id(scene: &psxed_project::Scene, node_id: NodeId) -> Option<NodeId> {
    let mut current = scene.node(node_id)?.parent;
    while let Some(parent_id) = current {
        let parent = scene.node(parent_id)?;
        if matches!(parent.kind, NodeKind::Section { .. }) {
            return Some(parent_id);
        }
        current = parent.parent;
    }
    None
}

/// Sector size a node's TRANSLATION is expressed in, the single source of
/// truth for editor-unit <-> world-unit conversion. BSP scenes author node
/// transforms in raw world units (1 editor unit = 1 world unit); grid scenes
/// author in sectors of the enclosing Section grid, or the World default for
/// nodes outside any room. Everything that moves, snaps, or displays a node
/// translation must resolve units through here, or BSP entities drift at
/// 1/1024 speed again.
pub(crate) fn node_translation_sector_size(
    _project: &psxed_project::ProjectDocument,
    _node_id: NodeId,
) -> i32 {
    1
}

pub(crate) fn portal_seam_bounds_3d(
    grid: &WorldGrid,
    node: &psxed_project::SceneNode,
) -> Option<([f32; 3], [f32; 3])> {
    if !matches!(node.kind, NodeKind::Portal { .. }) {
        return None;
    }

    let mut bounds = None;
    for edge in portal_seam_edges_for_node(grid, node) {
        let Some((a, b)) = portal_edge_room_local_segment(grid, edge) else {
            continue;
        };
        let (min_y, max_y) = portal_edge_pick_height_bounds(grid, edge);
        for point in [
            [a[0], min_y, a[2]],
            [a[0], max_y, a[2]],
            [b[0], min_y, b[2]],
            [b[0], max_y, b[2]],
        ] {
            merge_bounds_3d(&mut bounds, point, [0.0, 0.0, 0.0]);
        }
    }

    let (center, mut half) = bounds.map(bounds_3d_to_center_half)?;
    let pick_pad = (grid.sector_size as f32 * 0.08).clamp(48.0, 128.0);
    half[0] = half[0].max(pick_pad);
    half[1] = half[1].max(pick_pad);
    half[2] = half[2].max(pick_pad);
    Some((center, half))
}

pub(crate) fn portal_edge_room_local_segment(
    grid: &WorldGrid,
    edge: PortalEdge,
) -> Option<([f32; 3], [f32; 3])> {
    let (a, b) = portal_edge_editor_segment(grid, edge)?;
    Some((grid.editor_to_room_local(a), grid.editor_to_room_local(b)))
}

pub(crate) fn portal_edge_pick_height_bounds(grid: &WorldGrid, edge: PortalEdge) -> (f32, f32) {
    let mut min_y: Option<i32> = None;
    let mut max_y: Option<i32> = None;
    include_portal_pick_sector_heights(grid.sector(edge.x, edge.z), &mut min_y, &mut max_y);
    if let Some((nx, nz)) = portal_edge_neighbour(edge.x, edge.z, edge.direction)
        .filter(|(nx, nz)| *nx < grid.width && *nz < grid.depth)
    {
        include_portal_pick_sector_heights(grid.sector(nx, nz), &mut min_y, &mut max_y);
    }
    if let Some(wall) = grid.floor_transition_wall_for_edge(edge.x, edge.z, edge.direction) {
        for height in wall.heights {
            include_min_i32(&mut min_y, height);
            include_max_i32(&mut max_y, height);
        }
    }

    let fallback_min = min_y.unwrap_or(0);
    let mut fallback_max = max_y.unwrap_or(fallback_min + grid.sector_size.max(128));
    if fallback_max <= fallback_min {
        fallback_max = fallback_min + grid.sector_size.max(128);
    }
    (fallback_min as f32, fallback_max as f32)
}

pub(crate) fn include_portal_pick_sector_heights(
    sector: Option<&GridSector>,
    min_y: &mut Option<i32>,
    max_y: &mut Option<i32>,
) {
    let Some(sector) = sector else {
        return;
    };
    for heights in [
        sector.floor.as_ref().map(|face| face.heights),
        sector.ceiling.as_ref().map(|face| face.heights),
    ]
    .into_iter()
    .flatten()
    {
        for height in heights {
            include_min_i32(min_y, height);
            include_max_i32(max_y, height);
        }
    }
    for dir in GridDirection::CARDINAL {
        for wall in sector.walls.get(dir) {
            for height in wall.heights {
                include_min_i32(min_y, height);
                include_max_i32(max_y, height);
            }
        }
    }
}

pub(crate) fn include_min_i32(target: &mut Option<i32>, value: i32) {
    *target = Some(target.map_or(value, |current| current.min(value)));
}

pub(crate) fn include_max_i32(target: &mut Option<i32>, value: i32) {
    *target = Some(target.map_or(value, |current| current.max(value)));
}

/// Per-kind half-extents in world units. Picked so:
/// - bounds are big enough to click reliably at typical
///   editor zoom levels,
/// - small enough that a Light marker doesn't block
///   selection of nearby grid faces,
/// - distinct enough to read at a glance.
///
/// `None` for node kinds that don't get a 3D bound (Room,
/// World, Node, Node3D -- the structural / non-spatial ones).
pub(crate) fn entity_bound_kind_and_size(
    workspace: &EditorWorkspace,
    node: &psxed_project::SceneNode,
) -> Option<(EntityBoundKind, [f32; 3])> {
    match &node.kind {
        NodeKind::Section { .. }
        | NodeKind::World { .. }
        | NodeKind::Node
        | NodeKind::Group
        | NodeKind::Node3D
        | NodeKind::WaterVolume { .. } => None,
        NodeKind::ModelRenderer { .. }
        | NodeKind::Animator { .. }
        | NodeKind::Collider { .. }
        | NodeKind::CharacterController { .. }
        | NodeKind::Camera { .. }
        | NodeKind::Equipment { .. }
        | NodeKind::PhysicsBody { .. }
        | NodeKind::Interactable { .. } => None,
        NodeKind::Entity => {
            if let Some(model) = entity_model_resource(workspace, node) {
                let h = (model.world_height as f32).max(256.0);
                let half_h = h * 0.5;
                let half_xz = (h / 3.0).max(192.0);
                return Some((EntityBoundKind::Model, [half_xz, half_h, half_xz]));
            }
            Some((EntityBoundKind::MeshFallback, [256.0, 256.0, 256.0]))
        }
        NodeKind::MeshInstance { mesh, .. } => {
            // Model-backed instance: scale the bound to the
            // model's `world_height` so a Wraith reads as a
            // standing humanoid box, not a marker cube. Falls
            // back to a fixed mesh box for unbound instances.
            if let Some(id) = mesh {
                if let Some(resource) = workspace.project.resource(*id) {
                    if let psxed_project::ResourceData::Model(model) = &resource.data {
                        let h = (model.world_height as f32).max(256.0);
                        let half_h = h * 0.5;
                        // Square footprint sized as roughly
                        // a third of the model height -- wide
                        // enough to click, tight enough that
                        // adjacent models don't overlap.
                        let half_xz = (h / 3.0).max(192.0);
                        return Some((EntityBoundKind::Model, [half_xz, half_h, half_xz]));
                    }
                }
            }
            Some((EntityBoundKind::MeshFallback, [256.0, 256.0, 256.0]))
        }
        NodeKind::ImageProp { width, height, .. } => Some((
            EntityBoundKind::ImageProp,
            [
                (*width as f32 * 0.5).max(32.0),
                (*height as f32 * 0.5).max(32.0),
                32.0,
            ],
        )),
        NodeKind::BoxProp { vertices, .. } => {
            let mut min = [f32::MAX; 3];
            let mut max = [f32::MIN; 3];
            for vertex in vertices {
                for axis in 0..3 {
                    let value = vertex[axis] as f32;
                    min[axis] = min[axis].min(value);
                    max[axis] = max[axis].max(value);
                }
            }
            Some((
                EntityBoundKind::BoxProp,
                [
                    ((max[0] - min[0]).abs() * 0.5).max(32.0),
                    ((max[1] - min[1]).abs() * 0.5).max(32.0),
                    ((max[2] - min[2]).abs() * 0.5).max(32.0),
                ],
            ))
        }
        NodeKind::CylinderProp { geometry, .. } => {
            let base_scale = if geometry.base_bulge.enabled {
                geometry.base_bulge.radius_percent
            } else {
                100
            };
            let top_scale = if geometry.top_bulge.enabled {
                geometry
                    .top_bulge
                    .radius_percent
                    .saturating_mul(geometry.top_radius_percent)
                    / 100
            } else {
                geometry.top_radius_percent
            };
            let radial_scale = f32::from(base_scale.max(top_scale).max(100)) / 100.0;
            Some((
                EntityBoundKind::CylinderProp,
                [
                    (f32::from(geometry.radius[0]) * radial_scale).max(32.0),
                    (f32::from(geometry.height) * 0.5).max(32.0),
                    (f32::from(geometry.radius[1]) * radial_scale).max(32.0),
                ],
            ))
        }
        NodeKind::ArchProp { geometry, .. } => {
            let sector = node_translation_sector_size(&workspace.project, node.id).max(1) as f32;
            let span = f32::from(geometry.span_tiles.clamp(
                psxed_project::ARCH_PROP_MIN_TILES,
                psxed_project::ARCH_PROP_MAX_TILES,
            )) * sector;
            let depth = f32::from(geometry.depth_tiles.clamp(
                psxed_project::ARCH_PROP_MIN_TILES,
                psxed_project::ARCH_PROP_MAX_TILES,
            )) * sector;
            let height = f32::from(
                geometry
                    .rise_quanta
                    .saturating_add(geometry.leg_height_quanta),
            ) * psxed_project::HEIGHT_QUANTUM as f32;
            Some((
                EntityBoundKind::ArchProp,
                [
                    (span * 0.5).max(32.0),
                    (height * 0.5).max(32.0),
                    (depth * 0.5).max(32.0),
                ],
            ))
        }
        NodeKind::SpawnPoint { .. } => Some((EntityBoundKind::SpawnPoint, [128.0, 256.0, 128.0])),
        NodeKind::PointLight { .. } => Some((EntityBoundKind::PointLight, [128.0, 128.0, 128.0])),
        NodeKind::ParticleEmitter { .. } => {
            Some((EntityBoundKind::ParticleEmitter, [160.0, 160.0, 160.0]))
        }
        NodeKind::Portal { .. } => Some((EntityBoundKind::Portal, [256.0, 256.0, 64.0])),
        // Trigger volumes read as their authored extent so the box is
        // clickable where it fires; point-like logic nodes get a
        // small marker bound.
        NodeKind::Logic { kind, .. } => match kind {
            psxed_project::LogicNodeKind::TriggerVolume { size } => Some((
                EntityBoundKind::Logic,
                [
                    (f32::from(size[0]) * 0.5).max(32.0),
                    (f32::from(size[1]) * 0.5).max(32.0),
                    (f32::from(size[2]) * 0.5).max(32.0),
                ],
            )),
            _ => Some((EntityBoundKind::Logic, [128.0, 192.0, 128.0])),
        },
    }
}

pub(crate) fn node_is_floor_anchored(kind: &NodeKind) -> bool {
    // Trigger Volumes belong here because the cook anchors them floor-up
    // (`record.min[1] = origin`, growing to `origin + size`), so drawing
    // them centred on Y would show a box half its authored height below
    // where it actually fires. Other Logic kinds are point markers with a
    // symmetric gizmo and stay centred.
    if let NodeKind::Logic {
        kind: psxed_project::LogicNodeKind::TriggerVolume { .. },
        ..
    } = kind
    {
        return true;
    }
    matches!(
        kind,
        NodeKind::Entity
            | NodeKind::MeshInstance { .. }
            | NodeKind::BoxProp { .. }
            | NodeKind::CylinderProp { .. }
            | NodeKind::ArchProp { .. }
            | NodeKind::SpawnPoint { .. }
    )
}

pub(crate) fn entity_model_resource<'a>(
    workspace: &'a EditorWorkspace,
    node: &psxed_project::SceneNode,
) -> Option<&'a psxed_project::ModelResource> {
    let scene = workspace.project.active_scene();
    node.children
        .iter()
        .filter_map(|id| scene.node(*id))
        .find_map(|child| match &child.kind {
            NodeKind::ModelRenderer {
                model: Some(id), ..
            } => workspace
                .project
                .resource(*id)
                .and_then(|resource| match &resource.data {
                    ResourceData::Model(model) => Some(model),
                    _ => None,
                }),
            NodeKind::CharacterController {
                character: Some(id),
                ..
            } => workspace
                .project
                .resource(*id)
                .and_then(|resource| match &resource.data {
                    ResourceData::Character(character) => character.model.and_then(|model_id| {
                        workspace
                            .project
                            .resource(model_id)
                            .and_then(|model_resource| match &model_resource.data {
                                ResourceData::Model(model) => Some(model),
                                _ => None,
                            })
                    }),
                    _ => None,
                }),
            _ => None,
        })
}

pub(crate) fn entity_character_resource_id(
    workspace: &EditorWorkspace,
    node: &psxed_project::SceneNode,
) -> Option<ResourceId> {
    if let NodeKind::CharacterController {
        character: Some(id),
        ..
    } = node.kind
    {
        return Some(id);
    }
    let scene = workspace.project.active_scene();
    node.children
        .iter()
        .filter_map(|id| scene.node(*id))
        .find_map(|child| match child.kind {
            NodeKind::CharacterController {
                character: Some(id),
                ..
            } => Some(id),
            _ => None,
        })
}

pub(crate) fn collect_room_options(project: &ProjectDocument) -> Vec<(NodeId, String)> {
    project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| (node.id, node.name.clone()))
        .collect()
}

/// Collect every Model resource as a `(resource id, display
/// name, clip names)` row. The MeshInstance inspector uses
/// this to render its clip-name combo box.
pub(crate) fn collect_model_options(
    project: &ProjectDocument,
) -> Vec<(ResourceId, String, Vec<String>)> {
    let authoring_labels = collect_animation_clip_authoring_labels(project);
    project
        .resources
        .iter()
        .filter_map(|r| match &r.data {
            ResourceData::Model(_) => Some((
                r.id,
                r.name.clone(),
                project
                    .resolved_model_animation_clips(r.id)
                    .iter()
                    .map(|clip| {
                        clip.animation_resource.map_or_else(
                            || clip.name.clone(),
                            |clip_id| {
                                authoring_labels
                                    .get(&clip_id)
                                    .cloned()
                                    .unwrap_or_else(|| clip.name.clone())
                            },
                        )
                    })
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_skeleton_options(project: &ProjectDocument) -> Vec<(ResourceId, String)> {
    project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::Skeleton(_) => Some((resource.id, resource.name.clone())),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_animation_clip_options(
    project: &ProjectDocument,
) -> Vec<AnimationClipOption> {
    let authoring_labels = collect_animation_clip_authoring_labels(project);
    project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) => Some(AnimationClipOption {
                id: resource.id,
                name: authoring_labels
                    .get(&resource.id)
                    .cloned()
                    .unwrap_or_else(|| resource.name.clone()),
                skeleton: clip.skeleton,
                role: clip.role,
            }),
            _ => None,
        })
        .collect()
}

/// Index source-aware display names once for high-density animation pickers.
pub(crate) fn collect_animation_clip_authoring_labels(
    project: &ProjectDocument,
) -> HashMap<ResourceId, String> {
    let sources: HashMap<_, _> = project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationSource(source) => Some((resource.id, source)),
            _ => None,
        })
        .collect();
    project
        .resources
        .iter()
        .filter_map(|resource| {
            let ResourceData::AnimationClip(clip) = &resource.data else {
                return None;
            };
            let label = clip
                .source
                .and_then(|source_id| sources.get(&source_id).copied())
                .map_or_else(
                    || resource.name.clone(),
                    |source| animation_source_authoring_label(source, &resource.name),
                );
            Some((resource.id, label))
        })
        .collect()
}

/// Prefer the imported filename while retaining distinct embedded FBX takes.
/// Mixamo often calls those takes `Armature|mixamo.com.###`; presenting both
/// pieces makes the author's animation name visible and every take searchable.
pub(crate) fn animation_source_authoring_label(
    source: &psxed_project::AnimationSourceResource,
    fallback: &str,
) -> String {
    let path = source
        .source_path
        .rsplit_once("::")
        .map_or(source.source_path.as_str(), |(_, entry)| entry);
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty());
    let take = source.clip_name.trim();

    match (stem, take.is_empty()) {
        (Some(stem), false) if !stem.eq_ignore_ascii_case(take) => {
            format!("{stem} — {take}")
        }
        (Some(stem), _) => stem.to_string(),
        (None, false) => take.to_string(),
        (None, true) => fallback.to_string(),
    }
}

pub(crate) fn collect_animation_source_options(
    project: &ProjectDocument,
) -> Vec<(ResourceId, String)> {
    project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationSource(_) => Some((resource.id, resource.name.clone())),
            _ => None,
        })
        .collect()
}

pub(crate) fn collect_animation_set_options(project: &ProjectDocument) -> Vec<AnimationSetOption> {
    project
        .resources
        .iter()
        .filter_map(|resource| match &resource.data {
            ResourceData::AnimationSet(set) => {
                let mut action_clips = [None; psxed_project::CHARACTER_ANIMATION_ACTION_COUNT];
                for action in psxed_project::CharacterAnimationAction::ALL {
                    action_clips[action.to_index()] = set.action_clip(action);
                }
                Some(AnimationSetOption {
                    id: resource.id,
                    name: resource.name.clone(),
                    skeleton: set.skeleton,
                    action_clips,
                })
            }
            _ => None,
        })
        .collect()
}
