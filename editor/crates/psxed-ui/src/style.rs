//! Shared editor visual styling.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Rect, RichText, Stroke, TextStyle,
    Vec2,
};

use crate::icons;

pub(crate) const DEFAULT_VIEWPORT_ZOOM: f32 = 96.0;
pub(crate) const MIN_VIEWPORT_ZOOM: f32 = 24.0;
pub(crate) const MAX_VIEWPORT_ZOOM: f32 = 220.0;

// Layout tokens. Keeping compact control geometry here prevents individual
// inspectors from slowly drifting into different densities.
pub(crate) const CONTROL_HEIGHT: f32 = 24.0;
pub(crate) const ICON_BUTTON_SIZE: f32 = 28.0;
pub(crate) const PANEL_RADIUS: u8 = 6;
pub(crate) const CONTROL_RADIUS: u8 = 5;

// PSoXide's editor palette. The layers are deliberately close in value:
// hierarchy comes from spacing and interaction state instead of a border around
// every control.
pub(crate) const STUDIO_BG: Color32 = Color32::from_rgb(11, 13, 16);
pub(crate) const STUDIO_TOP_BAR: Color32 = Color32::from_rgb(18, 20, 25);
pub(crate) const STUDIO_PANEL: Color32 = Color32::from_rgb(20, 23, 28);
pub(crate) const STUDIO_DOCK: Color32 = Color32::from_rgb(14, 16, 20);
pub(crate) const STUDIO_PANEL_DARK: Color32 = STUDIO_DOCK;
pub(crate) const STUDIO_PANEL_HEADER: Color32 = Color32::from_rgb(24, 27, 33);
pub(crate) const STUDIO_POPUP: Color32 = Color32::from_rgb(22, 25, 31);
pub(crate) const STUDIO_HOVER: Color32 = Color32::from_rgb(31, 36, 44);
pub(crate) const STUDIO_SELECTION: Color32 = Color32::from_rgb(27, 58, 96);
pub(crate) const STUDIO_SELECTION_HOVER: Color32 = Color32::from_rgb(32, 69, 114);
pub(crate) const STUDIO_INPUT: Color32 = Color32::from_rgb(13, 16, 20);
pub(crate) const STUDIO_VIEWPORT: Color32 = Color32::from_rgb(9, 13, 17);
pub(crate) const STUDIO_BORDER: Color32 = Color32::from_rgb(43, 48, 58);
pub(crate) const STUDIO_BORDER_DARK: Color32 = Color32::from_rgb(31, 35, 42);
pub(crate) const STUDIO_TEXT: Color32 = Color32::from_rgb(229, 232, 239);
pub(crate) const STUDIO_TEXT_WEAK: Color32 = Color32::from_rgb(139, 147, 160);
pub(crate) const STUDIO_ACCENT: Color32 = Color32::from_rgb(72, 151, 255);
pub(crate) const STUDIO_ACCENT_HOVER: Color32 = Color32::from_rgb(112, 178, 255);
pub(crate) const STUDIO_ACCENT_DIM: Color32 = Color32::from_rgb(28, 66, 112);
pub(crate) const STUDIO_SUCCESS: Color32 = Color32::from_rgb(94, 201, 142);
pub(crate) const STUDIO_SUCCESS_DIM: Color32 = Color32::from_rgb(39, 83, 57);
pub(crate) const STUDIO_WARNING: Color32 = Color32::from_rgb(231, 177, 85);
pub(crate) const STUDIO_ERROR: Color32 = Color32::from_rgb(235, 107, 107);
pub(crate) const STUDIO_ERROR_DIM: Color32 = Color32::from_rgb(98, 42, 48);
pub(crate) const STUDIO_TREE_GUIDE: Color32 = Color32::from_rgb(45, 52, 63);
pub(crate) const STUDIO_ROOM_FLOOR: Color32 = Color32::from_rgb(119, 132, 143);
pub(crate) const STUDIO_ROOM_WALL: Color32 = Color32::from_rgb(126, 73, 43);

pub(crate) const PANEL_HEADER_MIN_HEIGHT: f32 = 26.0;

pub(crate) fn panel_header_margin() -> Margin {
    Margin {
        left: 8,
        right: 8,
        top: 3,
        bottom: 3,
    }
}

pub(crate) fn apply_studio_visuals(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut(|style| {
        style.text_styles = [
            (
                TextStyle::Heading,
                FontId::new(16.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
            (
                TextStyle::Monospace,
                FontId::new(12.0, FontFamily::Monospace),
            ),
            (
                TextStyle::Button,
                FontId::new(13.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(7.0, 5.0);
        style.spacing.button_padding = Vec2::new(8.0, 4.0);
        style.spacing.interact_size = Vec2::new(ICON_BUTTON_SIZE, CONTROL_HEIGHT);
        style.spacing.window_margin = Margin::same(8);
        style.spacing.menu_margin = Margin::symmetric(10, 7);
        style.spacing.indent = 16.0;
        style.animation_time = 0.08;
        style.visuals = studio_visuals();
    });
}

pub(crate) fn studio_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(STUDIO_TEXT);
    visuals.panel_fill = STUDIO_PANEL_DARK;
    visuals.window_fill = STUDIO_POPUP;
    visuals.window_stroke = Stroke::new(1.0, STUDIO_BORDER);
    visuals.window_corner_radius = CornerRadius::same(PANEL_RADIUS);
    visuals.menu_corner_radius = CornerRadius::same(PANEL_RADIUS);
    visuals.faint_bg_color = STUDIO_PANEL_HEADER;
    visuals.extreme_bg_color = STUDIO_INPUT;
    visuals.code_bg_color = Color32::from_rgb(15, 18, 23);
    visuals.hyperlink_color = STUDIO_ACCENT;
    visuals.selection.bg_fill = STUDIO_SELECTION;
    visuals.selection.stroke = Stroke::new(1.0, STUDIO_ACCENT);
    visuals.button_frame = true;
    visuals.collapsing_header_frame = false;
    visuals.indent_has_left_vline = false;
    visuals.slider_trailing_fill = true;
    visuals.warn_fg_color = STUDIO_WARNING;
    visuals.error_fg_color = STUDIO_ERROR;
    visuals.text_cursor.stroke = Stroke::new(1.5, STUDIO_ACCENT_HOVER);

    visuals.widgets.noninteractive.bg_fill = STUDIO_PANEL;
    visuals.widgets.noninteractive.weak_bg_fill = STUDIO_PANEL_DARK;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, STUDIO_BORDER_DARK);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, STUDIO_TEXT);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(CONTROL_RADIUS);

    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.weak_bg_fill = STUDIO_PANEL_HEADER;
    visuals.widgets.inactive.bg_stroke = Stroke::NONE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, STUDIO_TEXT);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(CONTROL_RADIUS);

    visuals.widgets.hovered.bg_fill = STUDIO_HOVER;
    visuals.widgets.hovered.weak_bg_fill = STUDIO_HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, STUDIO_BORDER);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(CONTROL_RADIUS);

    visuals.widgets.active.bg_fill = STUDIO_SELECTION_HOVER;
    visuals.widgets.active.weak_bg_fill = STUDIO_SELECTION_HOVER;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, STUDIO_ACCENT);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.widgets.active.corner_radius = CornerRadius::same(CONTROL_RADIUS);

    visuals.widgets.open.bg_fill = STUDIO_HOVER;
    visuals.widgets.open.weak_bg_fill = STUDIO_HOVER;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, STUDIO_BORDER);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, STUDIO_TEXT);
    visuals.widgets.open.corner_radius = CornerRadius::same(CONTROL_RADIUS);
    visuals
}

pub(crate) fn top_bar_frame() -> Frame {
    Frame::new()
        .fill(STUDIO_TOP_BAR)
        .stroke(Stroke::new(1.0, STUDIO_BORDER_DARK))
        .inner_margin(Margin::symmetric(8, 5))
}

pub(crate) fn dock_frame() -> Frame {
    Frame::new()
        .fill(STUDIO_DOCK)
        .inner_margin(Margin::symmetric(5, 5))
}

pub(crate) fn section_frame() -> Frame {
    Frame::new()
        .fill(STUDIO_PANEL_HEADER)
        .corner_radius(CornerRadius::same(PANEL_RADIUS))
        .inner_margin(Margin::symmetric(10, 9))
}

pub(crate) fn tool_panel_frame() -> Frame {
    Frame::new()
        .fill(STUDIO_PANEL)
        .corner_radius(CornerRadius::same(PANEL_RADIUS))
        .inner_margin(Margin::same(0))
}

pub(crate) fn tool_panel_body<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    Frame::new()
        .inner_margin(Margin::symmetric(9, 8))
        .show(ui, add_contents)
        .inner
}

pub(crate) fn viewport_frame() -> Frame {
    Frame::new().fill(STUDIO_BG).inner_margin(Margin::same(5))
}

pub(crate) fn panel_heading(ui: &mut egui::Ui, icon: char, label: &str) {
    ui.horizontal(|ui| {
        ui.label(icons::text(icon, 14.0).color(STUDIO_ACCENT));
        ui.label(RichText::new(label).strong().size(12.0).color(STUDIO_TEXT));
    });
}

pub(crate) fn tool_panel_header(
    ui: &mut egui::Ui,
    icon: char,
    label: &str,
    add_actions: impl FnOnce(&mut egui::Ui),
) {
    Frame::new()
        .fill(STUDIO_PANEL_HEADER)
        .inner_margin(panel_header_margin())
        .show(ui, |ui| {
            ui.set_min_height(PANEL_HEADER_MIN_HEIGHT);
            ui.horizontal(|ui| {
                ui.label(icons::text(icon, 14.0).color(STUDIO_ACCENT));
                ui.label(RichText::new(label).strong().size(12.0).color(STUDIO_TEXT));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    add_actions(ui);
                });
            });
        });
}

/// Shared visual treatment for dense hierarchy and filesystem rows.
pub(crate) fn paint_list_row(painter: &egui::Painter, rect: Rect, selected: bool, hovered: bool) {
    let row = rect.shrink2(Vec2::new(0.0, 1.0));
    if selected {
        painter.rect_filled(
            row,
            CornerRadius::same(4),
            if hovered {
                STUDIO_SELECTION_HOVER
            } else {
                STUDIO_SELECTION
            },
        );
        painter.rect_filled(
            Rect::from_min_max(
                row.left_top() + Vec2::new(1.0, 5.0),
                row.left_bottom() + Vec2::new(4.0, -5.0),
            ),
            CornerRadius::same(2),
            STUDIO_ACCENT,
        );
    } else if hovered {
        painter.rect_filled(row, CornerRadius::same(4), STUDIO_HOVER);
    }
}
