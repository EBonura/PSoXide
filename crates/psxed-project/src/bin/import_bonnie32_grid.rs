//! Import a bonnie-32 level as a grid world project.
//!
//! ```text
//! import-bonnie32-grid <level> <texture-packs-dir> <out-dir>
//!     [--base DIR] [--name NAME] [--scale NUM/DEN] [--spawn [ROOM:]X,Z]
//!     [--yaw DEG] [--enemies N]
//! ```
//!
//! `<level>` is a bonnie-32 `.ron` file, Brotli compressed (as bonnie saves
//! it) or plain. `<texture-packs-dir>` holds one folder per texture pack
//! (`bonnie-32/assets/samples/texture-packs`). The output project is based on
//! `--base` (default `editor/projects/graybox-reach`, which carries the
//! current Cortex character) and links its `assets`; put `<out-dir>` beside
//! the base so shared `../default/...` asset paths resolve.

use std::path::PathBuf;

use psxed_project::grid::bonnie_project::{import_project, ImportOptions, SpawnRequest};
use psxed_project::grid::GridScale;

fn usage() -> ! {
    eprintln!(
        "usage: import-bonnie32-grid <level> <texture-packs-dir> <out-dir> [--base DIR] [--name NAME] \
         [--scale NUM/DEN] [--spawn [ROOM:]X,Z] [--yaw DEG] [--enemies N]"
    );
    std::process::exit(2);
}

fn parse_spawn(text: &str) -> Option<SpawnRequest> {
    let (room, cell) = match text.split_once(':') {
        Some((room, cell)) => (room.parse().ok()?, cell),
        None => (0, text),
    };
    let (x, z) = cell.split_once(',')?;
    Some(SpawnRequest {
        room,
        cell: [x.parse().ok()?, z.parse().ok()?],
    })
}

fn main() {
    let mut positional = Vec::new();
    let mut base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/graybox-reach");
    let mut name = None;
    let mut scale = None;
    let mut spawn = None;
    let mut yaw = 0.0f32;
    let mut enemies = 2usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--base" => base = PathBuf::from(value()),
            "--name" => name = Some(value()),
            "--scale" => {
                let text = value();
                let (n, d) = text.split_once('/').unwrap_or_else(|| usage());
                scale = Some(
                    GridScale::new(
                        n.parse().unwrap_or_else(|_| usage()),
                        d.parse().unwrap_or_else(|_| usage()),
                    )
                    .unwrap_or_else(|| usage()),
                );
            }
            "--spawn" => spawn = Some(parse_spawn(&value()).unwrap_or_else(|| usage())),
            "--yaw" => yaw = value().parse().unwrap_or_else(|_| usage()),
            "--enemies" => enemies = value().parse().unwrap_or_else(|_| usage()),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let [level, packs, out]: [PathBuf; 3] = positional.try_into().unwrap_or_else(|_| usage());
    let bytes = std::fs::read(&level).unwrap_or_else(|e| {
        eprintln!("{}: {e}", level.display());
        std::process::exit(1);
    });
    let name = name.unwrap_or_else(|| {
        format!(
            "grid {}",
            level
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        )
    });
    if !out.parent().is_some_and(|p| p.join("default").is_dir()) {
        eprintln!(
            "warning: {} has no sibling `default` project; assets that point at ../default will not resolve",
            out.display()
        );
    }
    let options = ImportOptions {
        base_dir: base,
        packs_dir: packs,
        out_dir: out,
        name,
        scale,
        spawn,
        yaw_degrees: yaw,
        enemies,
    };
    match import_project(&bytes, &options) {
        Ok(report) => {
            let s = &report.stats;
            println!(
                "{} floors, {} ceilings, {} walls -> {} faces; scale {}/{}; {} materials",
                s.floors,
                s.ceilings,
                s.walls,
                report.faces,
                report.scale.num,
                report.scale.den,
                report.materials
            );
            println!(
                "lights: {} from the level, {} generated; spawn at {:?}",
                s.level_lights, s.generated_lights, report.spawn
            );
            if s.custom_uv_faces + s.off_lattice_heights + s.unresolved_objects > 0 {
                println!(
                    "dropped: {} custom-UV faces, {} off-lattice heights, {} asset objects",
                    s.custom_uv_faces, s.off_lattice_heights, s.unresolved_objects
                );
            }
            for texture in &report.missing_textures {
                println!("missing texture: {}/{}", texture.pack, texture.name);
            }
            for texture in &report.substituted_textures {
                println!("substituted texture: {}/{}", texture.pack, texture.name);
            }
            for issue in &report.warnings {
                println!("warning: {}", issue.describe());
            }
            println!("wrote {}", report.project_path.display());
        }
        Err(error) => {
            eprintln!("import failed: {error}");
            std::process::exit(1);
        }
    }
}
