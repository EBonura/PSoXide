//! Cook a project with far cards and write contact sheets of its pictures.
//!
//! Usage: `cards_probe <project.ron> <out dir> [room ...]`
//!
//! Writes `room-NNN.png` for each named room (every 20th when none are
//! named): the eight views side by side, magnified 3x on a checkerboard so
//! the transparent texels show, decoded from the cooked disc sectors
//! exactly as the GPU would read them.

use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};
use psxed_project::playtest::build_package;
use psxed_project::ProjectDocument;

/// Decode one picture sector: 63 rows of 4-bit texels, palette below.
fn decode(sector: &[u8]) -> Vec<[u8; 4]> {
    let palette: Vec<[u8; 4]> = (0..16)
        .map(|k| {
            let word = u16::from_le_bytes([sector[63 * 32 + 2 * k], sector[63 * 32 + 2 * k + 1]]);
            if word == 0 {
                [0, 0, 0, 0]
            } else {
                let c = |shift: u32| (((word >> shift) & 31) as u8) << 3;
                [c(0), c(5), c(10), 255]
            }
        })
        .collect();
    let mut out = Vec::with_capacity(64 * 63);
    for row in 0..63 {
        for x in 0..64 {
            let byte = sector[row * 32 + x / 2];
            let index = if x % 2 == 0 { byte & 15 } else { byte >> 4 };
            out.push(palette[usize::from(index)]);
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: cards_probe <project.ron> <out dir> [room ...]");
        std::process::exit(2);
    }
    let project_path = Path::new(&args[0]);
    let text = std::fs::read_to_string(project_path).expect("read the project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse the project");
    let root = project_path.parent().map_or_else(|| PathBuf::from("."), PathBuf::from);
    let out = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out).expect("create the output directory");
    let (package, report) = build_package(&project, &root);
    for line in report.warnings.iter().filter(|w| w.starts_with("cards:")) {
        println!("{line}");
    }
    for error in &report.errors {
        eprintln!("error: {error}");
    }
    let package = package.expect("a package");
    let cards = &package.world_geometry.cards;
    println!(
        "{} rows, {} pictures, far {}, fade {}",
        cards.rows.len(),
        cards.pictures.iter().filter(|p| !p.is_empty()).count(),
        cards.far,
        cards.fade
    );
    let rooms: Vec<usize> = if args.len() > 2 {
        args[2..].iter().filter_map(|a| a.parse().ok()).collect()
    } else {
        (0..cards.rows.len()).step_by(20).collect()
    };
    let scale = 3usize;
    for room in rooms {
        let Some(pictures) = cards.pictures.get(room).filter(|p| !p.is_empty()) else {
            println!("room {room}: no card");
            continue;
        };
        let row = cards.rows[room];
        println!(
            "room {room}: centre ({}, {}) base {} height {} half width {}",
            row[0], row[1], row[2], row[3], row[4]
        );
        let (w, h) = (64 * scale, 63 * scale);
        let mut sheet = RgbaImage::new((w * 8 + 7 * 4) as u32, h as u32);
        for pixel in sheet.pixels_mut() {
            *pixel = Rgba([255, 0, 255, 255]);
        }
        for view in 0..8 {
            let texels = decode(&pictures[view * 2048..(view + 1) * 2048]);
            for y in 0..h {
                for x in 0..w {
                    let t = texels[(y / scale) * 64 + x / scale];
                    // Checkerboard under transparent texels.
                    let check = if ((x / 8) + (y / 8)) % 2 == 0 { 60 } else { 90 };
                    let px = if t[3] == 0 {
                        Rgba([check, check, check, 255])
                    } else {
                        Rgba([t[0], t[1], t[2], 255])
                    };
                    sheet.put_pixel((view * (w + 4) + x) as u32, y as u32, px);
                }
            }
        }
        let path = out.join(format!("room-{room:03}.png"));
        sheet.save(&path).expect("write the sheet");
        println!("wrote {}", path.display());
    }
}
