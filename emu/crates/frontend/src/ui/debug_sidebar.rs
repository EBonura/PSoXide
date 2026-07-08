//! Unified emulator diagnostics sidebar.
//!
//! The individual debug tools still own their content rendering; this module
//! only docks them into one right-hand sidebar with collapsible sections.

use egui::{Align, Layout, Rect, RichText, SidePanel, UiBuilder};

use crate::app::AppState;
use crate::theme;

use super::{memory, profiler, registers, vram};

const SIDEBAR_MIN_WIDTH: f32 = 320.0;
const SIDEBAR_MAX_WIDTH: f32 = 900.0;
/// Inner padding, replacing the default panel frame margin we drop so the
/// slide-in content-width math stays exact.
const SIDEBAR_PAD: f32 = 8.0;

/// Draw the right-hand debug sidebar, sliding in/out from the right edge.
///
/// Mirrors the toolbar slide: the panel width animates to zero while the
/// content stays anchored to the panel's left edge at full width, so the
/// whole sidebar translates right and clips off under the window edge. The
/// panel is resizable only when fully open; its width is remembered as the
/// animation target so a resized sidebar animates to its own width.
pub fn draw(ctx: &egui::Context, state: &mut AppState, vram_tex: egui::TextureId) {
    // 0.0 = closed, 1.0 = open.
    let t = ctx.animate_bool_with_time(
        egui::Id::new("debug_sidebar_slide"),
        state.panels.debug_sidebar,
        0.22,
    );
    if t <= 0.002 {
        return;
    }
    let animating = t < 0.998;
    let target = state.sidebar_width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    let panel_fill = ctx.style().visuals.panel_fill;
    let separator = ctx.style().visuals.widgets.noninteractive.bg_stroke;

    // Zero-margin fill frame: content padding is handled by hand (SIDEBAR_PAD)
    // so both the animated and open paths share the exact same layout.
    let panel = SidePanel::right("debug-sidebar").frame(egui::Frame::NONE.fill(panel_fill));
    let panel = if animating {
        panel.resizable(false).exact_width((target * t).max(1.0))
    } else {
        panel
            .resizable(true)
            .default_width(target)
            .min_width(SIDEBAR_MIN_WIDTH)
            .max_width(SIDEBAR_MAX_WIDTH)
    };

    let response = panel.show(ctx, |ui| {
        let panel_rect = ui.max_rect();
        // Hairline on the leading (left) edge -- the sidebar edge sliding in.
        ui.painter()
            .vline(panel_rect.left() + 0.5, panel_rect.y_range(), separator);
        // Anchor content at full width so a narrow (animating) panel clips it
        // rather than reflowing; at rest it just fills the panel exactly.
        let content_w = if animating { target } else { panel_rect.width() };
        let content_rect = Rect::from_min_size(
            panel_rect.min,
            egui::vec2(content_w, panel_rect.height()),
        );
        ui.scope_builder(
            UiBuilder::new()
                .max_rect(content_rect.shrink(SIDEBAR_PAD))
                .layout(Layout::top_down(Align::Min)),
            |ui| {
                ui.set_clip_rect(content_rect.intersect(panel_rect));
                draw_contents(ui, state, vram_tex);
            },
        );
    });

    if !animating {
        state.sidebar_width = response
            .response
            .rect
            .width()
            .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
    }
}

fn draw_contents(ui: &mut egui::Ui, state: &mut AppState, vram_tex: egui::TextureId) {
    // No close button: the sidebar is toggled from the toolbar's bug icon,
    // same as it was opened.
    ui.label(
        RichText::new("Debug")
            .color(theme::ACCENT)
            .size(theme::FONT_SIZE_HEADING),
    );
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            collapsible(ui, "CPU Registers", state.panels.registers, |ui| {
                registers::draw_contents(
                    ui,
                    &state.cpu,
                    &state.exec_history,
                    &mut state.breakpoints,
                    &mut state.gpr_snapshot,
                );
            });
            collapsible(ui, "Memory", state.panels.memory, |ui| {
                memory::draw_contents(
                    ui,
                    &mut state.memory_view,
                    state.bus.as_ref(),
                    &state.cpu,
                    &mut state.breakpoints,
                );
            });
            collapsible(ui, "VRAM", state.panels.vram, |ui| {
                vram::draw_contents(ui, vram_tex);
            });
            collapsible(ui, "Frame Profiler", state.panels.profiler, |ui| {
                profiler::draw_contents(ui, &mut state.profiler);
            });
        });
}

fn collapsible(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    // `default_open` seeds the first render; egui persists collapse state
    // afterwards (the user expands/collapses each section by hand).
    egui::CollapsingHeader::new(RichText::new(title).color(theme::TEXT).strong())
        .default_open(default_open)
        .show(ui, |ui| {
            theme::viz_frame(ui, "", add_contents);
        });
    ui.add_space(4.0);
}
