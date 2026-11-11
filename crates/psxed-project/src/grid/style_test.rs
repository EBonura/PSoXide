//! The style test scene: one worm-dug canal with a sandstone megablock ruin
//! in it, built only to judge an art direction from real in-game frames.
//!
//! `examples/style_test.rs` calls [`create_project`]; the level is a pure
//! function of the code and the atmosphere, byte for byte. Textures are
//! bound by name (see [`TEXTURE_NAMES`]): swapping a file with the same name
//! is all it takes to restyle the scene.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::grid::edit::meshes::{add_asset, place_mesh};
use crate::grid::edit::planes::add_plane;
use crate::grid::edit::verbs::building::{build_building, BuildingParams};
use crate::grid::edit::verbs::doorway_door::{doorway_with_door, DoorwayDoor};
use crate::grid::edit::verbs::opening::{OpeningKind, OpeningParams, OpeningProfile};
use crate::grid::edit::verbs::room::{Footprint, RoomTarget};
use crate::grid::edit::verbs::stairs::{lay_stairs, RiserOption, StairsParams};
use crate::grid::edit::verbs::terrain::{lay_terrain, Edge, TerrainParams};
use crate::grid::edit::CellRect;
use crate::grid::mesh::{
    ArchProfile, ArchSpec, GridMesh, ImportProxy, ImportedFace, ImportedMesh, MeshAsset, MeshKind,
    RubbleSpec,
};
use crate::grid::plane::{GridPlane, PlaneCollision, PlaneUvMode};
use crate::grid::room::GridRoom;
use crate::grid::sector::WallEdge;
use crate::grid::units::SECTOR_UNITS;
use crate::grid::world::{GridWorld, SceneWorld};
use crate::{DoorDirection, DoorLock, NodeId, NodeKind, ResourceId, Scene, SwitchBehavior};
use crate::{ProjectDocument, SkyMode};

/// Texture names, in the order the generator example passes them.
pub const TEXTURE_NAMES: [&str; 18] = [
    "megablock_plain_q0",
    "megablock_plain_q1",
    "megablock_plain_q2",
    "megablock_plain_q3",
    "megablock_q0",
    "megablock_q1",
    "megablock_q2",
    "megablock_q3",
    "iron_panel",
    "iron_door",
    "arch_teal",
    "lamp_bright",
    "sand",
    "canal_calm",
    "rubble",
    "mud",
    "cap",
    "lever",
];

/// Where the textures are written, under the project.
pub const TEXTURE_DIR: &str = "textures/style_test";

/// The two atmospheres of the same level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Atmosphere {
    /// Perpetual twilight, sulphur haze (the owner's concept board).
    Twilight,
    /// Pale daylight (the style study).
    Daylight,
}

struct Mats {
    /// The plain megablock image, cut in four quadrants (q0 top left, q1 top
    /// right, q2 bottom left, q3 bottom right) that meet when wrapped.
    plain: [Option<ResourceId>; 4],
    /// The accent image (the sheared hole), the same way.
    accent: [Option<ResourceId>; 4],
    iron_panel: Option<ResourceId>,
    iron_door: Option<ResourceId>,
    arch_teal: Option<ResourceId>,
    lamp_bright: Option<ResourceId>,
    sand: Option<ResourceId>,
    canal_calm: Option<ResourceId>,
    rubble: Option<ResourceId>,
    mud: Option<ResourceId>,
    cap: Option<ResourceId>,
    lever: Option<ResourceId>,
}

/// Lay a megablock image across a room's outside walls at 32 texels a
/// sector: one image is 4 sectors wide and 4 tall, so each wall face shows a
/// 2 by 2 sector quadrant. Vertically the room gets bands every 2048 units
/// (a quadrant row each, alternating top and bottom rows, which meet when the
/// image wraps); horizontally each wall segment picks the left or the right
/// column set by its position along the wall (two sectors each).
fn megablock_walls(world: &mut GridWorld, room: usize, q: [Option<ResourceId>; 4]) {
    use crate::grid::bands::{Anchor, BandSet};
    use crate::grid::overrides::{Axis, EdgeKey, SEGMENT_CLICKS};
    let mut sets = Vec::new();
    for col in 0..2 {
        let (top, bottom) = (q[col], q[2 + col]);
        let mut set = BandSet::single(format!("block column {col}"), top);
        for i in 1..=6 {
            set.insert_line(i - 1, Anchor::Bottom(2048 * i as i32));
        }
        // The wall's bottom sits on a multiple of 4096 plus 2048: the first
        // band is a top row, then bottom, top, and so on.
        for b in 0..7 {
            set.set_band_material(b, if b % 2 == 0 { top } else { bottom });
        }
        sets.push(set);
    }
    let mut cells: BTreeSet<[i32; 2]> = BTreeSet::new();
    for (key, _) in world.rooms[room].leaves() {
        let (a, b) = key.sector_range();
        for z in a[1]..=b[1] {
            for x in a[0]..=b[0] {
                cells.insert([x, z]);
            }
        }
    }
    let c = i32::from(SEGMENT_CLICKS);
    world.rooms[room].band_sets = sets;
    for &[x, z] in &cells {
        for side in [
            WallEdge::North,
            WallEdge::East,
            WallEdge::South,
            WallEdge::West,
        ] {
            let (n, key, along, positive) = match side {
                WallEdge::North => (
                    [x, z - 1],
                    EdgeKey {
                        x: x * c,
                        z: z * c,
                        axis: Axis::X,
                        len: SEGMENT_CLICKS,
                    },
                    x,
                    true,
                ),
                WallEdge::South => (
                    [x, z + 1],
                    EdgeKey {
                        x: x * c,
                        z: (z + 1) * c,
                        axis: Axis::X,
                        len: SEGMENT_CLICKS,
                    },
                    x,
                    false,
                ),
                WallEdge::East => (
                    [x + 1, z],
                    EdgeKey {
                        x: (x + 1) * c,
                        z: z * c,
                        axis: Axis::Z,
                        len: SEGMENT_CLICKS,
                    },
                    z,
                    true,
                ),
                _ => (
                    [x - 1, z],
                    EdgeKey {
                        x: x * c,
                        z: z * c,
                        axis: Axis::Z,
                        len: SEGMENT_CLICKS,
                    },
                    z,
                    false,
                ),
            };
            if cells.contains(&n) {
                continue;
            }
            let group = (along.div_euclid(2)).rem_euclid(2);
            let col = if positive { group } else { 1 - group };
            world.rooms[room]
                .edge_overrides
                .edit(key, |e| e.band_set = Some(col as u8));
        }
    }
}

/// What the build placed, for the report and the tests.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// Room names by index.
    pub rooms: Vec<String>,
    /// Room the player starts in and the sector.
    pub spawn_room: usize,
    /// The sector the player starts on.
    pub spawn: [i32; 2],
    /// Yaw of the spawn, degrees (180 faces north).
    pub spawn_yaw: f32,
    /// Notes on what was left out.
    pub notes: Vec<String>,
}

fn rect(a: [i32; 2], b: [i32; 2]) -> CellRect {
    CellRect::from_corners(a, b)
}

fn cells_of(r: CellRect) -> BTreeSet<[i32; 2]> {
    r.cells().collect()
}

/// A mass: a hollow box seen from outside, `h` grid units tall.
fn mass(
    world: &mut GridWorld,
    name: &str,
    a: [i32; 2],
    b: [i32; 2],
    base: i32,
    h: i32,
    wall: Option<ResourceId>,
    ceiling: Option<ResourceId>,
) -> Result<usize, String> {
    let mut p = BuildingParams::new(RoomTarget::New(name.into()), Footprint::Rect(rect(a, b)));
    p.base = base;
    p.storeys = 1;
    p.storey_height = h;
    p.stair = false;
    p.roof_parapet = None;
    p.door_sides = vec![];
    p.window_every = 0;
    p.floor_material = wall;
    p.wall_material = wall;
    p.ceiling_material = ceiling;
    build_building(world, &p).map_err(|r| format!("{name}: {}", r.describe()))?;
    Ok(world.rooms.len() - 1)
}

/// Lathe a solid of revolution around the vertical axis: `rings` are
/// `(radius, height)` from the bottom up, the last one a point when its
/// radius is 0. Eight sides. Faces point outward.
fn lathe(rings: &[(i16, i16)], sides: usize) -> ImportedMesh {
    let mut points: Vec<[i16; 3]> = Vec::new();
    let mut index: Vec<Vec<u16>> = Vec::new();
    for &(r, y) in rings {
        if r == 0 {
            index.push(vec![points.len() as u16; sides]);
            points.push([0, y, 0]);
            continue;
        }
        let mut ring = Vec::new();
        for k in 0..sides {
            // Octagon corners from integer tables (no float in level data).
            const C: [i32; 8] = [1000, 707, 0, -707, -1000, -707, 0, 707];
            const S: [i32; 8] = [0, 707, 1000, 707, 0, -707, -1000, -707];
            let x = (i32::from(r) * C[k % 8] / 1000) as i16;
            let z = (i32::from(r) * S[k % 8] / 1000) as i16;
            ring.push(points.len() as u16);
            points.push([x, y, z]);
        }
        index.push(ring);
    }
    let mut faces = Vec::new();
    let uv = [[0, 0], [32, 0], [32, 32], [0, 32]];
    for w in index.windows(2) {
        for k in 0..sides {
            let n = (k + 1) % sides;
            let (a, b, c, d) = (w[0][k], w[0][n], w[1][n], w[1][k]);
            let quad = ImportedFace {
                v: [a, d, c, b],
                n: 4,
                slot: 0,
                uv,
            };
            if c == d {
                faces.push(ImportedFace {
                    v: [a, d, b, 0],
                    n: 3,
                    slot: 0,
                    uv,
                });
            } else {
                faces.push(quad);
            }
        }
    }
    ImportedMesh {
        source: "style_test lathe".into(),
        points,
        faces,
        slot_names: Vec::new(),
        proxy: ImportProxy::None,
    }
}

fn place(
    world: &mut GridWorld,
    room: usize,
    asset: u16,
    position: [i32; 3],
    rotation: [i32; 3],
    material: Option<ResourceId>,
    glow: bool,
) -> Result<(), String> {
    let mut mesh = GridMesh::new(0, asset, position);
    mesh.rotation = rotation;
    mesh.fullbright = glow;
    for slot in 0..4 {
        mesh.set_material(slot, material);
    }
    place_mesh(world, room, mesh).map_err(|e| format!("mesh: {e:?}"))?;
    Ok(())
}

/// Sky, fog and light of one atmosphere.
struct Look {
    top: [u8; 3],
    horizon: [u8; 3],
    ambient: [u8; 3],
    gather_sky: [u8; 3],
    sun_lit: [u16; 3],
    sun_shade: [u16; 3],
    sun_dir: [i16; 3],
    lamp: [u8; 3],
    far_ambient: [u8; 3],
}

impl Atmosphere {
    fn look(self) -> Look {
        match self {
            Self::Twilight => Look {
                top: [74, 82, 100],
                horizon: [186, 168, 96],
                ambient: [124, 122, 110],
                gather_sky: [118, 112, 98],
                sun_lit: [274, 252, 170],
                sun_shade: [196, 180, 236],
                sun_dir: [330, 300, 520],
                lamp: [255, 178, 62],
                far_ambient: [118, 122, 112],
            },
            Self::Daylight => Look {
                top: [111, 133, 145],
                horizon: [185, 180, 160],
                ambient: [122, 122, 116],
                gather_sky: [126, 134, 138],
                sun_lit: [270, 256, 196],
                sun_shade: [204, 192, 238],
                sun_dir: [330, 300, 520],
                lamp: [255, 178, 62],
                far_ambient: [118, 122, 114],
            },
        }
    }
}

/// Build the whole scene into `world` and `scene`.
fn build(
    world: &mut GridWorld,
    scene: &mut Scene,
    m: &Mats,
    atm: Atmosphere,
    spawn: ([i32; 2], f32),
    perch: bool,
) -> Result<Plan, String> {
    let mut plan = Plan::default();
    // Floor of the canal; masses sit on it, sand laps their feet.
    let floor = 2048;
    // ---- rock rims: the canal's walls ---------------------------------------
    let calm = m.canal_calm;
    mass(world, "West rim", [-1, -5], [-1, 27], 0, 11264, calm, calm)?;
    mass(world, "East rim", [12, -5], [12, 27], 0, 11264, calm, calm)?;
    mass(world, "North rim", [-1, -8], [12, -6], 0, 12288, calm, calm)?;
    mass(world, "South rim", [-1, 28], [12, 29], 0, 12288, calm, calm)?;
    // ---- the ruin: unequal heights, the gateway mass tall --------------------
    let w = m.plain[0];
    let wing_w = mass(
        world,
        "Wing west",
        [1, 2],
        [2, 4],
        floor,
        3584,
        m.accent[0],
        m.cap,
    )?;
    let pil_l = mass(
        world,
        "Pilaster west",
        [3, 2],
        [4, 4],
        floor,
        8192,
        w,
        m.cap,
    )?;
    let gate_block = mass(world, "Gate block", [5, 2], [8, 4], floor, 6656, w, m.cap)?;
    let pil_r = mass(
        world,
        "Pilaster east",
        [9, 2],
        [10, 4],
        floor,
        5120,
        w,
        m.cap,
    )?;
    let wing_e = mass(world, "Wing east", [11, 3], [11, 4], floor, 2560, w, m.cap)?;
    megablock_walls(world, wing_w, m.accent);
    for r in [pil_l, gate_block, pil_r, wing_e] {
        megablock_walls(world, r, m.plain);
    }
    // The drum behind: dark iron, taller than everything near.
    {
        let mut p = BuildingParams::new(
            RoomTarget::New("Tower".into()),
            Footprint::Octagon {
                rect: rect([5, -4], [8, -1]),
                cut: 1,
            },
        );
        p.base = floor;
        p.storeys = 1;
        p.storey_height = 8704;
        p.stair = false;
        p.roof_parapet = None;
        p.door_sides = vec![];
        p.window_every = 0;
        p.floor_material = m.iron_panel;
        p.wall_material = m.iron_panel;
        p.ceiling_material = m.iron_panel;
        build_building(world, &p).map_err(|r| format!("tower: {}", r.describe()))?;
    }
    // The cube under the dome.
    let dome_cube = mass(
        world,
        "Dome cube",
        [9, -3],
        [11, -1],
        floor,
        3584,
        m.cap,
        m.cap,
    )?;
    // Two distant masses at the canal's far end, in the same stone.
    let far_w = mass(
        world,
        "Far mass west",
        [2, 24],
        [5, 26],
        floor,
        7168,
        w,
        m.cap,
    )?;
    let far_e = mass(
        world,
        "Far mass east",
        [7, 26],
        [10, 27],
        floor,
        4608,
        w,
        m.cap,
    )?;
    for r in [far_w, far_e] {
        megablock_walls(world, r, m.plain);
    }
    // ---- the canal floor ---------------------------------------------------
    let mut taken: BTreeSet<[i32; 2]> = BTreeSet::new();
    for r in &world.rooms {
        for (key, _) in r.leaves() {
            let (a, b) = key.sector_range();
            for z in a[1]..=b[1] {
                for x in a[0]..=b[0] {
                    taken.insert([x, z]);
                }
            }
        }
    }
    let area: BTreeSet<[i32; 2]> = cells_of(rect([0, -5], [11, 27]))
        .into_iter()
        .filter(|c| !taken.contains(c))
        .collect();
    let canal = {
        let room = world.rooms.len();
        let mut r = GridRoom::new_derived("Canal", 1, 1);
        r.defaults.wall_material = m.canal_calm;
        r.defaults.open_wall = 0;
        world.rooms.push(r);
        let mut t = TerrainParams::new(room, area.clone(), 1);
        t.base = Some(floor);
        t.create = true;
        t.amplitude_clicks = 0;
        t.follow_ceiling = false;
        t.edge = Edge::Cliff;
        t.material = m.sand;
        lay_terrain(world, &t).map_err(|e| e.describe())?;
        room
    };
    // Dunes, kept off the forecourt and the stair column.
    {
        let dunes_area: BTreeSet<[i32; 2]> = cells_of(rect([1, 14], [11, 20]))
            .into_iter()
            .filter(|c| area.contains(c))
            .collect();
        let mut t = TerrainParams::new(canal, dunes_area, 7);
        t.amplitude_clicks = 1;
        t.wavelength = 6;
        t.follow_ceiling = false;
        t.edge = Edge::Blend(3);
        t.material = m.sand;
        lay_terrain(world, &t).map_err(|r| format!("dunes: {}", r.describe()))?;
    }
    // The mud pool: flush with the sand (a step down read as a hole in the
    // frames), told apart by its texture alone.
    {
        let pool: BTreeSet<[i32; 2]> = cells_of(rect([8, 9], [10, 12]))
            .into_iter()
            .filter(|c| area.contains(c))
            .collect();
        let mut t = TerrainParams::new(canal, pool, 3);
        t.base = Some(floor);
        t.create = true;
        t.amplitude_clicks = 0;
        t.follow_ceiling = false;
        t.keep_walkable = false;
        t.edge = Edge::Cliff;
        t.material = m.mud;
        lay_terrain(world, &t).map_err(|r| format!("mud: {}", r.describe()))?;
    }
    // The long straight stair along the west flank, climbing north.
    let path: Vec<[i32; 2]> = (6..=21).rev().map(|z| [0, z]).collect();
    lay_stairs(
        world,
        canal,
        &path,
        StairsParams {
            riser: RiserOption::Click,
            ramp: false,
            width: 1,
            follow_ceiling: false,
            wall_material: m.iron_panel,
        },
        3584,
    )
    .ok_or("the stair has no floor to start from")?;
    for &[x, z] in &path {
        if let Some([lx, lz]) = world.rooms[canal].local([x, z]) {
            if let Some(s) = world.rooms[canal].sector_mut(lx, lz) {
                if let Some(f) = s.ground_floor_mut() {
                    f.tri_a.material = m.iron_panel;
                    if let Some(b) = f.tri_b.as_mut() {
                        b.material = m.iron_panel;
                    }
                }
            }
        }
    }
    // ---- the gateway and its iron slab -------------------------------------
    doorway_with_door(
        world,
        scene,
        &DoorwayDoor {
            opening: OpeningParams {
                profile: OpeningProfile::Rectangular,
                room: gate_block,
                cell: [5, 4],
                side: WallEdge::South,
                from: 20,
                to: 36,
                kind: OpeningKind::Door,
                top: 4096,
                material: m.plain[0],
            },
            name: "gate".into(),
            material: m.iron_door,
            direction: DoorDirection::Up,
            ticks: 90,
            lock: DoorLock::Unlocked,
            refusal: String::new(),
            persistent: true,
            lever: None,
        },
    )
    .map_err(|r| format!("gateway: {}", r.describe()))?;
    let scale = world.scale;
    let ed = move |gu: i32| scale.to_editor(gu) as f32;
    super::lever::place_lever(
        scene,
        &super::lever::LeverSpec {
            name: "gate lever",
            wall_at: [
                ed(9 * SECTOR_UNITS + SECTOR_UNITS / 2),
                ed(floor),
                ed(5 * SECTOR_UNITS),
            ],
            facing: DoorDirection::South,
            target: "gate",
            behavior: SwitchBehavior::OneShot,
            persistent: true,
            radius: 760,
            material: m.iron_panel,
            handle_material: m.lever,
        },
    );
    // ---- lit slots in the wall (full-bright planes) ------------------------
    let face_z = 5 * SECTOR_UNITS + 12;
    for (x, y, w, h) in [
        (3 * SECTOR_UNITS + 512, floor + 2560, 1536, 384),
        (10 * SECTOR_UNITS, floor + 1792, 1280, 320),
    ] {
        let mut p = GridPlane::new([x, y, face_z], [w, h]);
        p.material = m.lamp_bright;
        p.uv_mode = PlaneUvMode::Stretch;
        p.fullbright = true;
        add_plane(world, canal, p).map_err(|e| format!("lamp slot: {e:?}"))?;
    }
    // ---- the catwalk at the stair's head -----------------------------------
    {
        let top = floor + 3584;
        let mut p = GridPlane::new(
            [3 * SECTOR_UNITS + 512, top, 5 * SECTOR_UNITS + 512],
            [7 * SECTOR_UNITS, SECTOR_UNITS],
        );
        p.rotation = [0, 90, 0];
        p.material = m.iron_panel;
        p.collision = PlaneCollision::Walkable;
        p.double_sided = true;
        add_plane(world, canal, p).map_err(|e| format!("catwalk: {e:?}"))?;
    }
    // Sand and rubble piled against the wall's foot: a low slope of rubble
    // texture along the two rows at the base.
    {
        let foot: BTreeSet<[i32; 2]> = cells_of(rect([1, 5], [11, 6]))
            .into_iter()
            .filter(|c| {
                area.contains(c) && c[1] != 5
                    || c[1] == 5 && area.contains(c) && !(5..=8).contains(&c[0])
            })
            .collect();
        let mut t = TerrainParams::new(canal, foot.clone(), 11);
        t.amplitude_clicks = 1;
        t.wavelength = 3;
        t.follow_ceiling = false;
        t.edge = Edge::Blend(2);
        t.material = m.rubble;
        lay_terrain(world, &t).map_err(|r| format!("rubble slope: {}", r.describe()))?;
        for &c in &foot {
            if let Some([lx, lz]) = world.rooms[canal].local(c) {
                if let Some(f) = world.rooms[canal]
                    .sector_mut(lx, lz)
                    .and_then(|s| s.ground_floor_mut())
                {
                    f.tri_a.material = m.rubble;
                    if let Some(b) = f.tri_b.as_mut() {
                        b.material = m.rubble;
                    }
                }
            }
        }
    }
    // A dark iron gantry bolted to the east pilaster's face: two posts, a
    // diagonal stringer and a platform (decor planes; the catwalk and the
    // stair are the walkable iron).
    for (x, y, w, h, roll) in [
        (9 * SECTOR_UNITS + 160, floor + 2200, 280, 4400, 0),
        (10 * SECTOR_UNITS + 800, floor + 2200, 280, 4400, 0),
        (10 * SECTOR_UNITS, floor + 1500, 2900, 560, 28),
    ] {
        let mut p = GridPlane::new([x, y, 5 * SECTOR_UNITS + 40], [w, h]);
        p.rotation = [0, 0, roll];
        p.material = m.iron_panel;
        p.double_sided = true;
        add_plane(world, canal, p).map_err(|e| format!("gantry: {e:?}"))?;
    }
    {
        let mut p = GridPlane::new(
            [10 * SECTOR_UNITS, floor + 2900, 5 * SECTOR_UNITS + 380],
            [2304, 700],
        );
        p.rotation = [0, 90, 0];
        p.material = m.iron_panel;
        p.collision = PlaneCollision::Walkable;
        p.double_sided = true;
        add_plane(world, canal, p).map_err(|e| format!("gantry deck: {e:?}"))?;
    }
    // ---- dressing meshes ---------------------------------------------------
    let arch = add_asset(
        world,
        MeshAsset::new(
            "floating arch",
            MeshKind::Arch(ArchSpec {
                profile: ArchProfile::Round,
                span: 3072,
                rise: 1536,
                spring: 640,
                thickness: 512,
                depth: 896,
                segments: 4,
                perch: true,
            }),
        ),
    )
    .map_err(|e| format!("arch: {e:?}"))?;
    // One over the gateway (the way over the wall), one mid-canal, one off
    // the west flank.
    place(
        world,
        canal,
        arch,
        [6 * SECTOR_UNITS + 512, floor + 6656 + 768, 6 * SECTOR_UNITS],
        [0, 0, 0],
        m.arch_teal,
        true,
    )?;
    // The crown of the arch over the gate, 9344 up: a small walkable plane
    // there so the player can be placed perched (the real route is the hook).
    let crown = [
        6 * SECTOR_UNITS + 512,
        floor + 6656 + 768 + 640 + 1536 + 512,
        6 * SECTOR_UNITS,
    ];
    if perch {
        let mut p = GridPlane::new(crown, [768, 768]);
        p.rotation = [0, 90, 0];
        p.material = m.arch_teal;
        p.collision = PlaneCollision::Walkable;
        p.double_sided = true;
        add_plane(world, canal, p).map_err(|e| format!("perch: {e:?}"))?;
    }
    place(
        world,
        canal,
        arch,
        [9 * SECTOR_UNITS, floor + 2304, 10 * SECTOR_UNITS],
        [30, 0, 0],
        m.arch_teal,
        true,
    )?;
    place(
        world,
        canal,
        arch,
        [2 * SECTOR_UNITS, floor + 4864, 9 * SECTOR_UNITS],
        [100, 0, 0],
        m.arch_teal,
        true,
    )?;
    let rubble = add_asset(
        world,
        MeshAsset::new(
            "base rubble",
            MeshKind::Rubble(RubbleSpec {
                size: 2048,
                pieces: 5,
                seed: 3,
            }),
        ),
    )
    .map_err(|e| format!("rubble: {e:?}"))?;
    for (x, z) in [(1, 6), (7, 6), (11, 6), (4, 9)] {
        place(
            world,
            canal,
            rubble,
            [x * SECTOR_UNITS + 512, floor - 128, z * SECTOR_UNITS],
            [0, 0, 0],
            m.rubble,
            false,
        )?;
    }
    // The dome on its cube.
    let dome = add_asset(
        world,
        MeshAsset::new(
            "dome",
            MeshKind::Imported(lathe(
                &[
                    (2000, 0),
                    (1960, 400),
                    (1800, 800),
                    (1480, 1160),
                    (1000, 1440),
                    (500, 1640),
                    (0, 1700),
                ],
                8,
            )),
        ),
    )
    .map_err(|e| format!("dome: {e:?}"))?;
    place(
        world,
        dome_cube,
        dome,
        [
            10 * SECTOR_UNITS + 512,
            floor + 3584,
            -2 * SECTOR_UNITS + 512,
        ],
        [0, 0, 0],
        m.cap,
        false,
    )?;
    // A far alien mushroom on the north rim, a thin stem and a cap.
    let mushroom = add_asset(
        world,
        MeshAsset::new(
            "mushroom",
            MeshKind::Imported(lathe(
                &[
                    (180, 0),
                    (160, 7000),
                    (260, 14000),
                    (240, 15800),
                    (2400, 16100),
                    (2600, 16500),
                    (1800, 17300),
                    (0, 17800),
                ],
                8,
            )),
        ),
    )
    .map_err(|e| format!("mushroom: {e:?}"))?;
    let north = world
        .rooms
        .iter()
        .position(|r| r.name == "North rim")
        .ok_or("north rim")?;
    place(
        world,
        north,
        mushroom,
        [8 * SECTOR_UNITS, 12288, -7 * SECTOR_UNITS],
        [0, 0, 0],
        m.arch_teal,
        true,
    )?;
    // ---- light, fog, ambient ------------------------------------------------
    let look = atm.look();
    for r in &mut world.rooms {
        r.fog = super::room::GridFog {
            enabled: true,
            color: look.horizon,
            start: 10 * SECTOR_UNITS,
            falloff: 16 * SECTOR_UNITS,
        };
        r.ambient = look.ambient;
        if r.name.starts_with("Far mass") {
            // Fog is a vertex depth cue (it scales a textured polygon, it does
            // not wash it toward the haze), so the far masses are lit to the
            // haze colour instead: pale cap stone at a haze ambient.
            r.ambient = look.far_ambient;
        }
    }
    plan.rooms = world.rooms.iter().map(|r| r.name.clone()).collect();
    // The amber glow of the lit slots on the ground in front of the wall.
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(1024)
        .max(1) as f32;
    let radius = |sectors: f32| sectors * world.scale.sector_editor_units() as f32 / unit;
    {
        // A cool pale lamp in front of the gateway so the teal band reads.
        let id = scene.add_node(
            NodeId::ROOT,
            "Gate lamp",
            NodeKind::PointLight {
                color: [200, 240, 236],
                intensity: 0.55,
                radius: radius(6.0),
            },
        );
        if let Some(n) = scene.node_mut(id) {
            n.transform.translation = [
                ed(6 * SECTOR_UNITS + 512),
                ed(floor + 2600),
                ed(7 * SECTOR_UNITS),
            ];
        }
    }
    for (name, x, y, z, rad) in [
        (
            "Slot lamp west",
            3 * SECTOR_UNITS + 512,
            floor + 2560,
            6 * SECTOR_UNITS,
            5.0f32,
        ),
        (
            "Slot lamp east",
            10 * SECTOR_UNITS,
            floor + 1792,
            6 * SECTOR_UNITS,
            4.0,
        ),
    ] {
        let id = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::PointLight {
                color: look.lamp,
                intensity: 0.5,
                radius: radius(rad),
            },
        );
        if let Some(n) = scene.node_mut(id) {
            n.transform.translation = [ed(x), ed(y), ed(z)];
        }
    }
    // ---- the player -----------------------------------------------------------
    let (cell, yaw) = spawn;
    plan.spawn = cell;
    plan.spawn_yaw = yaw;
    plan.spawn_room = canal;
    let player = scene
        .node(NodeId::ROOT)
        .map(|r| r.children.clone())
        .unwrap_or_default();
    for id in player {
        let is_player = scene.node(id).is_some_and(|n| {
            matches!(n.kind, NodeKind::Entity)
                && n.children.iter().any(|c| {
                    matches!(
                        scene.node(*c).map(|k| &k.kind),
                        Some(NodeKind::CharacterController { player: true, .. })
                    )
                })
        });
        if is_player {
            // A vista camera: further back and higher, looking up the wall.
            let kids = scene
                .node(id)
                .map(|n| n.children.clone())
                .unwrap_or_default();
            for c in kids {
                if let Some(NodeKind::Camera { settings }) = scene.node_mut(c).map(|k| &mut k.kind)
                {
                    settings.distance = 7000;
                    settings.height = 3200;
                    settings.target_height = 2600;
                    settings.fov_y_degrees = 48;
                }
            }
            let r = &world.rooms[canal];
            let top = r
                .local(cell)
                .and_then(|[x, z]| r.sector(x, z))
                .and_then(|s| s.ground_floor())
                .map_or(0, |f| f.heights.into_iter().max().unwrap_or(0));
            if let Some(n) = scene.node_mut(id) {
                n.transform.translation = if perch {
                    [ed(crown[0]), ed(crown[1]) + 64.0, ed(crown[2])]
                } else {
                    [
                        ed(cell[0] * SECTOR_UNITS + SECTOR_UNITS / 2),
                        ed(r.y_offset + top) + 64.0,
                        ed(cell[1] * SECTOR_UNITS + SECTOR_UNITS / 2),
                    ]
                };
                n.transform.rotation_degrees = [0.0, yaw, 0.0];
            }
        }
    }
    plan.notes
        .push("canal bend left out: one straight canal".into());
    Ok(plan)
}

fn add_materials(project: &mut ProjectDocument) -> Mats {
    let ids: Vec<Option<ResourceId>> = TEXTURE_NAMES
        .iter()
        .map(|name| {
            let title = format!("Style test / {name}");
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
        plain: [ids[0], ids[1], ids[2], ids[3]],
        accent: [ids[4], ids[5], ids[6], ids[7]],
        iron_panel: ids[8],
        iron_door: ids[9],
        arch_teal: ids[10],
        lamp_bright: ids[11],
        sand: ids[12],
        canal_calm: ids[13],
        rubble: ids[14],
        mud: ids[15],
        cap: ids[16],
        lever: ids[17],
    }
}

/// Turn `project` (a base with a player, camera, sky and UI) into the style
/// test in place.
pub fn into_style_test(
    project: &mut ProjectDocument,
    atm: Atmosphere,
    spawn: ([i32; 2], f32),
    perch: bool,
) -> Result<Plan, String> {
    super::base_project::strip_base(project, 0);
    let mats = add_materials(project);
    let look = atm.look();
    let mut world = GridWorld {
        scale: super::starter::STARTER_SCALE,
        texels_per_sector: super::units::SectorTexels::new(32).ok_or("texels")?,
        rooms: Vec::new(),
        shading: super::light::FacingShade {
            up: 256,
            down: 128,
            side_x: 236,
            side_z: 256,
            sun: super::light::SunTint {
                dir: look.sun_dir,
                lit: look.sun_lit,
                shade: look.sun_shade,
            },
        },
        // Flat ambient, the sun tint and the lamps: the gather would close the
        // canal's light in with its tall rims.
        global_light: super::lighting::GlobalLight {
            enabled: false,
            sky: Some(look.gather_sky),
            ..super::lighting::GlobalLight::DEFAULT
        },
        stream: super::world::StreamRadii {
            draw_far: 0,
            ..super::world::StreamRadii::DEFAULT
        },
        mesh_assets: Vec::new(),
        level_names: Default::default(),
        palette_pins: Vec::new(),
    };
    let scene = project.active_scene_mut();
    let plan = build(&mut world, scene, &mats, atm, spawn, perch)?;
    if let Some(root) = scene.node_mut(NodeId::ROOT) {
        if let NodeKind::World {
            sky,
            culling,
            camera,
            ..
        } = &mut root.kind
        {
            // A vista camera: further back and higher, looking up the wall.
            camera.distance = 5000;
            camera.height = 2000;
            camera.target_height = 2600;
            sky.mode = SkyMode::Panorama;
            // The base project draws its sky only through sky surfaces: a
            // grid world with none shows the clear colour. Always draws it.
            sky.visibility = crate::SkyVisibility::Always;
            sky.texture = None;
            sky.top_color = look.top;
            sky.horizon_color = look.horizon;
            sky.lower_color = look.horizon;
            sky.horizon_percent = 60;
            sky.horizon_thickness_percent = 6;
            sky.horizon_glow_percent = 0;
            sky.sun_enabled = false;
            sky.mountain_height_percent = 0;
            sky.match_room_fog = false;
            sky.cloud_layer.enabled = false;
            culling.draw_distance = 40 * 1024;
        }
    }
    scene.world = SceneWorld::Grid(world);
    project.editor_camera = crate::EditorCameraState::default();
    project.editor_viewport = crate::EditorViewportState::default();
    Ok(plan)
}

/// Create the project at `out_dir` from the base in `base_dir`, with the
/// textures `(name, psxt bytes)` written beside it.
pub fn create_project(
    base_dir: &Path,
    out_dir: &Path,
    name: &str,
    atm: Atmosphere,
    spawn: ([i32; 2], f32),
    perch: bool,
    textures: &[(&str, &[u8])],
) -> Result<(PathBuf, Plan), String> {
    let mut project = ProjectDocument::load_from_path(base_dir.join("project.ron"))
        .map_err(|e| format!("base project: {e}"))?;
    let plan = into_style_test(&mut project, atm, spawn, perch)?;
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
