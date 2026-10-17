//! Build the free-plane demo project `editor/projects/grid-planes`.
//!
//! A starter project (Cortex player, camera, sky, UI) whose world is an 11 by
//! 6 sector hall cut by a two-sector ditch. Across the ditch runs a bridge
//! made of planes: a walkable ramp up, a walkable deck with solid
//! guardrail planes (alpha cutout, both sides), a walkable ramp down. Past
//! the bridge a solid slanted wall stands across the way, and a translucent
//! banner hangs on the north wall.
//!
//! Every plane goes in through the document edit functions the editor uses.
//!
//! ```text
//! cargo run -p psxed-project --example grid_planes_demo [out-dir]
//! ```
//!
//! With `PLANES_NO_BRIDGE` set in the environment the ramps, deck and
//! guardrails are left out: the control for "the bridge is what carries the
//! character".

use std::path::PathBuf;

use psxed_project::grid::edit::add_plane;
use psxed_project::grid::starter::{create_project, STARTER_BASE};
use psxed_project::grid::{
    GridPlane, GridRoom, GridSurface, GridWall, PlaneCollision, SceneWorld, WallEdge, SECTOR_UNITS,
};
use psxed_project::{NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId};

const WIDTH: usize = 11;
const DEPTH: usize = 6;
const HEIGHT: i32 = 2 * SECTOR_UNITS;
const DITCH: [usize; 2] = [5, 6];
const DITCH_FLOOR: i32 = -SECTOR_UNITS;

fn material(project: &ProjectDocument, name: &str) -> Option<ResourceId> {
    project
        .resources
        .iter()
        .find(|r| r.name == name && matches!(r.data, ResourceData::Material(_)))
        .map(|r| r.id)
}

fn hall(
    floor: Option<ResourceId>,
    wall: Option<ResourceId>,
    ceiling: Option<ResourceId>,
) -> GridRoom {
    let mut room = GridRoom::new("Plane hall", WIDTH, DEPTH);
    for z in 0..DEPTH {
        for x in 0..WIDTH {
            let in_ditch = DITCH.contains(&x);
            let bottom = if in_ditch { DITCH_FLOOR } else { 0 };
            let sector = room.sector_or_insert(x, z).expect("inside the room");
            sector.floor = Some(GridSurface::flat(bottom, floor));
            sector.ceiling = Some(GridSurface::flat(HEIGHT, ceiling));
            for (on_edge, edge) in [
                (z == 0, WallEdge::North),
                (x == WIDTH - 1, WallEdge::East),
                (z == DEPTH - 1, WallEdge::South),
                (x == 0, WallEdge::West),
            ] {
                if on_edge {
                    sector
                        .walls_mut(edge)
                        .push(GridWall::span(bottom, HEIGHT, wall));
                }
            }
            // The risers of the ditch, seen from inside it.
            if x == DITCH[0] {
                sector
                    .walls_mut(WallEdge::West)
                    .push(GridWall::span(DITCH_FLOOR, 0, wall));
            }
            if x == DITCH[1] {
                sector
                    .walls_mut(WallEdge::East)
                    .push(GridWall::span(DITCH_FLOOR, 0, wall));
            }
        }
    }
    room
}

fn plane(center: [i32; 3], size: [i32; 2], rotation: [i32; 3]) -> GridPlane {
    let mut p = GridPlane::new(center, size);
    p.rotation = rotation;
    p
}

fn main() {
    let editor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::env::args()
        .nth(1)
        .map_or_else(|| editor.join("projects/grid-planes"), PathBuf::from);
    let path = create_project(
        &editor.join("projects").join(STARTER_BASE),
        &out,
        "Grid planes",
    )
    .expect("the starter project is made");
    let mut project = ProjectDocument::load_from_path(&path).expect("the starter loads");

    let ground = material(&project, "Graybox / Ground 64");
    let walls = material(&project, "Graybox / Original 64");
    let cliff = material(&project, "Graybox / Cliff 64");
    let beam = material(&project, "DP City / Structural Beam");
    let guardrail = material(&project, "DP City / Guardrail (Cutout)");
    let lattice = material(&project, "DP City / Hanging Lattice (Average)");

    let mut world = psxed_project::grid::GridWorld {
        scale: psxed_project::grid::starter::STARTER_SCALE,
        rooms: vec![hall(ground, walls, cliff)],
        shading: psxed_project::grid::FacingShade::SUBTLE,
    };
    // Ramp up: rises 18 degrees along +x from the floor to the deck.
    let mut ramp_up = plane([4352, 262, 3584], [1024, 1615], [-90, 72, 0]);
    // Deck over the ditch.
    let mut deck = plane([6144, 512, 3584], [2048, 1024], [0, 90, 0]);
    // Ramp down, the mirror of the first.
    let mut ramp_down = plane([7936, 262, 3584], [1024, 1615], [90, 72, 0]);
    for p in [&mut ramp_up, &mut deck, &mut ramp_down] {
        p.collision = PlaneCollision::Walkable;
        p.material = beam;
    }
    // Guardrails on the deck: alpha cutout, both sides, solid.
    let mut rails = Vec::new();
    for z in [3072 + 12, 4096 - 12] {
        let mut rail = plane([6144, 768, z], [2048, 512], [0, 0, 0]);
        rail.collision = PlaneCollision::Solid;
        rail.double_sided = true;
        rail.material = guardrail;
        rails.push(rail);
    }
    // A slanted solid wall across the way past the bridge.
    let mut slant = plane([10000, 768, 3584], [3072, 1536], [-70, 0, 0]);
    slant.collision = PlaneCollision::Solid;
    slant.material = cliff;
    // A translucent banner against the north wall.
    let mut banner = plane([4096, 1000, 40], [1024, 1536], [0, 0, 0]);
    banner.double_sided = true;
    banner.material = lattice;
    let bridge: Vec<GridPlane> = if std::env::var_os("PLANES_NO_BRIDGE").is_some() {
        Vec::new()
    } else {
        [ramp_up, deck, ramp_down]
            .into_iter()
            .chain(rails)
            .collect()
    };
    for p in bridge.into_iter().chain([slant, banner]) {
        add_plane(&mut world, 0, p).expect("room for the plane");
    }

    // Move the player and the lamp from the middle of the 6 by 6 starter to
    // sector (2, 3) of the hall and add a lamp over the bridge.
    let shift = world.scale.to_editor(-SECTOR_UNITS) as f32;
    let scene = project.active_scene_mut();
    let ids: Vec<NodeId> = scene
        .nodes()
        .iter()
        .filter(|n| n.id != NodeId::ROOT)
        .map(|n| n.id)
        .collect();
    for id in ids {
        if let Some(node) = scene.node_mut(id) {
            if node.parent == Some(NodeId::ROOT) {
                node.transform.translation[0] += shift;
            }
        }
    }
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(1024)
        .max(1) as f32;
    for (x, z) in [(6144, 3584), (9216, 3584)] {
        let lamp = scene.add_node(
            NodeId::ROOT,
            "Hall Light",
            NodeKind::PointLight {
                color: [255, 224, 180],
                intensity: 1.4,
                radius: 5.0 * world.scale.sector_editor_units() as f32 / unit,
            },
        );
        if let Some(node) = scene.node_mut(lamp) {
            node.transform.translation = [
                world.scale.to_editor(x) as f32,
                world.scale.to_editor(1500) as f32,
                world.scale.to_editor(z) as f32,
            ];
        }
    }
    scene.world = SceneWorld::Grid(world);
    project.save_to_path(&path).expect("the project saves");
    println!("wrote {}", path.display());
}
