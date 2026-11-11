//! The showcase level: one interconnected place made only from the level
//! verbs, to show what the engine and the editor can build.
//!
//! The generator example (`examples/showcase.rs`) and the tests both call
//! [`build`]; nothing here is random except through the verbs' own seeds.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::grid::edit::tilt::{tilt_cells, Hinge, TiltParams, TiltTarget};
use crate::grid::edit::verbs::arcade::{build_colonnade, Colonnade, Layout};
use crate::grid::edit::verbs::bridge::{build_bridge, BridgeParams};
use crate::grid::edit::verbs::building::{build_building, BuildingParams};
use crate::grid::edit::verbs::capture::{capture_bay, repeat_bay, Along};
use crate::grid::edit::verbs::cathedral::{build_cathedral, CathedralParams};
use crate::grid::edit::verbs::corridor::{LegShape, Waypoint};
use crate::grid::edit::verbs::doorway_door::{doorway_with_door, DoorwayDoor, Lever};
use crate::grid::edit::verbs::opening::{
    arch_head, cut_opening, cut_opening_on_span, ArchShape, OpeningKind, OpeningParams,
    OpeningProfile,
};
use crate::grid::edit::verbs::ramp::RampDir;
use crate::grid::edit::verbs::room::{lay_room, Footprint, LeafPolicy, RoomParams, RoomTarget};
use crate::grid::edit::verbs::ruin::{ruin_region, RuinParams};
use crate::grid::edit::verbs::style::{self, Roles};
use crate::grid::edit::verbs::styled::{
    lay_styled_corridor, lay_styled_room, StyledCorridor, StyledRoom,
};
use crate::grid::edit::verbs::terrain::{lay_terrain, Edge, TerrainParams};
use crate::grid::edit::CellRect;
use crate::grid::mesh::ColumnSpec;
use crate::grid::overrides::{Axis, EdgeKey};
use crate::grid::plane::GridPlane;
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::units::SECTOR_UNITS;
use crate::grid::world::{GridWorld, SceneWorld};
use crate::{
    DoorDirection, DoorLock, DoorSide, NodeId, NodeKind, ResourceId, Scene, SwitchBehavior,
};
use crate::{InteractableKind, ProjectDocument};

/// Materials by name; every one may be absent (tests build without any).
#[derive(Clone, Copy, Debug, Default)]
pub struct Mats {
    /// Ashlar walls.
    pub stone_wall: Option<ResourceId>,
    /// Rubble masonry.
    pub stone_rough: Option<ResourceId>,
    /// Worn paving.
    pub paving: Option<ResourceId>,
    /// Square flagstones.
    pub flagstone: Option<ResourceId>,
    /// Stair treads.
    pub stair_tread: Option<ResourceId>,
    /// Fluted pier.
    pub pier: Option<ResourceId>,
    /// Arch ring stones.
    pub voussoir: Option<ResourceId>,
    /// Moulded trim course.
    pub trim: Option<ResourceId>,
    /// Crypt stone.
    pub crypt_stone: Option<ResourceId>,
    /// Tower ashlar.
    pub tower_stone: Option<ResourceId>,
    /// Ribbed vault.
    pub vault: Option<ResourceId>,
    /// Crimson banner.
    pub banner: Option<ResourceId>,
    /// Iron gate.
    pub iron_gate: Option<ResourceId>,
    /// Wooden door.
    pub wood_door: Option<ResourceId>,
    /// Lit lancet windows.
    pub window_glow: Option<ResourceId>,
    /// Ash ground.
    pub ash_ground: Option<ResourceId>,
    /// Rubble.
    pub rubble: Option<ResourceId>,
    /// Sci-fi rib panel.
    pub scifi_panel: Option<ResourceId>,
    /// Sci-fi trim.
    pub scifi_trim: Option<ResourceId>,
    /// Skull frieze.
    pub frieze: Option<ResourceId>,
}

fn rect(a: [i32; 2], b: [i32; 2]) -> CellRect {
    CellRect::from_corners(a, b)
}

fn cells(a: [i32; 2], b: [i32; 2]) -> BTreeSet<[i32; 2]> {
    rect(a, b).cells().collect()
}

/// A flat open-air platform: a terrain room with no relief.
fn platform(
    world: &mut GridWorld,
    name: &str,
    area: BTreeSet<[i32; 2]>,
    height: i32,
    material: Option<ResourceId>,
) -> Result<usize, String> {
    let room = world.rooms.len();
    let mut r = GridRoom::new_derived(name, 1, 1);
    r.defaults.wall_material = material;
    world.rooms.push(r);
    let mut t = TerrainParams::new(room, area, 1);
    t.base = Some(height);
    t.create = true;
    t.amplitude_clicks = 0;
    t.follow_ceiling = false;
    t.edge = Edge::Cliff;
    t.material = material;
    lay_terrain(world, &t).map_err(|e| e.describe())?;
    Ok(room)
}

/// Floor cells under a bridge deck so rooms and the deck share a floor.
fn deck_cells(
    world: &mut GridWorld,
    room: usize,
    area: BTreeSet<[i32; 2]>,
    base: i32,
    slope: [i32; 2],
) -> Result<(), String> {
    let mut t = TerrainParams::new(room, area, 1);
    t.base = Some(base);
    t.create = true;
    t.amplitude_clicks = 0;
    t.follow_ceiling = false;
    t.keep_walkable = false;
    t.edge = Edge::Cliff;
    t.slope = slope;
    lay_terrain(world, &t).map_err(|e| format!("deck cells: {}", e.describe()))?;
    Ok(())
}

/// The deck mesh draws but does not collide: the floor cells under it carry
/// the body (their closure walls hold it on the deck), so the walk never
/// depends on the mesh slab. Parapets and piers keep their collision.
fn soften_decks(world: &mut GridWorld, room: usize) {
    let decks: Vec<u16> = world
        .mesh_assets
        .iter()
        .enumerate()
        .filter(|(_, a)| a.name == "bridge deck")
        .map(|(i, _)| i as u16)
        .collect();
    for mesh in &mut world.rooms[room].meshes {
        if decks.contains(&mesh.asset) {
            mesh.collide = false;
        }
    }
}

/// Parts of the level, for tests and the report.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Room names by index.
    pub rooms: Vec<String>,
    /// The sector the player starts on and the way they face (degrees).
    pub spawn: [i32; 2],
    /// Yaw of the spawn, degrees.
    pub spawn_yaw: f32,
    /// The lever's sector.
    pub lever: [i32; 2],
    /// Light positions: `(name, x, y, z)` in grid units.
    pub lights: Vec<(String, i32, i32, i32)>,
    /// Where the light enemy and the heavy enemy stand: grid units `[x, y, z]`.
    pub enemies: [[i32; 3]; 2],
    /// The checkpoint, grid units.
    pub checkpoint: [i32; 3],
    /// What was left out and why.
    pub notes: Vec<String>,
}

fn open(
    world: &mut GridWorld,
    room: usize,
    cell: [i32; 2],
    side: WallEdge,
    from: i32,
    to: i32,
    profile: OpeningProfile,
    kind: OpeningKind,
    top: i32,
    material: Option<ResourceId>,
) -> Result<(), String> {
    cut_opening(
        world,
        &OpeningParams {
            profile,
            room,
            cell,
            side,
            from,
            to,
            kind,
            top,
            material,
        },
    )
    .map(|_| ())
    .map_err(|r| format!("opening at {cell:?} {side:?}: {}", r.describe()))
}

/// Open the closure edge of a cell of a room with no wall to cut a doorway in
/// (an open-air platform): hidden and not solid, like the lever demo's gaps.
fn ghost_edge(world: &mut GridWorld, room: usize, cell: [i32; 2], side: WallEdge) {
    let c = crate::grid::overrides::SEGMENT_CLICKS as i32;
    let key = match side {
        WallEdge::East => EdgeKey {
            x: (cell[0] + 1) * c,
            z: cell[1] * c,
            axis: Axis::Z,
            len: c as u8,
        },
        WallEdge::West => EdgeKey {
            x: cell[0] * c,
            z: cell[1] * c,
            axis: Axis::Z,
            len: c as u8,
        },
        WallEdge::North => EdgeKey {
            x: cell[0] * c,
            z: cell[1] * c,
            axis: Axis::X,
            len: c as u8,
        },
        _ => EdgeKey {
            x: cell[0] * c,
            z: (cell[1] + 1) * c,
            axis: Axis::X,
            len: c as u8,
        },
    };
    world.rooms[room].edge_overrides.edit(key, |e| {
        e.hidden = true;
        e.solid = Some(false);
    });
}

fn room_named(world: &GridWorld, name: &str) -> usize {
    world
        .rooms
        .iter()
        .position(|r| r.name == name)
        .unwrap_or_else(|| panic!("room {name}"))
}

/// Give every mesh instance of `room` the materials, slot by slot.
fn paint_meshes(world: &mut GridWorld, room: usize, slots: &[Option<ResourceId>]) {
    for mesh in &mut world.rooms[room].meshes {
        for (i, m) in slots.iter().enumerate() {
            mesh.set_material(i, *m);
        }
    }
}

const LIGHT_RED: [u8; 3] = [255, 92, 52];

/// Build the whole level into `world` and `scene`.
pub fn build(world: &mut GridWorld, scene: &mut Scene, m: &Mats) -> Result<Plan, String> {
    let e = |what: &'static str| move |err: String| format!("{what}: {err}");
    let mut plan = Plan {
        spawn: [2, 6],
        spawn_yaw: 270.0,
        lever: [21, 6],
        ..Plan::default()
    };
    // ---- 1. Landing and the chained bridge ------------------------------
    // One room holds the landing, the floor cells under the deck and the deck
    // meshes: a mesh-only room next to a cell room is not something the
    // streamer lets a body walk onto (see the notes), and one room has no
    // joint to cross.
    let landing =
        platform(world, "Landing", cells([0, 4], [5, 9]), 0, m.paving).map_err(e("landing"))?;
    deck_cells(world, landing, cells([6, 6], [21, 6]), -64, [0, 0])?;
    let deck_z = 6 * SECTOR_UNITS + 512;
    // The middle piece has no parapet: the lodge door opens onto it.
    for (name, x0, x1, parapet) in [
        ("bridge west", 6, 12, true),
        ("bridge middle", 12, 16, false),
        ("bridge east", 16, 22, true),
    ] {
        let mut b = BridgeParams::deck(
            RoomTarget::Existing(landing),
            [x0 * SECTOR_UNITS, 0, deck_z],
            [x1 * SECTOR_UNITS, 0, deck_z],
            SECTOR_UNITS,
            512,
        );
        if parapet {
            b.parapet = Some(768);
        }
        b.pier_spacing = Some(3 * SECTOR_UNITS);
        b.pier_foot = -4096;
        build_bridge(world, &b).map_err(|r| format!("{name}: {}", r.describe()))?;
    }
    paint_meshes(
        world,
        landing,
        &[m.paving, m.stone_wall, m.crypt_stone, m.pier],
    );
    soften_decks(world, landing);
    // The deck's end opens onto the gatehouse door and the middle piece onto
    // the lodge door.
    ghost_edge(world, landing, [21, 6], WallEdge::East);
    ghost_edge(world, landing, [13, 6], WallEdge::South);
    // ---- 2. Gatehouse and lodge -----------------------------------------
    let mut gh = BuildingParams::new(
        RoomTarget::New("Gatehouse".into()),
        Footprint::Rect(rect([22, 5], [26, 9])),
    );
    gh.storeys = 1;
    gh.stair = false;
    gh.roof_parapet = None;
    gh.door_sides = vec![];
    gh.window_every = 0;
    gh.floor_material = m.flagstone;
    gh.wall_material = m.tower_stone;
    gh.ceiling_material = m.vault;
    build_building(world, &gh).map_err(|r| format!("gatehouse: {}", r.describe()))?;
    let mut lodge = BuildingParams::new(
        RoomTarget::New("Lodge".into()),
        Footprint::Rect(rect([12, 7], [15, 10])),
    );
    lodge.storeys = 1;
    lodge.stair = false;
    lodge.roof_parapet = None;
    lodge.door_sides = vec![];
    lodge.window_every = 0;
    lodge.floor_material = m.flagstone;
    lodge.wall_material = m.stone_rough;
    lodge.ceiling_material = m.vault;
    build_building(world, &lodge).map_err(|r| format!("lodge: {}", r.describe()))?;
    // ---- 3. Cathedral ----------------------------------------------------
    let mut cath = CathedralParams::new(RoomTarget::New("Cathedral".into()), [27, 4]);
    cath.arch = Some(ArchShape::Pointed);
    cath.bays = 4;
    cath.west_door = false;
    cath.floor_material = m.paving;
    cath.wall_material = m.stone_wall;
    cath.ceiling_material = m.vault;
    build_cathedral(world, &cath).map_err(|r| format!("cathedral: {}", r.describe()))?;
    paint_meshes(
        world,
        room_named(world, "Cathedral"),
        &[m.voussoir, m.vault, m.stone_wall, m.pier],
    );
    // ---- 4. Courtyard and cloister ---------------------------------------
    let mut yard = cells([10, 11], [23, 19]);
    yard.extend(cells([24, 14], [30, 19]));
    platform(world, "Courtyard", yard.clone(), 0, m.ash_ground).map_err(e("courtyard"))?;
    let cloister_roles = Roles {
        floor: m.flagstone,
        wall: m.stone_wall,
        ceiling: m.vault,
        trim: m.trim,
        accent: m.pier,
        ground: m.ash_ground,
    };
    lay_styled_corridor(
        world,
        &StyledCorridor {
            target: RoomTarget::New("Cloister".into()),
            waypoints: vec![
                Waypoint::at_height([24, 12], 0),
                Waypoint::at([31, 12]),
                Waypoint::at([31, 22]),
                Waypoint::at([24, 22]),
            ],
            width: 2,
            base: 0,
            leg: LegShape::Ramp,
            style: style::cloister(),
            roles: cloister_roles,
        },
    )
    .map_err(|r| format!("cloister: {}", r.describe()))?;
    // ---- 5. Stair, crypt, machine passage, dome ---------------------------
    let crypt_roles = Roles {
        floor: m.flagstone,
        wall: m.crypt_stone,
        ceiling: m.vault,
        trim: m.pier,
        accent: m.pier,
        ground: m.ash_ground,
    };
    lay_styled_corridor(
        world,
        &StyledCorridor {
            target: RoomTarget::New("Crypt".into()),
            waypoints: vec![
                Waypoint::at_height([28, 24], 0),
                Waypoint::at_height([28, 29], -1024),
                Waypoint::at([22, 29]),
            ],
            width: 2,
            base: 0,
            leg: LegShape::Stairs,
            style: style::crypt(),
            roles: crypt_roles,
        },
    )
    .map_err(|r| format!("crypt: {}", r.describe()))?;
    paint_meshes(world, room_named(world, "Crypt"), &[m.pier, m.trim]);
    let scifi_roles = Roles {
        floor: m.scifi_panel,
        wall: m.scifi_panel,
        ceiling: m.scifi_panel,
        trim: m.scifi_trim,
        accent: m.scifi_trim,
        ground: m.ash_ground,
    };
    lay_styled_corridor(
        world,
        &StyledCorridor {
            target: RoomTarget::New("Machine passage".into()),
            waypoints: vec![Waypoint::at_height([20, 29], -1024), Waypoint::at([14, 29])],
            width: 2,
            base: -1024,
            leg: LegShape::Ramp,
            style: style::scifi_rib(),
            roles: scifi_roles,
        },
    )
    .map_err(|r| format!("machine passage: {}", r.describe()))?;
    lay_styled_room(
        world,
        &StyledRoom {
            target: RoomTarget::New("Domed chamber".into()),
            rect: rect([10, 28], [13, 31]),
            base: -1024,
            style: {
                let mut dome = style::domed_chamber();
                dome.chord_clicks = 2;
                dome
            },
            roles: Roles {
                floor: m.paving,
                wall: m.tower_stone,
                ceiling: m.vault,
                trim: m.trim,
                accent: m.pier,
                ground: m.ash_ground,
            },
        },
    )
    .map_err(|r| format!("dome: {}", r.describe()))?;
    paint_meshes(world, room_named(world, "Domed chamber"), &[m.pier, m.trim]);
    // ---- 6. The tower -----------------------------------------------------
    let mut tower = BuildingParams::new(
        RoomTarget::New("Tower".into()),
        Footprint::Octagon {
            rect: rect([10, 20], [17, 27]),
            cut: 2,
        },
    );
    tower.base = -1024;
    tower.storeys = 2;
    tower.storey_height = 3072;
    tower.stair = true;
    tower.roof_parapet = None;
    tower.door_sides = vec![];
    tower.window_every = 0;
    tower.window_sill = 512;
    tower.window_top = 2048;
    tower.door_height = 1536;
    tower.floor_material = m.stair_tread;
    tower.wall_material = m.tower_stone;
    tower.ceiling_material = m.vault;
    let tr = build_building(world, &tower).map_err(|r| format!("tower: {}", r.describe()))?;
    plan.notes.extend(tr.notes.iter().cloned());
    plan.notes.push(format!(
        "tower: {} treads, well {:?}",
        tr.treads, tr.stairwell
    ));
    // ---- 7. The high bridge, from the tower's top floor to the lodge ------
    let tower_room = room_named(world, "Tower");
    let lodge_room = room_named(world, "Lodge");
    let gate_room = room_named(world, "Gatehouse");
    let cath_room = room_named(world, "Cathedral");
    let cloister_room = room_named(world, "Cloister");
    let crypt_room = room_named(world, "Crypt");
    let machine_room = room_named(world, "Machine passage");
    let dome_room = room_named(world, "Domed chamber");
    let rect_profile = OpeningProfile::Rectangular;
    let door = OpeningKind::Door;
    // Tower: the top storey's door onto the high bridge (outside the stair
    // well), and the dome door below.
    cut_opening_on_span(
        world,
        &OpeningParams {
            profile: rect_profile,
            room: tower_room,
            cell: [15, 20],
            side: WallEdge::North,
            from: 60,
            to: 64,
            kind: door,
            top: 1536,
            material: m.trim,
        },
        1,
    )
    .map_err(|r| format!("tower upper door: {}", r.describe()))?;
    let mut high = BridgeParams::deck(
        RoomTarget::New("High bridge".into()),
        [15 * SECTOR_UNITS + 512, 2048, 20 * SECTOR_UNITS],
        [15 * SECTOR_UNITS + 512, 0, 11 * SECTOR_UNITS],
        SECTOR_UNITS,
        512,
    );
    high.parapet = Some(768);
    build_bridge(world, &high).map_err(|r| format!("high bridge: {}", r.describe()))?;
    let hb = room_named(world, "High bridge");
    paint_meshes(world, hb, &[m.paving, m.stone_wall, m.crypt_stone, m.pier]);
    deck_cells(world, hb, cells([15, 11], [15, 19]), -64, [0, 228])?;
    ghost_edge(world, hb, [15, 19], WallEdge::South);
    ghost_edge(world, hb, [15, 11], WallEdge::North);
    // Lodge: the shortcut door onto the start bridge (one way), and a plain
    // doorway on the courtyard side for the high bridge to land at.
    let shortcut = doorway_with_door(
        world,
        scene,
        &DoorwayDoor {
            opening: OpeningParams {
                profile: rect_profile,
                room: lodge_room,
                cell: [13, 7],
                side: WallEdge::North,
                from: 52,
                to: 56,
                kind: door,
                top: 2048,
                material: m.trim,
            },
            name: "shortcut".into(),
            material: m.wood_door,
            direction: DoorDirection::Up,
            ticks: 45,
            lock: DoorLock::OneWay {
                refuse_from: DoorSide::North,
            },
            refusal: "It does not open from this side.".into(),
            persistent: true,
            lever: Some(Lever {
                cell: [13, 7],
                prompt: "PUSH DOOR".into(),
                radius: 1100,
                behavior: SwitchBehavior::Toggle,
            }),
        },
    )
    .map_err(|r| format!("shortcut door: {}", r.describe()))?;
    // The push button stands on the door plane so both sides reach it.
    if let Some(lever) = shortcut.lever {
        let to_editor = |gu: i32| world.scale.to_editor(gu) as f32;
        if let Some(n) = scene.node_mut(lever) {
            n.transform.translation = [
                to_editor(shortcut.plane.center[0]),
                to_editor(shortcut.plane.center[1]),
                to_editor(shortcut.plane.center[2]),
            ];
        }
    }
    open(
        world,
        lodge_room,
        [15, 10],
        WallEdge::South,
        60,
        64,
        rect_profile,
        door,
        2048,
        m.trim,
    )?;
    // Gatehouse: the lever gate to the bridge, and the pointed arch into the
    // nave.
    let gate = doorway_with_door(
        world,
        scene,
        &DoorwayDoor {
            opening: OpeningParams {
                profile: rect_profile,
                room: gate_room,
                cell: [22, 6],
                side: WallEdge::West,
                from: 24,
                to: 28,
                kind: door,
                top: 2048,
                material: m.trim,
            },
            name: "gate".into(),
            material: m.iron_gate,
            direction: DoorDirection::Up,
            ticks: 90,
            lock: DoorLock::Unlocked,
            refusal: String::new(),
            persistent: true,
            lever: Some(Lever {
                cell: [21, 6],
                prompt: "PULL LEVER".into(),
                radius: 700,
                behavior: SwitchBehavior::OneShot,
            }),
        },
    )
    .map_err(|r| format!("gate: {}", r.describe()))?;
    let _ = gate;
    open(
        world,
        gate_room,
        [26, 7],
        WallEdge::East,
        28,
        36,
        arch_head(ArchShape::Pointed, 8, 2816),
        door,
        0,
        m.voussoir,
    )?;
    // Cathedral south door into the cloister, the cloister's openings onto the
    // garth (a door and one window of each profile), and the stair door.
    open(
        world,
        cath_room,
        [31, 11],
        WallEdge::South,
        124,
        128,
        rect_profile,
        door,
        1792,
        m.trim,
    )?;
    open(
        world,
        cloister_room,
        [27, 13],
        WallEdge::South,
        108,
        112,
        arch_head(ArchShape::Round, 4, 2048),
        door,
        0,
        m.voussoir,
    )?;
    open(
        world,
        cloister_room,
        [25, 13],
        WallEdge::South,
        100,
        104,
        arch_head(ArchShape::Round, 4, 2048),
        OpeningKind::Window { sill: 768 },
        0,
        m.voussoir,
    )?;
    open(
        world,
        cloister_room,
        [29, 13],
        WallEdge::South,
        116,
        120,
        rect_profile,
        OpeningKind::Window { sill: 768 },
        1792,
        m.trim,
    )?;
    open(
        world,
        cloister_room,
        [31, 16],
        WallEdge::West,
        64,
        68,
        arch_head(ArchShape::Pointed, 4, 2304),
        OpeningKind::Window { sill: 768 },
        0,
        m.voussoir,
    )?;
    open(
        world,
        cloister_room,
        [28, 23],
        WallEdge::South,
        112,
        120,
        rect_profile,
        door,
        1536,
        m.trim,
    )?;
    // Crypt to machine passage to dome to tower.
    open(
        world,
        crypt_room,
        [22, 29],
        WallEdge::West,
        116,
        124,
        rect_profile,
        door,
        1536,
        m.scifi_trim,
    )?;
    open(
        world,
        machine_room,
        [14, 29],
        WallEdge::West,
        116,
        124,
        rect_profile,
        door,
        1536,
        m.scifi_trim,
    )?;
    open(
        world,
        dome_room,
        [12, 28],
        WallEdge::North,
        48,
        56,
        rect_profile,
        door,
        1536,
        m.trim,
    )?;
    // ---- 8. The gallery: a colonnade, a captured and repeated hand bay and
    //         a tilted floor, off the cathedral's north aisle ---------------
    let mut gal = RoomParams::new(Footprint::Rect(rect([28, 0], [37, 3])), "Gallery", 3072);
    gal.floor_material = m.flagstone;
    gal.ceiling_material = m.vault;
    gal.wall_material = m.tower_stone;
    gal.leaves = LeafPolicy::Sectors;
    lay_room(world, &gal).map_err(|r| format!("gallery: {}", r.describe()))?;
    let gallery = room_named(world, "Gallery");
    build_colonnade(
        world,
        &Colonnade {
            room: gallery,
            layout: Layout::Line {
                from: [29 * SECTOR_UNITS, SECTOR_UNITS],
                to: [36 * SECTOR_UNITS, SECTOR_UNITS],
            },
            base: 0,
            bays: 7,
            column: ColumnSpec::default(),
        },
    )
    .map_err(|r| format!("colonnade: {}", r.describe()))?;
    paint_meshes(world, gallery, &[m.pier, m.trim]);
    // A hand-made bay: a plinth and a step, captured and laid four more times.
    {
        let r = &mut world.rooms[gallery];
        for (cell, h) in [([28, 2], 256), ([29, 3], 128), ([28, 3], 128)] {
            if let Some(f) = r
                .local(cell)
                .and_then(|[x, z]| r.sector_mut(x, z))
                .and_then(|s| s.ground_floor_mut())
            {
                f.heights = [h; 4];
            }
        }
    }
    let bay = capture_bay(world, gallery, rect([28, 2], [29, 3]))
        .map_err(|r| format!("capture: {}", r.describe()))?;
    repeat_bay(world, gallery, &bay, [30, 2], Along::X, 3, 0)
        .map_err(|r| format!("repeat: {}", r.describe()))?;
    // A tilted floor at the east end: leaned by an offset, not flattened.
    tilt_cells(
        world,
        &TiltParams {
            room: gallery,
            cells: cells([36, 0], [37, 3]),
            target: TiltTarget::Floor,
            hinge: Hinge::Edge(RampDir::North),
            rise: 512,
            span: 0,
            force: false,
        },
    )
    .map_err(|r| format!("tilt: {}", r.describe()))?;
    open(
        world,
        cath_room,
        [32, 4],
        WallEdge::North,
        128,
        132,
        arch_head(ArchShape::Pointed, 4, 2816),
        door,
        0,
        m.voussoir,
    )?;
    // ---- 9. The cathedral falls into ruin along a kept path ----------------
    let all: BTreeSet<[i32; 2]> = world.rooms[cath_room]
        .leaves()
        .flat_map(|(k, _)| {
            let (lo, hi) = k.sector_range();
            rect(lo, hi).cells().collect::<Vec<_>>()
        })
        .collect();
    let mut keep: BTreeSet<[i32; 2]> = all
        .iter()
        .copied()
        .filter(|c| c[1] == 7 || c[1] == 8)
        .collect();
    keep.extend(cells([31, 9], [31, 11]));
    keep.extend(cells([32, 4], [32, 6]));
    let mut ruin = RuinParams::new(cath_room, all, 11, 32);
    ruin.keep = keep;
    ruin_region(world, &ruin).map_err(|r| format!("ruin: {}", r.describe()))?;
    // ---- 10. Banners ----------------------------------------------------------
    for (name, room) in [("Landing", 6..12), ("Landing", 16..22)].map(|(n, r)| (n, r)) {
        let room_idx = room_named(world, name);
        for x in room.step_by(3) {
            for (z, yaw) in [(6 * SECTOR_UNITS + 160, 0), (7 * SECTOR_UNITS - 160, 180)] {
                let mut p = GridPlane::new([x * SECTOR_UNITS + 512, 1152, z], [448, 1536]);
                p.rotation = [yaw, 0, 0];
                p.material = m.banner;
                p.double_sided = true;
                world.rooms[room_idx].planes.push(p);
            }
        }
    }
    Ok(plan)
}

/// The texture set, in the order of [`Mats`].
pub const TEXTURES: [&str; 20] = [
    "stone_wall",
    "stone_rough",
    "paving",
    "flagstone",
    "stair_tread",
    "pier",
    "voussoir",
    "trim",
    "crypt_stone",
    "tower_stone",
    "vault",
    "banner",
    "iron_gate",
    "wood_door",
    "window_glow",
    "ash_ground",
    "rubble",
    "scifi_panel",
    "scifi_trim",
    "frieze",
];

/// Folder of the textures inside a project.
pub const TEXTURE_DIR: &str = "textures/showcase";

/// Add one material per texture of the set (named `Showcase / <name>`) and
/// return them by role. Existing ones are reused.
pub fn add_materials(project: &mut ProjectDocument) -> Mats {
    let ids: Vec<Option<ResourceId>> = TEXTURES
        .iter()
        .map(|name| {
            let title = format!("Showcase / {name}");
            let found = project
                .resources
                .iter()
                .find(|r| r.name == title && matches!(r.data, crate::ResourceData::Material(_)))
                .map(|r| r.id);
            Some(found.unwrap_or_else(|| {
                project.add_resource(
                    title,
                    crate::ResourceData::Material(crate::MaterialResource::opaque(Some(format!(
                        "{TEXTURE_DIR}/{name}.psxt"
                    )))),
                )
            }))
        })
        .collect();
    Mats {
        stone_wall: ids[0],
        stone_rough: ids[1],
        paving: ids[2],
        flagstone: ids[3],
        stair_tread: ids[4],
        pier: ids[5],
        voussoir: ids[6],
        trim: ids[7],
        crypt_stone: ids[8],
        tower_stone: ids[9],
        vault: ids[10],
        banner: ids[11],
        iron_gate: ids[12],
        wood_door: ids[13],
        window_glow: ids[14],
        ash_ground: ids[15],
        rubble: ids[16],
        scifi_panel: ids[17],
        scifi_trim: ids[18],
        frieze: ids[19],
    }
}

fn player_and_enemies(scene: &Scene) -> (Option<NodeId>, Vec<NodeId>) {
    let mut player = None;
    let mut enemies = Vec::new();
    for id in scene
        .node(NodeId::ROOT)
        .map(|r| r.children.clone())
        .unwrap_or_default()
    {
        let Some(n) = scene.node(id) else { continue };
        if !matches!(n.kind, NodeKind::Entity) {
            continue;
        }
        for c in &n.children {
            if let Some(NodeKind::CharacterController { player: p, .. }) =
                scene.node(*c).map(|k| &k.kind)
            {
                if *p {
                    player = Some(id);
                } else {
                    enemies.push(id);
                }
            }
        }
    }
    (player, enemies)
}

/// Editor position of a body standing on the floor of world sector `cell`
/// of room `room`, `lift` editor units above it.
fn stand(world: &GridWorld, room: usize, cell: [i32; 2], lift: i32) -> [f32; 3] {
    let r = &world.rooms[room];
    let top = r
        .local(cell)
        .and_then(|[x, z]| r.sector(x, z))
        .and_then(|s| s.ground_floor())
        .map_or(0, |f| f.heights.into_iter().max().unwrap_or(0));
    let to_editor = |gu: i32| world.scale.to_editor(gu) as f32;
    [
        to_editor(cell[0] * SECTOR_UNITS + SECTOR_UNITS / 2),
        to_editor(r.y_offset + top) + lift as f32,
        to_editor(cell[1] * SECTOR_UNITS + SECTOR_UNITS / 2),
    ]
}

fn entity_with_interactable(
    scene: &mut Scene,
    name: &str,
    at: [f32; 3],
    kind: InteractableKind,
    prompt: &str,
    radius: u16,
) {
    let id = scene.add_node(NodeId::ROOT, name, NodeKind::Entity);
    if let Some(n) = scene.node_mut(id) {
        n.transform.translation = at;
    }
    scene.add_node(
        id,
        "Interactable",
        NodeKind::Interactable {
            kind,
            prompt: prompt.to_string(),
            radius,
            enabled: true,
        },
    );
}

/// Turn `project` (a base such as `default`, with its player, enemies,
/// camera, sky and UI) into the showcase project in place.
pub fn into_showcase(
    project: &mut ProjectDocument,
    enemies: usize,
    spawn: Option<[i32; 2]>,
) -> Result<Plan, String> {
    super::base_project::strip_base(project, enemies.min(1));
    let mats = add_materials(project);
    let mut world = GridWorld {
        scale: super::starter::STARTER_SCALE,
        texels_per_sector: super::units::SectorTexels::DEFAULT,
        rooms: Vec::new(),
        shading: super::light::FacingShade::SUBTLE,
        global_light: super::lighting::GlobalLight {
            sky: Some([120, 28, 30]),
            ..super::lighting::GlobalLight::NEW_WORLD
        },
        stream: super::world::StreamRadii {
            draw_far: 2,
            active_radius: 3,
            lead_radius: 2,
            ..super::world::StreamRadii::DEFAULT
        },
        mesh_assets: Vec::new(),
        level_names: Default::default(),
        palette_pins: Vec::new(),
    };
    let scene = project.active_scene_mut();
    let mut plan = build(&mut world, scene, &mats)?;
    // Red fog and a low red ambient in every room.
    for r in &mut world.rooms {
        r.fog = super::room::GridFog {
            enabled: true,
            color: [112, 24, 22],
            start: SECTOR_UNITS,
            falloff: 4 * SECTOR_UNITS,
        };
        r.ambient = [96, 70, 74];
    }
    // Actors.
    let (player, enemies) = player_and_enemies(scene);
    if let Some(cell) = spawn {
        plan.spawn = cell;
    }
    let spawn_room = world
        .rooms
        .iter()
        .position(|r| {
            r.local(plan.spawn)
                .and_then(|[x, z]| r.sector(x, z))
                .is_some()
        })
        .unwrap_or(0);
    let at = stand(&world, spawn_room, plan.spawn, 64);
    if let Some(id) = player.and_then(|p| scene.node_mut(p)) {
        id.transform.translation = at;
        id.transform.rotation_degrees = [0.0, plan.spawn_yaw, 0.0];
    }
    let cathedral = room_named(&world, "Cathedral");
    let tower = room_named(&world, "Tower");
    let spots = [(cathedral, [32, 8], 270.0f32)];
    let _ = tower;
    for (id, (room, cell, yaw)) in enemies.iter().zip(spots) {
        let p = stand(&world, room, cell, 64);
        if let Some(n) = scene.node_mut(*id) {
            n.transform.translation = p;
            n.transform.rotation_degrees = [0.0, yaw, 0.0];
        }
    }
    plan.enemies = [[32, 0, 8], [0, 0, 0]];
    // The checkpoint in the dome.
    let dome = room_named(&world, "Domed chamber");
    let cp = stand(&world, dome, [11, 29], 0);
    entity_with_interactable(
        scene,
        "Checkpoint",
        cp,
        InteractableKind::Checkpoint {
            checkpoint_id: "dome".into(),
            title: "SYNCHRONIZED".into(),
            body: "Progress kept.".into(),
        },
        "SYNCHRONIZE",
        700,
    );
    plan.checkpoint = [11, -1024, 29];
    // Lamps: few, warm red, with dark between.
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(1024)
        .max(1) as f32;
    let lamp_radius = |sectors: f32| sectors * world.scale.sector_editor_units() as f32 / unit;
    let lamps: [(&str, [i32; 2], i32, f32, f32); 11] = [
        ("Landing brazier", [2, 6], 1280, 5.0, 1.5),
        ("Gate lantern", [20, 6], 1536, 4.0, 1.4),
        ("Gatehouse lamp", [24, 7], 1536, 4.5, 1.4),
        ("Nave brazier", [31, 8], 2048, 6.0, 1.6),
        ("Chancel fire", [34, 8], 1792, 6.0, 1.8),
        ("Cloister lamp", [31, 13], 1280, 5.0, 1.4),
        ("Lodge lamp", [13, 8], 1024, 3.5, 1.4),
        ("Crypt lamp", [26, 30], -256, 4.5, 1.4),
        ("Machine light", [17, 30], -256, 4.5, 1.3),
        ("Dome fire", [12, 30], 1024, 5.5, 1.8),
        ("Tower lamp", [13, 25], 0, 5.0, 1.4),
    ];
    for (name, cell, y, radius, intensity) in lamps {
        let lamp = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::PointLight {
                color: LIGHT_RED,
                intensity,
                radius: lamp_radius(radius),
            },
        );
        if let Some(n) = scene.node_mut(lamp) {
            let to_editor = |gu: i32| world.scale.to_editor(gu) as f32;
            n.transform.translation = [
                to_editor(cell[0] * SECTOR_UNITS + SECTOR_UNITS / 2),
                to_editor(y),
                to_editor(cell[1] * SECTOR_UNITS + SECTOR_UNITS / 2),
            ];
        }
        plan.lights.push((name.to_string(), cell[0], y, cell[1]));
    }
    // The sky: a low red blood moon over black jagged silhouettes.
    if let Some(root) = scene.node_mut(NodeId::ROOT) {
        if let NodeKind::World { sky, culling, .. } = &mut root.kind {
            sky.mode = crate::SkyMode::Panorama;
            sky.texture = None;
            sky.top_color = [40, 6, 10];
            sky.horizon_color = [118, 26, 24];
            sky.lower_color = [40, 6, 10];
            sky.sun_enabled = true;
            sky.sun_color = [214, 96, 84];
            sky.sun_border_color = [150, 34, 30];
            sky.sun_size_percent = 34;
            sky.sun_glow_percent = 80;
            sky.sun_yaw_degrees = 90;
            sky.sun_pitch_degrees = 14;
            sky.mountain_height_percent = 60;
            sky.mountain_top_color = [10, 2, 6];
            sky.mountain_base_color = [44, 8, 12];
            sky.mountain_gap_percent = 0;
            sky.mountain_roughness_percent = 90;
            sky.match_room_fog = true;
            sky.cloud_layer.enabled = false;
            culling.draw_distance = 7 * 1024;
        }
    }
    scene.world = SceneWorld::Grid(world);
    project.editor_camera = crate::EditorCameraState::default();
    project.editor_viewport = crate::EditorViewportState::default();
    Ok(plan)
}

/// Create the showcase project at `out_dir` from the base project in
/// `base_dir`, with the texture files `(name, psxt bytes)` written beside
/// it. Returns the written `project.ron` and the plan.
pub fn create_project(
    base_dir: &Path,
    out_dir: &Path,
    name: &str,
    enemies: usize,
    spawn: Option<[i32; 2]>,
    textures: &[(&str, &[u8])],
) -> Result<(PathBuf, Plan), String> {
    let mut project = ProjectDocument::load_from_path(base_dir.join("project.ron"))
        .map_err(|e| format!("base project: {e}"))?;
    let plan = into_showcase(&mut project, enemies, spawn)?;
    project.name = name.to_string();
    std::fs::create_dir_all(out_dir.join(TEXTURE_DIR)).map_err(|e| format!("create: {e}"))?;
    for (n, bytes) in textures {
        std::fs::write(out_dir.join(TEXTURE_DIR).join(format!("{n}.psxt")), bytes)
            .map_err(|e| format!("texture {n}: {e}"))?;
    }
    super::base_project::link_assets(base_dir, out_dir)
        .map_err(|e| format!("link the base assets: {e}"))?;
    let path = out_dir.join("project.ron");
    project.save_to_path(&path).map_err(|e| e.to_string())?;
    Ok((path, plan))
}
