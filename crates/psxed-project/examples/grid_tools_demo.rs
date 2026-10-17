//! Build small demo projects for the grid level tools, one scenario each.
//!
//! ```text
//! cargo run -p psxed-project --example grid_tools_demo -- <scenario> <out-dir>
//! ```
//!
//! Scenarios:
//!
//! The player stands in the middle of the room facing north (-z).
//!
//! * `tiling`: a room with a fit 4x4 floor patch ahead of the player, a fit
//!   4x4 wall segment on the far wall and a leaning legacy wall (the old
//!   32 texels per sector mapping, taller than one tile) beside it.
//! * `platform`: a raised platform and a pit in the starter room, to see the
//!   derived risers against the floor.
//! * `bands`: a far wall with a trim, a main body and a frieze in three
//!   materials (T4).
//! * `hall`: a plain derived room, flat, to raise and ramp in the editor (T5).
//! * `ramp`: a long room with a ramp at the steepest slope the character walks,
//!   made with the Ramp verb, and a plateau at its top; the player stands at
//!   its foot facing it (T6).
//! * `stairs`: a long room with a flight of one click steps, three sectors
//!   wide, made with the Stairs verb, and a landing at the top; the player
//!   stands at its foot facing it (T7).
//! * `walk`: a room that shows every walkability class: a steep floor, a low
//!   ceiling, a tall step, a drop and an island (T6).
//!
//! `SHADE=off` leaves the facing shade off in the world; every scenario
//! otherwise uses the new-world default.

use std::path::PathBuf;

use psxed_project::face_uv::{FaceUv, UvMode};
use psxed_project::grid::derive::edge_key;
use psxed_project::grid::edit::verbs::ramp::{RampDir, RampDrag, RampParams};
use psxed_project::grid::edit::verbs::stairs::{StairsDrag, StairsParams};
use psxed_project::grid::edit::{CellRect, Region};
use psxed_project::grid::starter::{create_project, STARTER_BASE, STARTER_SCALE};
use psxed_project::grid::{
    FacingShade, GridRoom, GridSurface, GridWall, GridWorld, SceneWorld, WallEdge, SECTOR_UNITS,
};
use psxed_project::{ProjectDocument, ResourceData, ResourceId};

fn material(project: &ProjectDocument, name: &str) -> Option<ResourceId> {
    project
        .resources
        .iter()
        .find(|r| r.name == name && matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.id)
}

struct Materials {
    ground: Option<ResourceId>,
    wall: Option<ResourceId>,
    cliff: Option<ResourceId>,
    beam: Option<ResourceId>,
    signal: Option<ResourceId>,
    green: Option<ResourceId>,
}

fn materials(project: &ProjectDocument) -> Materials {
    Materials {
        ground: material(project, "Graybox / Ground 64"),
        wall: material(project, "Graybox / Original 64"),
        cliff: material(project, "Graybox / Cliff 64"),
        beam: material(project, "DP City / Structural Beam"),
        signal: material(project, "DP City / Bright Blue Signal"),
        green: material(project, "DP City / Bright Green Signal"),
    }
}

/// A derived `width` by `depth` room, floor at 0, ceiling `height` up.
fn room(m: &Materials, width: usize, depth: usize, height: i32) -> GridRoom {
    let mut room = GridRoom::new_derived("Demo room", width, depth);
    room.defaults.wall_material = m.wall;
    for z in 0..depth {
        for x in 0..width {
            let sector = room.sector_or_insert(x, z).expect("inside the room");
            sector.floor = Some(GridSurface::flat(0, m.ground));
            sector.ceiling = Some(GridSurface::flat(height, m.cliff));
        }
    }
    room
}

fn fit(h: u16, v: u16) -> FaceUv {
    FaceUv {
        mode: UvMode::fit(h, v),
        ..FaceUv::default()
    }
}

fn tiling(m: &Materials) -> GridRoom {
    let mut room = room(m, 7, 6, 3 * SECTOR_UNITS);
    // A fit 4x4 floor patch two sectors ahead of the player.
    let patch = room.sector_or_insert(3, 2).expect("inside");
    if let Some(floor) = patch.floor.as_mut() {
        floor.tri_a.material = m.beam;
        floor.tri_a.uv = fit(4, 4);
    }
    // The far wall segment straight ahead, fit 4x4 across the segment.
    let key = edge_key([3, 0], WallEdge::North);
    room.edge_overrides.edit(key, |o| {
        o.material = m.beam;
        o.uv = Some(fit(4, 4));
    });
    // A leaning legacy wall: the old mapping, 96 texels tall.
    let lean = room.sector_or_insert(2, 2).expect("inside");
    let mut wall = GridWall::span(0, 0, m.signal);
    wall.heights = [0, 0, 2 * SECTOR_UNITS, 3 * SECTOR_UNITS];
    lean.walls_mut(WallEdge::North).push(wall);
    room
}

fn platform(m: &Materials) -> GridRoom {
    let mut room = room(m, 7, 6, 3 * SECTOR_UNITS);
    // A platform two sectors wide ahead of the player, a pit beside it.
    for z in 1..3 {
        for x in 2..4 {
            let sector = room.sector_or_insert(x, z).expect("inside");
            sector.floor = Some(GridSurface::flat(SECTOR_UNITS / 2, m.ground));
            sector.wall_material = m.cliff;
        }
    }
    for x in 4..6 {
        let sector = room.sector_or_insert(x, 2).expect("inside");
        sector.floor = Some(GridSurface::flat(-SECTOR_UNITS / 2, m.ground));
    }
    room
}

/// Every floor and ceiling a fit 1x1 (`repeat` false: one plain primitive
/// each) or a fit 1x1 started 32 texels in (`repeat` true: the same
/// geometry and primitive count, but each touches two tiles, so it carries
/// the texture window). For the packet cost of `REPEAT`.
fn bench(m: &Materials, repeat: bool) -> GridRoom {
    let mut room = room(m, 7, 6, 3 * SECTOR_UNITS);
    let uv = FaceUv {
        mode: UvMode::fit(1, 1),
        offset_texels: [if repeat { 32 } else { 0 }, 0],
        ..FaceUv::default()
    };
    for z in 0..6 {
        for x in 0..7 {
            let sector = room.sector_or_insert(x, z).expect("inside");
            for surface in [sector.floor.as_mut(), sector.ceiling.as_mut()]
                .into_iter()
                .flatten()
            {
                surface.tri_a.material = m.beam;
                surface.tri_a.uv = uv;
            }
        }
    }
    room
}

/// The far (north) wall in three bands: a trim at the base, the main body
/// and a frieze hung from the top. The middle segment has a set of its own
/// with different cut heights, so the neighbours across its vertical edges
/// are cut at other heights and the stitching shows.
fn bands(m: &Materials) -> GridRoom {
    use psxed_project::grid::{Anchor, BandSet, BandV};
    let mut room = room(m, 7, 6, 2 * SECTOR_UNITS);
    let set = |trim: i32, frieze: i32| {
        let mut set = BandSet::single("wall", m.wall);
        set.insert_line(0, Anchor::Bottom(trim));
        set.insert_line(1, Anchor::Top(frieze));
        set.set_band_material(0, m.beam);
        set.set_band_material(2, m.green);
        set.bands[0].v = BandV::FromBottom;
        set.bands[2].v = BandV::FromTop;
        // One tile per sector across the whole run, one tile high.
        set.bands[2].uv = FaceUv {
            mode: UvMode::fit(7, 1),
            ..FaceUv::default()
        };
        set
    };
    room.band_sets.push(set(SECTOR_UNITS / 4, SECTOR_UNITS / 2));
    room.band_sets.push(set(SECTOR_UNITS / 2, SECTOR_UNITS / 4));
    for x in 0..7 {
        let index = u8::from(x == 3);
        room.edge_overrides
            .edit(edge_key([x, 0], WallEdge::North), |o| {
                o.band_set = Some(index)
            });
    }
    room
}

/// Raise sectors `x0..x1` (all rows) of `room` to a flat floor `height` with
/// the ceiling the same distance above it as before.
fn plateau(room: &mut GridRoom, x0: usize, x1: usize, height: i32) {
    for z in 0..room.depth {
        for x in x0..x1 {
            let sector = room.sector_or_insert(x, z).expect("inside");
            let floor = sector.floor.as_mut().expect("a floor");
            let lift = height - floor.heights[0];
            floor.heights = [height; 4];
            if let Some(ceiling) = sector.ceiling.as_mut() {
                for h in &mut ceiling.heights {
                    *h += lift;
                }
            }
        }
    }
}

/// A room with a ramp at the steepest walkable slope, over sectors 4 to 9,
/// and a plateau at its top.
fn ramp(m: &Materials) -> GridRoom {
    let mut world = GridWorld {
        scale: STARTER_SCALE,
        rooms: vec![room(m, 15, 5, 4 * SECTOR_UNITS)],
        shading: FacingShade::SUBTLE,
        ..GridWorld::default()
    };
    let region = Region::from_rect(0, CellRect::from_corners([4, 0], [9, 4]));
    let params = RampParams {
        from: RampDir::West,
        follow_ceiling: true,
        wall_material: m.cliff,
    };
    let drag = RampDrag::begin(&world, &region, params).expect("the ramp region has floors");
    let rise = drag.max_walkable_rise();
    drag.apply(&mut world, rise);
    let mut room = world.rooms.remove(0);
    plateau(&mut room, 10, 15, rise);
    room
}

/// A room with a flight of one click steps over sectors 4 to 9, three wide,
/// and a landing beyond.
fn stairs(m: &Materials) -> GridRoom {
    let mut world = GridWorld {
        scale: STARTER_SCALE,
        rooms: vec![room(m, 15, 5, 4 * SECTOR_UNITS)],
        shading: FacingShade::SUBTLE,
        ..GridWorld::default()
    };
    let params = StairsParams {
        width: 3,
        wall_material: m.cliff,
        ..StairsParams::default()
    };
    let drag = StairsDrag::begin(&world, 0, &[[4, 2], [9, 2]], params).expect("a path");
    let flight = drag.apply(&mut world, 6 * 256);
    assert!(flight.refusal.is_none());
    let mut room = world.rooms.remove(0);
    plateau(&mut room, 10, 15, flight.total);
    room
}

/// Every class the walkability overlay tells apart.
fn walk(m: &Materials) -> GridRoom {
    let mut room = room(m, 11, 7, 3 * SECTOR_UNITS);
    // A steep floor: 1.5 sectors of rise over one.
    room.sector_or_insert(2, 1)
        .unwrap()
        .floor
        .as_mut()
        .unwrap()
        .heights = [0, 0, 1536, 1536];
    // A low ceiling, a quarter of the body above the floor.
    room.sector_or_insert(4, 1)
        .unwrap()
        .ceiling
        .as_mut()
        .unwrap()
        .heights = [560; 4];
    // A tall step: three clicks, more than the body climbs.
    for z in 3..7 {
        room.sector_or_insert(7, z)
            .unwrap()
            .floor
            .as_mut()
            .unwrap()
            .heights = [768; 4];
    }
    // A pit that is only a step down, and a cliff to nothing.
    room.sector_or_insert(1, 5).unwrap().floor = None;
    room.sector_or_insert(9, 1).unwrap().floor = None;
    // An island behind a wall of void.
    for z in 0..7 {
        room.sector_or_insert(9, z).unwrap().floor = None;
    }
    room
}

/// Put the player at the middle of sector `cell` facing `yaw` degrees.
fn place_player(project: &mut ProjectDocument, world: &GridWorld, cell: [i32; 2], yaw: f32) {
    let scene = project.active_scene_mut();
    let player = scene
        .nodes()
        .iter()
        .find(|n| {
            n.children.iter().any(|c| {
                scene.node(*c).is_some_and(|k| {
                    matches!(
                        &k.kind,
                        psxed_project::NodeKind::CharacterController { player: true, .. }
                    )
                })
            })
        })
        .map(|n| n.id)
        .expect("a player entity");
    let half = SECTOR_UNITS / 2;
    let x = world.scale.to_editor(cell[0] * SECTOR_UNITS + half) as f32;
    let z = world.scale.to_editor(cell[1] * SECTOR_UNITS + half) as f32;
    if let Some(node) = scene.node_mut(player) {
        node.transform.translation[0] = x;
        node.transform.translation[2] = z;
        node.transform.rotation_degrees = [0.0, yaw, 0.0];
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let scenario = args
        .next()
        .expect("usage: grid_tools_demo <scenario> <out-dir>");
    let out = PathBuf::from(args.next().expect("an output directory"));
    let editor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let path = create_project(
        &editor.join("projects").join(STARTER_BASE),
        &out,
        &format!("Grid tools demo {scenario}"),
    )
    .expect("the starter project is made");
    let mut project = ProjectDocument::load_from_path(&path).expect("the starter loads");
    let m = materials(&project);
    let room = match scenario.as_str() {
        "tiling" => tiling(&m),
        "platform" => platform(&m),
        "bands" => bands(&m),
        "hall" => room(&m, 9, 8, 3 * SECTOR_UNITS),
        "ramp" => ramp(&m),
        "stairs" => stairs(&m),
        "walk" => walk(&m),
        "bench-plain" => bench(&m, false),
        "bench-repeat" => bench(&m, true),
        other => panic!("unknown scenario {other:?}"),
    };
    let shading = if std::env::var("SHADE").is_ok_and(|v| v == "off") {
        FacingShade::OFF
    } else {
        FacingShade::SUBTLE
    };
    let world = GridWorld {
        scale: STARTER_SCALE,
        rooms: vec![room],
        shading,
        ..GridWorld::default()
    };
    // Where the player starts, by scenario: facing east along the ramp.
    let start = match scenario.as_str() {
        "ramp" | "stairs" => Some(([1, 2], 270.0)),
        "walk" => Some(([0, 0], 90.0)),
        _ => None,
    };
    if let Some((cell, yaw)) = start {
        place_player(&mut project, &world, cell, yaw);
    }
    project.active_scene_mut().world = SceneWorld::Grid(world);
    project.save_to_path(&path).expect("the project saves");
    println!("wrote {}", path.display());
}
