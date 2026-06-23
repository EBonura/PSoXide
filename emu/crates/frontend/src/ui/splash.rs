//! One-shot boot splash.
//!
//! The PSoXide logo holds still briefly, then fades out while expanding.
//! The timeline is anchored to the FIRST frame the splash actually paints
//! (not egui's context-creation clock), so boot/asset load can't eat the
//! hold. It plays exactly once and then no-ops forever.

use std::cell::OnceCell;

use egui::{pos2, Color32, Id, LayerId, Order, Rect, TextureHandle, Vec2};

/// The branding logo, embedded at compile time (works for native and wasm).
const LOGO_PNG: &[u8] = include_bytes!("../../../../../assets/branding/psoxide-logo.png");

/// Seconds the logo holds fully visible before it starts leaving.
const HOLD: f32 = 0.5;
/// Seconds the fade-out + expand takes.
const DISMISS: f32 = 0.6;

/// Draw the boot splash on a foreground layer. No-op once it has finished.
pub fn draw(ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let t = (now - start_time(now)) as f32;
    if t >= HOLD + DISMISS {
        return;
    }
    // Keep frames coming so the animation advances even when nothing else is.
    ctx.request_repaint();

    // 0 while holding, eases 0..1 across the dismiss (ease-out quad).
    let p = ((t - HOLD) / DISMISS).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - p) * (1.0 - p);
    let scale = 1.0 + 0.4 * ease; // grows as it leaves
    let alpha = 1.0 - ease; // fades as it leaves

    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("boot-splash")));

    // Dark cover that fades out with the logo: hides boot flicker, then reveals.
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha((235.0 * alpha) as u8));

    // Single logo draw (the artwork already carries its own glow). Fading via a
    // white tint so the texture's alpha scales down uniformly.
    let tex = logo_texture(ctx);
    let [tw, th] = tex.size();
    let aspect = tw as f32 / th.max(1) as f32;
    let w = (screen.width() * 0.5).min(tw as f32).min(640.0) * scale;
    let rect = Rect::from_center_size(screen.center(), Vec2::new(w, w / aspect));
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    painter.image(tex.id(), rect, uv, Color32::from_white_alpha((255.0 * alpha) as u8));
}

/// The egui time of the first frame the splash painted (its timeline origin).
fn start_time(now: f64) -> f64 {
    thread_local! {
        static START: OnceCell<f64> = const { OnceCell::new() };
    }
    START.with(|c| *c.get_or_init(|| now))
}

/// Decode + upload the logo once, then hand back the cached handle. Kept in a
/// thread-local `OnceCell` so `draw` can stay stateless (egui runs on one
/// thread).
fn logo_texture(ctx: &egui::Context) -> TextureHandle {
    thread_local! {
        static TEX: OnceCell<TextureHandle> = const { OnceCell::new() };
    }
    TEX.with(|cell| {
        cell.get_or_init(|| {
            let rgba = image::load_from_memory(LOGO_PNG)
                .expect("embedded PSoXide logo decodes")
                .to_rgba8();
            let (w, h) = rgba.dimensions();
            let image =
                egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], rgba.as_raw());
            ctx.load_texture("psoxide-logo", image, egui::TextureOptions::LINEAR)
        })
        .clone()
    })
}
