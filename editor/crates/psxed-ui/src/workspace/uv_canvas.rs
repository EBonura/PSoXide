//! TrenchBroom-style face UV canvas: the selected face's polygon
//! projected into repeating texture space, with drag-to-slide,
//! handle-to-scale, and Alt-drag-to-rotate. Every edit flows through
//! the same [`crate::UvEditTransaction`] path as the numeric fields,
//! so anchoring and undo behave identically.

use super::*;
use psxed_project::brush::{paraxial_uv, FaceUv, BRUSH_UV_UNITS_PER_TEXEL};

/// Pan/zoom of the canvas, remembered per selected face.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UvCanvasViewState {
    pub(crate) brush: usize,
    pub(crate) face: usize,
    /// Texel coordinate shown at the canvas centre.
    pub(crate) center: [f32; 2],
    /// Zoom: screen pixels per texel.
    pub(crate) pixels_per_texel: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UvCanvasDragKind {
    /// Slide the face across the texture (offset).
    Move,
    /// Scale about the polygon centroid; the mask picks U/V axes.
    Scale([bool; 2]),
    /// Rotate about the polygon centroid.
    Rotate,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UvCanvasDragState {
    pub(crate) brush: usize,
    pub(crate) face: usize,
    pub(crate) kind: UvCanvasDragKind,
    /// Mapping at drag start: every frame solves fresh from here so a
    /// long drag never accumulates per-frame rounding.
    pub(crate) start_uv: FaceUv,
    /// Pointer position at drag start, texel space.
    pub(crate) start_mouse: [f64; 2],
    /// Applied-UV polygon centroid at drag start, texel space.
    pub(crate) center: [f64; 2],
}

const HANDLE_HIT_PX: f32 = 9.0;
const ROTATE_HANDLE_OFFSET_PX: f32 = 22.0;
/// Matches the numeric fields' 10%..1600% range.
const SCALE_Q8_MIN: i32 = 26;
const SCALE_Q8_MAX: i32 = 4096;

/// The selected face's solved polygon in applied texel space.
pub(crate) fn face_texel_polygon(
    brush: &psxed_project::brush::Brush,
    face_index: usize,
) -> Option<Vec<[f64; 2]>> {
    let solved = brush.solve();
    let polygon = solved.polygons.get(face_index)?.as_ref()?;
    let face = brush.faces.get(face_index)?;
    let plane = psxed_project::brush::Plane::from_points(face.points)?;
    Some(
        polygon
            .verts
            .iter()
            .map(|&vertex| {
                let raw = paraxial_uv(&plane, vertex);
                face.uv.apply([
                    raw[0] / BRUSH_UV_UNITS_PER_TEXEL,
                    raw[1] / BRUSH_UV_UNITS_PER_TEXEL,
                ])
            })
            .collect(),
    )
}

pub(crate) fn polygon_centroid(polygon: &[[f64; 2]]) -> [f64; 2] {
    let count = polygon.len().max(1) as f64;
    let mut center = [0.0; 2];
    for point in polygon {
        center[0] += point[0];
        center[1] += point[1];
    }
    [center[0] / count, center[1] / count]
}

fn polygon_bounds(polygon: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    let mut min = [f64::MAX; 2];
    let mut max = [f64::MIN; 2];
    for point in polygon {
        for axis in 0..2 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    (min, max)
}

/// Fit the polygon in the canvas with margin.
pub(crate) fn fit_view(
    brush: usize,
    face: usize,
    polygon: &[[f64; 2]],
    rect: egui::Rect,
) -> UvCanvasViewState {
    let (min, max) = polygon_bounds(polygon);
    let span = [(max[0] - min[0]).max(8.0), (max[1] - min[1]).max(8.0)];
    let zoom = ((rect.width() as f64 / (span[0] * 1.5)) as f32)
        .min((rect.height() as f64 / (span[1] * 1.5)) as f32)
        .clamp(0.05, 32.0);
    UvCanvasViewState {
        brush,
        face,
        center: [
            ((min[0] + max[0]) * 0.5) as f32,
            ((min[1] + max[1]) * 0.5) as f32,
        ],
        pixels_per_texel: zoom,
    }
}

pub(crate) fn texel_to_screen(view: &UvCanvasViewState, rect: egui::Rect, texel: [f64; 2]) -> Pos2 {
    Pos2::new(
        rect.center().x + (texel[0] as f32 - view.center[0]) * view.pixels_per_texel,
        rect.center().y + (texel[1] as f32 - view.center[1]) * view.pixels_per_texel,
    )
}

pub(crate) fn screen_to_texel(view: &UvCanvasViewState, rect: egui::Rect, pos: Pos2) -> [f64; 2] {
    [
        f64::from(view.center[0] + (pos.x - rect.center().x) / view.pixels_per_texel),
        f64::from(view.center[1] + (pos.y - rect.center().y) / view.pixels_per_texel),
    ]
}

fn clamp_i16(value: f64) -> i16 {
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Slide: the polygon follows the pointer in whole texels.
pub(crate) fn move_gesture(drag: &UvCanvasDragState, current: FaceUv, mouse: [f64; 2]) -> FaceUv {
    let mut edited = current;
    edited.offset_texels = [
        clamp_i16(f64::from(drag.start_uv.offset_texels[0]) + mouse[0] - drag.start_mouse[0]),
        clamp_i16(f64::from(drag.start_uv.offset_texels[1]) + mouse[1] - drag.start_mouse[1]),
    ];
    // A pure slide must read as one against the transaction logic:
    // shape terms stay exactly the live mapping's.
    edited.rotation_deg = current.rotation_deg;
    edited.scale_q8 = current.scale_q8;
    edited
}

/// Scale about the centroid: the grabbed handle tracks the pointer.
/// The polygon growing by `f` means the texture repeats more densely
/// across it, so the stored scale divides by `f`.
pub(crate) fn scale_gesture(
    drag: &UvCanvasDragState,
    current: FaceUv,
    mouse: [f64; 2],
    mask: [bool; 2],
) -> FaceUv {
    let mut edited = current;
    for axis in 0..2 {
        if !mask[axis] {
            continue;
        }
        let start_arm = drag.start_mouse[axis] - drag.center[axis];
        if start_arm.abs() < 2.0 {
            continue;
        }
        let factor = ((mouse[axis] - drag.center[axis]) / start_arm).clamp(0.05, 20.0);
        edited.scale_q8[axis] = (f64::from(drag.start_uv.scale_q8[axis]) / factor)
            .round()
            .clamp(f64::from(SCALE_Q8_MIN), f64::from(SCALE_Q8_MAX))
            as i16;
    }
    edited
}

/// Rotate about the centroid: the polygon follows the pointer's
/// angular sweep. `snap_deg` rounds the swept angle (1 = free).
pub(crate) fn rotate_gesture(
    drag: &UvCanvasDragState,
    current: FaceUv,
    mouse: [f64; 2],
    snap_deg: f64,
) -> FaceUv {
    let angle = |point: [f64; 2]| (point[1] - drag.center[1]).atan2(point[0] - drag.center[0]);
    let mut swept = (angle(mouse) - angle(drag.start_mouse)).to_degrees();
    while swept > 180.0 {
        swept -= 360.0;
    }
    while swept < -180.0 {
        swept += 360.0;
    }
    let snap = snap_deg.max(1.0);
    let swept = (swept / snap).round() * snap;
    let mut rotation = f64::from(drag.start_uv.rotation_deg) + swept;
    while rotation > 359.0 {
        rotation -= 360.0;
    }
    while rotation < -359.0 {
        rotation += 360.0;
    }
    let mut edited = current;
    edited.rotation_deg = rotation.round() as i16;
    edited
}

impl EditorWorkspace {
    /// Draw the face-in-texture canvas and run any live gesture.
    /// Returns `Some((edited, interacting))` while a canvas gesture
    /// owns the mapping this frame (the release frame reports
    /// `interacting = false` so the edit transaction closes).
    pub(crate) fn draw_face_uv_canvas(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        face_index: usize,
        current: FaceUv,
    ) -> Option<(FaceUv, bool)> {
        self.refresh_texture_thumbs(ui.ctx());
        let brush = self.project.active_scene().brushes.get(index)?;
        let polygon = face_texel_polygon(brush, face_index)?;
        if polygon.len() < 3 {
            return None;
        }
        let texture = brush
            .faces
            .get(face_index)
            .and_then(|face| face.material)
            .and_then(|id| self.texture_thumbs.get(&id))
            .map(|entry| {
                (
                    entry.handle.id(),
                    [
                        f32::from(entry.stats.width.max(8)),
                        f32::from(entry.stats.height.max(8)),
                    ],
                )
            });

        let width = ui.available_width().clamp(160.0, 340.0);
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(width, (width * 0.75).clamp(160.0, 260.0)),
            Sense::click_and_drag(),
        );
        if !ui.is_rect_visible(rect) {
            return None;
        }

        // View: refit on face change or double-click.
        let stale = self
            .brush_uv_canvas_view
            .is_none_or(|view| view.brush != index || view.face != face_index);
        if stale || response.double_clicked() {
            self.brush_uv_canvas_view = Some(fit_view(index, face_index, &polygon, rect));
        }
        let mut view = self.brush_uv_canvas_view.expect("seeded above");

        // Scroll zoom about the hovered texel.
        if response.hovered() {
            let scroll = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                if let Some(pointer) = response.hover_pos() {
                    let before = screen_to_texel(&view, rect, pointer);
                    view.pixels_per_texel =
                        (view.pixels_per_texel * 1.0035_f32.powf(scroll)).clamp(0.05, 64.0);
                    let after = screen_to_texel(&view, rect, pointer);
                    view.center[0] -= (after[0] - before[0]) as f32;
                    view.center[1] -= (after[1] - before[1]) as f32;
                }
            }
        }
        self.brush_uv_canvas_view = Some(view);

        // ---- paint ----
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, Color32::from_gray(18));
        let repeat = texture.map_or([64.0, 64.0], |(_, dims)| dims);
        // Bound the tile fill so extreme zoom-out cannot allocate an
        // unbounded mesh: below the cap the grid alone reads fine.
        let tiles_x = rect.width() / (repeat[0] * view.pixels_per_texel);
        let tiles_y = rect.height() / (repeat[1] * view.pixels_per_texel);
        if let Some((texture_id, dims)) = texture {
            if tiles_x <= 40.0 && tiles_y <= 40.0 {
                let min = screen_to_texel(&view, rect, rect.min);
                let max = screen_to_texel(&view, rect, rect.max);
                let first = [
                    (min[0] / f64::from(dims[0])).floor() as i32,
                    (min[1] / f64::from(dims[1])).floor() as i32,
                ];
                let last = [
                    (max[0] / f64::from(dims[0])).ceil() as i32,
                    (max[1] / f64::from(dims[1])).ceil() as i32,
                ];
                for tile_y in first[1]..last[1] {
                    for tile_x in first[0]..last[0] {
                        let corner = [
                            f64::from(tile_x) * f64::from(dims[0]),
                            f64::from(tile_y) * f64::from(dims[1]),
                        ];
                        let tile_rect = egui::Rect::from_min_max(
                            texel_to_screen(&view, rect, corner),
                            texel_to_screen(
                                &view,
                                rect,
                                [
                                    corner[0] + f64::from(dims[0]),
                                    corner[1] + f64::from(dims[1]),
                                ],
                            ),
                        );
                        painter.image(
                            texture_id,
                            tile_rect,
                            egui::Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                }
            }
        }
        // Repeat grid + the texture-space origin axes.
        if tiles_x <= 80.0 && tiles_y <= 80.0 {
            let min = screen_to_texel(&view, rect, rect.min);
            let max = screen_to_texel(&view, rect, rect.max);
            let grid = Stroke::new(1.0, Color32::from_white_alpha(14));
            let axis = Stroke::new(1.0, Color32::from_rgb(120, 150, 200));
            let mut line = (min[0] / f64::from(repeat[0])).floor() * f64::from(repeat[0]);
            while line <= max[0] {
                let x = texel_to_screen(&view, rect, [line, 0.0]).x;
                painter.vline(
                    x,
                    rect.y_range(),
                    if line.abs() < 0.5 { axis } else { grid },
                );
                line += f64::from(repeat[0]);
            }
            let mut line = (min[1] / f64::from(repeat[1])).floor() * f64::from(repeat[1]);
            while line <= max[1] {
                let y = texel_to_screen(&view, rect, [0.0, line]).y;
                painter.hline(
                    rect.x_range(),
                    y,
                    if line.abs() < 0.5 { axis } else { grid },
                );
                line += f64::from(repeat[1]);
            }
        }
        // Face polygon, vertices, centroid.
        let outline = Color32::from_rgb(255, 202, 74);
        let points: Vec<Pos2> = polygon
            .iter()
            .map(|&texel| texel_to_screen(&view, rect, texel))
            .collect();
        for (start, end) in points.iter().zip(points.iter().cycle().skip(1)) {
            painter.line_segment([*start, *end], Stroke::new(2.0, outline));
        }
        for point in &points {
            painter.circle_filled(*point, 2.5, Color32::WHITE);
        }
        let centroid = polygon_centroid(&polygon);
        let centroid_px = texel_to_screen(&view, rect, centroid);
        painter.line_segment(
            [
                centroid_px - Vec2::new(4.0, 0.0),
                centroid_px + Vec2::new(4.0, 0.0),
            ],
            Stroke::new(1.0, outline),
        );
        painter.line_segment(
            [
                centroid_px - Vec2::new(0.0, 4.0),
                centroid_px + Vec2::new(0.0, 4.0),
            ],
            Stroke::new(1.0, outline),
        );
        // Scale handles on the bbox, rotate handle above it.
        let (bb_min, bb_max) = polygon_bounds(&polygon);
        let bb_center = [(bb_min[0] + bb_max[0]) * 0.5, (bb_min[1] + bb_max[1]) * 0.5];
        let corner_handles = [
            [bb_min[0], bb_min[1]],
            [bb_max[0], bb_min[1]],
            [bb_max[0], bb_max[1]],
            [bb_min[0], bb_max[1]],
        ];
        let edge_handles = [
            ([bb_center[0], bb_min[1]], [false, true]),
            ([bb_center[0], bb_max[1]], [false, true]),
            ([bb_min[0], bb_center[1]], [true, false]),
            ([bb_max[0], bb_center[1]], [true, false]),
        ];
        let handle_fill = Color32::from_gray(235);
        for corner in corner_handles {
            let px = texel_to_screen(&view, rect, corner);
            painter.rect_filled(
                egui::Rect::from_center_size(px, Vec2::splat(6.0)),
                1.0,
                handle_fill,
            );
        }
        for (edge, _) in edge_handles {
            let px = texel_to_screen(&view, rect, edge);
            painter.circle_filled(px, 2.8, handle_fill);
        }
        let rotate_px = texel_to_screen(&view, rect, [bb_center[0], bb_min[1]])
            - Vec2::new(0.0, ROTATE_HANDLE_OFFSET_PX);
        painter.circle_stroke(rotate_px, 5.0, Stroke::new(1.5, outline));
        painter.rect_stroke(
            rect,
            3.0,
            Stroke::new(1.0, Color32::from_gray(70)),
            StrokeKind::Inside,
        );
        // Hover readout.
        if let Some(pointer) = response.hover_pos() {
            let texel = screen_to_texel(&view, rect, pointer);
            painter.text(
                rect.left_bottom() + Vec2::new(5.0, -4.0),
                Align2::LEFT_BOTTOM,
                format!("{:.0}, {:.0}", texel[0].floor(), texel[1].floor()),
                FontId::monospace(10.0),
                Color32::from_gray(150),
            );
        }

        // ---- gestures ----
        if response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let alt = ui.input(|input| input.modifiers.alt);
                let mut kind = UvCanvasDragKind::Move;
                if alt {
                    kind = UvCanvasDragKind::Rotate;
                } else if pointer.distance(rotate_px) <= HANDLE_HIT_PX {
                    kind = UvCanvasDragKind::Rotate;
                } else {
                    for corner in corner_handles {
                        if pointer.distance(texel_to_screen(&view, rect, corner)) <= HANDLE_HIT_PX {
                            kind = UvCanvasDragKind::Scale([true, true]);
                        }
                    }
                    for (edge, mask) in edge_handles {
                        if pointer.distance(texel_to_screen(&view, rect, edge)) <= HANDLE_HIT_PX {
                            kind = UvCanvasDragKind::Scale(mask);
                        }
                    }
                }
                self.brush_uv_canvas_drag = Some(UvCanvasDragState {
                    brush: index,
                    face: face_index,
                    kind,
                    start_uv: current,
                    start_mouse: screen_to_texel(&view, rect, pointer),
                    center: centroid,
                });
            }
        }
        let drag = self.brush_uv_canvas_drag?;
        if drag.brush != index || drag.face != face_index {
            self.brush_uv_canvas_drag = None;
            return None;
        }
        let pointer = response
            .interact_pointer_pos()
            .or_else(|| ui.input(|input| input.pointer.latest_pos()))?;
        let mouse = screen_to_texel(&view, rect, pointer);
        let shift = ui.input(|input| input.modifiers.shift);
        let edited = match drag.kind {
            UvCanvasDragKind::Move => move_gesture(&drag, current, mouse),
            UvCanvasDragKind::Scale(mask) => scale_gesture(&drag, current, mouse, mask),
            UvCanvasDragKind::Rotate => {
                rotate_gesture(&drag, current, mouse, if shift { 15.0 } else { 1.0 })
            }
        };
        if response.dragged() {
            Some((edited, true))
        } else {
            // Release frame: report the final mapping with the
            // interaction closed so the UV transaction ends.
            self.brush_uv_canvas_drag = None;
            Some((edited, false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use psxed_project::brush::Brush;

    fn drag(start_uv: FaceUv, start_mouse: [f64; 2], center: [f64; 2]) -> UvCanvasDragState {
        UvCanvasDragState {
            brush: 0,
            face: 0,
            kind: UvCanvasDragKind::Move,
            start_uv,
            start_mouse,
            center,
        }
    }

    #[test]
    fn view_roundtrips_texels_through_the_screen() {
        let rect = egui::Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(300.0, 200.0));
        let polygon = vec![[0.0, 0.0], [128.0, 0.0], [128.0, 64.0], [0.0, 64.0]];
        let view = fit_view(0, 0, &polygon, rect);
        let texel = [37.0, 11.0];
        let back = screen_to_texel(&view, rect, texel_to_screen(&view, rect, texel));
        assert!((back[0] - texel[0]).abs() < 0.05 && (back[1] - texel[1]).abs() < 0.05);
        // The polygon must land inside the canvas.
        for point in &polygon {
            assert!(rect.contains(texel_to_screen(&view, rect, *point)));
        }
    }

    #[test]
    fn move_gesture_slides_the_applied_uv_with_the_pointer() {
        let start = FaceUv::default();
        let drag = drag(start, [10.0, 10.0], [0.0, 0.0]);
        let edited = move_gesture(&drag, start, [15.3, 7.4]);
        assert_eq!(edited.offset_texels, [5, -3]);
        let anchor = [40.0, 40.0];
        let before = start.apply(anchor);
        let after = edited.apply(anchor);
        assert_eq!((after[0] - before[0]).round(), 5.0);
        assert_eq!((after[1] - before[1]).round(), -3.0);
    }

    #[test]
    fn scale_gesture_doubles_the_polygon_by_halving_the_stored_scale() {
        let start = FaceUv::default();
        let mut state = drag(start, [64.0, 32.0], [32.0, 32.0]);
        state.kind = UvCanvasDragKind::Scale([true, true]);
        // Pull the grabbed corner from 32 to 64 texels off-centre.
        let edited = scale_gesture(&state, start, [96.0, 32.0], [true, false]);
        assert_eq!(edited.scale_q8, [128, 256]);
        // Halving the pull halves the polygon: scale doubles.
        let edited = scale_gesture(&state, start, [48.0, 32.0], [true, false]);
        assert_eq!(edited.scale_q8, [512, 256]);
    }

    #[test]
    fn rotate_gesture_tracks_the_pointer_sweep() {
        let start = FaceUv::default();
        let state = drag(start, [64.0, 0.0], [0.0, 0.0]);
        // Sweep the pointer a quarter turn: +u toward +v.
        let edited = rotate_gesture(&state, start, [0.0, 64.0], 1.0);
        assert_eq!(edited.rotation_deg, 90);
        // The applied polygon must follow the same sweep: a point on
        // the +u axis lands on +v.
        let turned = edited.apply([64.0, 0.0]);
        assert!(turned[0].abs() < 0.001 && (turned[1] - 64.0).abs() < 0.001);
        // Shift-snap rounds the sweep to 15 degree steps.
        let snapped = rotate_gesture(&state, start, [64.0, 20.0], 15.0);
        assert_eq!(snapped.rotation_deg, 15);
    }

    #[test]
    fn face_polygon_projects_through_the_authored_mapping() {
        let brush = Brush::cuboid([0, 0, 0], [256, 128, 512]);
        for face in 0..brush.faces.len() {
            let polygon = face_texel_polygon(&brush, face).expect("polygon");
            assert!(polygon.len() >= 4);
            let (min, max) = polygon_bounds(&polygon);
            // 16 world units per texel: the largest face spans 32 texels.
            assert!(max[0] - min[0] <= 32.5);
            assert!(max[1] - min[1] <= 32.5);
        }
    }
}
