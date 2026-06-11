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
            // No close button: the sidebar is toggled from the toolbar's
            // bug icon (or Menu -> Debug), same as it was opened.
            ui.label(
                RichText::new("Debug")
                    .color(theme::ACCENT)
                    .size(theme::FONT_SIZE_HEADING),
            );
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let regs_override = state
                        .panels
                        .take_override(crate::app::DebugSection::Registers);
                    collapsible(ui, "CPU Registers", state.panels.registers, regs_override, |ui| {
                        registers::draw_contents(
                            ui,
                            &state.cpu,
                            &state.exec_history,
                            &mut state.breakpoints,
                            &mut state.gpr_snapshot,
                        );
                    });
                    let mem_override = state
                        .panels
                        .take_override(crate::app::DebugSection::Memory);
                    collapsible(ui, "Memory", state.panels.memory, mem_override, |ui| {
                        memory::draw_contents(
                            ui,
                            &mut state.memory_view,
                            state.bus.as_ref(),
                            &state.cpu,
                            &mut state.breakpoints,
                        );
                    });
                    let vram_override =
                        state.panels.take_override(crate::app::DebugSection::Vram);
                    collapsible(ui, "VRAM", state.panels.vram, vram_override, |ui| {
                        vram::draw_contents(ui, vram_tex);
                    });
                    let prof_override = state
                        .panels
                        .take_override(crate::app::DebugSection::Profiler);
                    collapsible(ui, "Frame Profiler", state.panels.profiler, prof_override, |ui| {
                        profiler::draw_contents(ui, &mut state.profiler);
                    });
                });
        });
}

fn collapsible(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    open_override: Option<bool>,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    // `default_open` only seeds the FIRST render; egui persists collapse
    // state afterwards. Menu toggles therefore pass a one-shot
    // `open_override` that forces the state for this frame.
    egui::CollapsingHeader::new(RichText::new(title).color(theme::TEXT).strong())
        .default_open(default_open)
        .open(open_override)
        .show(ui, |ui| {
            theme::viz_frame(ui, "", add_contents);
        });
    ui.add_space(4.0);
}
