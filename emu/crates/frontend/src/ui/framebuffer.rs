//! Framebuffer view for the central panel.
//!
//! The central panel presents the selected visible display texture.
//! Normal 15-bit gameplay samples the host-GPU renderer's VRAM-shaped
//! target; 24bpp scanout samples the packed display texture until the
//! HW path grows RGB888 display decoding.

use crate::theme;

/// CRT display aspect ratio for NTSC. The visible area is 4:3
/// regardless of which horizontal-resolution mode the game picks
/// (256/320/368/384/512/640) -- a 512×240 frame is supposed to
/// squash horizontally on a real CRT, not stretch into 16:9.
const CRT_ASPECT: f32 = 4.0 / 3.0;

pub fn draw(
    ui: &mut egui::Ui,
    display_tex: egui::TextureId,
    display_uv: egui::Rect,
    present_size_px: &mut (u32, u32),
) {
    theme::viz_frame(ui, "", |ui| {
        let avail = ui.available_rect_before_wrap();
        if avail.width() <= 0.0 || avail.height() <= 0.0 {
            return;
        }
        let h = avail.height().min(avail.width() / CRT_ASPECT);
        let w = h * CRT_ASPECT;
        let rect = egui::Rect::from_center_size(avail.center(), egui::vec2(w, h));

        let pixels_per_point = ui.ctx().pixels_per_point().max(1.0);
        *present_size_px = (
            (rect.width() * pixels_per_point).round().max(1.0) as u32,
            (rect.height() * pixels_per_point).round().max(1.0) as u32,
        );

        ui.allocate_rect(avail, egui::Sense::hover());
        egui::Image::new((display_tex, rect.size()))
            .uv(display_uv)
            .paint_at(ui, rect);
    });
}
