use super::*;

#[derive(Clone, Copy)]
pub(crate) struct UiPreviewAffine {
    m00: f32,
    m01: f32,
    m10: f32,
    m11: f32,
    tx: f32,
    ty: f32,
}

impl UiPreviewAffine {
    fn canvas(canvas: Rect, canvas_size: [u16; 2]) -> Self {
        Self {
            m00: canvas.width() / canvas_size[0].max(1) as f32,
            m01: 0.0,
            m10: 0.0,
            m11: canvas.height() / canvas_size[1].max(1) as f32,
            tx: canvas.left(),
            ty: canvas.top(),
        }
    }

    fn from_rect(x: f32, y: f32, rect: UiRect) -> Self {
        let radians = f32::from(rect.rotation_degrees).to_radians();
        let (sin, cos) = radians.sin_cos();
        let fx = if rect.flip_x { -1.0 } else { 1.0 };
        let fy = if rect.flip_y { -1.0 } else { 1.0 };
        let m00 = cos * fx;
        let m01 = -sin * fy;
        let m10 = sin * fx;
        let m11 = cos * fy;
        let hw = rect.width.max(1) as f32 * 0.5;
        let hh = rect.height.max(1) as f32 * 0.5;
        let cx = x + hw;
        let cy = y + hh;
        Self {
            m00,
            m01,
            m10,
            m11,
            tx: cx - (m00 * hw + m01 * hh),
            ty: cy - (m10 * hw + m11 * hh),
        }
    }

    fn compose(self, child: Self) -> Self {
        Self {
            m00: self.m00 * child.m00 + self.m01 * child.m10,
            m01: self.m00 * child.m01 + self.m01 * child.m11,
            m10: self.m10 * child.m00 + self.m11 * child.m10,
            m11: self.m10 * child.m01 + self.m11 * child.m11,
            tx: self.m00 * child.tx + self.m01 * child.ty + self.tx,
            ty: self.m10 * child.tx + self.m11 * child.ty + self.ty,
        }
    }

    fn point(self, x: f32, y: f32) -> Pos2 {
        Pos2::new(
            self.m00 * x + self.m01 * y + self.tx,
            self.m10 * x + self.m11 * y + self.ty,
        )
    }

    fn subrect(self, x: f32, y: f32, width: f32, height: f32) -> [Pos2; 4] {
        [
            self.point(x, y),
            self.point(x + width, y),
            self.point(x, y + height),
            self.point(x + width, y + height),
        ]
    }

    fn y_scale(self) -> f32 {
        (self.m10 * self.m10 + self.m11 * self.m11).sqrt().max(0.01)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct UiPreviewNode {
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) transform: UiPreviewAffine,
    pub(crate) quad: [Pos2; 4],
}

impl UiPreviewNode {
    fn subrect(self, x: f32, y: f32, width: f32, height: f32) -> [Pos2; 4] {
        self.transform.subrect(x, y, width, height)
    }

    fn bounds(self) -> Rect {
        quad_bounds_egui(self.quad)
    }
}

pub(crate) fn ui_scene_preview_node(
    scene: &psxed_project::UiScene,
    id: UiNodeId,
    canvas: Rect,
    canvas_size: [u16; 2],
) -> Option<UiPreviewNode> {
    ui_scene_preview_node_inner(scene, id, canvas, canvas_size, 0)
}

pub(crate) fn ui_scene_preview_node_inner(
    scene: &psxed_project::UiScene,
    id: UiNodeId,
    canvas: Rect,
    canvas_size: [u16; 2],
    depth: usize,
) -> Option<UiPreviewNode> {
    if depth > scene.nodes().len() {
        return None;
    }
    let node = scene.node(id)?;
    if let UiNodeKind::Canvas { width, height } = node.kind {
        let transform = UiPreviewAffine::canvas(canvas, canvas_size);
        let width = width.max(1) as f32;
        let height = height.max(1) as f32;
        return Some(UiPreviewNode {
            width,
            height,
            transform,
            quad: transform.subrect(0.0, 0.0, width, height),
        });
    }
    let local = node.kind.rect()?;
    let parent = node
        .parent
        .and_then(|parent| {
            ui_scene_preview_node_inner(scene, parent, canvas, canvas_size, depth + 1)
        })
        .unwrap_or_else(|| {
            let transform = UiPreviewAffine::canvas(canvas, canvas_size);
            let width = canvas_size[0].max(1) as f32;
            let height = canvas_size[1].max(1) as f32;
            UiPreviewNode {
                width,
                height,
                transform,
                quad: transform.subrect(0.0, 0.0, width, height),
            }
        });
    let (anchor_x, anchor_y) = ui_anchor_factors(local.anchor);
    let x = parent.width * anchor_x as f32 * 0.5 + f32::from(local.x);
    let y = parent.height * anchor_y as f32 * 0.5 + f32::from(local.y);
    let transform = parent
        .transform
        .compose(UiPreviewAffine::from_rect(x, y, local));
    let width = local.width.max(1) as f32;
    let height = local.height.max(1) as f32;
    Some(UiPreviewNode {
        width,
        height,
        transform,
        quad: transform.subrect(0.0, 0.0, width, height),
    })
}

pub(crate) fn quad_bounds_egui(points: [Pos2; 4]) -> Rect {
    let mut min = points[0];
    let mut max = points[0];
    for point in points.iter().skip(1) {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
    }
    Rect::from_min_max(min, max)
}

pub(crate) fn ui_anchor_factors(anchor: UiAnchor) -> (i32, i32) {
    match anchor {
        UiAnchor::TopLeft => (0, 0),
        UiAnchor::Top => (1, 0),
        UiAnchor::TopRight => (2, 0),
        UiAnchor::Left => (0, 1),
        UiAnchor::Center => (1, 1),
        UiAnchor::Right => (2, 1),
        UiAnchor::BottomLeft => (0, 2),
        UiAnchor::Bottom => (1, 2),
        UiAnchor::BottomRight => (2, 2),
    }
}

/// Project resources, canvas placement, and selection state shared by every
/// node drawn in one UI-scene preview pass.
pub(crate) struct UiScenePreviewContext<'a> {
    pub(crate) project: &'a ProjectDocument,
    pub(crate) texture_thumbs: &'a HashMap<ResourceId, ThumbnailEntry>,
    pub(crate) font_textures: &'a [egui::TextureHandle],
    pub(crate) canvas: Rect,
    pub(crate) canvas_size: [u16; 2],
    pub(crate) hidden_ui_nodes: &'a HashSet<(UiSceneId, UiNodeId)>,
    pub(crate) selected: UiNodeId,
    pub(crate) hovered_handle: Option<UiResizeHandle>,
    pub(crate) frame: u16,
}

pub(crate) fn draw_ui_scene_preview(
    painter: &egui::Painter,
    scene: &psxed_project::UiScene,
    ctx: UiScenePreviewContext<'_>,
) {
    let UiScenePreviewContext {
        project,
        texture_thumbs,
        font_textures,
        canvas,
        canvas_size,
        hidden_ui_nodes,
        selected,
        hovered_handle,
        frame,
    } = ctx;
    for id in scene.hierarchy_node_ids() {
        if ui_node_hidden(scene, hidden_ui_nodes, id) {
            continue;
        }
        let Some(node) = scene.node(id) else {
            continue;
        };
        match &node.kind {
            UiNodeKind::Canvas { .. } => {}
            UiNodeKind::Music { .. } | UiNodeKind::Timer { .. } => {}
            UiNodeKind::Group { .. } => {
                if let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size) {
                    draw_ui_preview_quad_stroke(
                        painter,
                        preview.quad,
                        Stroke::new(1.0, Color32::from_rgb(74, 83, 102)),
                    );
                }
            }
            UiNodeKind::Rect {
                color,
                gradient,
                transparent,
                shape,
                ..
            } => {
                if let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size) {
                    draw_ui_preview_shape(
                        painter,
                        preview,
                        (!*transparent).then(|| ui_preview_paint(*color, *gradient)),
                        shape.and_then(|style| {
                            (style.border_width != 0).then(|| {
                                (
                                    ui_preview_paint(style.border_color, style.border_gradient),
                                    style.border_width,
                                )
                            })
                        }),
                        *shape,
                        None,
                    );
                }
            }
            UiNodeKind::Label {
                text,
                random_message,
                messages,
                color,
                gradient,
                align,
                wrap,
                font,
                font_scale,
                letter_spacing,
                ..
            } => {
                let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size)
                else {
                    continue;
                };
                let base_scale = preview.transform.y_scale().clamp(1.0, 8.0);
                let scale = base_scale * ui_font_scale_q8_to_f32(*font_scale);
                let texture = ui_preview_font_texture(font_textures, *font);
                let preview_text = if *random_message {
                    messages
                        .iter()
                        .find(|message| !message.trim().is_empty())
                        .map(String::as_str)
                        .unwrap_or(text)
                } else {
                    text
                };
                draw_ui_preview_text(
                    painter,
                    texture,
                    preview,
                    preview_text,
                    *font,
                    *align,
                    *wrap,
                    false,
                    scale,
                    *letter_spacing,
                    base_scale,
                    ui_preview_paint(*color, *gradient),
                );
            }
            UiNodeKind::Image {
                rect,
                texture,
                tint,
                effect,
            } => {
                let absolute = scene.absolute_rect(node.id).unwrap_or(*rect);
                let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size)
                else {
                    continue;
                };
                if let Some(thumb) = texture
                    .and_then(|id| project.resource(id).map(|resource| resource.id))
                    .and_then(|id| texture_thumbs.get(&id))
                {
                    draw_ui_preview_image(
                        painter,
                        thumb.handle.id(),
                        preview.quad,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        *tint,
                        *effect,
                        frame,
                        absolute,
                        false,
                    );
                } else if *effect == UiImageEffect::None {
                    draw_ui_preview_quad_mesh(
                        painter,
                        egui::TextureId::default(),
                        preview.quad,
                        Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
                        [ui_psx_tint_to_egui(*tint); 4],
                    );
                } else {
                    draw_ui_preview_image(
                        painter,
                        egui::TextureId::default(),
                        preview.quad,
                        Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
                        *tint,
                        *effect,
                        frame,
                        absolute,
                        true,
                    );
                }
                draw_ui_preview_quad_stroke(
                    painter,
                    preview.quad,
                    Stroke::new(1.0, Color32::from_rgb(180, 190, 210)),
                );
            }
            UiNodeKind::Bar {
                rect,
                value,
                max,
                texture,
                frame_count,
                fill,
                fill_gradient,
                background,
                background_gradient,
                ..
            } => {
                let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size)
                else {
                    continue;
                };
                let max_q12 = ui_binding_preview_q12(*max).max(1);
                let value_q12 = ui_binding_preview_q12(*value).clamp(0, max_q12);
                if *frame_count >= 2 {
                    if let Some(thumb) = texture
                        .and_then(|id| project.resource(id).map(|resource| resource.id))
                        .and_then(|id| texture_thumbs.get(&id))
                    {
                        let frame_index =
                            psxed_project::ui_bar_frame_index(value_q12, max_q12, *frame_count)
                                as f32;
                        let frames = f32::from(*frame_count);
                        let uv = Rect::from_min_max(
                            Pos2::new(0.0, frame_index / frames),
                            Pos2::new(1.0, (frame_index + 1.0) / frames),
                        );
                        let absolute = scene.absolute_rect(node.id).unwrap_or(*rect);
                        draw_ui_preview_image(
                            painter,
                            thumb.handle.id(),
                            preview.quad,
                            uv,
                            *fill,
                            UiImageEffect::None,
                            frame,
                            absolute,
                            false,
                        );
                        continue;
                    }
                }
                draw_ui_preview_quad_paint(
                    painter,
                    preview.quad,
                    ui_preview_paint(*background, *background_gradient),
                );
                let fill_w = preview.width * (value_q12 as f32 / max_q12 as f32);
                if fill_w > 0.0 {
                    draw_ui_preview_quad_paint(
                        painter,
                        preview.subrect(0.0, 0.0, fill_w, preview.height),
                        ui_preview_paint(*fill, *fill_gradient),
                    );
                }
            }
            UiNodeKind::Button {
                label,
                align,
                font,
                font_scale,
                letter_spacing,
                color,
                background_gradient,
                text_color,
                text_gradient,
                transparent,
                focus_chrome,
                shape,
                ..
            } => {
                let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size)
                else {
                    continue;
                };
                let draw_chrome = !*focus_chrome || scene.default_focus == Some(node.id);
                if draw_chrome {
                    let fill = ui_preview_paint(*color, *background_gradient);
                    let fill = if *focus_chrome {
                        ui_preview_focus_chrome_sweep(fill, frame, scene.focus_style.period)
                    } else {
                        fill
                    };
                    draw_ui_preview_shape(
                        painter,
                        preview,
                        (!*transparent).then_some(fill),
                        shape.and_then(|style| {
                            (style.border_width != 0).then(|| {
                                (
                                    ui_preview_paint(style.border_color, style.border_gradient),
                                    style.border_width,
                                )
                            })
                        }),
                        *shape,
                        (*focus_chrome).then_some(UiPreviewFocusSweep {
                            frame,
                            period: scene.focus_style.period,
                            color: scene.focus_style.color_a,
                        }),
                    );
                }
                let base_scale = preview.transform.y_scale().clamp(1.0, 8.0);
                let scale = base_scale * ui_font_scale_q8_to_f32(*font_scale);
                let texture = ui_preview_font_texture(font_textures, *font);
                let text_paint = ui_preview_paint(*text_color, *text_gradient);
                let text_paint = if *focus_chrome && !draw_chrome {
                    ui_preview_paint_dimmed(text_paint)
                } else {
                    text_paint
                };
                draw_ui_preview_text(
                    painter,
                    texture,
                    preview,
                    label,
                    *font,
                    *align,
                    false,
                    true,
                    scale,
                    *letter_spacing,
                    base_scale,
                    text_paint,
                );
            }
            UiNodeKind::Slider {
                track,
                track_gradient,
                fill,
                fill_gradient,
                knob,
                knob_gradient,
                ..
            } => {
                let Some(preview) = ui_scene_preview_node(scene, node.id, canvas, canvas_size)
                else {
                    continue;
                };
                draw_ui_preview_quad_paint(
                    painter,
                    preview.quad,
                    ui_preview_paint(*track, *track_gradient),
                );
                // Half-way preview fill until the runtime option store
                // drives the value (matches the engine renderer).
                let fill_w = preview.width * 0.5;
                if fill_w > 0.0 {
                    draw_ui_preview_quad_paint(
                        painter,
                        preview.subrect(0.0, 0.0, fill_w, preview.height),
                        ui_preview_paint(*fill, *fill_gradient),
                    );
                }
                let knob_w = (preview.height + 2.0).clamp(3.0, preview.width.max(3.0));
                let edge = fill_w;
                let knob_x = (edge - knob_w / 2.0).clamp(0.0, (preview.width - knob_w).max(0.0));
                draw_ui_preview_quad_paint(
                    painter,
                    preview.subrect(knob_x, -1.0, knob_w, preview.height + 2.0),
                    ui_preview_paint(*knob, *knob_gradient),
                );
            }
        }
    }

    if !ui_node_hidden(scene, hidden_ui_nodes, selected) {
        if let Some(selected_node) = scene.node(selected) {
            let selected_preview = match &selected_node.kind {
                UiNodeKind::Canvas { .. } => None,
                _ => ui_scene_preview_node(scene, selected_node.id, canvas, canvas_size),
            };
            if let Some(preview) = selected_preview {
                draw_ui_preview_quad_stroke(painter, preview.quad, Stroke::new(2.0, STUDIO_ACCENT));
                let bounds = preview.bounds();
                if !matches!(selected_node.kind, UiNodeKind::Canvas { .. }) {
                    draw_ui_resize_handles(painter, bounds, hovered_handle);
                }
            } else if matches!(selected_node.kind, UiNodeKind::Canvas { .. }) {
                painter.rect_stroke(
                    canvas.expand(2.0),
                    0.0,
                    Stroke::new(2.0, STUDIO_ACCENT),
                    StrokeKind::Outside,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum UiPreviewPaint {
    Solid(Color32),
    Gradient {
        from: Color32,
        to: Color32,
        direction: UiGradientDirection,
    },
}

pub(crate) fn ui_preview_paint(color: [u8; 3], gradient: Option<UiGradient>) -> UiPreviewPaint {
    let from = Color32::from_rgb(color[0], color[1], color[2]);
    match gradient {
        Some(gradient) if gradient.to != color => UiPreviewPaint::Gradient {
            from,
            to: Color32::from_rgb(gradient.to[0], gradient.to[1], gradient.to[2]),
            direction: gradient.direction,
        },
        _ => UiPreviewPaint::Solid(from),
    }
}

pub(crate) fn draw_ui_preview_quad_paint(
    painter: &egui::Painter,
    points: [Pos2; 4],
    paint: UiPreviewPaint,
) {
    match paint {
        UiPreviewPaint::Solid(color) => {
            draw_ui_preview_quad_mesh(
                painter,
                egui::TextureId::default(),
                points,
                Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
                [color; 4],
            );
        }
        UiPreviewPaint::Gradient {
            from,
            to,
            direction,
        } => draw_ui_preview_quad_mesh(
            painter,
            egui::TextureId::default(),
            points,
            Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
            preview_gradient_vertex_colors(from, to, direction),
        ),
    }
}

/// Draw a convex clipped-corner panel with independent fill and inset border.
/// Geometry is authored in PS1 pixels, then transformed through the same
/// affine used by every other 2D preview node.
pub(crate) fn draw_ui_preview_shape(
    painter: &egui::Painter,
    preview: UiPreviewNode,
    fill: Option<UiPreviewPaint>,
    border: Option<(UiPreviewPaint, u8)>,
    style: Option<UiShapeStyle>,
    sweep: Option<UiPreviewFocusSweep>,
) {
    let style = style.unwrap_or_default();
    let fill = fill.map(|paint| {
        if style.semi_transparent_fill {
            ui_preview_paint_with_alpha(paint, 128)
        } else {
            paint
        }
    });
    let corners = style.corner_mask();
    let cut = f32::from(style.corner_cut)
        .min(preview.width * 0.5)
        .min(preview.height * 0.5);
    let outer = ui_preview_shape_points(preview.width, preview.height, cut, corners, 0.0);
    if let Some(fill) = fill {
        draw_ui_preview_convex_paint(painter, preview, &outer, fill);
    }
    if let Some(sweep) = sweep {
        draw_ui_preview_focus_sweep(painter, preview, style, sweep);
    }

    let Some((paint, authored_width)) = border else {
        return;
    };
    let border_width = f32::from(authored_width)
        .min(preview.width * 0.5)
        .min(preview.height * 0.5);
    if border_width <= 0.0 {
        return;
    }
    if preview.width <= border_width * 2.0 || preview.height <= border_width * 2.0 {
        draw_ui_preview_convex_paint(painter, preview, &outer, paint);
        return;
    }
    let inner_cut = cut
        .min((preview.width - border_width * 2.0) * 0.5)
        .min((preview.height - border_width * 2.0) * 0.5);
    let inner = ui_preview_shape_points(
        preview.width,
        preview.height,
        inner_cut,
        corners,
        border_width,
    );
    let mut mesh = egui::Mesh::with_texture(egui::TextureId::default());
    for index in 0..outer.len() {
        let next = (index + 1) % outer.len();
        let base = mesh.vertices.len() as u32;
        for local in [outer[index], outer[next], inner[index], inner[next]] {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: preview.transform.point(local.x, local.y),
                uv: Pos2::ZERO,
                color: ui_preview_paint_at(paint, local.x, local.y, preview.width, preview.height),
            });
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base + 1, base + 2, base + 3);
    }
    painter.add(egui::Shape::mesh(mesh));
}

#[derive(Clone, Copy)]
pub(crate) struct UiPreviewFocusSweep {
    frame: u16,
    period: u16,
    color: [u8; 3],
}

fn draw_ui_preview_focus_sweep(
    painter: &egui::Painter,
    preview: UiPreviewNode,
    style: UiShapeStyle,
    sweep: UiPreviewFocusSweep,
) {
    let safe_inset = f32::from(style.corner_cut.max(style.border_width).max(1));
    let available = preview.width - safe_inset * 2.0;
    if available < 3.0 || preview.height < 3.0 {
        return;
    }
    let band_width = (preview.width / 5.0).clamp(12.0, 28.0).min(available);
    let phase = if sweep.period == 0 {
        255
    } else {
        let position = u32::from(sweep.frame % sweep.period);
        preview_triangle_wave_u8(((position * 512) / u32::from(sweep.period)) as u16)
    };
    let x = safe_inset + (available - band_width) * (f32::from(phase) / 255.0);
    let y = f32::from(style.border_width.max(1)).min(preview.height * 0.5);
    let height = preview.height - y * 2.0;
    let left_width = (band_width * 0.5).max(1.0);
    let right_width = (band_width - left_width).max(1.0);
    let edge = Color32::from_rgba_unmultiplied(sweep.color[0], sweep.color[1], sweep.color[2], 0);
    let peak = Color32::from_rgba_unmultiplied(sweep.color[0], sweep.color[1], sweep.color[2], 112);
    draw_ui_preview_quad_mesh(
        painter,
        egui::TextureId::default(),
        preview.subrect(x, y, left_width, height),
        Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
        [edge, peak, edge, peak],
    );
    draw_ui_preview_quad_mesh(
        painter,
        egui::TextureId::default(),
        preview.subrect(x + left_width, y, right_width, height),
        Rect::from_min_max(Pos2::ZERO, Pos2::ZERO),
        [peak, edge, peak, edge],
    );
}

fn ui_preview_shape_points(
    width: f32,
    height: f32,
    cut: f32,
    corners: u8,
    inset: f32,
) -> Vec<Pos2> {
    let left = inset;
    let top = inset;
    let right = (width - inset).max(left);
    let bottom = (height - inset).max(top);
    let mut points = Vec::with_capacity(8);
    if corners & 1 != 0 {
        points.push(Pos2::new((left + cut).min(right), top));
    } else {
        points.push(Pos2::new(left, top));
    }
    if corners & 2 != 0 {
        points.push(Pos2::new((right - cut).max(left), top));
        points.push(Pos2::new(right, (top + cut).min(bottom)));
    } else {
        points.push(Pos2::new(right, top));
    }
    if corners & 4 != 0 {
        points.push(Pos2::new(right, (bottom - cut).max(top)));
        points.push(Pos2::new((right - cut).max(left), bottom));
    } else {
        points.push(Pos2::new(right, bottom));
    }
    if corners & 8 != 0 {
        points.push(Pos2::new((left + cut).min(right), bottom));
        points.push(Pos2::new(left, (bottom - cut).max(top)));
    } else {
        points.push(Pos2::new(left, bottom));
    }
    if corners & 1 != 0 {
        points.push(Pos2::new(left, (top + cut).min(bottom)));
    }
    points
}

fn draw_ui_preview_convex_paint(
    painter: &egui::Painter,
    preview: UiPreviewNode,
    local_points: &[Pos2],
    paint: UiPreviewPaint,
) {
    if local_points.len() < 3 {
        return;
    }
    let mut mesh = egui::Mesh::with_texture(egui::TextureId::default());
    for local in local_points {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: preview.transform.point(local.x, local.y),
            uv: Pos2::ZERO,
            color: ui_preview_paint_at(paint, local.x, local.y, preview.width, preview.height),
        });
    }
    for index in 1..local_points.len() - 1 {
        mesh.add_triangle(0, index as u32, index as u32 + 1);
    }
    painter.add(egui::Shape::mesh(mesh));
}

fn ui_preview_paint_at(paint: UiPreviewPaint, x: f32, y: f32, width: f32, height: f32) -> Color32 {
    match paint {
        UiPreviewPaint::Solid(color) => color,
        UiPreviewPaint::Gradient {
            from,
            to,
            direction,
        } => {
            let t = match direction {
                UiGradientDirection::Horizontal => x / width.max(1.0),
                UiGradientDirection::Vertical => y / height.max(1.0),
            }
            .clamp(0.0, 1.0);
            let mix =
                |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
            Color32::from_rgba_unmultiplied(
                mix(from.r(), to.r()),
                mix(from.g(), to.g()),
                mix(from.b(), to.b()),
                mix(from.a(), to.a()),
            )
        }
    }
}

fn ui_preview_paint_with_alpha(paint: UiPreviewPaint, alpha: u8) -> UiPreviewPaint {
    let with_alpha =
        |color: Color32| Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha);
    match paint {
        UiPreviewPaint::Solid(color) => UiPreviewPaint::Solid(with_alpha(color)),
        UiPreviewPaint::Gradient {
            from,
            to,
            direction,
        } => UiPreviewPaint::Gradient {
            from: with_alpha(from),
            to: with_alpha(to),
            direction,
        },
    }
}

fn ui_preview_paint_dimmed(paint: UiPreviewPaint) -> UiPreviewPaint {
    let dim = |color: Color32| {
        Color32::from_rgba_unmultiplied(
            ((u16::from(color.r()) * 9) / 16) as u8,
            ((u16::from(color.g()) * 9) / 16) as u8,
            ((u16::from(color.b()) * 9) / 16) as u8,
            color.a(),
        )
    };
    match paint {
        UiPreviewPaint::Solid(color) => UiPreviewPaint::Solid(dim(color)),
        UiPreviewPaint::Gradient {
            from,
            to,
            direction,
        } => UiPreviewPaint::Gradient {
            from: dim(from),
            to: dim(to),
            direction,
        },
    }
}

fn ui_preview_focus_chrome_sweep(paint: UiPreviewPaint, frame: u16, period: u16) -> UiPreviewPaint {
    let brighten = |color: Color32| {
        Color32::from_rgba_unmultiplied(
            color.r().saturating_add(34),
            color.g().saturating_add(34),
            color.b().saturating_add(34),
            color.a(),
        )
    };
    let (dark, bright) = match paint {
        UiPreviewPaint::Solid(color) => (color, brighten(color)),
        UiPreviewPaint::Gradient { from, to, .. } => (from, to),
    };
    let phase = if period == 0 {
        255
    } else {
        let position = u32::from(frame % period);
        preview_triangle_wave_u8(((position * 512) / u32::from(period)) as u16)
    };
    let mix = |a: Color32, b: Color32| {
        let phase = u16::from(phase);
        let channel =
            |a: u8, b: u8| ((u16::from(a) * phase + u16::from(b) * (255 - phase)) / 255) as u8;
        Color32::from_rgba_unmultiplied(
            channel(a.r(), b.r()),
            channel(a.g(), b.g()),
            channel(a.b(), b.b()),
            channel(a.a(), b.a()),
        )
    };
    UiPreviewPaint::Gradient {
        from: mix(dark, bright),
        to: mix(bright, dark),
        direction: UiGradientDirection::Horizontal,
    }
}

pub(crate) fn draw_ui_preview_textured_quad(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    points: [Pos2; 4],
    uv: Rect,
    paint: UiPreviewPaint,
) {
    match paint {
        UiPreviewPaint::Solid(color) => {
            draw_ui_preview_quad_mesh(painter, texture_id, points, uv, [color; 4]);
        }
        UiPreviewPaint::Gradient {
            from,
            to,
            direction,
        } => draw_ui_preview_quad_mesh(
            painter,
            texture_id,
            points,
            uv,
            preview_gradient_vertex_colors(from, to, direction),
        ),
    }
}

pub(crate) fn draw_ui_preview_image(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    points: [Pos2; 4],
    uv: Rect,
    tint: [u8; 3],
    effect: UiImageEffect,
    frame: u16,
    logical_rect: UiRect,
    untextured: bool,
) {
    let (offset_x, offset_y) = match effect {
        UiImageEffect::Rise => {
            let spatial_phase = logical_rect
                .x
                .wrapping_mul(13)
                .wrapping_add(logical_rect.y.wrapping_mul(7))
                as u16;
            (
                0.0,
                -((frame.wrapping_div(2).wrapping_add(spatial_phase) & 0x003f) as f32),
            )
        }
        UiImageEffect::Wind => {
            let seed = (logical_rect.y as u16)
                .wrapping_mul(11)
                .wrapping_add((logical_rect.x as u16).wrapping_mul(3));
            let phase = frame.wrapping_mul(2).wrapping_add(seed) & 0x007f;
            let gust = i16::from(preview_triangle_wave_u8(
                frame.wrapping_mul(5).wrapping_add(seed),
            )) - 128;
            (
                f32::from((phase as i16).saturating_add(gust / 32)),
                f32::from((-((phase as i16) / 6)).saturating_add(gust / 64)),
            )
        }
        _ => (0.0, 0.0),
    };
    let logical_width = f32::from(logical_rect.width.max(1));
    let logical_height = f32::from(logical_rect.height.max(1));
    let screen_width = points[1].distance(points[0]).max(1.0);
    let screen_height = points[2].distance(points[0]).max(1.0);
    let screen_offset_x = offset_x * screen_width / logical_width;
    let screen_offset_y = offset_y * screen_height / logical_height;
    let points =
        points.map(|point| Pos2::new(point.x + screen_offset_x, point.y + screen_offset_y));
    let points = if untextured && effect == UiImageEffect::Rise {
        let midpoint = |a: Pos2, b: Pos2| Pos2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        [
            midpoint(points[0], points[1]),
            midpoint(points[1], points[3]),
            midpoint(points[0], points[2]),
            midpoint(points[2], points[3]),
        ]
    } else {
        points
    };
    if effect == UiImageEffect::None {
        draw_ui_preview_quad_mesh(
            painter,
            texture_id,
            points,
            uv,
            [ui_psx_tint_to_egui(tint); 4],
        );
        return;
    }
    draw_ui_preview_quad_mesh(
        painter,
        texture_id,
        points,
        uv,
        [ui_psx_tint_to_egui(tint); 4],
    );
    draw_ui_preview_quad_mesh(
        painter,
        texture_id,
        points,
        uv,
        ui_preview_image_effect_overlay_colors(effect, frame, logical_rect),
    );
}

pub(crate) fn draw_ui_preview_quad_mesh(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    points: [Pos2; 4],
    uv: Rect,
    colors: [Color32; 4],
) {
    let mut mesh = egui::Mesh::with_texture(texture_id);
    mesh.vertices.push(egui::epaint::Vertex {
        pos: points[0],
        uv: uv.left_top(),
        color: colors[0],
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: points[1],
        uv: uv.right_top(),
        color: colors[1],
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: points[2],
        uv: uv.left_bottom(),
        color: colors[2],
    });
    mesh.vertices.push(egui::epaint::Vertex {
        pos: points[3],
        uv: uv.right_bottom(),
        color: colors[3],
    });
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

pub(crate) fn draw_ui_preview_quad_stroke(
    painter: &egui::Painter,
    points: [Pos2; 4],
    stroke: Stroke,
) {
    painter.line_segment([points[0], points[1]], stroke);
    painter.line_segment([points[1], points[3]], stroke);
    painter.line_segment([points[3], points[2]], stroke);
    painter.line_segment([points[2], points[0]], stroke);
}

pub(crate) fn preview_gradient_vertex_colors(
    from: Color32,
    to: Color32,
    direction: UiGradientDirection,
) -> [Color32; 4] {
    match direction {
        UiGradientDirection::Vertical => [from, from, to, to],
        UiGradientDirection::Horizontal => [from, to, from, to],
    }
}

pub(crate) fn ui_preview_image_effect_overlay_colors(
    effect: UiImageEffect,
    frame: u16,
    rect: UiRect,
) -> [Color32; 4] {
    let left = rect.x;
    let right = rect
        .x
        .saturating_add(rect.width.min(i16::MAX as u16) as i16);
    let top = rect.y;
    let bottom = rect
        .y
        .saturating_add(rect.height.min(i16::MAX as u16) as i16);
    let lift = match effect {
        UiImageEffect::None => [0; 4],
        UiImageEffect::Shimmer => preview_sweep_lifts(frame, 3, 88, [left, right, left, right]),
        UiImageEffect::FastShimmer => {
            preview_sweep_lifts(frame, 7, 112, [left, right, left, right])
        }
        UiImageEffect::DiagonalSweep => preview_sweep_lifts(
            frame,
            4,
            96,
            [
                left.saturating_add(top / 2),
                right.saturating_add(top / 2),
                left.saturating_add(bottom / 2),
                right.saturating_add(bottom / 2),
            ],
        ),
        UiImageEffect::SoftPulse => {
            let lift =
                10 + (u16::from(preview_triangle_wave_u8(frame.wrapping_mul(3))) * 44 / 255) as u8;
            [lift; 4]
        }
        // Bob displaces the quad instead of tinting it; the preview
        // approximates the motion in the node rect, not the colours.
        UiImageEffect::Bob | UiImageEffect::Rise | UiImageEffect::Wind => [0; 4],
    };
    [
        ui_preview_light_overlay(lift[0]),
        ui_preview_light_overlay(lift[1]),
        ui_preview_light_overlay(lift[2]),
        ui_preview_light_overlay(lift[3]),
    ]
}

pub(crate) fn preview_sweep_lifts(
    frame: u16,
    speed: u16,
    intensity: u8,
    positions: [i16; 4],
) -> [u8; 4] {
    let phase = ((frame.wrapping_mul(speed) & 0x01ff) as i16) - 128;
    [
        preview_sweep_lift(positions[0], phase, intensity),
        preview_sweep_lift(positions[1], phase, intensity),
        preview_sweep_lift(positions[2], phase, intensity),
        preview_sweep_lift(positions[3], phase, intensity),
    ]
}

pub(crate) fn preview_sweep_lift(position: i16, phase: i16, intensity: u8) -> u8 {
    let distance = (i32::from(position) - i32::from(phase)).unsigned_abs();
    let falloff = (distance / 2).min(u32::from(u8::MAX)) as u8;
    intensity.saturating_sub(falloff)
}

pub(crate) fn preview_triangle_wave_u8(value: u16) -> u8 {
    let phase = value & 0x01ff;
    if phase < 256 {
        phase as u8
    } else {
        (511 - phase) as u8
    }
}

pub(crate) fn ui_preview_light_overlay(lift: u8) -> Color32 {
    Color32::from_white_alpha(lift.saturating_mul(2))
}

pub(crate) fn ui_psx_tint_to_egui(tint: [u8; 3]) -> Color32 {
    ui_psx_rgb_to_egui((tint[0], tint[1], tint[2]))
}

pub(crate) fn ui_psx_rgb_to_egui(color: (u8, u8, u8)) -> Color32 {
    Color32::from_rgb(
        color.0.saturating_mul(2),
        color.1.saturating_mul(2),
        color.2.saturating_mul(2),
    )
}

pub(crate) const UI_FONT_COUNT: usize = UiFontChoice::ALL.len();
pub(crate) const UI_FONT_CHOICES: [UiFontChoice; UI_FONT_COUNT] = UiFontChoice::ALL;

/// Preview atlas grid for the 128-glyph built-in ASCII fonts.
pub(crate) const UI_FONT_COLS: usize = 16;

#[derive(Clone, Copy)]
pub(crate) struct UiPreviewFontSpec {
    pub(crate) texture_index: usize,
    pub(crate) glyph_w: usize,
    pub(crate) glyph_h: usize,
    pub(crate) line_height: usize,
    pub(crate) cols: usize,
    pub(crate) glyph_count: usize,
}

pub(crate) fn ui_preview_font_spec(font: UiFontChoice) -> UiPreviewFontSpec {
    let source = ui_preview_font_source(font);
    UiPreviewFontSpec {
        texture_index: font.runtime_index() as usize,
        glyph_w: source.glyph_w as usize,
        glyph_h: source.glyph_h as usize,
        line_height: source.line_height as usize,
        cols: UI_FONT_COLS,
        glyph_count: source.glyph_count as usize,
    }
}

pub(crate) fn ui_preview_font_source(font: UiFontChoice) -> &'static psx_font::BitmapFont {
    match font {
        UiFontChoice::Basic => &psx_font::fonts::BASIC,
        UiFontChoice::Basic8x16 => &psx_font::fonts::BASIC_8X16,
        UiFontChoice::KenneyBlocks => &psx_font::fonts::KENNEY_BLOCKS,
        UiFontChoice::KenneyFuture => &psx_font::fonts::KENNEY_FUTURE,
        UiFontChoice::KenneyFutureNarrow => &psx_font::fonts::KENNEY_FUTURE_NARROW,
        UiFontChoice::KenneyHigh => &psx_font::fonts::KENNEY_HIGH,
        UiFontChoice::KenneyHighSquare => &psx_font::fonts::KENNEY_HIGH_SQUARE,
        UiFontChoice::KenneyMini => &psx_font::fonts::KENNEY_MINI,
        UiFontChoice::KenneyMiniSquare => &psx_font::fonts::KENNEY_MINI_SQUARE,
        UiFontChoice::KenneyMiniSquareMono => &psx_font::fonts::KENNEY_MINI_SQUARE_MONO,
        UiFontChoice::KenneyPixel => &psx_font::fonts::KENNEY_PIXEL,
        UiFontChoice::KenneyPixelSquare => &psx_font::fonts::KENNEY_PIXEL_SQUARE,
        UiFontChoice::KenneyRocket => &psx_font::fonts::KENNEY_ROCKET,
        UiFontChoice::KenneyRocketSquare => &psx_font::fonts::KENNEY_ROCKET_SQUARE,
        UiFontChoice::PressStart2P => &psx_font::fonts::PRESS_START_2P,
        UiFontChoice::Silkscreen => &psx_font::fonts::SILKSCREEN,
        UiFontChoice::PixelifySans => &psx_font::fonts::PIXELIFY_SANS,
        UiFontChoice::Orbitron => &psx_font::fonts::ORBITRON,
        UiFontChoice::Audiowide => &psx_font::fonts::AUDIOWIDE,
        UiFontChoice::Michroma => &psx_font::fonts::MICHROMA,
        UiFontChoice::Electrolize => &psx_font::fonts::ELECTROLIZE,
        UiFontChoice::Oxanium => &psx_font::fonts::OXANIUM,
        UiFontChoice::Rajdhani => &psx_font::fonts::RAJDHANI,
        UiFontChoice::ChakraPetch => &psx_font::fonts::CHAKRA_PETCH,
        UiFontChoice::Tektur => &psx_font::fonts::TEKTUR,
        UiFontChoice::Tomorrow => &psx_font::fonts::TOMORROW,
        UiFontChoice::ZenDots => &psx_font::fonts::ZEN_DOTS,
        UiFontChoice::TurretRoad => &psx_font::fonts::TURRET_ROAD,
        UiFontChoice::Tiny5 => &psx_font::fonts::TINY5,
        UiFontChoice::Jersey10 => &psx_font::fonts::JERSEY_10,
        UiFontChoice::SpaceMono => &psx_font::fonts::SPACE_MONO,
        UiFontChoice::BrunoAce => &psx_font::fonts::BRUNO_ACE,
        UiFontChoice::Aldrich => &psx_font::fonts::ALDRICH,
        UiFontChoice::Syncopate => &psx_font::fonts::SYNCOPATE,
        UiFontChoice::ShareTechMono => &psx_font::fonts::SHARE_TECH_MONO,
        UiFontChoice::Jura => &psx_font::fonts::JURA,
        UiFontChoice::ZenDotsDisplay => &psx_font::fonts::ZEN_DOTS_DISPLAY,
    }
}

pub(crate) fn ui_preview_font_texture(
    font_textures: &[egui::TextureHandle],
    font: UiFontChoice,
) -> &egui::TextureHandle {
    let spec = ui_preview_font_spec(font);
    &font_textures[spec
        .texture_index
        .min(font_textures.len().saturating_sub(1))]
}

/// Rasterize an on-device UI bitmap font into an RGBA egui image: an atlas of
/// white glyphs whose alpha is the source bit (1 -> opaque white, 0 ->
/// transparent). Tinting happens at blit time, mirroring the PS1 "monochrome
/// glyph * vertex colour" path.
pub(crate) fn rasterize_ui_font_atlas(font: UiFontChoice) -> egui::ColorImage {
    let source = ui_preview_font_source(font);
    let spec = ui_preview_font_spec(font);
    let rows = spec.glyph_count.div_ceil(spec.cols);
    let aw = spec.cols * spec.glyph_w;
    let ah = rows * spec.glyph_h;
    let mut pixels = vec![Color32::TRANSPARENT; aw * ah];
    for glyph in 0..source.glyph_count {
        let gx = (glyph as usize % spec.cols) * spec.glyph_w;
        let gy = (glyph as usize / spec.cols) * spec.glyph_h;
        for row in 0..spec.glyph_h {
            // `glyph_row_packed` normalises bit order: pixel 0 is bit 0.
            let bits = source.glyph_row_packed(glyph, row as u8);
            for col in 0..spec.glyph_w {
                if bits & (1 << col) != 0 {
                    pixels[(gy + row) * aw + (gx + col)] = Color32::WHITE;
                }
            }
        }
    }
    egui::ColorImage {
        size: [aw, ah],
        pixels,
    }
}

/// UV rect of glyph `code` within the atlas, in 0..1 space. Codes outside the
/// font's range map to glyph 0 (the missing-glyph cell).
pub(crate) fn ui_font_glyph_uv(font: UiFontChoice, code: u8) -> Rect {
    let source = ui_preview_font_source(font);
    let spec = ui_preview_font_spec(font);
    let first = source.first_char;
    let cp = code as u16;
    let index = if cp >= first && cp < first.saturating_add(source.glyph_count) {
        (cp - first) as usize
    } else {
        0
    };
    let rows = spec.glyph_count.div_ceil(spec.cols);
    let index = index.min(spec.glyph_count.saturating_sub(1));
    let gx = (index % spec.cols) as f32 / spec.cols as f32;
    let gy = (index / spec.cols) as f32 / rows as f32;
    let gw = 1.0 / spec.cols as f32;
    let gh = 1.0 / rows as f32;
    Rect::from_min_size(Pos2::new(gx, gy), Vec2::new(gw, gh))
}

pub(crate) fn ui_preview_glyph_advance(font: UiFontChoice, ch: char) -> usize {
    ui_preview_font_source(font).glyph_advance(ch) as usize
}

pub(crate) fn ui_preview_text_width(
    font: UiFontChoice,
    text: &str,
    scale: f32,
    letter_spacing: i8,
    canvas_scale: f32,
) -> f32 {
    let base: f32 = text
        .chars()
        .map(|ch| ui_preview_glyph_advance(font, ch) as f32 * scale)
        .sum();
    let gaps = text.chars().count().saturating_sub(1) as f32;
    (base + gaps * f32::from(letter_spacing) * canvas_scale).max(0.0)
}

/// Draw preview text using the on-device bitmap font, so the editor canvas
/// matches what the runtime renderer ([`psx_engine::ui::draw_scene`]) draws:
/// source advance/line height, all scaled by `scale`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_ui_preview_text(
    painter: &egui::Painter,
    font_texture: &egui::TextureHandle,
    preview: UiPreviewNode,
    text: &str,
    font: UiFontChoice,
    align: UiTextAlign,
    wrap: bool,
    vcenter: bool,
    scale: f32,
    letter_spacing: i8,
    canvas_scale: f32,
    paint: UiPreviewPaint,
) {
    let spec = ui_preview_font_spec(font);
    let local_scale = (scale / canvas_scale.max(0.01)).max(0.01);
    let line_height = (spec.line_height as f32 * local_scale).max(1.0);
    let glyph_size = Vec2::new(
        (spec.glyph_w as f32 * local_scale).max(1.0),
        (spec.glyph_h as f32 * local_scale).max(1.0),
    );
    let lines: Vec<&str> = if wrap {
        wrap_preview_text_lines(text, font, preview.width, local_scale, letter_spacing, 1.0)
    } else {
        text.lines().collect()
    };
    let total_h = lines.len().max(1) as f32 * line_height;
    let mut y = if vcenter {
        (preview.height - total_h).max(0.0) / 2.0
    } else {
        0.0
    };
    for line in lines {
        if y > preview.height {
            break;
        }
        draw_ui_preview_text_line(
            painter,
            font_texture,
            preview,
            y,
            line,
            font,
            align,
            glyph_size,
            local_scale,
            letter_spacing,
            1.0,
            paint,
        );
        y += line_height;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_ui_preview_text_line(
    painter: &egui::Painter,
    font_texture: &egui::TextureHandle,
    preview: UiPreviewNode,
    y: f32,
    line: &str,
    font: UiFontChoice,
    align: UiTextAlign,
    glyph_size: Vec2,
    scale: f32,
    letter_spacing: i8,
    canvas_scale: f32,
    paint: UiPreviewPaint,
) {
    let line_w = ui_preview_text_width(font, line, scale, letter_spacing, canvas_scale);
    let start_x = match align {
        UiTextAlign::Left => 0.0,
        UiTextAlign::Center => preview.width * 0.5 - line_w / 2.0,
        UiTextAlign::Right => preview.width - line_w,
    };
    let mut x = start_x;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if x > preview.width + 0.5 {
            break;
        }
        // Only the 128-glyph ASCII range is in the atlas; others draw the
        // missing-glyph cell (index 0), matching the runtime's fallback.
        let code = if (ch as u32) < 128 { ch as u8 } else { 0 };
        draw_ui_preview_textured_quad(
            painter,
            font_texture.id(),
            preview.subrect(x, y, glyph_size.x, glyph_size.y),
            ui_font_glyph_uv(font, code),
            paint,
        );
        let gap = if chars.peek().is_some() {
            f32::from(letter_spacing) * canvas_scale
        } else {
            0.0
        };
        x += ui_preview_glyph_advance(font, ch) as f32 * scale + gap;
    }
}

pub(crate) fn wrap_preview_text_lines(
    text: &str,
    font: UiFontChoice,
    max_width: f32,
    scale: f32,
    letter_spacing: i8,
    canvas_scale: f32,
) -> Vec<&str> {
    let mut out = Vec::new();
    for source_line in text.lines() {
        let mut start = 0usize;
        while start < source_line.len() {
            let remaining = &source_line[start..];
            if ui_preview_text_width(font, remaining, scale, letter_spacing, canvas_scale)
                <= max_width
            {
                out.push(remaining);
                break;
            }
            let hard_split = preview_wrap_hard_split(
                remaining,
                font,
                max_width,
                scale,
                letter_spacing,
                canvas_scale,
            );
            let split = remaining[..hard_split]
                .rfind(' ')
                .filter(|idx| *idx > 0)
                .unwrap_or(hard_split);
            out.push(remaining[..split].trim_end());
            start += split;
            while source_line.as_bytes().get(start) == Some(&b' ') {
                start += 1;
            }
        }
        if source_line.is_empty() {
            out.push("");
        }
    }
    out
}

pub(crate) fn preview_wrap_hard_split(
    text: &str,
    font: UiFontChoice,
    max_width: f32,
    scale: f32,
    letter_spacing: i8,
    canvas_scale: f32,
) -> usize {
    let max_width = max_width.max(1.0);
    let mut width = 0.0;
    let mut last = 0usize;
    let mut chars = text.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        let next = idx + ch.len_utf8();
        let next_width = width + ui_preview_glyph_advance(font, ch) as f32 * scale;
        if next_width > max_width && last > 0 {
            return last;
        }
        width = next_width;
        last = next;
        if chars.peek().is_some() {
            width = (width + f32::from(letter_spacing) * canvas_scale).max(0.0);
        }
    }
    text.len()
}

pub(crate) fn draw_ui_resize_handles(
    painter: &egui::Painter,
    rect: Rect,
    hovered: Option<UiResizeHandle>,
) {
    for (handle, handle_rect) in ui_resize_handle_rects(rect) {
        let fill = if Some(handle) == hovered {
            Color32::from_rgb(235, 242, 248)
        } else {
            Color32::from_rgb(18, 22, 30)
        };
        painter.rect_filled(handle_rect, 1.0, fill);
        painter.rect_stroke(
            handle_rect,
            1.0,
            Stroke::new(1.0, STUDIO_ACCENT),
            StrokeKind::Outside,
        );
    }
}

pub(crate) fn ui_scene_resize_handle_target(
    scene: &psxed_project::UiScene,
    hidden_ui_nodes: &HashSet<(UiSceneId, UiNodeId)>,
    selected: UiNodeId,
    canvas: Rect,
    canvas_size: [u16; 2],
    pos: Pos2,
) -> Option<(UiNodeId, UiResizeHandle)> {
    if let Some(handle) =
        ui_scene_node_resize_handle_hit(scene, hidden_ui_nodes, selected, canvas, canvas_size, pos)
    {
        return Some((selected, handle));
    }
    scene
        .hierarchy_node_ids()
        .into_iter()
        .rev()
        .filter(|id| *id != selected)
        .find_map(|id| {
            ui_scene_node_resize_handle_hit(scene, hidden_ui_nodes, id, canvas, canvas_size, pos)
                .map(|handle| (id, handle))
        })
}

pub(crate) fn ui_scene_node_resize_handle_hit(
    scene: &psxed_project::UiScene,
    hidden_ui_nodes: &HashSet<(UiSceneId, UiNodeId)>,
    id: UiNodeId,
    canvas: Rect,
    canvas_size: [u16; 2],
    pos: Pos2,
) -> Option<UiResizeHandle> {
    if ui_node_hidden(scene, hidden_ui_nodes, id) {
        return None;
    }
    let node = scene.node(id)?;
    node.kind.rect()?;
    let rect = ui_scene_preview_node(scene, id, canvas, canvas_size)?.bounds();
    ui_resize_handle_hit_rects(rect)
        .into_iter()
        .find_map(|(handle, handle_rect)| handle_rect.contains(pos).then_some(handle))
}

pub(crate) fn ui_resize_handle_rects(rect: Rect) -> Vec<(UiResizeHandle, Rect)> {
    ui_resize_handle_rects_with_size(rect, UI_RESIZE_HANDLE_SIZE)
}

pub(crate) fn ui_resize_handle_hit_rects(rect: Rect) -> Vec<(UiResizeHandle, Rect)> {
    ui_resize_handle_rects_with_size(rect, UI_RESIZE_HANDLE_HIT_SIZE)
}

pub(crate) fn ui_resize_handle_rects_with_size(
    rect: Rect,
    size: f32,
) -> Vec<(UiResizeHandle, Rect)> {
    let c = rect.center();
    let centers = [
        (UiResizeHandle::TopLeft, rect.left_top()),
        (UiResizeHandle::Top, Pos2::new(c.x, rect.top())),
        (UiResizeHandle::TopRight, rect.right_top()),
        (UiResizeHandle::Right, Pos2::new(rect.right(), c.y)),
        (UiResizeHandle::BottomRight, rect.right_bottom()),
        (UiResizeHandle::Bottom, Pos2::new(c.x, rect.bottom())),
        (UiResizeHandle::BottomLeft, rect.left_bottom()),
        (UiResizeHandle::Left, Pos2::new(rect.left(), c.y)),
    ];
    centers
        .into_iter()
        .map(|(handle, center)| (handle, Rect::from_center_size(center, Vec2::splat(size))))
        .collect()
}

pub(crate) fn ui_scene_hit_test(
    scene: &psxed_project::UiScene,
    hidden_ui_nodes: &HashSet<(UiSceneId, UiNodeId)>,
    canvas: Rect,
    canvas_size: [u16; 2],
    pos: Pos2,
) -> Option<UiNodeId> {
    scene.hierarchy_node_ids().into_iter().rev().find_map(|id| {
        if ui_node_hidden(scene, hidden_ui_nodes, id) {
            return None;
        }
        let node = scene.node(id)?;
        node.kind.rect()?;
        let preview = ui_scene_preview_node(scene, id, canvas, canvas_size)?;
        (point_in_quad(pos, preview.quad) || ui_node_hit_rect(preview.bounds()).contains(pos))
            .then_some(id)
    })
}

pub(crate) fn point_in_quad(point: Pos2, quad: [Pos2; 4]) -> bool {
    point_in_triangle(point, quad[0], quad[1], quad[2])
        || point_in_triangle(point, quad[1], quad[3], quad[2])
}

pub(crate) fn point_in_triangle(point: Pos2, a: Pos2, b: Pos2, c: Pos2) -> bool {
    let d1 = signed_triangle_area(point, a, b);
    let d2 = signed_triangle_area(point, b, c);
    let d3 = signed_triangle_area(point, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

pub(crate) fn signed_triangle_area(a: Pos2, b: Pos2, c: Pos2) -> f32 {
    (a.x - c.x) * (b.y - c.y) - (b.x - c.x) * (a.y - c.y)
}

pub(crate) fn ui_node_hit_rect(rect: Rect) -> Rect {
    let expand_x = ((UI_NODE_HIT_MIN_SIZE - rect.width()) * 0.5).max(0.0);
    let expand_y = ((UI_NODE_HIT_MIN_SIZE - rect.height()) * 0.5).max(0.0);
    rect.expand2(Vec2::new(expand_x, expand_y))
}

pub(crate) fn ui_screen_to_canvas(
    pos: Pos2,
    canvas: Rect,
    canvas_size: [u16; 2],
) -> Option<[f32; 2]> {
    if canvas.width() <= f32::EPSILON || canvas.height() <= f32::EPSILON {
        return None;
    }
    Some([
        (pos.x - canvas.left()) * canvas_size[0].max(1) as f32 / canvas.width(),
        (pos.y - canvas.top()) * canvas_size[1].max(1) as f32 / canvas.height(),
    ])
}

pub(crate) fn draw_ui_center_snap_guides(
    painter: &egui::Painter,
    canvas: Rect,
    drag: Option<&UiCanvasDrag>,
) {
    let Some(drag) = drag else {
        return;
    };
    if !drag.snap_center_x && !drag.snap_center_y {
        return;
    }
    let stroke = Stroke::new(1.5, UI_CENTER_GUIDE_COLOR);
    let center = canvas.center();
    if drag.snap_center_x {
        painter.line_segment(
            [
                Pos2::new(center.x.round(), canvas.top()),
                Pos2::new(center.x.round(), canvas.bottom()),
            ],
            stroke,
        );
    }
    if drag.snap_center_y {
        painter.line_segment(
            [
                Pos2::new(canvas.left(), center.y.round()),
                Pos2::new(canvas.right(), center.y.round()),
            ],
            stroke,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UiCenterSnapResult {
    pub(crate) rect: UiRect,
    pub(crate) snap_x: bool,
    pub(crate) snap_y: bool,
}

pub(crate) fn snap_ui_rect_to_canvas_center(
    rect: UiRect,
    canvas_size: [u16; 2],
) -> UiCenterSnapResult {
    let canvas_w = i32::from(canvas_size[0].max(1));
    let canvas_h = i32::from(canvas_size[1].max(1));
    let mut next = rect;
    let tolerance_twice = UI_CENTER_SNAP_TOLERANCE * 2;

    let node_center_x_twice = i32::from(rect.x) * 2 + i32::from(rect.width);
    let canvas_center_x_twice = canvas_w;
    let snap_x = (node_center_x_twice - canvas_center_x_twice).abs() <= tolerance_twice;
    if snap_x {
        next.x = clamp_ui_coord((canvas_w - i32::from(rect.width)) / 2) as i16;
    }

    let node_center_y_twice = i32::from(rect.y) * 2 + i32::from(rect.height);
    let canvas_center_y_twice = canvas_h;
    let snap_y = (node_center_y_twice - canvas_center_y_twice).abs() <= tolerance_twice;
    if snap_y {
        next.y = clamp_ui_coord((canvas_h - i32::from(rect.height)) / 2) as i16;
    }

    UiCenterSnapResult {
        rect: next,
        snap_x,
        snap_y,
    }
}

pub(crate) fn snap_moved_ui_rect_to_canvas_center(
    local_rect: UiRect,
    absolute_rect: UiRect,
    canvas_size: [u16; 2],
) -> UiCenterSnapResult {
    let snapped_absolute = snap_ui_rect_to_canvas_center(absolute_rect, canvas_size);
    let mut next = local_rect;
    if snapped_absolute.snap_x {
        let dx = i32::from(snapped_absolute.rect.x) - i32::from(absolute_rect.x);
        next.x = clamp_ui_coord(i32::from(next.x) + dx) as i16;
    }
    if snapped_absolute.snap_y {
        let dy = i32::from(snapped_absolute.rect.y) - i32::from(absolute_rect.y);
        next.y = clamp_ui_coord(i32::from(next.y) + dy) as i16;
    }
    UiCenterSnapResult {
        rect: next,
        snap_x: snapped_absolute.snap_x,
        snap_y: snapped_absolute.snap_y,
    }
}

pub(crate) fn move_ui_rect(rect: UiRect, delta: [i32; 2]) -> UiRect {
    UiRect {
        x: clamp_ui_coord(rect.x as i32 + delta[0]) as i16,
        y: clamp_ui_coord(rect.y as i32 + delta[1]) as i16,
        width: rect.width.min(UI_NODE_SIZE_MAX as u16).max(1),
        height: rect.height.min(UI_NODE_SIZE_MAX as u16).max(1),
        ..rect
    }
}

pub(crate) fn resize_ui_rect(rect: UiRect, handle: UiResizeHandle, delta: [i32; 2]) -> UiRect {
    let mut left = rect.x as i32;
    let mut top = rect.y as i32;
    let mut right = left + rect.width as i32;
    let mut bottom = top + rect.height as i32;

    if handle.moves_left() {
        left += delta[0];
    }
    if handle.moves_right() {
        right += delta[0];
    }
    if handle.moves_top() {
        top += delta[1];
    }
    if handle.moves_bottom() {
        bottom += delta[1];
    }

    if right - left < UI_NODE_MIN_SIZE {
        if handle.moves_left() {
            left = right - UI_NODE_MIN_SIZE;
        } else {
            right = left + UI_NODE_MIN_SIZE;
        }
    }
    if bottom - top < UI_NODE_MIN_SIZE {
        if handle.moves_top() {
            top = bottom - UI_NODE_MIN_SIZE;
        } else {
            bottom = top + UI_NODE_MIN_SIZE;
        }
    }
    if right - left > UI_NODE_SIZE_MAX {
        if handle.moves_left() {
            left = right - UI_NODE_SIZE_MAX;
        } else {
            right = left + UI_NODE_SIZE_MAX;
        }
    }
    if bottom - top > UI_NODE_SIZE_MAX {
        if handle.moves_top() {
            top = bottom - UI_NODE_SIZE_MAX;
        } else {
            bottom = top + UI_NODE_SIZE_MAX;
        }
    }

    let x = clamp_ui_coord(left);
    let y = clamp_ui_coord(top);
    let width = (right - left).clamp(UI_NODE_MIN_SIZE, UI_NODE_SIZE_MAX) as u16;
    let height = (bottom - top).clamp(UI_NODE_MIN_SIZE, UI_NODE_SIZE_MAX) as u16;
    UiRect {
        x: x as i16,
        y: y as i16,
        width,
        height,
        ..rect
    }
}

pub(crate) fn clamp_ui_coord(value: i32) -> i32 {
    value.clamp(UI_NODE_COORD_MIN, UI_NODE_COORD_MAX)
}

pub(crate) fn normalize_ui_rotation_degrees(value: i32) -> i16 {
    let mut value = value % 360;
    if value > 359 {
        value -= 360;
    } else if value < -359 {
        value += 360;
    }
    value as i16
}

pub(crate) fn ui_binding_preview_q12(binding: UiValueBinding) -> i32 {
    match binding {
        UiValueBinding::ConstantQ12(value) => value,
        UiValueBinding::Option(_) => 4096,
        UiValueBinding::PlayerHealth => 4096,
        UiValueBinding::PlayerHealthMax => 4096,
        UiValueBinding::PlayerStamina => 3072,
        UiValueBinding::PlayerStaminaMax => 4096,
        // Preview a load two-thirds done so the bar reads as a bar.
        UiValueBinding::LoadingProgress => 2730,
    }
}
