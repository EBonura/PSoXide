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
//!
//! `SHADE=off` leaves the facing shade off in the world; every scenario
//! otherwise uses the new-world default.

use std::path::PathBuf;

use psxed_project::face_uv::{FaceUv, UvMode};
use psxed_project::grid::derive::edge_key;
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
    };
    project.active_scene_mut().world = SceneWorld::Grid(world);
    project.save_to_path(&path).expect("the project saves");
    println!("wrote {}", path.display());
}
