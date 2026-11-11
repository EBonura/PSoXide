//! Build the showcase project: one interconnected, navigable place made only
//! from the level verbs, to show what the engine and the editor can build.
//!
//! ```text
//! cargo run -p psxed-project --example showcase -- \
//!     --base <project dir> --out <new project dir>
//! cargo run -p psxed-project --example showcase -- --check <project dir>
//! ```
//!
//! `--base` is a project that carries a player, two enemies (a light and a
//! heavy one), a camera, sky and UI: the editor repository's
//! `projects/default`. `--out` must not exist; the folder is made beside the
//! base so the shared `assets` link resolves. The level is a pure function of
//! the code: the same build gives the same project, byte for byte. The 64x64
//! 16 colour textures are embedded from `examples/showcase_assets/*.psxt`
//! (made by an art script outside the repository) and written to
//! `<out>/textures/showcase/`.
//!
//! `--enemy` keeps the base's one enemy. It costs about 440 KB of the guest's
//! RAM (model, clips, arenas), which the full level cannot spare yet, so it is
//! off by default.
//!
//! `--check` cooks a written project the way Play does and prints the budget
//! and the walkability report.

use std::path::PathBuf;

use psxed_project::grid::showcase::create_project;
use psxed_project::grid::walk::{analyse, Spawn};
use psxed_project::grid::{GridBudget, SECTOR_UNITS};
use psxed_project::ProjectDocument;

const TEXTURES: &[(&str, &[u8])] = &[
    (
        "block_big",
        include_bytes!("showcase_assets/block_big.psxt"),
    ),
    (
        "block_mid",
        include_bytes!("showcase_assets/block_mid.psxt"),
    ),
    (
        "block_small",
        include_bytes!("showcase_assets/block_small.psxt"),
    ),
    (
        "block_dark",
        include_bytes!("showcase_assets/block_dark.psxt"),
    ),
    (
        "block_pale",
        include_bytes!("showcase_assets/block_pale.psxt"),
    ),
    ("voussoir", include_bytes!("showcase_assets/voussoir.psxt")),
    (
        "trim_course",
        include_bytes!("showcase_assets/trim_course.psxt"),
    ),
    (
        "dome_stone",
        include_bytes!("showcase_assets/dome_stone.psxt"),
    ),
    (
        "floor_stone",
        include_bytes!("showcase_assets/floor_stone.psxt"),
    ),
    ("sand", include_bytes!("showcase_assets/sand.psxt")),
    (
        "sand_stone",
        include_bytes!("showcase_assets/sand_stone.psxt"),
    ),
    ("rock", include_bytes!("showcase_assets/rock.psxt")),
    ("rubble", include_bytes!("showcase_assets/rubble.psxt")),
    (
        "step_stone",
        include_bytes!("showcase_assets/step_stone.psxt"),
    ),
    (
        "iron_plate",
        include_bytes!("showcase_assets/iron_plate.psxt"),
    ),
    ("grating", include_bytes!("showcase_assets/grating.psxt")),
    ("girder", include_bytes!("showcase_assets/girder.psxt")),
    ("pipes", include_bytes!("showcase_assets/pipes.psxt")),
    (
        "rust_panel",
        include_bytes!("showcase_assets/rust_panel.psxt"),
    ),
    ("hatch", include_bytes!("showcase_assets/hatch.psxt")),
    (
        "lamp_window",
        include_bytes!("showcase_assets/lamp_window.psxt"),
    ),
    ("cable", include_bytes!("showcase_assets/cable.psxt")),
    (
        "step_metal",
        include_bytes!("showcase_assets/step_metal.psxt"),
    ),
    ("pipes_v", include_bytes!("showcase_assets/pipes_v.psxt")),
    (
        "mega_face",
        include_bytes!("showcase_assets/mega_face.psxt"),
    ),
    (
        "mega_face_pale",
        include_bytes!("showcase_assets/mega_face_pale.psxt"),
    ),
];

fn arg_flag(name: &str) -> bool {
    std::env::args().skip(1).any(|a| a == name)
}

fn arg(name: &str) -> Option<PathBuf> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next().map(PathBuf::from);
        }
    }
    None
}

fn check(dir: &std::path::Path) {
    let project = ProjectDocument::load_from_path(dir.join("project.ron")).expect("it loads");
    let world = project.active_scene().world.grid().expect("a grid world");
    let budget = GridBudget::of(world);
    println!(
        "rooms {}, faces {}, largest room {} faces (cap {}), leaves {}, meshes {}",
        budget.rooms,
        budget.faces,
        budget.largest_room_faces,
        psxed_project::grid::limits::MAX_ROOM_FACES,
        budget.leaves,
        budget.meshes
    );
    let faces = psxed_project::grid::faces::room_face_counts(world);
    for (i, r) in world.rooms.iter().enumerate() {
        println!("  room {i:>2} {:>22}: {:>5} faces", r.name, faces[i]);
    }
    let spawn = Spawn {
        room: 0,
        cell: [2, 6],
    };
    if let Some(a) = analyse(world, &[spawn], &psx_engine::WalkLimits::DEFAULT) {
        println!(
            "walk counts {:?} (steep, low, step, drop, unreachable)",
            a.counts()
        );
    }
    let (package, report) = psxed_project::playtest::build_package(&project, dir);
    println!("cook ok: {}", report.is_ok());
    for e in report.error_messages() {
        println!("  error: {e}");
    }
    for w in &report.warnings {
        println!("  warning: {w}");
    }
    if let Some(p) = package {
        let g = &p.world_geometry;
        println!(
            "cooked: faces {}, triangles {}, busiest room {} surfaces, pool {:?}",
            g.faces, g.triangles, g.max_room_surfaces, g.pool
        );
    }
    let _ = SECTOR_UNITS;
}

fn main() {
    if let Some(dir) = arg("--check") {
        check(&dir);
        return;
    }
    let usage = "usage: showcase --base <project dir> --out <new dir> | --check <dir>";
    let base = arg("--base").expect(usage);
    let out = arg("--out").expect(usage);
    assert!(!out.exists(), "{} exists: pick a new folder", out.display());
    // `--spawn x,z[,yaw]` starts the player on another sector (for testing a
    // part); the yaw is the node's, 270 faces east, 90 west, 0 south, 180 north.
    let spawn = arg("--spawn").and_then(|v| {
        let v = v.to_string_lossy().into_owned();
        let mut it = v.split(',');
        let x = it.next()?.parse().ok()?;
        let z = it.next()?.parse().ok()?;
        let yaw = it.next().and_then(|y| y.parse().ok()).unwrap_or(270.0);
        Some(([x, z], yaw))
    });
    let (path, plan) = create_project(
        &base,
        &out,
        "Showcase",
        usize::from(arg_flag("--enemy")),
        spawn,
        TEXTURES,
    )
    .expect("the showcase");
    println!("wrote {}", path.display());
    for n in &plan.notes {
        println!("note: {n}");
    }
    check(&out);
}
