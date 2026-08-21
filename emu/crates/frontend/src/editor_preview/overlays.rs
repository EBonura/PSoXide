//! Editor-only viewport overlay drawing: selection, paint ghosts, bounds, and gizmos.

use super::*;

/// Surface-grid work is editor-only but still runs during camera navigation,
/// so keep both the per-face density and total host stroke count bounded.
/// Coarser lines stay on the authored grid by increasing the interval in
/// power-of-two multiples instead of inventing an unrelated spacing.
const BSP_SURFACE_GRID_LINES_PER_AXIS: usize = 24;
const BSP_SURFACE_GRID_SEGMENT_CAP: usize = 2_048;
const BSP_SURFACE_GRID_MAJOR_EVERY: i64 = 8;
const BSP_SURFACE_GRID_MINOR_RGBA: (u8, u8, u8, u8) = (176, 208, 220, 68);
const BSP_SURFACE_GRID_MAJOR_RGBA: (u8, u8, u8, u8) = (218, 232, 238, 104);
const BSP_SURFACE_GRID_MINOR_WIDTH: f32 = 0.65;
const BSP_SURFACE_GRID_MAJOR_WIDTH: f32 = 0.9;
const GRID_INTERSECTION_EPSILON: f64 = 1.0 / 4096.0;
const GRID_INTERSECTION_CAP: usize = 8;
const BSP_LEAK_PATH_RGB: (u8, u8, u8) = (72, 236, 126);
const BSP_LEAK_PATH_WIDTH: f32 = 2.5;

struct GridIntersections {
    points: [[f64; 3]; GRID_INTERSECTION_CAP],
    len: usize,
}

impl GridIntersections {
    const fn new() -> Self {
        Self {
            points: [[0.0; 3]; GRID_INTERSECTION_CAP],
            len: 0,
        }
    }

    fn as_slice(&self) -> &[[f64; 3]] {
        &self.points[..self.len]
    }

    fn push_unique(&mut self, candidate: [f64; 3]) {
        if self.len >= self.points.len()
            || self.as_slice().iter().any(|point| {
                squared_distance_f64(*point, candidate)
                    <= GRID_INTERSECTION_EPSILON * GRID_INTERSECTION_EPSILON
            })
        {
            return;
        }
        self.points[self.len] = candidate;
        self.len += 1;
    }
}

/// Append the Quake pointfile route from an occupied leaf to the exterior.
/// The line is opaque green to match TrenchBroom's pointfile convention and
/// is near-clipped per segment so walking the camera along it never makes the
/// whole diagnostic disappear when one endpoint passes behind the viewer.
pub(super) fn append_bsp_leak_path_overlay(
    camera: psx_engine::WorldCamera,
    leak_path: &[[i32; 3]],
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let color = egui::Color32::from_rgb(
        BSP_LEAK_PATH_RGB.0,
        BSP_LEAK_PATH_RGB.1,
        BSP_LEAK_PATH_RGB.2,
    );
    for points in leak_path.windows(2) {
        let Some((a, b)) = project_clipped_world_segment(camera, points[0], points[1]) else {
            continue;
        };
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        if dx * dx + dy * dy < 0.25 {
            continue;
        }
        overlay_lines.push(psxed_ui::EditorViewportOverlayLine::new(
            a,
            b,
            color,
            BSP_LEAK_PATH_WIDTH,
        ));
    }
}

fn project_clipped_world_segment(
    camera: psx_engine::WorldCamera,
    a: [i32; 3],
    b: [i32; 3],
) -> Option<(egui::Pos2, egui::Pos2)> {
    let world = |point: [i32; 3]| psx_engine::WorldVertex::new(point[0], point[1], point[2]);
    let mut a = camera.view_vertex(world(a));
    let mut b = camera.view_vertex(world(b));
    let near = camera.projection.near_z.max(1);
    if a.z < near && b.z < near {
        return None;
    }
    if a.z < near {
        a = view_segment_near_intersection(a, b, near)?;
    } else if b.z < near {
        b = view_segment_near_intersection(b, a, near)?;
    }
    let a = camera.projection.project_view(a)?;
    let b = camera.projection.project_view(b)?;
    Some((
        egui::pos2(f32::from(a.sx), f32::from(a.sy)),
        egui::pos2(f32::from(b.sx), f32::from(b.sy)),
    ))
}

fn view_segment_near_intersection(
    behind: psx_engine::ViewVertex,
    front: psx_engine::ViewVertex,
    near: i32,
) -> Option<psx_engine::ViewVertex> {
    let numerator = i64::from(near.saturating_sub(behind.z));
    let denominator = i64::from(front.z).checked_sub(i64::from(behind.z))?;
    if denominator <= 0 {
        return None;
    }
    let interpolate = |from: i32, to: i32| {
        i64::from(from)
            .saturating_add(
                i64::from(to)
                    .saturating_sub(i64::from(from))
                    .saturating_mul(numerator)
                    / denominator,
            )
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    Some(psx_engine::ViewVertex::new(
        interpolate(behind.x, front.x),
        interpolate(behind.y, front.y),
        near,
    ))
}

/// Prepend a TrenchBroom-style world grid projected onto visible BSP brush
/// faces. Grid strokes are host-composited, translucent, and painted before
/// selection affordances, preserving both the material preview and the
/// stronger hover/selection outlines.
pub(super) fn prepend_bsp_surface_grid_overlay(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    grid_units: u16,
    hidden_scene_nodes: &std::collections::HashSet<NodeId>,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    if project.active_scene().brushes.is_empty() {
        return;
    }

    let step = f64::from(grid_units.max(1));
    let mut candidate_count = 0u64;
    let scene = project.active_scene();
    with_cached_solved_brush_faces(project, |brush_index, _, plane, verts| {
        let Some(brush) = scene.brushes.get(brush_index) else {
            return;
        };
        if super::brush_group_hidden(scene, hidden_scene_nodes, brush) {
            return;
        }
        if let Some(ranges) = surface_grid_face_ranges(
            verts,
            plane.normal.map(|component| component as f64),
            camera,
            step,
        ) {
            candidate_count = candidate_count.saturating_add(
                ranges
                    .iter()
                    .map(|(_, range)| grid_index_count(*range))
                    .sum::<u64>(),
            );
        }
    });

    let mut grid_lines = Vec::new();
    let mut candidate_ordinal = 0u64;
    with_cached_solved_brush_faces(project, |brush_index, _, plane, verts| {
        let Some(brush) = scene.brushes.get(brush_index) else {
            return;
        };
        if super::brush_group_hidden(scene, hidden_scene_nodes, brush) {
            return;
        }
        append_bsp_surface_grid_face(
            verts,
            plane.normal.map(|component| component as f64),
            camera,
            step,
            candidate_count,
            &mut candidate_ordinal,
            &mut grid_lines,
        );
    });
    if grid_lines.is_empty() {
        return;
    }
    grid_lines.append(overlay_lines);
    *overlay_lines = grid_lines;
}

/// Pre-cache behavior retained only for a strict real-project performance
/// comparison. It deliberately repeats the convex solve inside the overlay
/// pass; production must use `prepend_bsp_surface_grid_overlay` above.
#[cfg(all(test, feature = "editor-preview-benchmark"))]
pub(super) fn prepend_bsp_surface_grid_overlay_uncached(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    grid_units: u16,
    hidden_scene_nodes: &std::collections::HashSet<NodeId>,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    if project.active_scene().brushes.is_empty() {
        return;
    }

    let step = f64::from(grid_units.max(1));
    let mut candidate_count = 0u64;
    for brush in &project.active_scene().brushes {
        if super::brush_group_hidden(project.active_scene(), hidden_scene_nodes, brush) {
            continue;
        }
        let solved = brush.solve();
        for (face_index, polygon) in solved.polygons.iter().enumerate() {
            let Some(polygon) = polygon else { continue };
            let Some(face) = brush.faces.get(face_index) else {
                continue;
            };
            let Some(plane) = psxed_project::brush::Plane::from_points(face.points) else {
                continue;
            };
            if let Some(ranges) = surface_grid_face_ranges(
                &polygon.verts,
                plane.normal.map(|component| component as f64),
                camera,
                step,
            ) {
                candidate_count = candidate_count.saturating_add(
                    ranges
                        .iter()
                        .map(|(_, range)| grid_index_count(*range))
                        .sum::<u64>(),
                );
            }
        }
    }

    let mut grid_lines = Vec::new();
    let mut candidate_ordinal = 0u64;
    for brush in &project.active_scene().brushes {
        if super::brush_group_hidden(project.active_scene(), hidden_scene_nodes, brush) {
            continue;
        }
        let solved = brush.solve();
        for (face_index, polygon) in solved.polygons.iter().enumerate() {
            let Some(polygon) = polygon else { continue };
            let Some(face) = brush.faces.get(face_index) else {
                continue;
            };
            let Some(plane) = psxed_project::brush::Plane::from_points(face.points) else {
                continue;
            };
            append_bsp_surface_grid_face(
                &polygon.verts,
                plane.normal.map(|component| component as f64),
                camera,
                step,
                candidate_count,
                &mut candidate_ordinal,
                &mut grid_lines,
            );
        }
    }
    if grid_lines.is_empty() {
        return;
    }
    grid_lines.append(overlay_lines);
    *overlay_lines = grid_lines;
}

fn append_bsp_surface_grid_face(
    polygon: &[[f64; 3]],
    normal: [f64; 3],
    camera: psx_engine::WorldCamera,
    step: f64,
    candidate_count: u64,
    candidate_ordinal: &mut u64,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let Some(ranges) = surface_grid_face_ranges(polygon, normal, camera, step) else {
        return;
    };
    for (axis, (first, last, stride)) in ranges {
        let mut index = first;
        while index <= last {
            let ordinal = *candidate_ordinal;
            *candidate_ordinal = candidate_ordinal.saturating_add(1);
            if grid_candidate_selected(ordinal, candidate_count) {
                if let Some((a, b)) = clip_grid_line_to_polygon(polygon, axis, index as f64 * step)
                {
                    append_projected_grid_segment(camera, a, b, index, overlay_lines);
                }
            }
            let Some(next) = index.checked_add(stride) else {
                break;
            };
            index = next;
        }
    }
}

fn surface_grid_face_ranges(
    polygon: &[[f64; 3]],
    normal: [f64; 3],
    camera: psx_engine::WorldCamera,
    step: f64,
) -> Option<[(usize, (i64, i64, i64)); 2]> {
    if polygon.len() < 3 || !surface_grid_face_maybe_visible(polygon, normal, camera) {
        return None;
    }
    let axes = match dominant_normal_axis(normal) {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    Some([
        (axes[0], grid_index_range(polygon, axes[0], step)),
        (axes[1], grid_index_range(polygon, axes[1], step)),
    ])
}

fn surface_grid_face_maybe_visible(
    polygon: &[[f64; 3]],
    normal: [f64; 3],
    camera: psx_engine::WorldCamera,
) -> bool {
    let camera_position = [
        f64::from(camera.position.x),
        f64::from(camera.position.y),
        f64::from(camera.position.z),
    ];
    let camera_side = dot3_f64(
        normal,
        [
            camera_position[0] - polygon[0][0],
            camera_position[1] - polygon[0][1],
            camera_position[2] - polygon[0][2],
        ],
    );
    // Brush face points are CCW from outside. Only the material-visible side
    // gets a grid; otherwise lines from rear/interior planes would wash over
    // the actual surface preview because host overlays do not have a Z buffer.
    if !camera_side.is_finite() || camera_side <= GRID_INTERSECTION_EPSILON {
        return false;
    }
    const SAFE_COORD_LIMIT: f64 = 500_000.0;
    if polygon
        .iter()
        .flatten()
        .any(|value| !value.is_finite() || value.abs() > SAFE_COORD_LIMIT)
    {
        return false;
    }
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    let mut any_projected = false;
    for point in polygon {
        let point = psx_engine::WorldVertex::new(
            point[0].round() as i32,
            point[1].round() as i32,
            point[2].round() as i32,
        );
        if let Some(projected) = camera.project_world(point) {
            any_projected = true;
            let x = f32::from(projected.sx);
            let y = f32::from(projected.sy);
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        }
    }
    const SCREEN_MARGIN: f32 = 8.0;
    any_projected
        && bounds[2] >= -SCREEN_MARGIN
        && bounds[0] <= SCREEN_W as f32 + SCREEN_MARGIN
        && bounds[3] >= -SCREEN_MARGIN
        && bounds[1] <= SCREEN_H as f32 + SCREEN_MARGIN
}

fn grid_index_count((first, last, stride): (i64, i64, i64)) -> u64 {
    if first > last || stride <= 0 {
        0
    } else {
        (last.saturating_sub(first) / stride).saturating_add(1) as u64
    }
}

fn grid_candidate_selected(ordinal: u64, candidate_count: u64) -> bool {
    if candidate_count <= BSP_SURFACE_GRID_SEGMENT_CAP as u64 {
        return true;
    }
    let cap = BSP_SURFACE_GRID_SEGMENT_CAP as u128;
    let total = candidate_count as u128;
    ((ordinal as u128 + 1) * cap) / total > (ordinal as u128 * cap) / total
}

fn append_projected_grid_segment(
    camera: psx_engine::WorldCamera,
    a: [f64; 3],
    b: [f64; 3],
    grid_index: i64,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    const SAFE_COORD_LIMIT: f64 = 500_000.0;
    if [a, b]
        .iter()
        .flatten()
        .any(|value| !value.is_finite() || value.abs() > SAFE_COORD_LIMIT)
    {
        return;
    }
    let world_vertex = |point: [f64; 3]| {
        psx_engine::WorldVertex::new(
            point[0].round() as i32,
            point[1].round() as i32,
            point[2].round() as i32,
        )
    };
    let Some(a) = camera.project_world(world_vertex(a)) else {
        return;
    };
    let Some(b) = camera.project_world(world_vertex(b)) else {
        return;
    };
    let a = egui::pos2(f32::from(a.sx), f32::from(a.sy));
    let b = egui::pos2(f32::from(b.sx), f32::from(b.sy));
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    if dx * dx + dy * dy < 2.25 {
        return;
    }
    let major = grid_index.rem_euclid(BSP_SURFACE_GRID_MAJOR_EVERY) == 0;
    let rgba = if major {
        BSP_SURFACE_GRID_MAJOR_RGBA
    } else {
        BSP_SURFACE_GRID_MINOR_RGBA
    };
    overlay_lines.push(psxed_ui::EditorViewportOverlayLine::new(
        a,
        b,
        egui::Color32::from_rgba_unmultiplied(rgba.0, rgba.1, rgba.2, rgba.3),
        if major {
            BSP_SURFACE_GRID_MAJOR_WIDTH
        } else {
            BSP_SURFACE_GRID_MINOR_WIDTH
        },
    ));
}

fn dominant_normal_axis(normal: [f64; 3]) -> usize {
    let absolute = normal.map(f64::abs);
    if absolute[1] > absolute[0] && absolute[1] >= absolute[2] {
        1
    } else if absolute[2] > absolute[0] && absolute[2] > absolute[1] {
        2
    } else {
        0
    }
}

fn grid_index_range(polygon: &[[f64; 3]], axis: usize, step: f64) -> (i64, i64, i64) {
    let (min, max) = polygon
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), vertex| {
            (min.min(vertex[axis]), max.max(vertex[axis]))
        });
    if !min.is_finite() || !max.is_finite() || step <= 0.0 {
        return (1, 0, 1);
    }
    let first = (min / step).ceil() as i64;
    let last = (max / step).floor() as i64;
    if first > last {
        return (1, 0, 1);
    }
    let count = last.saturating_sub(first).saturating_add(1) as u64;
    let required = count
        .div_ceil(BSP_SURFACE_GRID_LINES_PER_AXIS as u64)
        .max(1);
    let stride = required.next_power_of_two().min(i64::MAX as u64) as i64;
    let aligned_first = first.div_euclid(stride) + i64::from(first.rem_euclid(stride) != 0);
    (aligned_first.saturating_mul(stride), last, stride)
}

/// Intersect one world-axis grid plane with a convex brush polygon. Edge
/// crossings are used rather than a texture-space basis so the result stays
/// anchored to global world coordinates on axis-aligned and sloped faces.
fn clip_grid_line_to_polygon(
    polygon: &[[f64; 3]],
    axis: usize,
    coordinate: f64,
) -> Option<([f64; 3], [f64; 3])> {
    let mut intersections = GridIntersections::new();
    for edge_index in 0..polygon.len() {
        let a = polygon[edge_index];
        let b = polygon[(edge_index + 1) % polygon.len()];
        let da = a[axis] - coordinate;
        let db = b[axis] - coordinate;
        if da.abs() <= GRID_INTERSECTION_EPSILON {
            intersections.push_unique(a);
        }
        if db.abs() <= GRID_INTERSECTION_EPSILON {
            intersections.push_unique(b);
        }
        if (da < -GRID_INTERSECTION_EPSILON && db > GRID_INTERSECTION_EPSILON)
            || (da > GRID_INTERSECTION_EPSILON && db < -GRID_INTERSECTION_EPSILON)
        {
            let t = da / (da - db);
            intersections.push_unique([
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]);
        }
    }
    let intersections = intersections.as_slice();
    if intersections.len() < 2 {
        return None;
    }
    let mut farthest = (0, 1);
    let mut farthest_distance = squared_distance_f64(intersections[0], intersections[1]);
    for a in 0..intersections.len() {
        for b in (a + 1)..intersections.len() {
            let distance = squared_distance_f64(intersections[a], intersections[b]);
            if distance > farthest_distance {
                farthest = (a, b);
                farthest_distance = distance;
            }
        }
    }
    (farthest_distance > GRID_INTERSECTION_EPSILON * GRID_INTERSECTION_EPSILON)
        .then(|| (intersections[farthest.0], intersections[farthest.1]))
}

fn squared_distance_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn dot3_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Draw a horizontal radius ring plus a bulb icon for every
/// PointLight in the scene. The bulb
/// replaces the old coloured square marker so lights read as editor
/// light gizmos rather than generic entities.
pub(super) fn walk_light_gizmos(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    room_id: NodeId,
    grid: &WorldGrid,
    floor_index: usize,
    y_offset: i32,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    for light in preview_lights(scene, hidden_scene_nodes) {
        if !is_descendant_of_room(scene, light.host_id, room_id) {
            continue;
        }
        if node_enclosing_floor(scene, light.host_id) != floor_index {
            continue;
        }
        let center = node_room_local_origin(grid, &light.transform);
        let center_world = [center.x, center.y + y_offset, center.z];
        let is_selected = preview_reference_selected(selected, light.host_id, None, None, None);
        let is_hovered = hovered
            .is_some_and(|id| preview_reference_selected(id, light.host_id, None, None, None));
        let style = if is_selected {
            FaceOutlineStyle {
                rgb: (0xFF, 0xE0, 0x80),
                thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
            }
        } else if is_hovered {
            FaceOutlineStyle {
                rgb: (0xFF, 0xF0, 0x90),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        } else {
            // Tint the unlit ring toward the authored colour
            // so multiple lights in a room read at a glance.
            FaceOutlineStyle {
                rgb: (
                    light.color[0].max(0x40),
                    light.color[1].max(0x40),
                    light.color[2].max(0x40),
                ),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        };
        // Light radius is authored in *sector units*; scale to
        // engine units so the ring matches the light's actual
        // attenuation footprint. The bulb remains visible even
        // for radius-zero lights.
        let radius_engine = spatial::light_radius_engine_units(grid, light.radius);
        if radius_engine > 0 && (is_selected || is_hovered) {
            push_horizontal_ring(scratch, camera, center_world, radius_engine, 16, style);
        }
        push_light_bulb_icon(
            scratch,
            center_world,
            (
                light.color[0].max(0x40),
                light.color[1].max(0x40),
                light.color[2].max(0x40),
            ),
            is_selected || is_hovered,
        );
    }
}

/// Ring + bulb gizmos for lights that live in world space (no enclosing
/// Section room), the BSP-scene counterpart of [`walk_light_gizmos`].
/// Positions are raw world units; the radius scales by the World sector
/// size, matching both the brush bake and the runtime light records.
pub(super) fn walk_roomless_light_gizmos(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    hidden_scene_nodes: &HashSet<NodeId>,
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    let scene = project.active_scene();
    let radius_units = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1) as f32;
    for light in preview_lights(scene, hidden_scene_nodes) {
        if node_has_section_ancestor(scene, light.host_id) {
            continue;
        }
        let center_world = light
            .transform
            .translation
            .map(|value| value.round() as i32);
        let center_world = [center_world[0], center_world[1], center_world[2]];
        let is_selected = preview_reference_selected(selected, light.host_id, None, None, None);
        let is_hovered = hovered
            .is_some_and(|id| preview_reference_selected(id, light.host_id, None, None, None));
        let style = if is_selected {
            FaceOutlineStyle {
                rgb: (0xFF, 0xE0, 0x80),
                thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
            }
        } else if is_hovered {
            FaceOutlineStyle {
                rgb: (0xFF, 0xF0, 0x90),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        } else {
            FaceOutlineStyle {
                rgb: (
                    light.color[0].max(0x40),
                    light.color[1].max(0x40),
                    light.color[2].max(0x40),
                ),
                thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
            }
        };
        let radius_engine = (light.radius * radius_units) as i32;
        if radius_engine > 0 && (is_selected || is_hovered) {
            push_horizontal_ring(scratch, camera, center_world, radius_engine, 16, style);
        }
        push_light_bulb_icon(
            scratch,
            center_world,
            (
                light.color[0].max(0x40),
                light.color[1].max(0x40),
                light.color[2].max(0x40),
            ),
            is_selected || is_hovered,
        );
    }
}

/// Wireframe AABB + facing arrow per selectable scene entity.
/// Bounds are gathered by `EditorWorkspace::collect_entity_bounds`
/// -- every entity-kind node carries an AABB the user can click to
/// select and drag to move. Image props, box props, and portals render
/// their own authored frames/seams, so this pass leaves those out and
/// draws simple manipulation boxes for the remaining object-like nodes.
///
/// Idle bounds draw thin and muted so they don't dominate the
/// viewport over the room they sit in. Hover and selected reuse
/// the room face palette for cross-tool consistency: yellow for
/// hover, cyan-bold for selected.
pub(super) fn walk_entity_bounds(
    bounds: &[psxed_ui::EntityBounds],
    selected: psxed_project::NodeId,
    hovered: Option<psxed_project::NodeId>,
    scratch: &mut PreviewScratch,
) {
    for b in bounds {
        if matches!(
            b.kind,
            psxed_ui::EntityBoundKind::ImageProp
                | psxed_ui::EntityBoundKind::BoxProp
                | psxed_ui::EntityBoundKind::CylinderProp
                | psxed_ui::EntityBoundKind::Portal
        ) {
            continue;
        }
        let is_selected = b.node == selected;
        let is_hovered = hovered == Some(b.node);
        let style = entity_bound_style(b.kind, is_selected, is_hovered);
        push_aabb_wireframe(scratch, b.center, b.half_extents, style);

        // Yaw arrow only for kinds with meaningful facing --
        // models and spawn points point at where they'll
        // render / face. Lights and portals are either
        // omnidirectional or carry their own
        // direction gizmo elsewhere (light radius ring).
        if matches!(
            b.kind,
            psxed_ui::EntityBoundKind::Model
                | psxed_ui::EntityBoundKind::SpawnPoint
                | psxed_ui::EntityBoundKind::MeshFallback
        ) {
            push_facing_arrow(scratch, b.center, b.half_extents, b.yaw_degrees, style);
        }
    }
}

/// Pick the outline style for one bound. Selected wins over
/// hover; idle uses a muted kind-tinted thin line so multiple
/// boxes in a busy room read at a glance without dominating.
pub(super) fn entity_bound_style(
    kind: psxed_ui::EntityBoundKind,
    selected: bool,
    hovered: bool,
) -> FaceOutlineStyle {
    if selected {
        return ENTITY_BOUND_SELECTED;
    }
    if hovered {
        return ENTITY_BOUND_HOVER;
    }
    let rgb = match kind {
        psxed_ui::EntityBoundKind::Model => (0xC0, 0xC8, 0xD0),
        psxed_ui::EntityBoundKind::MeshFallback => (0x90, 0x98, 0xA0),
        psxed_ui::EntityBoundKind::ImageProp => (0xD0, 0xAA, 0x78),
        psxed_ui::EntityBoundKind::BoxProp => (0x87, 0xB4, 0xDC),
        psxed_ui::EntityBoundKind::CylinderProp => (0x78, 0xB8, 0xC8),
        psxed_ui::EntityBoundKind::ArchProp => (0xC0, 0xA0, 0x70),
        psxed_ui::EntityBoundKind::SpawnPoint => (0x60, 0xE0, 0x80),
        psxed_ui::EntityBoundKind::PointLight => (0xFF, 0xD8, 0x70),
        psxed_ui::EntityBoundKind::ParticleEmitter => (0x98, 0xD6, 0xE6),
        psxed_ui::EntityBoundKind::Portal => PORTAL_SEAM_STYLE.rgb,
        psxed_ui::EntityBoundKind::Logic => (0xC8, 0x8C, 0xE8),
    };
    FaceOutlineStyle {
        rgb,
        thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
    }
}

/// Project the 8 corners of a world-space AABB and emit the 12
/// edges as `push_screen_line` segments. Coordinates are stored
/// `f32` in the bound; rounded to `i32` here because the GTE
/// shim wants integer world coords.
pub(super) fn push_aabb_wireframe(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    style: FaceOutlineStyle,
) {
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let hx = half_extents[0].round() as i32;
    let hy = half_extents[1].round() as i32;
    let hz = half_extents[2].round() as i32;
    if hx <= 0 || hy <= 0 || hz <= 0 {
        return;
    }
    let lo = [cx - hx, cy - hy, cz - hz];
    let hi = [cx + hx, cy + hy, cz + hz];
    // Corner index encoding: bit0 = X (lo/hi), bit1 = Y, bit2 = Z.
    let corner = |i: usize| -> [i32; 3] {
        [
            if i & 1 != 0 { hi[0] } else { lo[0] },
            if i & 2 != 0 { hi[1] } else { lo[1] },
            if i & 4 != 0 { hi[2] } else { lo[2] },
        ]
    };
    let corners: [_; 8] = std::array::from_fn(corner);
    push_world_box_wireframe(scratch, corners, style);
}

pub(super) fn push_world_box_wireframe(
    scratch: &mut PreviewScratch,
    corners: [[i32; 3]; 8],
    style: FaceOutlineStyle,
) {
    let p = corners.map(|corner| gte_scene::project_vertex(world_to_view(corner)));
    // 12 edges of a box: 4 along X, 4 along Y, 4 along Z. Pairs
    // of corners that differ in exactly one bit.
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (2, 3),
        (4, 5),
        (6, 7), // along X
        (0, 2),
        (1, 3),
        (4, 6),
        (5, 7), // along Y
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7), // along Z
    ];
    for (a, b) in EDGES {
        if p[a].sz == 0 || p[b].sz == 0 {
            continue;
        }
        push_screen_line(scratch, p[a], p[b], style);
    }
}

pub(super) fn selected_node_is_image_prop(project: &ProjectDocument, selected: NodeId) -> bool {
    project.active_scene().node(selected).is_some_and(|node| {
        matches!(
            node.kind,
            NodeKind::ImageProp { .. } | NodeKind::BoxProp { .. } | NodeKind::CylinderProp { .. }
        )
    })
}

/// Draw a forward-pointing arrow from the bound centre out
/// past the front face, indicating where the entity faces.
/// Length scales with the bound's horizontal extent so big
/// models get a visible arrow and tiny markers don't grow
/// horns.
pub(super) fn push_facing_arrow(
    scratch: &mut PreviewScratch,
    center: [f32; 3],
    half_extents: [f32; 3],
    yaw_degrees: f32,
    style: FaceOutlineStyle,
) {
    let yaw_q12 = yaw_to_q12(yaw_degrees);
    let s = sin_q12_turn(yaw_q12);
    let c = cos_q12_turn(yaw_q12);
    // Arrow length = bound's horizontal half-extent + a small
    // overshoot so the head sits clearly outside the box.
    let reach = (half_extents[0].max(half_extents[2]) * 1.5).max(96.0) as i32;
    let cx = center[0].round() as i32;
    let cy = center[1].round() as i32;
    let cz = center[2].round() as i32;
    let tip = [cx + ((s * reach) >> 12), cy, cz + ((c * reach) >> 12)];
    let p_origin = gte_scene::project_vertex(world_to_view([cx, cy, cz]));
    let p_tip = gte_scene::project_vertex(world_to_view(tip));
    if p_origin.sz != 0 && p_tip.sz != 0 {
        push_screen_line(scratch, p_origin, p_tip, style);
    }
}

/// Project a horizontal `segments`-sided polygon at world
/// `center` with `radius` into screen space and emit one
/// `push_screen_line` per edge. Used for light radius gizmos
/// and any future ground-plane affordances.
pub(super) fn push_horizontal_ring(
    scratch: &mut PreviewScratch,
    camera: psx_engine::WorldCamera,
    center: [i32; 3],
    radius: i32,
    segments: u16,
    style: FaceOutlineStyle,
) {
    if segments < 3 || radius <= 0 {
        return;
    }
    let mut prev_world = [center[0] + radius, center[1], center[2]];
    for i in 1..=segments {
        // Authored editor angles use 4096 units per turn; sample
        // the unit circle around the light origin once per segment.
        let angle_q12 = ((i as u32 * 4096) / segments as u32) as u16;
        let s = sin_q12_turn(angle_q12);
        let c = cos_q12_turn(angle_q12);
        let next_world = [
            center[0] + ((c * radius) >> 12),
            center[1],
            center[2] + ((s * radius) >> 12),
        ];
        if let Some([prev_proj, next_proj]) =
            clip_preview_world_segment(camera, prev_world, next_world)
        {
            push_screen_line(scratch, prev_proj, next_proj, style);
        }
        prev_world = next_world;
    }
}

pub(super) fn push_light_bulb_icon(
    scratch: &mut PreviewScratch,
    center_world: [i32; 3],
    rgb: (u8, u8, u8),
    emphasized: bool,
) {
    let projected = gte_scene::project_vertex(world_to_view(center_world));
    if projected.sz == 0 {
        return;
    }
    let cx = projected.sx as f32;
    let cy = projected.sy as f32;
    let radius = if emphasized { 8.5 } else { 7.0 };
    let icon = FaceOutlineStyle {
        rgb,
        thickness_px: if emphasized { 1.0 } else { 0.75 },
    };
    let glass = egui::pos2(cx, cy - radius * 0.28);
    let segments = 16;
    for i in 0..segments {
        let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
        let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
        push_overlay_line(
            scratch,
            egui::pos2(glass.x + a0.cos() * radius, glass.y + a0.sin() * radius),
            egui::pos2(glass.x + a1.cos() * radius, glass.y + a1.sin() * radius),
            icon,
        );
    }

    let neck_y = glass.y + radius * 0.72;
    let base_y = neck_y + radius * 0.62;
    let neck_half = radius * 0.46;
    let base_half = radius * 0.34;
    push_overlay_line(
        scratch,
        egui::pos2(cx - neck_half, neck_y),
        egui::pos2(cx - base_half, base_y),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + neck_half, neck_y),
        egui::pos2(cx + base_half, base_y),
        icon,
    );
    for step in [0.0, 0.28, 0.56] {
        let y = neck_y + radius * step;
        push_overlay_line(
            scratch,
            egui::pos2(cx - base_half, y),
            egui::pos2(cx + base_half, y),
            icon,
        );
    }

    let filament_y = glass.y + radius * 0.18;
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.36, filament_y),
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx - radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        icon,
    );
    push_overlay_line(
        scratch,
        egui::pos2(cx + radius * 0.12, filament_y + radius * 0.2),
        egui::pos2(cx + radius * 0.36, filament_y),
        icon,
    );

    if emphasized {
        let halo = FaceOutlineStyle {
            rgb: (0xFF, 0xFF, 0xFF),
            thickness_px: 0.45,
        };
        let halo_radius = radius + 2.0;
        for i in 0..segments {
            let a0 = i as f32 * std::f32::consts::TAU / segments as f32;
            let a1 = (i + 1) as f32 * std::f32::consts::TAU / segments as f32;
            push_overlay_line(
                scratch,
                egui::pos2(
                    glass.x + a0.cos() * halo_radius,
                    glass.y + a0.sin() * halo_radius,
                ),
                egui::pos2(
                    glass.x + a1.cos() * halo_radius,
                    glass.y + a1.sin() * halo_radius,
                ),
                halo,
            );
        }
    }
}

pub(super) fn synth(sx: i16, sy: i16, sz: u16) -> psx_gte::scene::Projected {
    psx_gte::scene::Projected { sx, sy, sz }
}

/// Render the paint-target ghost outline. Cell ghosts trace the
/// would-be cell surface; wall ghosts use
/// `push_face_outline` with a synthetic `FaceRef` whose world cell
/// might lie outside the current grid. Missing wall-stack previews
/// project explicit candidate heights so the outline appears where
/// the auto-grow or next-stack click will create the wall.
pub(super) fn push_paint_preview(
    grid: &WorldGrid,
    preview: psxed_ui::PaintTargetPreview,
    scratch: &mut PreviewScratch,
) {
    match preview {
        psxed_ui::PaintTargetPreview::Cell {
            world_cell_x,
            world_cell_z,
            kind,
        } => push_cell_ghost_outline(grid, world_cell_x, world_cell_z, kind, scratch),
        psxed_ui::PaintTargetPreview::Wall {
            world_cell_x,
            world_cell_z,
            dir,
            stack,
        } => {
            // Translate world cell -> array when in bounds so an
            // existing wall stack can still use the regular face
            // outline path.
            let (sx, sz) = grid
                .world_cell_to_array(world_cell_x, world_cell_z)
                .unwrap_or((u16::MAX, u16::MAX));
            // Fake a FaceRef. `room` field is unused by
            // push_face_outline; safe to fill with anything.
            let face = psxed_ui::FaceRef {
                room: psxed_project::NodeId::ROOT,
                sx,
                sz,
                kind: psxed_ui::FaceKind::Wall { dir, stack },
            };
            // For off-grid or next-stack wall ghosts we have to
            // project the outline ourselves. `push_face_outline`
            // either short-circuits when sx/sz are out of grid
            // bounds or falls back to floor-to-ceiling placement
            // for missing stack indices.
            let existing_stack = grid
                .sector(sx, sz)
                .is_some_and(|sector| (stack as usize) < sector.walls.get(dir).len());
            if sx == u16::MAX || sz == u16::MAX || !existing_stack {
                let heights = grid.wall_heights_above_stack_or_surfaces_for_world_cell(
                    world_cell_x,
                    world_cell_z,
                    dir,
                );
                push_ghost_wall_outline(grid, world_cell_x, world_cell_z, dir, heights, scratch);
            } else {
                push_face_outline(grid, face, FACE_OUTLINE_WALL_PAINT, scratch);
            }
        }
        psxed_ui::PaintTargetPreview::PortalEdge {
            world_cell_x,
            world_cell_z,
            dir,
            valid,
        } => {
            let style = if valid {
                PORTAL_SEAM_STYLE
            } else {
                PORTAL_SEAM_INVALID_STYLE
            };
            if valid {
                if let Some((sx, sz)) = grid.world_cell_to_array(world_cell_x, world_cell_z) {
                    if let Some(edge) = canonical_portal_edge_for_array_cell(sx, sz, dir) {
                        let seam = portal_seam_edges_for_edge(grid, edge);
                        if !seam.is_empty() {
                            push_portal_seam_edges(grid, seam, style, scratch);
                            return;
                        }
                    }
                }
            }
            push_portal_edge_wall_outline(grid, world_cell_x, world_cell_z, dir, style, scratch);
        }
    }
}

/// Outline a cell at world-cell `(wcx, wcz)`. Floor / ceiling paint
/// previews use the same candidate heights the click path will
/// commit; generic cell previews stay on the ground footprint.
pub(super) fn push_cell_ghost_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    kind: PaintCellPreviewKind,
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    let x0 = wcx * s;
    let x1 = x0 + s;
    let z0 = wcz * s;
    let z1 = z0 + s;
    let (heights, style) = match kind {
        PaintCellPreviewKind::Ground => ([0; 4], FACE_OUTLINE_HOVER),
        PaintCellPreviewKind::Floor => (
            grid.floor_heights_aligned_to_neighbors_for_world_cell(wcx, wcz, 0),
            FACE_OUTLINE_FLOOR_PAINT,
        ),
        PaintCellPreviewKind::Ceiling => (
            grid.ceiling_heights_aligned_to_neighbors_for_world_cell(wcx, wcz),
            FACE_OUTLINE_CEILING_PAINT,
        ),
    };
    const LIFT: i32 = 4;
    let nw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::NW.idx()] + LIFT, z1]));
    let ne = gte_scene::project_vertex(world_to_view([x1, heights[Corner::NE.idx()] + LIFT, z1]));
    let se = gte_scene::project_vertex(world_to_view([x1, heights[Corner::SE.idx()] + LIFT, z0]));
    let sw = gte_scene::project_vertex(world_to_view([x0, heights[Corner::SW.idx()] + LIFT, z0]));
    if [nw, ne, se, sw].iter().any(|p| p.sz == 0) {
        return;
    }
    for (a, b) in [(nw, ne), (ne, se), (se, sw), (sw, nw)] {
        push_screen_line(scratch, a, b, style);
    }
}

/// Outline a wall at world-cell `(wcx, wcz)` on edge `dir`. Used
/// when `push_face_outline`'s array-bound check rejects an off-grid
/// ghost so the user still sees where the wall will land.
pub(super) fn push_ghost_wall_outline(
    grid: &WorldGrid,
    wcx: i32,
    wcz: i32,
    dir: GridDirection,
    heights: [i32; 4],
    scratch: &mut PreviewScratch,
) {
    let s = grid.sector_size;
    const LIFT: i32 = 4;
    let bounds = spatial::cell_bounds_from_world_cell(wcx, wcz, s);
    let Some(corners) = spatial::editor_wall_outline_corners(bounds, dir, heights, LIFT) else {
        return;
    };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        push_screen_line(
            scratch,
            projected[i],
            projected[(i + 1) % 4],
            FACE_OUTLINE_WALL_PAINT,
        );
    }
}

/// Hover and Selected outline styling. RGB plus screen-space line
/// thickness in pixels. Keep these light: they are editor affordances,
/// not scene geometry, and thick strokes obscure PS1-scale surfaces.
pub(super) const FACE_OUTLINE_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_ERROR: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x40, 0x40),
    thickness_px: 4.0,
};
pub(super) const FACE_OUTLINE_CULLED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x88, 0xA0, 0xAE),
    thickness_px: 1.0,
};
pub(super) const ENTITY_BOUND_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
pub(super) const ENTITY_BOUND_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
/// Wireframe outline drawn around an image prop's authored
/// collision AABB. Distinct warm orange to read as "collision /
/// physics" against the cooler selection / hover palette.
pub(super) const IMAGE_PROP_COLLISION_BOX: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xA0, 0x40),
    thickness_px: 1.5,
};
/// PaintWall hover preview -- green for "this would be added /
/// replaced". Slightly stronger than hover, but still thin enough
/// to leave the underlying face readable.
pub(super) const FACE_OUTLINE_WALL_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_FLOOR_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0x90),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const FACE_OUTLINE_CEILING_PAINT: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x80, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_PAINT_STROKE_WIDTH,
};
pub(super) const PORTAL_SEAM_STYLE: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x48, 0xD6),
    thickness_px: 3.0,
};
pub(super) const PORTAL_SEAM_INVALID_STYLE: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x40, 0x60),
    thickness_px: 2.0,
};
pub(super) const STREAMING_CHUNK_BOUNDARY: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xFF, 0xC4),
    thickness_px: 2.0,
};

#[derive(Copy, Clone)]
pub(super) struct FaceOutlineStyle {
    pub(super) rgb: (u8, u8, u8),
    pub(super) thickness_px: f32,
}

/// Hover vs Selected -- outline style picker for the unified
/// selection dispatch. Hover uses the lighter yellow; selected
/// uses the bolder cyan. Same constants the original face-only
/// path consumed.
#[derive(Copy, Clone)]
pub(super) enum OutlineRole {
    Hover,
    Selected,
    Error,
}

impl OutlineRole {
    pub(super) fn face_style(self) -> FaceOutlineStyle {
        match self {
            Self::Hover => FACE_OUTLINE_HOVER,
            Self::Selected => FACE_OUTLINE_SELECTED,
            Self::Error => FACE_OUTLINE_ERROR,
        }
    }
}

/// Dispatch a `Selection` to the appropriate outline helper.
/// Each variant gets its own screen-space overlay: face → 4
/// edge lines, edge → 1 line, vertex → cross.
pub(super) fn push_selection_outline(
    grid: &WorldGrid,
    selection: psxed_ui::Selection,
    role: OutlineRole,
    scratch: &mut PreviewScratch,
) {
    match selection {
        psxed_ui::Selection::Face(face) => {
            push_face_outline(grid, face, role.face_style(), scratch);
        }
        psxed_ui::Selection::Triangle(triangle) => {
            push_triangle_outline(grid, triangle, role.face_style(), scratch);
        }
        psxed_ui::Selection::Edge(edge) => {
            push_edge_outline(grid, edge, role.face_style(), scratch);
        }
        psxed_ui::Selection::Vertex(vertex) => {
            push_vertex_outline(grid, vertex, role.face_style(), scratch);
        }
    }
}

pub(super) fn push_triangle_outline(
    grid: &WorldGrid,
    triangle: psxed_ui::HorizontalTriangleRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let [c0, c1, c2] = triangle.corners;
    let Some(w0) = psxed_ui::face_corner_world(grid, triangle.face_corner(c0)) else {
        return;
    };
    let Some(w1) = psxed_ui::face_corner_world(grid, triangle.face_corner(c1)) else {
        return;
    };
    let Some(w2) = psxed_ui::face_corner_world(grid, triangle.face_corner(c2)) else {
        return;
    };
    let projected = [
        gte_scene::project_vertex(world_to_view(w0)),
        gte_scene::project_vertex(world_to_view(w1)),
        gte_scene::project_vertex(world_to_view(w2)),
    ];
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..3 {
        push_screen_line(scratch, projected[i], projected[(i + 1) % 3], style);
    }
}

pub(super) fn push_streaming_chunk_boundaries(
    project: &ProjectDocument,
    room_id: NodeId,
    grid: &WorldGrid,
    scratch: &mut PreviewScratch,
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
    let s = grid.sector_size;
    let y = 10;
    for room in plan.rooms {
        let x0 = room.world_origin[0] * s;
        let z0 = room.world_origin[1] * s;
        let x1 = x0 + room.size[0] as i32 * s;
        let z1 = z0 + room.size[1] as i32 * s;
        let projected: [psx_gte::scene::Projected; 4] = [
            gte_scene::project_vertex(world_to_view([x0, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z1])),
            gte_scene::project_vertex(world_to_view([x1, y, z0])),
            gte_scene::project_vertex(world_to_view([x0, y, z0])),
        ];
        if projected.iter().any(|p| p.sz == 0) {
            continue;
        }
        for i in 0..4 {
            push_screen_line(
                scratch,
                projected[i],
                projected[(i + 1) % 4],
                STREAMING_CHUNK_BOUNDARY,
            );
        }
    }
}

/// One thick screen-space line between the edge's two world
/// endpoints. Lifted slightly off the surface (same `LIFT` as
/// `push_face_outline`) so it doesn't z-fight the geometry it
/// outlines.
pub(super) fn push_edge_outline(
    grid: &WorldGrid,
    edge: psxed_ui::EdgeRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some((a, b)) = edge_world_endpoints(grid, edge) else {
        return;
    };
    let projected_a = gte_scene::project_vertex(world_to_view(a));
    let projected_b = gte_scene::project_vertex(world_to_view(b));
    if projected_a.sz == 0 || projected_b.sz == 0 {
        return;
    }
    push_screen_line(scratch, projected_a, projected_b, style);
}

/// Small screen-space cross at the vertex's world position.
/// The cross is drawn as four short line segments offset along
/// world axes so its on-screen size scales naturally with
/// distance -- close vertices read clearly, far ones don't
/// dominate the viewport.
pub(super) fn push_vertex_outline(
    grid: &WorldGrid,
    vertex: psxed_ui::VertexRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    let Some(world) = vertex_world_position(grid, vertex) else {
        return;
    };
    // Half-extent of the cross in world units. ~32 reads as a
    // few px in the viewport at orbit distances we use.
    const ARM: i32 = 32;
    let arms = [
        (
            [world[0] - ARM, world[1], world[2]],
            [world[0] + ARM, world[1], world[2]],
        ),
        (
            [world[0], world[1] - ARM, world[2]],
            [world[0], world[1] + ARM, world[2]],
        ),
        (
            [world[0], world[1], world[2] - ARM],
            [world[0], world[1], world[2] + ARM],
        ),
    ];
    for (a, b) in arms {
        let pa = gte_scene::project_vertex(world_to_view(a));
        let pb = gte_scene::project_vertex(world_to_view(b));
        if pa.sz == 0 || pb.sz == 0 {
            continue;
        }
        push_screen_line(scratch, pa, pb, style);
    }
}

pub(super) fn edge_world_endpoints(
    grid: &WorldGrid,
    edge: psxed_ui::EdgeRef,
) -> Option<([i32; 3], [i32; 3])> {
    use psxed_ui::{EdgeAnchor, FaceCornerRef};
    let (a, b) = match edge.anchor {
        EdgeAnchor::Floor { sx, sz, dir } => (
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Floor {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Ceiling { sx, sz, dir } => (
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_a(dir),
            },
            FaceCornerRef::Ceiling {
                sx,
                sz,
                corner: floor_edge_b(dir),
            },
        ),
        EdgeAnchor::Wall {
            sx,
            sz,
            dir,
            stack,
            edge: e,
        } => (
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_a(e),
            },
            FaceCornerRef::Wall {
                sx,
                sz,
                dir,
                stack,
                corner: wall_edge_b(e),
            },
        ),
    };
    Some((
        psxed_ui::face_corner_world(grid, a)?,
        psxed_ui::face_corner_world(grid, b)?,
    ))
}

pub(super) fn vertex_world_position(
    grid: &WorldGrid,
    vertex: psxed_ui::VertexRef,
) -> Option<[i32; 3]> {
    psxed_ui::face_corner_world(grid, vertex.anchor.as_face_corner())
}

pub(super) const fn floor_edge_a(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NW,
        GridDirection::East => psxed_ui::Corner::NE,
        GridDirection::South => psxed_ui::Corner::SE,
        GridDirection::West => psxed_ui::Corner::SW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::NW,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::NE,
    }
}

pub(super) const fn floor_edge_b(dir: GridDirection) -> psxed_ui::Corner {
    match dir {
        GridDirection::North => psxed_ui::Corner::NE,
        GridDirection::East => psxed_ui::Corner::SE,
        GridDirection::South => psxed_ui::Corner::SW,
        GridDirection::West => psxed_ui::Corner::NW,
        GridDirection::NorthWestSouthEast => psxed_ui::Corner::SE,
        GridDirection::NorthEastSouthWest => psxed_ui::Corner::SW,
    }
}

pub(super) const fn wall_edge_a(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BL,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::TL,
    }
}

pub(super) const fn wall_edge_b(edge: psxed_ui::WallEdge) -> psxed_ui::WallCorner {
    match edge {
        psxed_ui::WallEdge::Bottom => psxed_ui::WallCorner::BR,
        psxed_ui::WallEdge::Right => psxed_ui::WallCorner::TR,
        psxed_ui::WallEdge::Top => psxed_ui::WallCorner::TL,
        psxed_ui::WallEdge::Left => psxed_ui::WallCorner::BL,
    }
}

/// Stamp four short, screen-space-thick line segments along the
/// edges of a picked face. Drawing in screen space (after GTE
/// projection) keeps the outline a constant pixel weight regardless
/// of perspective, which matches Godot / Unity's "selection halo"
/// look. Lines pinned to OT slot 0 so they paint on top of every
/// floor / wall / ceiling.
pub(super) fn push_face_outline(
    grid: &WorldGrid,
    face: psxed_ui::FaceRef,
    style: FaceOutlineStyle,
    scratch: &mut PreviewScratch,
) {
    if face.sx >= grid.width || face.sz >= grid.depth {
        return;
    }
    let sector = grid.sector(face.sx, face.sz);
    let s = grid.sector_size;
    let x0 = grid.cell_world_x(face.sx);
    let x1 = x0 + s;
    let z0 = grid.cell_world_z(face.sz);
    let z1 = z0 + s;
    // Lift a hair off the surface so the outline doesn't z-fight
    // the face it's marking. Sloped floors keep their relative
    // outline position because we lift each corner by the same
    // amount along the local up axis.
    const LIFT: i32 = 4;
    let corners = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|f| {
            let h = f.heights;
            [
                [x0, h[0] + LIFT, z1],
                [x1, h[1] + LIFT, z1],
                [x1, h[2] + LIFT, z0],
                [x0, h[3] + LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Ceiling => sector.and_then(|s| s.ceiling.as_ref()).map(|c| {
            let h = c.heights;
            [
                [x0, h[0] - LIFT, z1],
                [x1, h[1] - LIFT, z1],
                [x1, h[2] - LIFT, z0],
                [x0, h[3] - LIFT, z0],
            ]
        }),
        psxed_ui::FaceKind::Wall { dir, stack } => {
            let h = sector
                .and_then(|s| s.walls.get(dir).get(stack as usize))
                .map(|wall| wall.heights)
                .unwrap_or_else(|| grid.wall_heights_aligned_to_surfaces(face.sx, face.sz, dir));
            // Inset along the wall's inward normal so the outline
            // sits inside the room rather than z-fighting the
            // wall surface when viewed from inside.
            spatial::editor_wall_outline_corners(
                grid.cell_bounds_world(face.sx, face.sz),
                dir,
                h,
                LIFT,
            )
        }
    };
    let Some(corners) = corners else { return };
    let projected: [psx_gte::scene::Projected; 4] = [
        gte_scene::project_vertex(world_to_view(corners[0])),
        gte_scene::project_vertex(world_to_view(corners[1])),
        gte_scene::project_vertex(world_to_view(corners[2])),
        gte_scene::project_vertex(world_to_view(corners[3])),
    ];
    // Skip outlines whose corners didn't project -- `project_vertex`
    // returns `sz == 0` for behind-camera or near-plane-clipped
    // points, which would produce nonsense screen lines.
    if projected.iter().any(|p| p.sz == 0) {
        return;
    }
    for i in 0..4 {
        let a = projected[i];
        let b = projected[(i + 1) % 4];
        push_screen_line(scratch, a, b, style);
    }
    let split = match face.kind {
        psxed_ui::FaceKind::Floor => sector.and_then(|s| s.floor.as_ref()).map(|face| face.split),
        psxed_ui::FaceKind::Ceiling => sector
            .and_then(|s| s.ceiling.as_ref())
            .map(|face| face.split),
        psxed_ui::FaceKind::Wall { .. } => None,
    };
    if let Some(split) = split {
        let (a, b) = match split {
            GridSplit::NorthWestSouthEast => (projected[0], projected[2]),
            GridSplit::NorthEastSouthWest => (projected[1], projected[3]),
        };
        push_screen_line(scratch, a, b, style);
    }
}

/// Queue one host-drawn overlay segment between two screen-projected
/// vertices. Unlike the scene command log, this is painted by egui on
/// top of the preview texture, so fractional widths and normal UI
/// compositing work as expected.
pub(super) fn push_screen_line(
    scratch: &mut PreviewScratch,
    a: psx_gte::scene::Projected,
    b: psx_gte::scene::Projected,
    style: FaceOutlineStyle,
) {
    push_overlay_line(
        scratch,
        egui::pos2(a.sx as f32, a.sy as f32),
        egui::pos2(b.sx as f32, b.sy as f32),
        style,
    );
}

pub(super) fn push_overlay_line(
    scratch: &mut PreviewScratch,
    a: egui::Pos2,
    b: egui::Pos2,
    style: FaceOutlineStyle,
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 0.5 {
        return;
    }
    scratch
        .overlay_lines
        .push(psxed_ui::EditorViewportOverlayLine::new(
            a,
            b,
            bright_overlay_color(style.rgb),
            style.thickness_px,
        ));
}

pub(super) fn bright_overlay_color(rgb: (u8, u8, u8)) -> egui::Color32 {
    let lift = |channel: u8| -> u8 {
        let c = channel as u16;
        (c + ((255 - c) * 3 / 5)).min(255) as u8
    };
    egui::Color32::from_rgb(lift(rgb.0), lift(rgb.1), lift(rgb.2))
}

#[cfg(test)]
mod surface_grid_tests {
    use super::*;

    fn leak_test_camera() -> psx_engine::WorldCamera {
        psx_engine::WorldCamera::orbit(
            psx_engine::WorldProjection::new(160, 120, 320, 32),
            psx_engine::WorldVertex::ZERO,
            512,
            psx_engine::Angle::from_q12(0),
            psx_engine::Angle::from_q12(0),
        )
    }

    #[test]
    fn bsp_pointfile_draws_an_opaque_green_segment() {
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(leak_test_camera(), &[[-64, 0, 0], [64, 0, 0]], &mut lines);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].color, egui::Color32::from_rgb(72, 236, 126));
        assert_eq!(lines[0].width, BSP_LEAK_PATH_WIDTH);
    }

    #[test]
    fn bsp_pointfile_near_clips_instead_of_dropping_a_crossing_segment() {
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(leak_test_camera(), &[[0, 0, 600], [64, 0, 0]], &mut lines);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn brush_grid_line_clips_to_convex_face_edges() {
        let polygon = [
            [-32.0, -16.0, 0.0],
            [32.0, -16.0, 0.0],
            [32.0, 16.0, 0.0],
            [-32.0, 16.0, 0.0],
        ];
        let (a, b) = clip_grid_line_to_polygon(&polygon, 0, 0.0).expect("center grid line");
        assert_eq!(a[0], 0.0);
        assert_eq!(b[0], 0.0);
        assert_eq!([a[1].min(b[1]), a[1].max(b[1])], [-16.0, 16.0]);
    }

    #[test]
    fn brush_grid_line_handles_a_line_coincident_with_an_edge() {
        let polygon = [
            [-32.0, -16.0, 0.0],
            [32.0, -16.0, 0.0],
            [32.0, 16.0, 0.0],
            [-32.0, 16.0, 0.0],
        ];
        let (a, b) = clip_grid_line_to_polygon(&polygon, 1, -16.0).expect("bottom grid edge");
        assert_eq!(a[1], -16.0);
        assert_eq!(b[1], -16.0);
        assert_eq!([a[0].min(b[0]), a[0].max(b[0])], [-32.0, 32.0]);
    }

    #[test]
    fn dense_surface_grid_coarsens_on_power_of_two_grid_multiples() {
        let polygon = [
            [-1024.0, -64.0, 0.0],
            [1024.0, -64.0, 0.0],
            [1024.0, 64.0, 0.0],
            [-1024.0, 64.0, 0.0],
        ];
        let (first, last, stride) = grid_index_range(&polygon, 0, 16.0);
        assert_eq!((first, last, stride), (-64, 64, 8));
        assert_eq!(first.rem_euclid(stride), 0);
        assert!(((last - first) / stride + 1) as usize <= BSP_SURFACE_GRID_LINES_PER_AXIS);
    }

    #[test]
    fn global_grid_cap_samples_evenly_across_all_candidates() {
        let total = 10_000u64;
        let selected: Vec<_> = (0..total)
            .filter(|ordinal| grid_candidate_selected(*ordinal, total))
            .collect();
        assert_eq!(selected.len(), BSP_SURFACE_GRID_SEGMENT_CAP);
        assert!(selected[0] < total / 100);
        assert!(selected[selected.len() - 1] > total * 99 / 100);
    }

    #[test]
    fn dominant_axis_is_the_face_projection_axis() {
        assert_eq!(dominant_normal_axis([8.0, 2.0, 1.0]), 0);
        assert_eq!(dominant_normal_axis([1.0, -9.0, 2.0]), 1);
        assert_eq!(dominant_normal_axis([1.0, 2.0, -10.0]), 2);
    }

    #[test]
    fn brush_surface_overlay_tracks_grid_interval_and_stays_translucent() {
        let mut project = ProjectDocument::new("surface grid");
        let root = project.active_scene().root;
        let group = project
            .active_scene_mut()
            .add_node(root, "Shell", NodeKind::Group);
        let mut brush = psxed_project::brush::Brush::cuboid([-64, -64, -64], [64, 64, 64]);
        brush.group = Some(group);
        project.active_scene_mut().brushes.push(brush);
        let projection = psx_engine::WorldProjection::new(160, 120, 320, 32);
        let camera = psx_engine::WorldCamera::orbit(
            projection,
            psx_engine::WorldVertex::ZERO,
            512,
            psx_engine::Angle::from_q12(0),
            psx_engine::Angle::from_q12(0),
        );

        let mut fine = Vec::new();
        prepend_bsp_surface_grid_overlay(
            &project,
            camera,
            16,
            &std::collections::HashSet::new(),
            &mut fine,
        );
        let mut coarse = Vec::new();
        prepend_bsp_surface_grid_overlay(
            &project,
            camera,
            32,
            &std::collections::HashSet::new(),
            &mut coarse,
        );

        assert!(!fine.is_empty());
        assert!(coarse.len() < fine.len());
        assert!(fine.iter().all(|line| line.color.a() < u8::MAX));
        assert!(fine.len() <= BSP_SURFACE_GRID_SEGMENT_CAP);

        let mut hidden = Vec::new();
        prepend_bsp_surface_grid_overlay(
            &project,
            camera,
            16,
            &std::collections::HashSet::from([group]),
            &mut hidden,
        );
        assert!(
            hidden.is_empty(),
            "hidden groups must not leave grid ghosts"
        );
    }
}
