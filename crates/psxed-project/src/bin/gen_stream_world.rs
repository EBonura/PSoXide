//! Generate the world-streaming stress project (M8 generator).
//!
//! Usage:
//!   gen-stream-world [CONFIG.ron] [OUTPUT_DIR] [--dense]
//!
//! Defaults: config `editor/projects/stream-world.config.ron` (the built-in
//! default when absent), output `editor/projects/stream-world`. The output is
//! reproducible and untracked; regenerate instead of committing it.

use std::path::PathBuf;

use psxed_project::stream_world::{generate, load_donor, write_world, StreamWorldConfig};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let projects = manifest.join("../../projects");
    let mut positional = Vec::new();
    let mut dense = false;
    for arg in std::env::args().skip(1) {
        if arg == "--dense" {
            dense = true;
        } else {
            positional.push(arg);
        }
    }
    let config_path = positional
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| projects.join("stream-world.config.ron"));
    let out_dir = positional.get(1).map(PathBuf::from).unwrap_or_else(|| {
        projects.join(if dense {
            "stream-world-dense"
        } else {
            "stream-world"
        })
    });
    let mut config = match std::fs::read_to_string(&config_path) {
        Ok(text) => StreamWorldConfig::from_ron_str(&text).unwrap_or_else(|e| panic!("{e}")),
        Err(_) => StreamWorldConfig::default(),
    };
    if dense {
        config = config.dense_variant();
    }
    let donor = load_donor(&projects).unwrap_or_else(|e| panic!("donor: {e}"));
    let world = generate(&config, &donor).unwrap_or_else(|e| panic!("generate: {e}"));
    write_world(&world, &out_dir, &projects.join("graybox-reach"))
        .unwrap_or_else(|e| panic!("write: {e}"));
    let s = &world.stats;
    println!(
        "[gen-stream-world] wrote {}: {} modules ({} rooms, {} corridors, {} interiors, {} courtyards, {} terrain), {} door edges (degree <= {}, {} overflows), {} brushes, {} enemies, {} hooks, {} routes",
        out_dir.display(), s.modules, s.rooms, s.corridors, s.interiors, s.courtyards, s.terrains,
        s.edges, s.max_door_degree, s.degree_overflows, s.brushes, s.enemies, s.hooks, world.routes.routes.len(),
    );
    println!(
        "[gen-stream-world] modules estimate {} to {} B in the cut model (target {} B), {} B in all; {} over budget, {} trimmed",
        s.min_module_bytes, s.max_module_bytes, config.region_target_bytes, s.estimated_bytes, s.over_budget_modules, s.trimmed_modules,
    );
    println!(
        "[gen-stream-world] {}",
        world
            .plan
            .describe(config.region_target_bytes)
            .replace('\n', "\n[gen-stream-world] ")
    );
}
