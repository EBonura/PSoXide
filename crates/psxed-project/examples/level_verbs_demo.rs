//! Build the level-verbs demo project: a courtyard of seeded terrain with
//! five things standing in it, each made only from the level verbs.
//!
//! ```text
//! cargo run -p psxed-project --example level_verbs_demo -- \
//!     --base <project dir> --out <new project dir>
//! ```
//!
//! `--base` is the project the starter borrows its player, camera, sky and
//! assets from (the editor repo's `projects/grid-starter`, or any project
//! File > New Grid Project can use); `--out` must not exist. The project is
//! made through the same starter code path as File > New Grid Project and then
//! given this world.
//!
//! ```text
//!  spawn (courtyard, west)      x 0..55, z 0..33 sectors
//!  (a) intact cathedral, arched west door and windows      x  8..20, z  2..9
//!  (c) three storey building, arched windows, stair, roof  x 30..37, z  2..9
//!  (d) octagonal tower, bridge to the building's east door x 46..53, z  2..9
//!  (b) the same cathedral through Ruin, medium intensity   x  8..20, z 16..23
//!  (e) corridor with stairs up to a hall with an arcade    x 28..51, z 16..24
//! ```

use std::collections::BTreeSet;
use std::path::PathBuf;

use psxed_project::grid::edit::verbs::arcade::{build_arcade, ArcadeParams};
use psxed_project::grid::edit::verbs::bridge::{build_bridge, BridgeParams};
use psxed_project::grid::edit::verbs::building::{build_building, BuildingParams};
use psxed_project::grid::edit::verbs::cathedral::{build_cathedral, CathedralParams};
use psxed_project::grid::edit::verbs::corridor::{
    lay_corridor, CorridorParams, LegShape, Waypoint,
};
use psxed_project::grid::edit::verbs::opening::{
    arch_head, cut_opening, ArchShape, OpeningKind, OpeningParams, OpeningProfile,
};
use psxed_project::grid::edit::verbs::room::{lay_room, Footprint, RoomParams, RoomTarget};
use psxed_project::grid::edit::verbs::ruin::{ruin_region, RuinParams};
use psxed_project::grid::edit::verbs::style::{self, Roles};
use psxed_project::grid::edit::verbs::styled::{
    lay_styled_corridor, lay_styled_room, StyledCorridor, StyledRoom,
};
use psxed_project::grid::edit::verbs::terrain::{lay_terrain, Edge, TerrainParams};
use psxed_project::grid::edit::CellRect;
use psxed_project::grid::starter::{create_project, starter_world};
use psxed_project::grid::walk::{analyse, Spawn};
use psxed_project::grid::{GridBudget, GridRoom, GridWorld, SceneWorld, WallEdge, SECTOR_UNITS};
use psxed_project::{NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId};

const COURTYARD: [[i32; 2]; 2] = [[0, 0], [55, 33]];
const SPAWN: [i32; 2] = [3, 12];

fn material(project: &ProjectDocument, name: &str) -> Option<ResourceId> {
    project
        .resources
        .iter()
        .find(|r| r.name == name && matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.id)
}

fn rect(a: [i32; 2], b: [i32; 2]) -> CellRect {
    CellRect::from_corners(a, b)
}

/// Every world sector a room's leaves touch.
fn occupied(room: &GridRoom) -> BTreeSet<[i32; 2]> {
    let mut out = BTreeSet::new();
    for (key, _) in room.leaves() {
        let (lo, hi) = key.sector_range();
        out.extend(rect(lo, hi).cells());
    }
    out
}

/// The sector a body leaves a door into.
fn outside_cell(door: &psxed_project::grid::edit::verbs::opening::DoorPlane) -> [i32; 2] {
    let x = door.center[0] + door.through[0] * SECTOR_UNITS / 2;
    let z = door.center[2] + door.through[1] * SECTOR_UNITS / 2;
    [x.div_euclid(SECTOR_UNITS), z.div_euclid(SECTOR_UNITS)]
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

/// Cook a written project the way Play does and print the verdict and the
/// numbers the caps are judged on.
fn cook_check(project: &ProjectDocument, dir: &std::path::Path) {
    let world = project.active_scene().world.grid().expect("a grid world");
    let budget = GridBudget::of(world);
    println!(
        "rooms {}, faces {}, largest room {} faces (cap {}), leaves {}, meshes {} (cap {} a room)",
        budget.rooms,
        budget.faces,
        budget.largest_room_faces,
        psxed_project::grid::limits::MAX_ROOM_FACES,
        budget.leaves,
        budget.meshes,
        psxed_project::grid::mesh::MAX_MESHES_PER_ROOM
    );
    let (package, report) = psxed_project::playtest::build_package(project, dir);
    println!("cook ok: {}", report.is_ok());
    for e in report.error_messages() {
        println!("  error: {e}");
    }
    for w in report
        .warnings
        .iter()
        .filter(|w| !w.contains("slab(s) lean"))
    {
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

/// Put the player on `spawn`, replace the starter lamp by `lamps` and save.
fn finish(
    project: &mut ProjectDocument,
    path: &std::path::Path,
    world: &GridWorld,
    spawn: [i32; 2],
    lamps: &[(&str, i32, i32)],
) {
    // The player and a lamp at each piece.
    let scene = project.active_scene_mut();
    let to_editor = |gu: i32| world.scale.to_editor(gu) as f32;
    let player = scene
        .nodes()
        .iter()
        .find(|n| {
            n.children.iter().any(|c| {
                scene.node(*c).is_some_and(|k| {
                    matches!(&k.kind, NodeKind::CharacterController { player: true, .. })
                })
            })
        })
        .map(|n| n.id)
        .expect("a player entity");
    let half = SECTOR_UNITS / 2;
    if let Some(node) = scene.node_mut(player) {
        node.transform.translation[0] = to_editor(spawn[0] * SECTOR_UNITS + half);
        node.transform.translation[2] = to_editor(spawn[1] * SECTOR_UNITS + half);
        node.transform.rotation_degrees = [0.0, 90.0, 0.0];
    }
    let old: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::PointLight { .. }))
        .map(|n| n.id)
        .collect();
    for id in old {
        scene.remove_node(id);
    }
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(1024)
        .max(1) as f32;
    for &(name, x, z) in lamps {
        let lamp = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::PointLight {
                color: [255, 224, 180],
                intensity: 1.4,
                radius: 5.0 * world.scale.sector_editor_units() as f32 / unit,
            },
        );
        if let Some(node) = scene.node_mut(lamp) {
            node.transform.translation = [to_editor(x), to_editor(1500), to_editor(z)];
        }
    }
    scene.world = SceneWorld::Grid(world.clone());
    project.save_to_path(path).expect("the project saves");
    println!("wrote {}", path.display());
}

/// The styles demo: one corridor per built-in style side by side, a domed
/// chamber and a cloister walk, all opening onto a flat courtyard.
fn styles_demo(mut project: ProjectDocument, path: &std::path::Path, roles: Roles) {
    let mut world: GridWorld = starter_world(roles.floor, roles.wall, roles.ceiling);
    world.rooms.clear();
    let mut yard = GridRoom::new_derived("Courtyard", 1, 1);
    yard.defaults.wall_material = roles.wall;
    world.rooms.push(yard);
    let mut t = TerrainParams::new(0, rect([0, 0], [11, 45]).cells().collect(), 1);
    t.base = Some(0);
    t.create = true;
    t.amplitude_clicks = 0;
    t.follow_ceiling = false;
    t.material = roles.ground;
    lay_terrain(&mut world, &t).expect("the courtyard");
    let mut names: Vec<(usize, String)> = Vec::new();
    let mut lamps: Vec<(String, i32, i32)> = Vec::new();
    let door = |world: &mut GridWorld, room: usize, z: i32, top: i32| {
        cut_opening(
            world,
            &OpeningParams {
                profile: OpeningProfile::Rectangular,
                room,
                cell: [12, z],
                side: WallEdge::West,
                from: z * 4,
                to: (z + 2) * 4,
                kind: OpeningKind::Door,
                top,
                material: roles.trim,
            },
        )
        .expect("a door onto the courtyard");
    };
    // One corridor per style, 20 sectors long, two wide.
    for (i, s) in [
        style::plain(),
        style::scifi_rib(),
        style::gothic_nave(),
        style::cloister(),
        style::crypt(),
    ]
    .into_iter()
    .enumerate()
    {
        let z = 2 + 5 * i as i32;
        let top = (s.headroom - 256).min(2048);
        let name = s.name.clone();
        let out = lay_styled_corridor(
            &mut world,
            &StyledCorridor {
                target: RoomTarget::New(name.clone()),
                waypoints: vec![Waypoint::at_height([12, z], 0), Waypoint::at([32, z])],
                width: 2,
                base: 0,
                leg: LegShape::Ramp,
                style: s,
                roles,
            },
        )
        .expect("a styled corridor");
        door(&mut world, out.room, z, top);
        names.push((out.room, name.clone()));
        lamps.push((format!("{name} light"), 20 * 1024, (z * 1024) + 1024));
    }
    // The domed chamber on four piers.
    let dome = lay_styled_room(
        &mut world,
        &StyledRoom {
            target: RoomTarget::New("Domed chamber".into()),
            rect: rect([12, 27], [15, 30]),
            base: 0,
            style: style::domed_chamber(),
            roles,
        },
    )
    .expect("the domed chamber");
    door(&mut world, dome.room, 28, 2048);
    names.push((dome.room, "Domed chamber".into()));
    lamps.push(("Dome light".into(), 14 * 1024, 29 * 1024));
    // A cloister walk: groin vaults round a U.
    let walk = lay_styled_corridor(
        &mut world,
        &StyledCorridor {
            target: RoomTarget::New("Cloister walk".into()),
            waypoints: vec![
                Waypoint::at_height([12, 34], 0),
                Waypoint::at([28, 34]),
                Waypoint::at([28, 42]),
                Waypoint::at([12, 42]),
            ],
            width: 2,
            base: 0,
            leg: LegShape::Ramp,
            style: style::cloister(),
            roles,
        },
    )
    .expect("the cloister walk");
    door(&mut world, walk.room, 34, 2048);
    names.push((walk.room, "Cloister walk".into()));
    lamps.push(("Cloister light".into(), 20 * 1024, 35 * 1024));
    lamps.push((
        "Spawn Light".into(),
        SPAWN[0] * 1024 + 512,
        SPAWN[1] * 1024 + 512,
    ));
    let lamp_refs: Vec<(&str, i32, i32)> =
        lamps.iter().map(|(n, x, z)| (n.as_str(), *x, *z)).collect();
    finish(&mut project, path, &world, SPAWN, &lamp_refs);
    // What each style costs.
    let faces = psxed_project::grid::faces::room_face_counts(&world);
    println!("faces per room (cost of each style):");
    for (room, name) in &names {
        let r = &world.rooms[*room];
        println!(
            "  {name:>14}: {:>5} faces, {:>4} leaves, {:>2} meshes",
            faces[*room],
            r.leaf_count(),
            r.meshes.len()
        );
    }
    println!("  {:>14}: {:>5} faces", "Courtyard", faces[0]);
    cook_check(
        &project,
        path.parent()
            .map_or(path, |_| path.parent().unwrap_or(path)),
    );
}

fn main() {
    if let Some(dir) = arg("--check") {
        let project =
            ProjectDocument::load_from_path(dir.join("project.ron")).expect("the project loads");
        cook_check(&project, &dir);
        return;
    }
    let base = arg("--base").expect("usage: level_verbs_demo --base <project dir> --out <dir>");
    let out = arg("--out").expect("usage: level_verbs_demo --base <project dir> --out <dir>");
    assert!(!out.exists(), "{} exists: pick a new folder", out.display());
    let path = create_project(&base, &out, "Level verbs demo").expect("the starter is made");
    let mut project = ProjectDocument::load_from_path(&path).expect("the starter loads");

    // The six distinct Graybox materials by role (stone floor, light walls,
    // wood ceiling, dark trim, metal accent, moss ground); the near-identical
    // blue ones only as a fallback.
    let roles = Roles::graybox(&project);
    if arg_flag("--styles") {
        styles_demo(project, &path, roles);
        return;
    }
    let ground = roles
        .floor
        .or_else(|| material(&project, "Graybox / Ground 64"));
    let walls = roles
        .wall
        .or_else(|| material(&project, "Graybox / Original 64"));
    let roof = roles
        .ceiling
        .or_else(|| material(&project, "Graybox / Cliff 64"));
    // The starter's own world settings (shading, global light, scale); the
    // rooms are ours.
    let mut world: GridWorld = starter_world(ground, walls, roof);
    world.rooms.clear();
    let mut notes: Vec<String> = Vec::new();

    // Room 0 is the courtyard; the terrain goes in last, around the rest.
    let mut yard = GridRoom::new_derived("Courtyard", 1, 1);
    yard.defaults.wall_material = walls;
    world.rooms.push(yard);

    let mut doors: Vec<[i32; 2]> = Vec::new();

    // (a) The intact cathedral, pointed arches on the west door and windows.
    let mut a = CathedralParams::new(RoomTarget::New("Cathedral".into()), [8, 2]);
    a.arch = Some(ArchShape::Pointed);
    a.floor_material = ground;
    a.ceiling_material = roof;
    a.wall_material = walls;
    let ra = build_cathedral(&mut world, &a).expect("the cathedral");
    notes.extend(ra.notes.iter().cloned());
    doors.extend(ra.doors.iter().map(outside_cell));

    // (b) The same cathedral, ruined at a medium intensity along a kept path.
    let mut b = a.clone();
    b.target = RoomTarget::New("Ruined cathedral".into());
    b.origin = [8, 16];
    let rb = build_cathedral(&mut world, &b).expect("the second cathedral");
    notes.extend(rb.notes.iter().cloned());
    doors.extend(rb.doors.iter().map(outside_cell));
    let ruined_cells = occupied(&world.rooms[rb.room]);
    let axis = rb.nave.min[1] + rb.nave.depth() / 2;
    let keep: BTreeSet<[i32; 2]> = ruined_cells
        .iter()
        .copied()
        .filter(|c| c[1] == axis - 1 || c[1] == axis)
        .collect();
    let mut ruin = RuinParams::new(rb.room, ruined_cells, 7, 40);
    ruin.keep = keep;
    ruin.spawns = vec![Spawn {
        room: 0,
        cell: SPAWN,
    }];
    // Spawns need the courtyard floor, which comes later: ruin runs below.
    let ruin_later = ruin;

    // (c) A three storey building with arched windows, a stair and a roof.
    let mut c = BuildingParams::new(
        RoomTarget::New("Building".into()),
        Footprint::Rect(rect([30, 2], [37, 9])),
    );
    c.storeys = 3;
    c.door_sides = vec![WallEdge::South, WallEdge::East];
    c.window_arch = Some(ArchShape::Round);
    c.floor_material = ground;
    c.ceiling_material = roof;
    c.wall_material = walls;
    let rc = build_building(&mut world, &c).expect("the building");
    notes.extend(rc.notes.iter().cloned());
    let east = rc
        .doors
        .iter()
        .find(|d| d.through == [1, 0])
        .copied()
        .expect("an east door");
    doors.extend(
        rc.doors
            .iter()
            .filter(|d| d.through != [1, 0])
            .map(outside_cell),
    );

    // (d) An octagonal tower whose west door lines up with the building's
    // east door; the seed moves the door, so look for the one that does.
    let mut tower: Option<(GridWorld, _)> = None;
    for seed in 0..64 {
        let mut t = BuildingParams::new(
            RoomTarget::New("Tower".into()),
            Footprint::Octagon {
                rect: rect([46, 2], [53, 9]),
                cut: 2,
            },
        );
        t.storeys = 1;
        t.stair = false;
        t.roof_parapet = None;
        t.window_every = 0;
        t.door_sides = vec![WallEdge::West, WallEdge::South];
        t.seed = seed;
        t.floor_material = ground;
        t.ceiling_material = roof;
        t.wall_material = walls;
        let mut trial = world.clone();
        let Ok(rt) = build_building(&mut trial, &t) else {
            continue;
        };
        let west = rt.doors.iter().find(|d| d.through == [-1, 0]);
        if west.is_some_and(|d| d.center[2] == east.center[2]) {
            tower = Some((trial, rt));
            break;
        }
    }
    let (trial, rt) = tower.expect("a tower seed whose door meets the bridge");
    world = trial;
    let west = rt
        .doors
        .iter()
        .find(|d| d.through == [-1, 0])
        .copied()
        .unwrap();
    doors.extend(
        rt.doors
            .iter()
            .filter(|d| d.through != [-1, 0])
            .map(outside_cell),
    );
    let mut bridge = BridgeParams::deck(
        RoomTarget::New("Bridge".into()),
        [east.center[0], 0, east.center[2]],
        [west.center[0], 0, west.center[2]],
        SECTOR_UNITS,
        512,
    );
    bridge.parapet = Some(768);
    bridge.pier_spacing = Some(2 * SECTOR_UNITS);
    bridge.pier_foot = -2 * SECTOR_UNITS;
    build_bridge(&mut world, &bridge).expect("the bridge");

    // (e) A hall with an arcade, reached by a corridor with stairs.
    let mut hall = RoomParams::new(Footprint::Rect(rect([40, 16], [51, 24])), "Hall", 4096);
    hall.base = 1536;
    hall.floor_material = ground;
    hall.ceiling_material = roof;
    hall.wall_material = walls;
    let hall_room = lay_room(&mut world, &hall).expect("the hall").room;
    let mut cor = CorridorParams::new(
        RoomTarget::Existing(hall_room),
        vec![
            Waypoint::at_height([28, 20], 0),
            Waypoint::at([34, 20]),
            Waypoint::at_height([40, 20], 1536),
        ],
        3072,
    );
    cor.leg = LegShape::Stairs;
    cor.floor_material = ground;
    cor.ceiling_material = roof;
    cor.wall_material = walls;
    lay_corridor(&mut world, &cor).expect("the corridor");
    let mut arcade = ArcadeParams::line(
        hall_room,
        [42 * SECTOR_UNITS, 18 * SECTOR_UNITS],
        [50 * SECTOR_UNITS, 18 * SECTOR_UNITS],
        1536,
        4,
    );
    arcade.spring = 1536;
    build_arcade(&mut world, &arcade).expect("the arcade");
    // The corridor's west door, arched, onto the courtyard.
    let door = cut_opening(
        &mut world,
        &OpeningParams {
            profile: arch_head(ArchShape::Pointed, 4, 2560),
            room: hall_room,
            cell: [28, 20],
            side: WallEdge::West,
            from: 80,
            to: 84,
            kind: OpeningKind::Door,
            top: 2560,
            material: walls,
        },
    )
    .expect("the corridor door");
    doors.extend(door.door.iter().map(outside_cell));
    let _ = OpeningProfile::Rectangular;

    // The courtyard: seeded terrain everywhere the rest does not stand, with
    // a gap under the bridge, then flat pads at every door and the spawn.
    let mut taken: BTreeSet<[i32; 2]> = BTreeSet::new();
    for (i, r) in world.rooms.iter().enumerate() {
        if i != 0 {
            taken.extend(occupied(r));
        }
    }
    taken.extend(rect([38, 1], [45, 10]).cells());
    let all: BTreeSet<[i32; 2]> = rect(COURTYARD[0], COURTYARD[1])
        .cells()
        .filter(|c| !taken.contains(c))
        .collect();
    let mut t = TerrainParams::new(0, all.clone(), 2026);
    t.base = Some(0);
    t.create = true;
    t.amplitude_clicks = 1;
    t.edge = Edge::Cliff;
    t.follow_ceiling = false;
    t.material = roles.ground.or(ground);
    let report = lay_terrain(&mut world, &t).expect("the courtyard");
    let mut pads: Vec<[i32; 2]> = doors.clone();
    pads.push(SPAWN);
    for p in pads {
        let cells: BTreeSet<[i32; 2]> = rect([p[0] - 1, p[1] - 1], [p[0] + 1, p[1] + 1])
            .cells()
            .filter(|c| all.contains(c))
            .collect();
        if cells.is_empty() {
            continue;
        }
        let mut pad = TerrainParams::new(0, cells, 1);
        pad.base = Some(0);
        pad.amplitude_clicks = 0;
        pad.edge = Edge::Blend(2);
        pad.follow_ceiling = false;
        lay_terrain(&mut world, &pad).expect("a pad");
    }
    // Now the ruin, with the spawn on a floor.
    let ruin_report = ruin_region(&mut world, &ruin_later).expect("the ruin");

    finish(
        &mut project,
        &path,
        &world,
        SPAWN,
        &[
            ("Spawn Light", SPAWN[0] * 1024 + 512, SPAWN[1] * 1024 + 512),
            ("Cathedral Light", 14 * 1024, 6 * 1024),
            ("Ruin Light", 14 * 1024, 20 * 1024),
            ("Building Light", 34 * 1024, 6 * 1024),
            ("Tower Light", 50 * 1024, 6 * 1024),
            ("Hall Light", 46 * 1024, 20 * 1024),
        ],
    );

    // Report: budget, reachability and the playtest cook.
    let budget = GridBudget::of(&world);
    println!(
        "rooms {}, faces {}, largest room {} faces, leaves {}, meshes {}, walls {}",
        budget.rooms,
        budget.faces,
        budget.largest_room_faces,
        budget.leaves,
        budget.meshes,
        budget.walls
    );
    println!("terrain {report:?}");
    println!("ruin {ruin_report:?}");
    for n in &notes {
        println!("note: {n}");
    }
    let spawn = Spawn {
        room: 0,
        cell: SPAWN,
    };
    if let Some(a) = analyse(&world, &[spawn], &psx_engine::WalkLimits::DEFAULT) {
        println!(
            "walk counts {:?} (steep, low, step, drop, unreachable)",
            a.counts()
        );
        for e in a
            .edges
            .iter()
            .filter(|e| format!("{:?}", e.kind) == "StepTooTall")
        {
            println!(
                "  step too tall: room {} cell {:?} {:?} ({} ru)",
                e.room, e.cell, e.edge, e.delta_ru
            );
        }
        for (i, r) in world.rooms.iter().enumerate() {
            let lost: Vec<[i32; 2]> = rect(
                [r.origin[0], r.origin[1]],
                [
                    r.origin[0] + r.width as i32 - 1,
                    r.origin[1] + r.depth as i32 - 1,
                ],
            )
            .cells()
            .filter(|c| a.rooms[i].cell(*c).is_some_and(|w| w.unreachable))
            .collect();
            if !lost.is_empty() {
                println!("  unreachable in room {i}: {lost:?}");
            }
            println!(
                "  room {i} {:>18}: {} standable",
                r.name,
                a.rooms[i].standable_count()
            );
        }
    }
    cook_check(&project, &out);
}
