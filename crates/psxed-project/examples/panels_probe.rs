//! The bake eyes of a project's panels, and how long baking them takes.
//!
//! Usage: `panels_probe <project.ron> [spacing sectors] [bake]`

use std::path::{Path, PathBuf};

use psxed_project::playtest::{bake_panels_for_tools, cook_for_cards, panel_eyes};
use psxed_project::ProjectDocument;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = Path::new(args.first().expect("a project path"));
    let spacing: i32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(10);
    let text = std::fs::read_to_string(path).expect("read the project");
    let project = ProjectDocument::from_ron_str(&text).expect("parse the project");
    let root = path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let scene = project.active_scene();
    let world = scene.world.grid().expect("a grid world");
    let inputs = cook_for_cards(&project, world, &root).expect("cook the level");
    let started = std::time::Instant::now();
    let mut scaled = project.clone();
    psxed_project::units::scale_project_to_engine_units(&mut scaled);
    let scaled_world = scaled.active_scene().world.grid().expect("a grid world");
    let eyes = panel_eyes(&scaled, scaled_world, &inputs.cooked, spacing);
    println!(
        "{} eyes at {spacing} sectors in {:.2} s (sector {})",
        eyes.len(),
        started.elapsed().as_secs_f64(),
        inputs.cooked.level.sector_units
    );
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for e in &eyes {
        for k in 0..3 {
            lo[k] = lo[k].min(e.eye[k]);
            hi[k] = hi[k].max(e.eye[k]);
        }
    }
    println!(
        "eyes span x {:?} y {:?} z {:?}",
        (lo[0], hi[0]),
        (lo[1], hi[1]),
        (lo[2], hi[2])
    );
    for e in eyes.iter().take(12) {
        println!("room {:3} at {:?}", e.room, e.eye);
    }
    if args.get(2).map(String::as_str) == Some("bake") {
        let mut flood_world = scaled_world.clone();
        flood_world.stream.flood.panels = spacing as u8;
        let panels =
            bake_panels_for_tools(&scaled, &root, &flood_world, &inputs.cooked).expect("bake");
        println!(
            "baked: {} eyes, {} KiB",
            panels.blocks.len(),
            panels.blocks.iter().map(Vec::len).sum::<usize>() / 1024
        );
        let which: Vec<usize> = args[3..].iter().filter_map(|a| a.parse().ok()).collect();
        for &eye in &which {
            let block = &panels.blocks[eye];
            println!(
                "eye {eye} at {:?}: {} sectors",
                panels.eyes[eye],
                block.len() / 2048
            );
            for h in 0..8 {
                let cols = &block[h * 5..h * 5 + 5];
                let tiles: u32 = cols.iter().map(|c| c.count_ones()).sum();
                println!("  heading {h}: {tiles} tiles, columns {cols:?}");
            }
        }
        let tiles: Vec<u32> = panels
            .blocks
            .iter()
            .map(|b| {
                if b.is_empty() {
                    0
                } else {
                    (0..40).map(|k| b[k].count_ones()).sum()
                }
            })
            .collect();
        println!("tiles per eye: {tiles:?}");
    }
}
