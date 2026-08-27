//! Render the cooked run through the actual editor animation-viewer rasterizer
//! (front, auto-framed) so we can compare to the gltf-viewer/Mixamo reference.
//!   cargo run -p psxed-ui --example render_preview -- <model.psxmdl> <clip.psxanim> <atlas.psxt>

use egui::{Color32, ColorImage};
use psx_asset::Texture;
use psxed_ui::model_import_preview::{
    render_import_model_preview_with_palette_banks, ImportPreviewOptions,
};

fn decode_atlas_banks(bytes: &[u8]) -> Vec<ColorImage> {
    let t = Texture::from_bytes(bytes).unwrap();
    let (w, h) = (t.width() as usize, t.height() as usize);
    let pix = t.pixel_bytes();
    let clut = t.clut_bytes();
    let row_bytes = usize::from(t.halfwords_per_row()) * 2;
    let clut_entries = t.clut_entries() as usize;
    let entries_per_bank = match t.depth() as u8 {
        4 => 16,
        8 => 256,
        15 => 0,
        _ => unreachable!("Texture parser rejects unsupported depths"),
    };
    let bank_count = if entries_per_bank == 0 {
        1
    } else {
        clut_entries / entries_per_bank
    };
    let mut banks = Vec::with_capacity(bank_count);
    for bank in 0..bank_count {
        let mut pixels = vec![Color32::BLACK; w * h];
        for y in 0..h {
            for x in 0..w {
                let raw = match t.depth() as u8 {
                    4 => {
                        let packed = pix[y * row_bytes + x / 2];
                        let index = (if x & 1 == 0 {
                            packed & 0x0f
                        } else {
                            packed >> 4
                        }) as usize;
                        let offset = (bank * entries_per_bank + index) * 2;
                        u16::from_le_bytes([clut[offset], clut[offset + 1]])
                    }
                    8 => {
                        let index = pix[y * row_bytes + x] as usize;
                        let offset = (bank * entries_per_bank + index) * 2;
                        u16::from_le_bytes([clut[offset], clut[offset + 1]])
                    }
                    15 => {
                        let offset = y * row_bytes + x * 2;
                        u16::from_le_bytes([pix[offset], pix[offset + 1]])
                    }
                    _ => unreachable!("Texture parser rejects unsupported depths"),
                } & 0x7fff;
                let r = ((raw & 31) * 255 / 31) as u8;
                let g = (((raw >> 5) & 31) * 255 / 31) as u8;
                let b = (((raw >> 10) & 31) * 255 / 31) as u8;
                pixels[y * w + x] = Color32::from_rgb(r, g, b);
            }
        }
        banks.push(ColorImage {
            size: [w, h],
            pixels,
        });
    }
    banks
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
    let atlas_banks = decode_atlas_banks(&std::fs::read(a.next().unwrap()).unwrap());
    let time_seconds = a.next().and_then(|value| value.parse().ok()).unwrap_or(0.5);
    // Framing height: the auto-frame is sized for a ~1700-unit humanoid, so a
    // prop needs its own or it renders as a speck.
    let world_height = a
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1700);
    let out_prefix = a.next().unwrap_or_else(|| String::from("/tmp/preview"));

    for (yi, &yaw) in [0u16, 1024, 2048, 3072].iter().enumerate() {
        let opts = ImportPreviewOptions {
            world_height,
            visual_scale_q8: 256,
            visual_yaw_q12: 0,
            collision_radius: 200,
            time_seconds,
            yaw_q12: yaw,
            pitch_q12: 200,
            radius: 0,
            focus_on_animated_bounds: true,
            preview_in_place: true,
            pose_offset: [0, 0, 0],
            show_animation_root: false,
            show_collision_guides: false,
            show_bones: false,
        };
        if let Some(img) =
            render_import_model_preview_with_palette_banks(&model, &clip, &atlas_banks, opts)
        {
            let p = format!("{out_prefix}_yaw{yi}.ppm");
            save_ppm(&img, &p);
            println!("wrote {p}");
        } else {
            println!("render failed yaw{yi}");
        }
    }
}
