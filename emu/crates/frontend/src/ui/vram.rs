//! VRAM viewer -- renders the full 1024×512 VRAM as an image panel.
//!
//! The texture upload happens in `gfx::Graphics::prepare_vram`; this
//! module is purely the egui layout that places the image inside the
//! debug sidebar. A later milestone will add overlays for framebuffer
//! regions, texture pages, and CLUT rows.

use emulator_core::{VRAM_HEIGHT, VRAM_WIDTH};

/// Draw the VRAM texture inside an existing sidebar/container.
pub fn draw_contents(ui: &mut egui::Ui, tex: egui::TextureId) {
    let width = ui.available_width().max(1.0);
    let height = (width * VRAM_HEIGHT as f32 / VRAM_WIDTH as f32).clamp(120.0, 260.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    egui::Image::new((tex, rect.size()))
        .uv(full_uv())
        .paint_at(ui, rect);
}

fn full_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
}
