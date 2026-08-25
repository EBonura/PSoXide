//! Editor-only viewport overlay drawing: selection, paint ghosts, bounds, and gizmos.

use super::*;

/// Surface-grid work is editor-only but still runs during camera navigation,
/// so keep both the per-face density and total host stroke count bounded.
/// Coarser lines stay on the authored grid by increasing the interval in
/// power-of-two multiples instead of inventing an unrelated spacing.
const BSP_SURFACE_GRID_LINES_PER_AXIS: usize = 24;
const BSP_SURFACE_GRID_SEGMENT_CAP: usize = 2_048;
/// TrenchBroom keeps major lines pinned to 64 world units even while the
/// visible minor interval coarsens with distance.
const BSP_SURFACE_GRID_MAJOR_WORLD_UNITS: i64 = 64;
const BSP_SURFACE_GRID_DARK: u8 = 26;
const BSP_SURFACE_GRID_LIGHT: u8 = 230;
const BSP_SURFACE_GRID_MINOR_ALPHA: u8 = 68;
const BSP_SURFACE_GRID_MAJOR_ALPHA: u8 = 104;
const BSP_SURFACE_GRID_MINOR_WIDTH: f32 = 0.65;
const BSP_SURFACE_GRID_MAJOR_WIDTH: f32 = 0.9;
const BSP_SURFACE_GRID_FADE_START_CELLS: f64 = 32.0;
const BSP_SURFACE_GRID_FADE_END_CELLS: f64 = 64.0;
const GRID_INTERSECTION_EPSILON: f64 = 1.0 / 4096.0;
const GRID_INTERSECTION_CAP: usize = 8;
const BSP_LEAK_PATH_RGB: (u8, u8, u8) = (72, 236, 126);
const BSP_LEAK_PATH_WIDTH: f32 = 2.5;
const BSP_LEAK_OPENING_RGB: (u8, u8, u8) = (255, 74, 52);
const BSP_LEAK_OPENING_WIDTH: f32 = 4.5;
/// Occlusion is evaluated at the centre of short screen-space spans. Two
/// pixels keeps a covered path visually continuous at brush silhouettes
/// without turning a ten-point pointfile into an expensive per-pixel trace.
const BSP_LEAK_OCCLUSION_SAMPLE_PIXELS: f32 = 2.0;
const BSP_LEAK_OCCLUSION_SEGMENT_CAP: usize = 256;
const BSP_LEAK_OCCLUSION_EPSILON: f64 = 1.0 / 1024.0;
const BSP_LEAK_ENDPOINT_NUDGE: f64 = 0.25;

#[derive(Copy, Clone)]
pub(super) struct FaceOutlineStyle {
    pub(super) rgb: (u8, u8, u8),
    pub(super) thickness_px: f32,
}

pub(super) const ENTITY_BOUND_HOVER: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0xE0, 0x60),
    thickness_px: EDITOR_PREVIEW_HOVER_STROKE_WIDTH,
};
pub(super) const ENTITY_BOUND_SELECTED: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0x60, 0xC8, 0xFF),
    thickness_px: EDITOR_PREVIEW_SELECTED_STROKE_WIDTH,
};
pub(super) const PORTAL_SEAM_STYLE: FaceOutlineStyle = FaceOutlineStyle {
    rgb: (0xFF, 0x48, 0xD6),
    thickness_px: 3.0,
};

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
/// The line is opaque green to match TrenchBroom's pointfile convention,
/// near-clipped per segment, and visibility-tested against every rendered
/// solid brush. This is necessary because host overlay strokes otherwise
/// paint through walls regardless of the scene ordering table.
pub(super) fn append_bsp_leak_path_overlay(
    project: &ProjectDocument,
    camera: psx_engine::WorldCamera,
    leak_path: &[[i32; 3]],
    likely_opening: &[[i32; 3]],
    hidden_scene_nodes: &HashSet<NodeId>,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    if leak_path.len() < 2 && likely_opening.len() < 3 {
        return;
    }
    let color = egui::Color32::from_rgb(
        BSP_LEAK_PATH_RGB.0,
        BSP_LEAK_PATH_RGB.1,
        BSP_LEAK_PATH_RGB.2,
    );
    with_cached_solved_brushes(project, |solved_brushes| {
        let scene = project.active_scene();
        for points in leak_path.windows(2) {
            append_occluded_bsp_segment(
                scene,
                solved_brushes,
                hidden_scene_nodes,
                camera,
                points[0],
                points[1],
                color,
                BSP_LEAK_PATH_WIDTH,
                overlay_lines,
            );
        }

        if likely_opening.len() >= 3 {
            let warning = egui::Color32::from_rgb(
                BSP_LEAK_OPENING_RGB.0,
                BSP_LEAK_OPENING_RGB.1,
                BSP_LEAK_OPENING_RGB.2,
            );
            let centroid: [i32; 3] = std::array::from_fn(|axis| {
                likely_opening
                    .iter()
                    .map(|point| i64::from(point[axis]))
                    .sum::<i64>()
                    .checked_div(likely_opening.len() as i64)
                    .unwrap_or(0)
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
            });
            // The exact component boundary commonly coincides with adjacent
            // sealing brushes. Inset the visual marker slightly, but retain
            // most of its authored extent: this polygon is already the merged
            // outer boundary, not one tiny BSP partition fragment.
            let marker_vertices: Vec<_> = likely_opening
                .iter()
                .map(|vertex| {
                    std::array::from_fn(|axis| {
                        let center = i64::from(centroid[axis]);
                        (center + (i64::from(vertex[axis]) - center) * 9 / 10)
                            .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                            as i32
                    })
                })
                .collect();
            for index in 0..marker_vertices.len() {
                let vertex = marker_vertices[index];
                let next = marker_vertices[(index + 1) % marker_vertices.len()];
                append_occluded_bsp_segment(
                    scene,
                    solved_brushes,
                    hidden_scene_nodes,
                    camera,
                    vertex,
                    next,
                    warning,
                    BSP_LEAK_OPENING_WIDTH,
                    overlay_lines,
                );
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn append_occluded_bsp_segment(
    scene: &Scene,
    solved_brushes: &[PreviewSolvedBrush],
    hidden_scene_nodes: &HashSet<NodeId>,
    camera: psx_engine::WorldCamera,
    segment_a: [i32; 3],
    segment_b: [i32; 3],
    color: egui::Color32,
    width: f32,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let Some((projected_a, projected_b)) =
        project_clipped_world_segment(camera, segment_a, segment_b)
    else {
        return;
    };
    let dx = projected_b.x - projected_a.x;
    let dy = projected_b.y - projected_a.y;
    let screen_length = (dx * dx + dy * dy).sqrt();
    if screen_length < 0.5 {
        return;
    }
    let steps = ((screen_length / BSP_LEAK_OCCLUSION_SAMPLE_PIXELS).ceil() as usize)
        .clamp(1, BSP_LEAK_OCCLUSION_SEGMENT_CAP);
    let mut visible_run_start = None;
    for step in 0..steps {
        let midpoint = interpolate_world_point(segment_a, segment_b, step * 2 + 1, steps * 2);
        let in_front = camera.view_vertex(world_vertex_from_f64(midpoint)).z
            >= camera.projection.near_z.max(1);
        let visible = in_front
            && !leak_point_occluded(
                scene,
                solved_brushes,
                hidden_scene_nodes,
                camera.position,
                midpoint,
            );
        match (visible_run_start, visible) {
            (None, true) => visible_run_start = Some(step),
            (Some(start), false) => {
                push_bsp_leak_visible_run(
                    camera,
                    segment_a,
                    segment_b,
                    start,
                    step,
                    steps,
                    color,
                    width,
                    overlay_lines,
                );
                visible_run_start = None;
            }
            _ => {}
        }
    }
    if let Some(start) = visible_run_start {
        push_bsp_leak_visible_run(
            camera,
            segment_a,
            segment_b,
            start,
            steps,
            steps,
            color,
            width,
            overlay_lines,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_bsp_leak_visible_run(
    camera: psx_engine::WorldCamera,
    segment_a: [i32; 3],
    segment_b: [i32; 3],
    start_step: usize,
    end_step: usize,
    steps: usize,
    color: egui::Color32,
    width: f32,
    overlay_lines: &mut Vec<psxed_ui::EditorViewportOverlayLine>,
) {
    let a = interpolate_world_point(segment_a, segment_b, start_step, steps)
        .map(|value| value.round() as i32);
    let b = interpolate_world_point(segment_a, segment_b, end_step, steps)
        .map(|value| value.round() as i32);
    let Some((a, b)) = project_clipped_world_segment(camera, a, b) else {
        return;
    };
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    if dx * dx + dy * dy < 0.25 {
        return;
    }
    overlay_lines.push(psxed_ui::EditorViewportOverlayLine::new(a, b, color, width));
}

fn interpolate_world_point(
    a: [i32; 3],
    b: [i32; 3],
    numerator: usize,
    denominator: usize,
) -> [f64; 3] {
    let amount = numerator as f64 / denominator.max(1) as f64;
    std::array::from_fn(|axis| {
        f64::from(a[axis]) + (f64::from(b[axis]) - f64::from(a[axis])) * amount
    })
}

fn world_vertex_from_f64(point: [f64; 3]) -> psx_engine::WorldVertex {
    psx_engine::WorldVertex::new(
        point[0].round() as i32,
        point[1].round() as i32,
        point[2].round() as i32,
    )
}

fn leak_point_occluded(
    scene: &Scene,
    solved_brushes: &[PreviewSolvedBrush],
    hidden_scene_nodes: &HashSet<NodeId>,
    camera: psx_engine::WorldVertex,
    target: [f64; 3],
) -> bool {
    let start = [
        f64::from(camera.x),
        f64::from(camera.y),
        f64::from(camera.z),
    ];
    let direction = std::array::from_fn(|axis| target[axis] - start[axis]);
    scene
        .brushes
        .iter()
        .zip(solved_brushes)
        .any(|(brush, solved)| {
            brush.contents.is_solid()
                && solved.pickable
                && !brush_group_hidden(scene, hidden_scene_nodes, brush)
                && segment_intersects_bounds(start, direction, solved.bounds)
                && segment_intersects_convex_brush(start, direction, &solved.normalized_planes)
        })
}

fn segment_intersects_bounds(
    start: [f64; 3],
    direction: [f64; 3],
    bounds: PreviewBrushBounds,
) -> bool {
    let mut enter = 0.0f64;
    let mut exit = 1.0f64;
    for axis in 0..3 {
        if direction[axis].abs() <= BSP_LEAK_OCCLUSION_EPSILON {
            if start[axis] < bounds.min[axis] - BSP_LEAK_OCCLUSION_EPSILON
                || start[axis] > bounds.max[axis] + BSP_LEAK_OCCLUSION_EPSILON
            {
                return false;
            }
            continue;
        }
        let a = (bounds.min[axis] - start[axis]) / direction[axis];
        let b = (bounds.max[axis] - start[axis]) / direction[axis];
        enter = enter.max(a.min(b));
        exit = exit.min(a.max(b));
        if enter > exit {
            return false;
        }
    }
    exit > 0.0 && enter < 1.0
}

fn segment_intersects_convex_brush(
    start: [f64; 3],
    direction: [f64; 3],
    planes: &[([f64; 3], f64)],
) -> bool {
    let mut enter = 0.0f64;
    let mut exit = 1.0f64;
    for &(normal, distance) in planes {
        let start_distance = dot_f64(normal, start) - distance;
        let denominator = dot_f64(normal, direction);
        if denominator.abs() <= BSP_LEAK_OCCLUSION_EPSILON {
            if start_distance > BSP_LEAK_OCCLUSION_EPSILON {
                return false;
            }
            continue;
        }
        let crossing = -start_distance / denominator;
        if denominator < 0.0 {
            enter = enter.max(crossing);
        } else {
            exit = exit.min(crossing);
        }
        if enter > exit {
            return false;
        }
    }
    let segment_length = dot_f64(direction, direction).sqrt();
    let endpoint_nudge = (BSP_LEAK_ENDPOINT_NUDGE / segment_length.max(1.0)).min(0.25);
    exit > endpoint_nudge && enter < 1.0 - endpoint_nudge
}

fn dot_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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
    // The material pass renders the CSG-resolved exterior, not every raw
    // authored plane. Sharing that exact surface set prevents internal or
    // overlap-hidden brush faces from leaking host grid strokes through the
    // visible scene.
    with_cached_csg_surfaces(project, hidden_scene_nodes, |surfaces| {
        for cached in surfaces {
            let surface = &cached.surface;
            if let Some(ranges) = surface_grid_face_ranges(
                &surface.vertices,
                surface.plane.normal.map(|component| component as f64),
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
    });

    let mut grid_lines = Vec::new();
    let mut candidate_ordinal = 0u64;
    with_cached_csg_surfaces(project, hidden_scene_nodes, |surfaces| {
        for cached in surfaces {
            let surface = &cached.surface;
            let Some(brush) = scene.brushes.get(surface.source_brush) else {
                continue;
            };
            let face_index = surface.source_face;
            if brush.faces.get(face_index).is_some_and(|face| {
                face.material.is_some_and(|material| {
                    project.resource(material).is_some_and(|resource| {
                        matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                    })
                })
            }) {
                continue;
            }
            append_bsp_surface_grid_face(
                &surface.vertices,
                surface.plane.normal.map(|component| component as f64),
                camera,
                step,
                candidate_count,
                &mut candidate_ordinal,
                adaptive_grid_value(material_color(
                    project,
                    brush.faces.get(face_index).and_then(|face| face.material),
                    brush_fallback_color(face_index),
                )),
                &mut grid_lines,
            );
        }
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

    let surfaces = rebuild_csg_surfaces(project, hidden_scene_nodes);
    let step = f64::from(grid_units.max(1));
    let mut candidate_count = 0u64;
    for cached in &surfaces {
        let surface = &cached.surface;
        if let Some(ranges) = surface_grid_face_ranges(
            &surface.vertices,
            surface.plane.normal.map(|component| component as f64),
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

    let mut grid_lines = Vec::new();
    let mut candidate_ordinal = 0u64;
    let scene = project.active_scene();
    for cached in &surfaces {
        let surface = &cached.surface;
        let Some(brush) = scene.brushes.get(surface.source_brush) else {
            continue;
        };
        let face_index = surface.source_face;
        if brush.faces.get(face_index).is_some_and(|face| {
            face.material.is_some_and(|material| {
                project.resource(material).is_some_and(|resource| {
                    matches!(&resource.data, ResourceData::Material(material) if material.sky_aperture)
                })
            })
        }) {
            continue;
        }
        append_bsp_surface_grid_face(
            &surface.vertices,
            surface.plane.normal.map(|component| component as f64),
            camera,
            step,
            candidate_count,
            &mut candidate_ordinal,
            adaptive_grid_value(material_color(
                project,
                brush.faces.get(face_index).and_then(|face| face.material),
                brush_fallback_color(face_index),
            )),
            &mut grid_lines,
        );
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
    grid_value: u8,
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
                    append_projected_grid_segment(
                        camera,
                        a,
                        b,
                        index as f64 * step,
                        step,
                        grid_value,
                        overlay_lines,
                    );
                }
            }
            let Some(next) = index.checked_add(stride) else {
                break;
            };
            index = next;
        }
    }
}

type SurfaceGridRange = (usize, (i64, i64, i64));
type SurfaceGridFaceRanges = [SurfaceGridRange; 2];

fn surface_grid_face_ranges(
    polygon: &[[f64; 3]],
    normal: [f64; 3],
    camera: psx_engine::WorldCamera,
    step: f64,
) -> Option<SurfaceGridFaceRanges> {
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
    world_coordinate: f64,
    grid_step: f64,
    grid_value: u8,
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
    let camera_position = [
        f64::from(camera.position.x),
        f64::from(camera.position.y),
        f64::from(camera.position.z),
    ];
    // TrenchBroom fades per fragment. A host overlay is one whole segment, so
    // use its closest point to the camera; using the midpoint incorrectly
    // erases a long line whose visible portion is nearby.
    let camera_distance = point_segment_distance_squared(camera_position, a, b).sqrt();
    let distance_fade = surface_grid_distance_fade(grid_step, camera_distance);
    if distance_fade <= 0.0 {
        return;
    }

    let world_vertex = |point: [f64; 3]| {
        [
            point[0].round() as i32,
            point[1].round() as i32,
            point[2].round() as i32,
        ]
    };
    let Some((a, b)) = project_clipped_world_segment(camera, world_vertex(a), world_vertex(b))
    else {
        return;
    };
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    if dx * dx + dy * dy < 2.25 {
        return;
    }
    let major = (world_coordinate / BSP_SURFACE_GRID_MAJOR_WORLD_UNITS as f64)
        .fract()
        .abs()
        <= GRID_INTERSECTION_EPSILON;
    let base_alpha = if major {
        BSP_SURFACE_GRID_MAJOR_ALPHA
    } else {
        BSP_SURFACE_GRID_MINOR_ALPHA
    };
    let alpha = (f64::from(base_alpha) * distance_fade).round() as u8;
    if alpha == 0 {
        return;
    }
    overlay_lines.push(psxed_ui::EditorViewportOverlayLine::new(
        a,
        b,
        egui::Color32::from_rgba_unmultiplied(grid_value, grid_value, grid_value, alpha),
        if major {
            BSP_SURFACE_GRID_MAJOR_WIDTH
        } else {
            BSP_SURFACE_GRID_MINOR_WIDTH
        },
    ));
}

fn adaptive_grid_value(background: (u8, u8, u8)) -> u8 {
    let luma = 0.299 * f64::from(background.0)
        + 0.587 * f64::from(background.1)
        + 0.114 * f64::from(background.2);
    if luma < 127.5 {
        BSP_SURFACE_GRID_LIGHT
    } else {
        BSP_SURFACE_GRID_DARK
    }
}

fn surface_grid_distance_fade(grid_step: f64, camera_distance: f64) -> f64 {
    1.0 - smoothstep(
        grid_step * BSP_SURFACE_GRID_FADE_START_CELLS,
        grid_step * BSP_SURFACE_GRID_FADE_END_CELLS,
        camera_distance,
    )
}

fn smoothstep(edge0: f64, edge1: f64, value: f64) -> f64 {
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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

fn point_segment_distance_squared(point: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [point[0] - a[0], point[1] - a[1], point[2] - a[2]];
    let denominator = dot3_f64(ab, ab);
    if denominator <= GRID_INTERSECTION_EPSILON * GRID_INTERSECTION_EPSILON {
        return squared_distance_f64(point, a);
    }
    let t = (dot3_f64(ap, ab) / denominator).clamp(0.0, 1.0);
    squared_distance_f64(
        point,
        [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t],
    )
}

fn dot3_f64(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Ring + bulb gizmos for BSP world-space lights. Positions are raw world
/// units; the radius scales by the World sector
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
        let project = ProjectDocument::new("pointfile overlay");
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[[-64, 0, 0], [64, 0, 0]],
            &[],
            &HashSet::new(),
            &mut lines,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].color, egui::Color32::from_rgb(72, 236, 126));
        assert_eq!(lines[0].width, BSP_LEAK_PATH_WIDTH);
    }

    #[test]
    fn bsp_pointfile_near_clips_instead_of_dropping_a_crossing_segment() {
        let project = ProjectDocument::new("pointfile near clip");
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[[0, 0, 600], [64, 0, 0]],
            &[],
            &HashSet::new(),
            &mut lines,
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn bsp_pointfile_is_hidden_by_solid_brush_geometry() {
        let mut project = ProjectDocument::new("pointfile occlusion");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [-128, -128, 128],
                [128, 128, 256],
            ));
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[[-64, 0, 0], [64, 0, 0]],
            &[],
            &HashSet::new(),
            &mut lines,
        );
        assert!(
            lines.is_empty(),
            "the wall between camera and path must occlude it"
        );
    }

    #[test]
    fn bsp_pointfile_splits_around_a_partial_brush_occluder() {
        let mut project = ProjectDocument::new("partial pointfile occlusion");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [-32, -128, 128],
                [32, 128, 256],
            ));
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[[-256, 0, 0], [256, 0, 0]],
            &[],
            &HashSet::new(),
            &mut lines,
        );
        assert_eq!(
            lines.len(),
            2,
            "the visible path should stop on both wall edges"
        );
        assert!(lines[0].b.x < 160.0);
        assert!(lines[1].a.x > 160.0);
    }

    #[test]
    fn hidden_brush_group_does_not_occlude_the_pointfile() {
        let mut project = ProjectDocument::new("hidden pointfile occluder");
        let root = project.active_scene().root;
        let group = project
            .active_scene_mut()
            .add_node(root, "Hidden shell", NodeKind::Group);
        let mut brush = psxed_project::brush::Brush::cuboid([-128, -128, 128], [128, 128, 256]);
        brush.group = Some(group);
        project.active_scene_mut().brushes.push(brush);
        let mut hidden = HashSet::new();
        hidden.insert(group);
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[[-64, 0, 0], [64, 0, 0]],
            &[],
            &hidden,
            &mut lines,
        );
        assert_eq!(
            lines.len(),
            1,
            "hidden editor geometry must not cast an overlay shadow"
        );
    }

    #[test]
    fn bsp_likely_opening_draws_a_prominent_red_outline() {
        let project = ProjectDocument::new("likely opening overlay");
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[],
            &[[-64, -64, 0], [64, -64, 0], [64, 64, 0], [-64, 64, 0]],
            &HashSet::new(),
            &mut lines,
        );
        assert_eq!(
            lines.len(),
            4,
            "the marker must not obscure the map with spokes"
        );
        assert!(lines.iter().all(|line| {
            line.color
                == egui::Color32::from_rgb(
                    BSP_LEAK_OPENING_RGB.0,
                    BSP_LEAK_OPENING_RGB.1,
                    BSP_LEAK_OPENING_RGB.2,
                )
        }));
        assert!(lines
            .iter()
            .any(|line| line.width == BSP_LEAK_OPENING_WIDTH));
    }

    #[test]
    fn bsp_likely_opening_is_hidden_by_solid_brush_geometry() {
        let mut project = ProjectDocument::new("likely opening occlusion");
        project
            .active_scene_mut()
            .brushes
            .push(psxed_project::brush::Brush::cuboid(
                [-128, -128, 128],
                [128, 128, 256],
            ));
        let mut lines = Vec::new();
        append_bsp_leak_path_overlay(
            &project,
            leak_test_camera(),
            &[],
            &[[-64, -64, 0], [64, -64, 0], [64, 64, 0], [-64, 64, 0]],
            &HashSet::new(),
            &mut lines,
        );
        assert!(
            lines.is_empty(),
            "the likely-opening marker must obey the same wall occlusion as the route"
        );
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
    fn surface_grid_chooses_the_farthest_luminance_extreme() {
        assert_eq!(adaptive_grid_value((0, 0, 0)), BSP_SURFACE_GRID_LIGHT);
        assert_eq!(adaptive_grid_value((255, 255, 255)), BSP_SURFACE_GRID_DARK);
        assert_eq!(adaptive_grid_value((127, 127, 127)), BSP_SURFACE_GRID_LIGHT);
        assert_eq!(adaptive_grid_value((128, 128, 128)), BSP_SURFACE_GRID_DARK);
    }

    #[test]
    fn surface_grid_distance_fade_matches_the_trenchbroom_ramp() {
        assert_eq!(surface_grid_distance_fade(16.0, 511.0), 1.0);
        assert!((surface_grid_distance_fade(16.0, 768.0) - 0.5).abs() <= f64::EPSILON);
        assert_eq!(surface_grid_distance_fade(16.0, 1025.0), 0.0);
        assert_eq!(
            surface_grid_distance_fade(128.0, 4095.0),
            1.0,
            "larger authored grids keep the same fade distance in cells"
        );
    }

    #[test]
    fn long_surface_line_uses_its_closest_point_for_distance_fade() {
        let distance = point_segment_distance_squared(
            [0.0, 10.0, 0.0],
            [-10_000.0, 0.0, 0.0],
            [10_000.0, 0.0, 0.0],
        )
        .sqrt();
        assert!((distance - 10.0).abs() <= f64::EPSILON);
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
