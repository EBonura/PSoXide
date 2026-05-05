//! Unified emulator diagnostics sidebar.
//!
//! The individual debug tools still own their content rendering; this module
//! only docks them into one right-hand sidebar with collapsible sections.

use egui::{RichText, SidePanel};

use crate::app::AppState;
use crate::theme;

use super::{memory, profiler, registers, vram};

const SIDEBAR_WIDTH: f32 = 430.0;
const SIDEBAR_MIN_WIDTH: f32 = 320.0;

pub fn draw(ctx: &egui::Context, state: &mut AppState, vram_tex: egui::TextureId) {
    SidePanel::right("debug-sidebar")
        .resizable(true)
        .default_width(SIDEBAR_WIDTH)
        .min_width(SIDEBAR_MIN_WIDTH)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Debug")
                        .color(theme::ACCENT)
                        .size(theme::FONT_SIZE_HEADING),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Close").clicked() {
                        state.panels.debug_sidebar = false;
                    }
                });
            });
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
        });
}

fn collapsible(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    egui::CollapsingHeader::new(RichText::new(title).color(theme::TEXT).strong())
        .default_open(default_open)
        .show(ui, |ui| {
            theme::viz_frame(ui, "", add_contents);
        });
    ui.add_space(4.0);
}
