//! Build the style-test project: one small canal scene, to judge an art
//! direction from real in-game frames.
//!
//! ```text
//! cargo run -p psxed-project --example style_test -- \
//!     --base <project dir> --out <new project dir> [--daylight] [--perch] [--spawn x,z[,yaw]]
//! cargo run -p psxed-project --example style_test -- --check <project dir>
//! ```
//!
//! `--out` must not exist. The level is a pure function of the code, the
//! atmosphere (twilight by default, `--daylight` for the study's palette) and
//! the spawn. Textures are embedded from `examples/style_test_assets/*.psxt`
//! and bound by name; replace a file to restyle the scene.

use std::path::PathBuf;

use psxed_project::grid::style_test::{create_project, Atmosphere, TEXTURE_NAMES};
use psxed_project::grid::walk::{analyse, Spawn};
use psxed_project::grid::GridBudget;
use psxed_project::ProjectDocument;

macro_rules! tex {
    ($n:literal) => {
        include_bytes!(concat!("style_test_assets/", $n, ".psxt")).as_slice()
    };
}

fn bytes_of(name: &str) -> &'static [u8] {
    match name {
        "megablock_plain_q0" => tex!("megablock_plain_q0"),
        "megablock_plain_q1" => tex!("megablock_plain_q1"),
        "megablock_plain_q2" => tex!("megablock_plain_q2"),
        "megablock_plain_q3" => tex!("megablock_plain_q3"),
        "megablock_q0" => tex!("megablock_q0"),
        "megablock_q1" => tex!("megablock_q1"),
        "megablock_q2" => tex!("megablock_q2"),
        "megablock_q3" => tex!("megablock_q3"),
        "iron_panel" => tex!("iron_panel"),
        "iron_door" => tex!("iron_door"),
        "arch_teal" => tex!("arch_teal"),
        "lamp_bright" => tex!("lamp_bright"),
        "sand" => tex!("sand"),
        "canal_calm" => tex!("canal_calm"),
        "rubble" => tex!("rubble"),
        "mud" => tex!("mud"),
        "cap" => tex!("cap"),
        "lever" => tex!("lever"),
        other => panic!("no texture {other}"),
    }
}

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

fn check(dir: &std::path::Path, spawn: Option<(usize, [i32; 2])>) {
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
        println!("  room {i:>2} {:>16}: {:>5} faces", r.name, faces[i]);
    }
    if arg_flag("--map") {
        // Floor height of the canal in hundreds of grid units, north at the top.
        let canal = world.rooms.iter().position(|r| r.name == "Canal").unwrap();
        let r = &world.rooms[canal];
        for z in -5..=27 {
            let mut line = format!("{z:>3} ");
            for x in 0..=11 {
                let h = r
                    .local([x, z])
                    .and_then(|[lx, lz]| r.sector(lx, lz))
                    .and_then(|s| s.ground_floor())
                    .map(|f| f.heights.into_iter().max().unwrap_or(0) / 100);
                line += &match h {
                    Some(h) => format!("{h:>3}"),
                    None => "  .".to_string(),
                };
            }
            println!("{line}");
        }
    }
    if let Some((room, cell)) = spawn {
        if let Some(a) = analyse(world, &[Spawn { room, cell }], &psx_engine::WalkLimits::DEFAULT) {
            println!(
                "walk counts {:?} (steep, low, step, drop, unreachable)",
                a.counts()
            );
        }
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
}

fn main() {
    if let Some(dir) = arg("--check") {
        check(&dir, None);
        return;
    }
    let usage = "usage: style_test --base <project dir> --out <new dir> [--daylight] [--spawn x,z[,yaw]] | --check <dir>";
    let base = arg("--base").expect(usage);
    let out = arg("--out").expect(usage);
    assert!(!out.exists(), "{} exists: pick a new folder", out.display());
    let daylight = arg_flag("--daylight");
    // Default: the forecourt, facing north at the gate. node yaw 0 faces north (-Z), 180 south.
    let spawn = arg("--spawn")
        .and_then(|v| {
            let v = v.to_string_lossy().into_owned();
            let mut it = v.split(',');
            let x = it.next()?.parse().ok()?;
            let z = it.next()?.parse().ok()?;
            let yaw = it.next().and_then(|y| y.parse().ok()).unwrap_or(0.0);
            Some(([x, z], yaw))
        })
        .unwrap_or(([5, 14], 0.0));
    let textures: Vec<(&str, &[u8])> = TEXTURE_NAMES
        .iter()
        .map(|n| (*n, bytes_of(n)))
        .collect();
    let atm = if daylight {
        Atmosphere::Daylight
    } else {
        Atmosphere::Twilight
    };
    let (path, plan) =
        create_project(&base, &out, "Style test", atm, spawn, arg_flag("--perch"), &textures).expect("the scene");
    println!("wrote {}", path.display());
    for n in &plan.notes {
        println!("note: {n}");
    }
    check(&out, Some((plan.spawn_room, plan.spawn)));
}
