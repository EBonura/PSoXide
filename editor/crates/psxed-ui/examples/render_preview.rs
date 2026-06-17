//! Render the cooked run through the actual editor animation-viewer rasterizer
//! (front, auto-framed) so we can compare to the gltf-viewer/Mixamo reference.
//!   cargo run -p psxed-ui --example render_preview -- <model.psxmdl> <clip.psxanim> <atlas.psxt>

use egui::{Color32, ColorImage};
use psx_asset::Texture;
use psxed_ui::model_import_preview::{render_import_model_preview_with_options, ImportPreviewOptions};

fn decode_atlas(bytes: &[u8]) -> ColorImage {
    let t = Texture::from_bytes(bytes).unwrap();
    let (w, h) = (t.width() as usize, t.height() as usize);
    let pix = t.pixel_bytes();
    let clut = t.clut_bytes();
    let mut pixels = vec![Color32::BLACK; w * h];
    for y in 0..h {
        for x in 0..w {
            let idx = pix[y * w + x] as usize;
            let v = u16::from_le_bytes([clut[idx * 2], clut[idx * 2 + 1]]);
            let r = ((v & 31) * 255 / 31) as u8;
            let g = (((v >> 5) & 31) * 255 / 31) as u8;
            let b = (((v >> 10) & 31) * 255 / 31) as u8;
            pixels[y * w + x] = Color32::from_rgb(r, g, b);
        }
    }
    ColorImage { size: [w, h], pixels }
}

fn save_ppm(img: &ColorImage, path: &str) {
    let [w, h] = img.size;
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for p in &img.pixels {
        out.push(p.r());
        out.push(p.g());
        out.push(p.b());
    }
    std::fs::write(path, out).unwrap();
}

fn main() {
    let mut a = std::env::args().skip(1);
    let model = std::fs::read(a.next().unwrap()).unwrap();
    let clip = std::fs::read(a.next().unwrap()).unwrap();
    let atlas = decode_atlas(&std::fs::read(a.next().unwrap()).unwrap());

    for (yi, &yaw) in [0u16, 1024, 2048, 3072].iter().enumerate() {
        let opts = ImportPreviewOptions {
            world_height: 1700,
            visual_scale_q8: 256,
            visual_yaw_q12: 0,
            collision_radius: 200,
            time_seconds: 0.5,
            yaw_q12: yaw,
            pitch_q12: 200,
            radius: 0,
            focus_on_animated_bounds: true,
            preview_in_place: true,
            pose_offset: [0, 0, 0],
            show_animation_root: false,
            show_collision_guides: false,
            show_bones: false,
            show_hitboxes: true,
        };
        if let Some(img) = render_import_model_preview_with_options(&model, &clip, &atlas, opts) {
            let p = format!("/tmp/preview_yaw{yi}.ppm");
            save_ppm(&img, &p);
            println!("wrote {p}");
        } else {
            println!("render failed yaw{yi}");
        }
    }
}
