use super::*;

pub(crate) fn human_bytes(n: u32) -> String {
    human_bytes_u64(n as u64)
}

pub(crate) fn human_bytes_u64(n: u64) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", (n as f64) / 1024.0)
    } else {
        format!("{:.1} MB", (n as f64) / (1024.0 * 1024.0))
    }
}

pub(crate) fn draw_play_overlay_icon_button(
    ui: &mut egui::Ui,
    rect: Rect,
    id_source: &'static str,
    icon: char,
    tooltip: &'static str,
    active: bool,
    enabled: bool,
    active_fill: Option<Color32>,
) -> bool {
    let response = ui
        .interact(
            rect,
            ui.id().with(("play_overlay_icon_button", id_source)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        )
        .on_hover_text(tooltip);
    let hovered = response.hovered();
    let fill = if active {
        active_fill.unwrap_or(STUDIO_ACCENT_DIM)
    } else if hovered && enabled {
        Color32::from_rgba_unmultiplied(34, 48, 58, 232)
    } else if enabled {
        Color32::from_black_alpha(176)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 112)
    };
    let stroke = if active {
        Stroke::new(1.0, STUDIO_ACCENT)
    } else if hovered && enabled {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 128))
    } else {
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(210, 220, 235, 84))
    };
    let icon_color = if !enabled {
        Color32::from_rgba_unmultiplied(142, 154, 168, 108)
    } else if hovered || active {
        Color32::WHITE
    } else {
        STUDIO_TEXT
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, fill);
    painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon.to_string(),
        icons::font(14.0),
        icon_color,
    );
    enabled && response.clicked()
}

pub(crate) fn q12_degrees(angle: u16) -> f32 {
    angle as f32 * 360.0 / 4096.0
}

pub(crate) fn play_frame_rate_from_ms(frame_ms: f32) -> f32 {
    if frame_ms > 0.0 && frame_ms.is_finite() {
        (1000.0 / frame_ms).clamp(0.0, PLAY_FRAME_TARGET_FPS)
    } else {
        0.0
    }
}

pub(crate) fn draw_play_frame_rate_chart(
    painter: &egui::Painter,
    rect: Rect,
    samples: &VecDeque<f32>,
) {
    painter.rect_filled(rect, 3.0, Color32::from_black_alpha(88));
    painter.rect_stroke(
        rect,
        3.0,
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(180, 200, 220, 64)),
        StrokeKind::Inside,
    );
    let plot = rect.shrink2(Vec2::new(2.0, 3.0));
    let target_y = plot.top();
    painter.line_segment(
        [
            Pos2::new(plot.left(), target_y),
            Pos2::new(plot.right(), target_y),
        ],
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(235, 240, 248, 126)),
    );

    if samples.len() < 2 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "VIS fps",
            FontId::monospace(9.0),
            STUDIO_TEXT_WEAK,
        );
        return;
    }

    let step = plot.width() / (samples.len().saturating_sub(1) as f32).max(1.0);
    let mut fps_points = Vec::with_capacity(samples.len());
    let fill = Color32::from_rgba_unmultiplied(42, 214, 124, 38);
    for (index, frame_ms) in samples.iter().copied().enumerate() {
        let rate = play_frame_rate_from_ms(frame_ms);
        let t = (rate / PLAY_FRAME_TARGET_FPS).clamp(0.0, 1.0);
        let point = Pos2::new(
            plot.left() + step * index as f32,
            plot.bottom() - plot.height() * t,
        );
        let x0 = if index == 0 {
            plot.left()
        } else {
            point.x - step * 0.5
        };
        let x1 = if index + 1 == samples.len() {
            plot.right()
        } else {
            point.x + step * 0.5
        };
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(x0, point.y), Pos2::new(x1, plot.bottom())),
            0.0,
            fill,
        );
        fps_points.push(point);
    }
    painter.add(egui::Shape::line(
        fps_points,
        Stroke::new(1.2, Color32::from_rgba_unmultiplied(150, 238, 172, 184)),
    ));
}

pub(crate) fn draw_play_metric_line(
    painter: &egui::Painter,
    x: f32,
    y: &mut f32,
    text: &str,
    color: Color32,
) {
    painter.text(
        Pos2::new(x, *y),
        Align2::LEFT_TOP,
        text,
        FontId::monospace(11.0),
        color,
    );
    *y += 13.0;
}
