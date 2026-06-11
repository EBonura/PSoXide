//! VRAM viewer -- renders the full 1024×512 VRAM as an image panel.
//!
//! The texture upload happens in `gfx::Graphics::prepare_vram`; this
//! module is purely the egui layout that places the image inside the
//! debug sidebar. A later milestone will add overlays for framebuffer
//! regions, texture pages, and CLUT rows.

use emulator_core::{VRAM_HEIGHT, VRAM_WIDTH};

/// Draw the VRAM texture inside an existing sidebar/container.
pub fn draw_contents(ui: &mut egui::Ui, tex: egui::TextureId) {
    // Always preserve VRAM's true 2:1 aspect: width-driven height with a
    // height cap, never the old clamp that distorted the image once the
    // panel width left the clamp band.
    const MAX_HEIGHT: f32 = 320.0;
    let aspect = VRAM_HEIGHT as f32 / VRAM_WIDTH as f32;
    let width = ui.available_width().max(1.0).min(MAX_HEIGHT / aspect);
    let height = width * aspect;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    egui::Image::new((tex, rect.size()))
        .uv(full_uv())
        .paint_at(ui, rect);
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}
