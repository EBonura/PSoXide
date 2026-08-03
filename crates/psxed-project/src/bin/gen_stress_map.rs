//! Generate synthetic stress-test projects for the streaming/culling
//! campaign (docs/perf-30fps.md). Each pattern isolates one engine axis so
//! headless benches can measure it without hand-authoring content:
//!
//! - `corridor`: a W x L straight run with portal seams every K sectors and
//!   periodic door-frame cross walls. Constant forward motion crosses one
//!   seam after another, exercising room streaming, prefetch and eviction.
//!   Pairs with `launch --hold-forward`.
//! - `field`: a W x W open plaza cut into a portal lattice. Many rooms are
//!   visible at once, exercising PVS breadth and cell-select cost.
//!
//! The generated project reuses the starter project's resources (materials,
//! player character, UI scenes, boot flow) so it cooks with the standard
//! pipeline; only the room geometry and portals are synthetic. Boot goes
//! straight into gameplay so input tapes and --hold-forward line up from
//! frame 0.
//!
//! Usage (from editor/):
//!   cargo run --release -p psxed-project --bin gen-stress-map -- \
//!     corridor --out projects/stress_corridor \
//!     [--length 240] [--width 6] [--portal-every 16] [--doors-every 8]
//!   cargo run --release -p psxed-project --bin gen-stress-map -- \
//!     field --out projects/stress_field [--size 64] [--portal-every 16]

use psxed_project::{
    BootTarget, GridHorizontalFace, GridSector, GridVerticalFace, NodeId, NodeKind,
    ProjectDocument, WorldGrid,
};

const WALL_TOP: i32 = 6144;

struct Args {
    pattern: String,
    out: String,
    length: u16,
    width: u16,
    size: u16,
    portal_every: u16,
    doors_every: u16,
    ceiling: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1);
    let pattern = argv.next().ok_or("missing pattern (corridor|field)")?;
    let mut args = Args {
        pattern,
        out: String::new(),
        length: 240,
        width: 6,
        size: 64,
        portal_every: 16,
        doors_every: 8,
        ceiling: true,
    };
    while let Some(flag) = argv.next() {
        let mut value = || argv.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--out" => args.out = value()?,
            "--length" => args.length = value()?.parse().map_err(|e| format!("{e}"))?,
            "--width" => args.width = value()?.parse().map_err(|e| format!("{e}"))?,
            "--size" => args.size = value()?.parse().map_err(|e| format!("{e}"))?,
            "--portal-every" => args.portal_every = value()?.parse().map_err(|e| format!("{e}"))?,
            "--doors-every" => args.doors_every = value()?.parse().map_err(|e| format!("{e}"))?,
            "--no-ceiling" => args.ceiling = false,
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.out.is_empty() {
        return Err("--out is required".into());
    }
    Ok(args)
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("gen-stress-map: {err}");
            std::process::exit(2);
        }
    };
    if let Err(err) = run(&args) {
        eprintln!("gen-stress-map: {err}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let mut project = ProjectDocument::starter();
    project.name = format!("stress {}", args.pattern);

    // Reuse the starter room's authored materials so the synthetic grid
    // cooks against real textures.
    let (floor_mat, wall_mat, ceiling_mat, sector_size) = harvest_template(&project)?;

    let grid = match args.pattern.as_str() {
        "corridor" => corridor_grid(args, sector_size, floor_mat, wall_mat, ceiling_mat),
        "field" => field_grid(args, sector_size, floor_mat, wall_mat),
        other => return Err(format!("unknown pattern {other} (corridor|field)")),
    };

    let player_character = project
        .resources
        .iter()
        .find(|r| {
            matches!(r.data, psxed_project::ResourceData::Character(_))
                && r.name.to_lowercase().contains("player")
        })
        .or_else(|| {
            project
                .resources
                .iter()
                .find(|r| matches!(r.data, psxed_project::ResourceData::Character(_)))
        })
        .map(|r| r.id);

    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .ok_or("starter has no Room node")?;

    // Drop the starter room's demo contents; keep only the player spawn,
    // which is re-positioned onto the synthetic grid below.
    let doomed: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|n| {
            n.id != room_id
                && scene.is_descendant_of(n.id, room_id)
                && !matches!(n.kind, NodeKind::SpawnPoint { .. })
        })
        .map(|n| n.id)
        .collect();
    for id in doomed {
        scene.remove_node(id);
    }

    // Portal seams and the spawn are positioned in editor cell space
    // (sector units, grid-centre relative).
    let seams: Vec<(f32, f32, String)> = match args.pattern.as_str() {
        // Marker X sits at a sector CENTRE (+0.5) so the tiny +0.05 Z
        // offset is unambiguously the nearest edge; an on-boundary X
        // would tie-break to the west edge and cut lengthwise.
        "corridor" => (1..(args.length / args.portal_every))
            .map(|k| {
                (
                    (args.width / 2) as f32 + 0.5,
                    (k * args.portal_every) as f32 + 0.05,
                    format!("seam_z{}", k * args.portal_every),
                )
            })
            .collect(),
        _ => {
            let mut seams = Vec::new();
            let centre = (args.size / 2) as f32 + 0.5;
            for k in 1..(args.size / args.portal_every) {
                let cut = (k * args.portal_every) as f32 + 0.05;
                seams.push((centre, cut, format!("lat_z{k}")));
                seams.push((cut, centre, format!("lat_x{k}")));
            }
            seams
        }
    };
    // Spawn at the far +Z end facing -Z (the cook consumes the spawn
    // node's Y rotation now, playtest.rs yaw_from_degrees) so
    // --hold-forward walks the full run.
    let spawn_world = match args.pattern.as_str() {
        "corridor" => [(args.width / 2) as f32 + 0.5, args.length as f32 - 1.5],
        _ => [(args.size / 2) as f32 + 0.5, args.size as f32 - 1.5],
    };

    let to_editor = |world: [f32; 2]| grid.world_cells_to_editor(world);
    for (wx, wz, name) in &seams {
        let editor = to_editor([*wx, *wz]);
        let id = scene.add_node(
            room_id,
            name.clone(),
            NodeKind::Portal {
                target_room: None,
                target_entry: String::new(),
                entry_name: name.clone(),
                geometry: None,
            },
        );
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation = [editor[0], 0.0, editor[1]];
        }
    }

    let spawn_editor = to_editor(spawn_world);
    let spawn_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::SpawnPoint { player: true, .. }))
        .map(|n| n.id);
    let spawn_id = spawn_id.unwrap_or_else(|| {
        scene.add_node(
            room_id,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        )
    });
    if let Some(node) = scene.node_mut(spawn_id) {
        node.transform.translation = [spawn_editor[0], 0.0, spawn_editor[1]];
        // Yaw 0 faces -Z; the spawn sits at the far +Z end, so face the
        // player straight down the corridor for --hold-forward.
        node.transform.rotation_degrees = [0.0, 0.0, 0.0];
        // The starter defines several Characters; the cook refuses to
        // auto-pick, so wire the player profile explicitly.
        if let NodeKind::SpawnPoint { character, .. } = &mut node.kind {
            *character = player_character;
        }
    }

    if let Some(node) = scene.node_mut(room_id) {
        node.kind = NodeKind::Section { grid };
    }

    // Boot straight into gameplay so --hold-forward and tapes drive from
    // frame 0 (the menu flow desyncs headless input, docs/perf-30fps.md).
    project.boot = BootTarget::Gameplay;

    let out_root = std::path::Path::new(&args.out);
    std::fs::create_dir_all(out_root).map_err(|e| e.to_string())?;
    copy_assets(
        std::path::Path::new("projects/default/assets"),
        &out_root.join("assets"),
    )?;
    project
        .save_to_path(out_root.join("project.ron"))
        .map_err(|e| format!("{e:?}"))?;
    println!(
        "wrote {} ({} pattern, {} portal seams)",
        out_root.join("project.ron").display(),
        args.pattern,
        seams.len(),
    );
    Ok(())
}

/// Pull the starter room's floor/wall/ceiling materials and sector size so
/// the synthetic grid cooks against known-good resources.
/// The material, model and light resource ids harvested from a template
/// scene, plus its floor height.
type HarvestedTemplate = (
    Option<psxed_project::ResourceId>,
    Option<psxed_project::ResourceId>,
    Option<psxed_project::ResourceId>,
    i32,
);

fn harvest_template(project: &ProjectDocument) -> Result<HarvestedTemplate, String> {
    let scene = project.active_scene();
    let grid = scene
        .nodes()
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Section { grid } => Some(grid),
            _ => None,
        })
        .ok_or("starter has no Room node")?;
    let mut floor_mat = None;
    let mut wall_mat = None;
    let mut ceiling_mat = None;
    for sector in grid.sectors.iter().flatten() {
        if floor_mat.is_none() {
            floor_mat = sector.floor.as_ref().and_then(|f| f.material);
        }
        if ceiling_mat.is_none() {
            ceiling_mat = sector.ceiling.as_ref().and_then(|f| f.material);
        }
        if wall_mat.is_none() {
            wall_mat = [
                &sector.walls.north,
                &sector.walls.east,
                &sector.walls.south,
                &sector.walls.west,
            ]
            .into_iter()
            .flat_map(|faces| faces.iter())
            .find_map(|face| face.material);
        }
    }
    let wall_mat = wall_mat.or(floor_mat);
    let ceiling_mat = ceiling_mat.or(floor_mat);
    Ok((floor_mat, wall_mat, ceiling_mat, grid.sector_size))
}

fn corridor_grid(
    args: &Args,
    sector_size: i32,
    floor_mat: Option<psxed_project::ResourceId>,
    wall_mat: Option<psxed_project::ResourceId>,
    ceiling_mat: Option<psxed_project::ResourceId>,
) -> WorldGrid {
    let (w, l) = (args.width, args.length);
    let mut grid = WorldGrid::empty(w, l, sector_size);
    for x in 0..w {
        for z in 0..l {
            let mut sector = GridSector::with_floor(0, floor_mat);
            if args.ceiling {
                sector.ceiling = Some(GridHorizontalFace::flat(WALL_TOP, ceiling_mat));
            }
            if x == 0 {
                sector
                    .walls
                    .west
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if x == w - 1 {
                sector
                    .walls
                    .east
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if z == 0 {
                sector
                    .walls
                    .south
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if z == l - 1 {
                sector
                    .walls
                    .north
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            // Door frames: periodic cross walls with a two-sector gap in the
            // middle. Adds triangle load and occlusion without blocking the
            // hold-forward run line.
            if args.doors_every > 0
                && z % args.doors_every == 0
                && z != 0
                && (x < w / 2 - 1 || x > w / 2)
            {
                sector
                    .walls
                    .south
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            let index = x as usize * l as usize + z as usize;
            grid.sectors[index] = Some(sector);
        }
    }
    grid
}

fn field_grid(
    args: &Args,
    sector_size: i32,
    floor_mat: Option<psxed_project::ResourceId>,
    wall_mat: Option<psxed_project::ResourceId>,
) -> WorldGrid {
    let s = args.size;
    let mut grid = WorldGrid::empty(s, s, sector_size);
    for x in 0..s {
        for z in 0..s {
            let mut sector = GridSector::with_floor(0, floor_mat);
            if x == 0 {
                sector
                    .walls
                    .west
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if x == s - 1 {
                sector
                    .walls
                    .east
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if z == 0 {
                sector
                    .walls
                    .south
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            if z == s - 1 {
                sector
                    .walls
                    .north
                    .push(GridVerticalFace::flat(0, WALL_TOP, wall_mat));
            }
            let index = x as usize * s as usize + z as usize;
            grid.sectors[index] = Some(sector);
        }
    }
    grid
}

fn copy_assets(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    if !from.exists() {
        return Err(format!(
            "template assets not found at {} (run from editor/)",
            from.display()
        ));
    }
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_assets(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
