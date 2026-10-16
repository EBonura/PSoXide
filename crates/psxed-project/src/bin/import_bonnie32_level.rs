//! Import a bonnie-32 sector-grid level into a BSP brush project.
//!
//! Usage:
//!   import-bonnie32-level <level.ron> <texture-pack-dir> <out-dir> [--spawn X,Z] [--yaw DEG]
//!                         [--base <project-dir>] [--enemies N] [--perch X,Z]...
//!
//! With `--base`, the base project supplies characters, camera, sky and UI:
//! its world brushes and every top-level node except the player and the
//! first two enemies are dropped, and `<out-dir>/assets` must already link
//! to the base project's assets. Each `--perch X,Z` adds a Graybox Reach arch
//! perch (a `HookPoint`) over that sector. Without it, the level gets the characterless
//! debug controller.
//!
//! The level must be plain RON (bonnie-32 saves brotli; decompress first).
//! Every authored floor, ceiling and wall surface becomes a thin convex brush
//! that sits behind the visible side of that surface, so the cook's CSG and
//! rectangle merging see the same surfaces the bonnie-32 renderer drew.
//! Coordinates scale by 9/16: bonnie's 1800-unit player becomes ~1012 units,
//! matching the editor's 1024-unit character, and a 1024 sector becomes 576.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use psxed_project::brush::{Brush, BrushContents, BrushFace, FaceUv, Plane};
use psxed_project::{
    MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId,
    SceneWorldLayer,
};
use psxed_tex::{convert, Config, CropMode, PsxtDepth, Resampler};
use psxed_project::brush_grid::{texture_key, FALLBACK_TEXTURE};
use serde::Deserialize;

const SECTOR: i32 = 1024;
const SCALE_NUM: i32 = 9;
const SCALE_DEN: i32 = 16;
const WALL_THICKNESS: i32 = 72;
const SLAB_THICKNESS: i32 = 144;
/// bonnie maps 32 texels per sector; one scaled sector is 576 units, so 18
/// units per texel against the editor's 16 (Q8 288 / 256).
const UV_SCALE_Q8: i16 = 288;
const QUAKE_SKY_SOURCE: &str = "quake_layered_sunset_4bpp.psxt";
const CUBE_SKY_SOURCE: &str = "directional_sunset_cube_4bpp.psxt";

#[derive(Deserialize, Clone, Copy)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

#[derive(Deserialize)]
struct TexRef {
    #[serde(default)]
    pack: String,
    name: String,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Default)]
enum NormalMode {
    #[default]
    Front,
    Both,
    Back,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Default)]
enum SplitDirection {
    #[default]
    NwSe,
    NeSw,
}

#[derive(Deserialize)]
struct HFace {
    heights: [f32; 4],
    texture: TexRef,
    #[serde(default)]
    split_direction: SplitDirection,
    #[serde(default)]
    heights_2: Option<[f32; 4]>,
    #[serde(default)]
    texture_2: Option<TexRef>,
}

#[derive(Deserialize)]
struct VFace {
    heights: [f32; 4],
    texture: TexRef,
    #[serde(default)]
    normal_mode: NormalMode,
}

#[derive(Deserialize, Default)]
struct Sector {
    floor: Option<HFace>,
    ceiling: Option<HFace>,
    #[serde(default)]
    walls_north: Vec<VFace>,
    #[serde(default)]
    walls_east: Vec<VFace>,
    #[serde(default)]
    walls_south: Vec<VFace>,
    #[serde(default)]
    walls_west: Vec<VFace>,
    #[serde(default)]
    walls_nwse: Vec<VFace>,
    #[serde(default)]
    walls_nesw: Vec<VFace>,
}

#[derive(Deserialize)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Deserialize)]
enum ObjectType {
    Light {
        color: Rgb,
        intensity: f32,
        radius: f32,
    },
    /// Anything else (player starts, ...): ignored.
    Spawn(serde::de::IgnoredAny),
}

/// One placed object. Mesh instances carry an `asset_id` instead of an
/// `object_type`; both are tolerated and only lights are used.
#[derive(Deserialize)]
struct Object {
    sector_x: usize,
    sector_z: usize,
    height: f32,
    #[serde(default)]
    object_type: Option<ObjectType>,
}

#[derive(Deserialize)]
struct Room {
    position: V3,
    width: usize,
    depth: usize,
    sectors: Vec<Vec<Option<Sector>>>,
    #[serde(default)]
    ambient: f32,
    #[serde(default)]
    objects: Vec<Object>,
}

#[derive(Deserialize)]
struct Level {
    rooms: Vec<Room>,
}

struct Args {
    level: PathBuf,
    textures: PathBuf,
    out: PathBuf,
    spawn: Option<(usize, usize)>,
    yaw: f32,
    base: Option<PathBuf>,
    enemies: usize,
    perches: Vec<(usize, usize)>,
}

fn parse_args() -> Args {
    let mut positional = Vec::new();
    let mut spawn = None;
    let mut yaw = 0.0;
    let mut base = None;
    let mut enemies = 2;
    let mut perches = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--spawn" => {
                let value = args.next().expect("--spawn X,Z");
                let (x, z) = value.split_once(',').expect("--spawn X,Z");
                spawn = Some((x.parse().expect("spawn x"), z.parse().expect("spawn z")));
            }
            "--yaw" => yaw = args.next().expect("--yaw DEG").parse().expect("yaw"),
            "--base" => base = Some(PathBuf::from(args.next().expect("--base DIR"))),
            "--perch" => {
                let value = args.next().expect("--perch X,Z");
                let (x, z) = value.split_once(',').expect("--perch X,Z");
                perches.push((x.parse().expect("perch x"), z.parse().expect("perch z")));
            }
            "--enemies" => enemies = args.next().expect("--enemies N").parse().expect("enemies"),
            _ => positional.push(PathBuf::from(arg)),
        }
    }
    let [level, textures, out]: [PathBuf; 3] = positional.try_into().unwrap_or_else(|_| {
        eprintln!(
            "usage: import-bonnie32-level <level.ron> <texture-pack-dir> <out-dir> [--spawn X,Z] [--yaw DEG]"
        );
        std::process::exit(2);
    });
    Args {
        level,
        textures,
        out,
        spawn,
        yaw,
        base,
        enemies,
        perches,
    }
}

fn scale(value: f32) -> i32 {
    let units = value.round() as i64;
    let scaled = units * i64::from(SCALE_NUM);
    assert!(
        scaled % i64::from(SCALE_DEN) == 0,
        "bonnie coordinate {value} is off the 16-unit grid"
    );
    (scaled / i64::from(SCALE_DEN)) as i32
}

fn main() {
    let args = parse_args();
    let source = std::fs::read_to_string(&args.level).expect("read level");
    let level: Level = ron::from_str(&source).expect("parse bonnie-32 level");
    let name = args
        .level
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "bonnie32".to_string());

    std::fs::create_dir_all(args.out.join(LEVEL_TEXTURES)).expect("create texture dir");
    let mut project = match &args.base {
        Some(base) => base_project(base, args.enemies),
        None => characterless_project(&args.out, &name),
    };
    let mut materials = MaterialTable {
        textures: args.textures.clone(),
        out: args.out.clone(),
        ids: BTreeMap::new(),
        packs: BTreeMap::new(),
    };
    let mut brushes = Vec::new();
    let mut stats = Stats::default();
    for room in &level.rooms {
        import_room(room, &mut project, &mut materials, &mut brushes, &mut stats);
    }

    let (spawn_cell, spawn_floor) = pick_spawn(&level, args.spawn);
    let room = &level.rooms[0];
    let step = SECTOR * SCALE_NUM / SCALE_DEN;
    let cell_center = |gx: usize, gz: usize| {
        [
            scale(room.position.x) + gx as i32 * step + step / 2,
            scale(room.position.z) + gz as i32 * step + step / 2,
        ]
    };
    let [sx, sz] = cell_center(spawn_cell.0, spawn_cell.1);
    let spawn = [sx, spawn_floor + 64, sz];

    if let Some(lid) = sky_lid(&project, &brushes) {
        brushes.push(lid);
    }
    let scene = project.active_scene_mut();
    scene.brushes = brushes;
    let yaw = args.yaw;
    let place = |scene: &mut psxed_project::Scene, id: NodeId, at: [i32; 3]| {
        let node = scene.node_mut(id).expect("placed node");
        node.transform.translation = at.map(|v| v as f32);
        node.transform.rotation_degrees = [0.0, yaw, 0.0];
    };
    if args.base.is_some() {
        let actors = top_level_entities(scene);
        let (forward_x, forward_z) = (yaw.to_radians().sin(), yaw.to_radians().cos());
        for (index, id) in actors.iter().enumerate() {
            let ahead = index as f32 * 4.0 * step as f32;
            let side = if index == 2 { -1.5 } else { 1.5 } * step as f32 * (index.min(1) as f32 + (index / 2) as f32);
            let at = [
                sx + (forward_x * ahead + forward_z * side) as i32,
                spawn_floor + 64,
                sz + (forward_z * ahead - forward_x * side) as i32,
            ];
            place(scene, *id, at);
        }
    } else {
        let spawn_node = scene.add_node(
            NodeId::ROOT,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        place(scene, spawn_node, spawn);
    }

    if args.base.is_some() {
        place_arch_perches(project.active_scene_mut(), &level, &args.perches, args.yaw - 180.0);
    }
    let lighting = if name == "Cathedral" {
        place_cathedral_lights(project.active_scene_mut(), room, &level);
        "cathedral lamps".to_string()
    } else {
        place_level_lights(project.active_scene_mut(), &level, &args.out)
    };
    println!("lighting: {lighting}");
    project.editor_camera.orbit_target = spawn;
    project.editor_camera.orbit_radius = 12_000;
    let project_path = args.out.join("project.ron");
    project.save_to_path(&project_path).expect("save project");

    print_map(&level, spawn_cell);
    println!(
        "{name}: {} brushes from {} floors, {} ceilings, {} walls ({} split triangles, {} rejected); {} materials; spawn cell {:?} at {:?}",
        scene_brush_count(&project),
        stats.floors,
        stats.ceilings,
        stats.walls,
        stats.split,
        stats.rejected,
        materials.ids.len(),
        spawn_cell,
        spawn
    );
    println!("wrote {}", project_path.display());
}

const LEVEL_TEXTURES: &str = "level_textures";

/// A sky-aperture slab over the whole level at its highest point. bonnie
/// levels leave roofs open; the runtime draws sky only through aperture
/// surfaces, and the lid also closes the hull for the leak check.
fn sky_lid(project: &ProjectDocument, brushes: &[Brush]) -> Option<Brush> {
    let sky = project.resources.iter().find_map(|r| match &r.data {
        ResourceData::Material(m) if m.sky_aperture => Some(r.id),
        _ => None,
    })?;
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for brush in brushes {
        let solved = brush.solve();
        for axis in 0..3 {
            min[axis] = min[axis].min(solved.min[axis]);
            max[axis] = max[axis].max(solved.max[axis]);
        }
    }
    let top = max[1].ceil() as i32;
    let mut lid = Brush::cuboid(
        [min[0].floor() as i32, top, min[2].floor() as i32],
        [max[0].ceil() as i32, top + SLAB_THICKNESS, max[2].ceil() as i32],
    );
    for face in &mut lid.faces {
        face.material = Some(sky);
    }
    Some(lid)
}

/// One authored light: bonnie grid cell, height above that cell's floor in
/// bonnie units, colour, intensity and radius in world sectors.
struct LightSpot {
    cell: (usize, usize),
    above_floor: i32,
    color: [u8; 3],
    intensity: f32,
    radius: f32,
}

/// bonnie-32 levels carry no lights. The Cathedral gets warm lamps down the
/// nave and side aisles and a cool wash over the sunken apse, so the bake
/// gives the hall depth instead of uniform fullbright.
fn place_cathedral_lights(scene: &mut psxed_project::Scene, room: &Room, _level: &Level) {
    const WARM: [u8; 3] = [255, 176, 104];
    const EMBER: [u8; 3] = [255, 150, 80];
    const COOL: [u8; 3] = [136, 168, 255];
    let mut spots = Vec::new();
    for gx in [3, 9, 15, 21] {
        for gz in [8, 16] {
            spots.push(LightSpot { cell: (gx, gz), above_floor: 2816, color: WARM, intensity: 1.25, radius: 6.0 });
        }
    }
    for gx in [4, 10, 16] {
        for gz in [2, 23] {
            spots.push(LightSpot { cell: (gx, gz), above_floor: 2048, color: EMBER, intensity: 1.3, radius: 4.0 });
        }
    }
    spots.push(LightSpot { cell: (27, 12), above_floor: 3072, color: COOL, intensity: 2.0, radius: 7.0 });
    spots.push(LightSpot { cell: (30, 8), above_floor: 2048, color: COOL, intensity: 1.4, radius: 4.0 });
    spots.push(LightSpot { cell: (30, 16), above_floor: 2048, color: COOL, intensity: 1.4, radius: 4.0 });
    spots.push(LightSpot { cell: (1, 12), above_floor: 2048, color: WARM, intensity: 1.2, radius: 4.0 });

    let step = SECTOR * SCALE_NUM / SCALE_DEN;
    for (index, spot) in spots.iter().enumerate() {
        let (gx, gz) = spot.cell;
        let floor = room
            .sectors
            .get(gx)
            .and_then(|c| c.get(gz))
            .and_then(|s| s.as_ref())
            .and_then(|s| s.floor.as_ref())
            .map(|f| f.heights[0])
            .unwrap_or_else(|| panic!("light cell {gx},{gz} has no floor"));
        let at = [
            scale(room.position.x) + gx as i32 * step + step / 2,
            scale(room.position.y) + scale(floor) + scale(spot.above_floor as f32),
            scale(room.position.z) + gz as i32 * step + step / 2,
        ];
        let id = scene.add_node(
            NodeId::ROOT,
            format!("Cathedral Light {index}"),
            NodeKind::PointLight {
                color: spot.color,
                intensity: spot.intensity,
                radius: spot.radius,
            },
        );
        scene.node_mut(id).expect("light node").transform.translation = at.map(|v| v as f32);
    }
}

/// Sectors per generated light when a level carries none (about one lamp
/// per 36 floor cells, the Cathedral nave's density).
const LATTICE_CELLS: usize = 6;
const LATTICE_WARM: [u8; 3] = [255, 176, 104];
const LATTICE_INTENSITY: f32 = 1.25;
const LATTICE_RADIUS: f32 = 6.0;
/// Lamp height above the cell floor, bonnie units, and the clearance kept
/// under a ceiling.
const LATTICE_HEIGHT: f32 = 2048.0;
const LATTICE_CEILING_GAP: f32 = 256.0;

fn average(h: &[f32; 4]) -> f32 {
    h.iter().sum::<f32>() / 4.0
}

/// Generic lighting for a level that is not the Cathedral: the ambient from
/// the rooms (written next to the project, where the playtest cook reads it),
/// the level's own lights when it has any, otherwise warm lamps on a lattice.
/// Returns a one-line description for the import log.
fn place_level_lights(scene: &mut psxed_project::Scene, level: &Level, out: &Path) -> String {
    let ambient = level.rooms.iter().map(|r| r.ambient).sum::<f32>() / level.rooms.len() as f32;
    let ambient = (ambient * 255.0).round().clamp(0.0, 255.0) as u8;
    std::fs::write(out.join("ambient.txt"), format!("{ambient}\n")).expect("write ambient");

    let step = SECTOR * SCALE_NUM / SCALE_DEN;
    let mut count = 0usize;
    let mut add = |scene: &mut psxed_project::Scene, at: [i32; 3], color: [u8; 3], intensity: f32, radius: f32| {
        let id = scene.add_node(
            NodeId::ROOT,
            format!("Level Light {count}"),
            NodeKind::PointLight { color, intensity, radius },
        );
        scene.node_mut(id).expect("light node").transform.translation = at.map(|v| v as f32);
        count += 1;
    };
    let cell_floor = |room: &Room, gx: usize, gz: usize| -> Option<f32> {
        let sector = room.sectors.get(gx)?.get(gz)?.as_ref()?;
        Some(average(&sector.floor.as_ref()?.heights))
    };

    let authored = level
        .rooms
        .iter()
        .any(|r| r.objects.iter().any(|o| matches!(o.object_type, Some(ObjectType::Light { .. }))));
    if authored {
        for room in &level.rooms {
            for object in &room.objects {
                let Some(ObjectType::Light { color, intensity, radius }) = &object.object_type else {
                    continue;
                };
                let floor = cell_floor(room, object.sector_x, object.sector_z).unwrap_or(0.0);
                let at = [
                    scale(room.position.x) + object.sector_x as i32 * step + step / 2,
                    scale(room.position.y) + scale(floor.round()) + scale(object.height.round()),
                    scale(room.position.z) + object.sector_z as i32 * step + step / 2,
                ];
                add(scene, at, [color.r, color.g, color.b], *intensity, radius / SECTOR as f32);
            }
        }
        return format!("ambient {ambient}, {count} authored lights");
    }

    for room in &level.rooms {
        let floor_cells: Vec<(usize, usize)> = (0..room.width)
            .flat_map(|gx| (0..room.depth).map(move |gz| (gx, gz)))
            .filter(|&(gx, gz)| cell_floor(room, gx, gz).is_some())
            .collect();
        if floor_cells.is_empty() {
            continue;
        }
        let half = LATTICE_CELLS / 2;
        let mut chosen: Vec<(usize, usize)> = Vec::new();
        for lz in (half..room.depth + half).step_by(LATTICE_CELLS) {
            for lx in (half..room.width + half).step_by(LATTICE_CELLS) {
                let best = floor_cells
                    .iter()
                    .map(|&(gx, gz)| (gx.abs_diff(lx).max(gz.abs_diff(lz)), gx, gz))
                    .min();
                if let Some((distance, gx, gz)) = best {
                    if distance <= half && !chosen.contains(&(gx, gz)) {
                        chosen.push((gx, gz));
                    }
                }
            }
        }
        if chosen.is_empty() {
            // No lattice point landed near a floor: light the cell nearest the centre.
            let (cx, cz) = (room.width / 2, room.depth / 2);
            let best = floor_cells
                .iter()
                .min_by_key(|&&(gx, gz)| gx.abs_diff(cx).pow(2) + gz.abs_diff(cz).pow(2))
                .copied()
                .expect("room has floor cells");
            chosen.push(best);
        }
        for (gx, gz) in chosen {
            let sector = room.sectors[gx][gz].as_ref().expect("lit cell exists");
            let floor = average(&sector.floor.as_ref().expect("lit cell has a floor").heights);
            let mut height = LATTICE_HEIGHT;
            if let Some(ceiling) = &sector.ceiling {
                let lowest = ceiling.heights.iter().cloned().fold(f32::MAX, f32::min);
                height = height.min((lowest - floor - LATTICE_CEILING_GAP).max(LATTICE_CEILING_GAP));
            }
            let at = [
                scale(room.position.x) + gx as i32 * step + step / 2,
                scale(room.position.y) + scale(floor.round()) + scale(height.round()),
                scale(room.position.z) + gz as i32 * step + step / 2,
            ];
            add(scene, at, LATTICE_WARM, LATTICE_INTENSITY, LATTICE_RADIUS);
        }
    }
    format!("ambient {ambient}, {count} lattice lights")
}

/// Height of a hook point above the floor under it, authored units. Graybox
/// Reach authors its one arch perch (`Archlight / Fractured ruin`) 1792 above
/// the floor, a jump and a half over the 1024 unit character.
const PERCH_HEIGHT: i32 = 1792;

/// One `HookPoint` per requested sector, at the cell centre over its floor.
/// A hook point is Graybox Reach's whole arch mechanic: the runtime draws the
/// beacon, validates range and clearance, and flies the player to it. `yaw`
/// keeps the base relation between the player's heading and the hook's.
fn place_arch_perches(
    scene: &mut psxed_project::Scene,
    level: &Level,
    cells: &[(usize, usize)],
    yaw: f32,
) {
    let room = &level.rooms[0];
    let step = SECTOR * SCALE_NUM / SCALE_DEN;
    for (index, &(gx, gz)) in cells.iter().enumerate() {
        let sector = room
            .sectors
            .get(gx)
            .and_then(|c| c.get(gz))
            .and_then(|s| s.as_ref())
            .and_then(|s| s.floor.as_ref())
            .unwrap_or_else(|| panic!("no floor at perch cell {gx},{gz}"));
        let top = sector.heights.iter().cloned().fold(f32::MIN, f32::max);
        let at = [
            scale(room.position.x) + gx as i32 * step + step / 2,
            scale(room.position.y) + scale(top) + PERCH_HEIGHT,
            scale(room.position.z) + gz as i32 * step + step / 2,
        ];
        let id = scene.add_node(NodeId::ROOT, format!("Arch Perch {}", index + 1), NodeKind::HookPoint);
        let node = scene.node_mut(id).expect("perch node");
        node.transform.translation = at.map(|v| v as f32);
        node.transform.rotation_degrees = [0.0, yaw, 0.0];
        println!("arch perch {} at cell ({gx},{gz}) -> {at:?}", index + 1);
    }
}

/// The base project minus its level: brushes, lights, props and pickups go;
/// the player and the first `enemies` enemies stay so frame costs match a real
/// playthrough. The once-per-launch world message is dropped.
fn base_project(base: &Path, enemies: usize) -> ProjectDocument {
    let mut project =
        ProjectDocument::load_from_path(base.join("project.ron")).expect("load base project");
    let scene = project.active_scene_mut();
    scene.brushes.clear();
    let root = scene.node(NodeId::ROOT).expect("world root");
    let top: Vec<NodeId> = root.children.clone();
    let mut kept_enemies = 0;
    for id in top {
        let keep = match entity_role(scene, id) {
            Some(true) => true,
            Some(false) if kept_enemies < enemies => {
                kept_enemies += 1;
                true
            }
            _ => false,
        };
        if !keep {
            scene.remove_node(id);
        }
    }
    if let Some(root) = scene.node_mut(NodeId::ROOT) {
        if let NodeKind::World { world_message, .. } = &mut root.kind {
            *world_message = None;
        }
    }
    project
}

/// `Some(true)` for the player entity, `Some(false)` for an enemy, `None`
/// for anything else.
fn entity_role(scene: &psxed_project::Scene, id: NodeId) -> Option<bool> {
    let node = scene.node(id)?;
    if !matches!(node.kind, NodeKind::Entity { .. }) {
        return None;
    }
    node.children.iter().find_map(|child| match &scene.node(*child)?.kind {
        NodeKind::CharacterController { player, .. } => Some(*player),
        _ => None,
    })
}

/// Player first, then enemies, in storage order.
fn top_level_entities(scene: &psxed_project::Scene) -> Vec<NodeId> {
    let root = scene.node(NodeId::ROOT).expect("world root");
    let mut ids: Vec<(bool, NodeId)> = root
        .children
        .iter()
        .filter_map(|id| entity_role(scene, *id).map(|player| (!player, *id)))
        .collect();
    ids.sort_by_key(|(enemy, _)| *enemy);
    ids.into_iter().map(|(_, id)| id).collect()
}

fn characterless_project(out: &Path, name: &str) -> ProjectDocument {
    std::fs::create_dir_all(out.join("assets/textures/sky")).expect("create project dirs");
    let editor_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for sky in [QUAKE_SKY_SOURCE, CUBE_SKY_SOURCE] {
        std::fs::copy(
            editor_root.join("assets/sky").join(sky),
            out.join("assets/textures/sky").join(sky),
        )
        .expect("copy sky texture");
    }
    let mut project = ProjectDocument::new(&format!("bonnie-32 {name}"));
    let hud = ProjectDocument::starter()
        .ui_scenes
        .into_iter()
        .find(|scene| scene.name == "HUD")
        .expect("default project HUD scene");
    let hud_id = hud.id;
    project.ui_scenes[0] = hud;
    project
        .scene_states
        .iter_mut()
        .find(|state| state.world == SceneWorldLayer::Gameplay)
        .expect("default gameplay state")
        .ui_scene = Some(hud_id);
    for (label, file) in [
        ("Sky: Quake Layered Sunset", QUAKE_SKY_SOURCE),
        ("Sky: Directional Sunset Cube", CUBE_SKY_SOURCE),
    ] {
        let mut material = MaterialResource::opaque(Some(format!("assets/textures/sky/{file}")));
        material.sky_aperture = true;
        project.add_resource(label, ResourceData::Material(material));
    }
    project
}

fn scene_brush_count(project: &ProjectDocument) -> usize {
    project.active_scene().brushes.len()
}

#[derive(Default)]
struct Stats {
    floors: usize,
    ceilings: usize,
    walls: usize,
    split: usize,
    rejected: usize,
}

struct MaterialTable {
    /// Either a texture-packs root holding one directory per pack, or (the
    /// Cathedral invocation) a single pack directory holding the PNGs.
    textures: PathBuf,
    out: PathBuf,
    ids: BTreeMap<String, ResourceId>,
    /// Pack each material key came from, to catch two packs sharing a name
    /// (the cook finds materials by name alone).
    packs: BTreeMap<String, String>,
}

/// `<root>/<pack>/<name>.png`, else `<root>/<name>.png`, the extension in
/// any case (the retro pack ships `.PNG`).
fn find_texture(root: &Path, pack: &str, name: &str) -> PathBuf {
    let pack_dir = root.join(pack);
    let dir = if !pack.is_empty() && pack_dir.is_dir() {
        pack_dir
    } else {
        root.to_path_buf()
    };
    for ext in ["png", "PNG"] {
        let candidate = dir.join(format!("{name}.{ext}"));
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("texture {pack}/{name}.png not found under {}", root.display());
}

/// bonnie's own stand-in for a missing texture is a checkerboard; a
/// 64x64 grey-and-magenta one is generated here so those faces still draw.
fn fallback_png() -> Vec<u8> {
    let mut image = image::RgbaImage::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let on = (x / 8 + y / 8) % 2 == 0;
        *pixel = if on {
            image::Rgba([168, 56, 168, 255])
        } else {
            image::Rgba([48, 48, 56, 255])
        };
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("encode fallback png");
    bytes.into_inner()
}

impl MaterialTable {
    fn get(&mut self, project: &mut ProjectDocument, texture: &TexRef) -> ResourceId {
        let name = texture_key(&texture.pack, &texture.name);
        let name = name.as_str();
        if let Some(id) = self.ids.get(name) {
            if name != FALLBACK_TEXTURE {
                assert_eq!(
                    self.packs.get(name).map(String::as_str),
                    Some(texture.pack.as_str()),
                    "texture {name} appears in two packs; the grid cook keys materials by name"
                );
            }
            return *id;
        }
        let bytes = if name == FALLBACK_TEXTURE {
            fallback_png()
        } else {
            let png = find_texture(&self.textures, &texture.pack, name);
            std::fs::read(&png).unwrap_or_else(|e| panic!("{}: {e}", png.display()))
        };
        self.packs.insert(name.to_string(), texture.pack.clone());
        let psxt = convert(
            &bytes,
            &Config {
                width: 64,
                height: 64,
                depth: PsxtDepth::Bit4,
                crop: CropMode::None,
                resampler: Resampler::Lanczos3,
                transparent_index_zero: false,
                clut_rows: 1,
            },
        )
        .unwrap_or_else(|e| panic!("convert {name}: {e:?}"));
        let relative = format!("{LEVEL_TEXTURES}/{}.psxt", name.to_lowercase());
        std::fs::write(self.out.join(&relative), psxt).expect("write psxt");
        let id = project.add_resource(
            name,
            ResourceData::Material(MaterialResource::opaque(Some(relative))),
        );
        self.ids.insert(name.to_string(), id);
        id
    }
}

fn import_room(
    room: &Room,
    project: &mut ProjectDocument,
    materials: &mut MaterialTable,
    brushes: &mut Vec<Brush>,
    stats: &mut Stats,
) {
    let ox = scale(room.position.x);
    let oy = scale(room.position.y);
    let oz = scale(room.position.z);
    let step = SECTOR * SCALE_NUM / SCALE_DEN;
    for gx in 0..room.width {
        for gz in 0..room.depth {
            let Some(Some(sector)) = room.sectors.get(gx).and_then(|c| c.get(gz)) else {
                continue;
            };
            let x0 = ox + gx as i32 * step;
            let z0 = oz + gz as i32 * step;
            let (x1, z1) = (x0 + step, z0 + step);
            for (face, up) in [(&sector.floor, true), (&sector.ceiling, false)] {
                let Some(face) = face else { continue };
                if up {
                    stats.floors += 1;
                } else {
                    stats.ceilings += 1;
                }
                let corners = |h: [f32; 4]| -> [[i32; 3]; 4] {
                    [
                        [x0, oy + scale(h[0]), z0],
                        [x1, oy + scale(h[1]), z0],
                        [x1, oy + scale(h[2]), z1],
                        [x0, oy + scale(h[3]), z1],
                    ]
                };
                let c1 = corners(face.heights);
                let c2 = corners(face.heights_2.unwrap_or(face.heights));
                let m1 = materials.get(project, &face.texture);
                let m2 = face
                    .texture_2
                    .as_ref()
                    .map(|t| materials.get(project, t))
                    .unwrap_or(m1);
                let normal = if up { [0.0, 1.0, 0.0] } else { [0.0, -1.0, 0.0] };
                let planar = face.heights_2.is_none() && m1 == m2 && coplanar(&c1);
                if planar {
                    push(brushes, stats, slab(&c1, normal, SLAB_THICKNESS, m1));
                } else {
                    stats.split += 1;
                    let (t1, t2) = match face.split_direction {
                        SplitDirection::NwSe => ([0, 1, 2], [0, 2, 3]),
                        SplitDirection::NeSw => ([0, 1, 3], [1, 2, 3]),
                    };
                    push(brushes, stats, slab(&t1.map(|i| c1[i]), normal, SLAB_THICKNESS, m1));
                    push(brushes, stats, slab(&t2.map(|i| c2[i]), normal, SLAB_THICKNESS, m2));
                }
            }
            // Edge endpoints in bonnie's [bottom-left, bottom-right] order and
            // the normal of its Front side (pointing into the owning sector).
            let s = std::f64::consts::FRAC_1_SQRT_2;
            let edges: [(&Vec<VFace>, [i32; 2], [i32; 2], [f64; 3]); 6] = [
                (&sector.walls_north, [x0, z0], [x1, z0], [0.0, 0.0, 1.0]),
                (&sector.walls_east, [x1, z0], [x1, z1], [-1.0, 0.0, 0.0]),
                (&sector.walls_south, [x1, z1], [x0, z1], [0.0, 0.0, -1.0]),
                (&sector.walls_west, [x0, z1], [x0, z0], [1.0, 0.0, 0.0]),
                (&sector.walls_nwse, [x0, z0], [x1, z1], [s, 0.0, -s]),
                (&sector.walls_nesw, [x1, z0], [x0, z1], [-s, 0.0, -s]),
            ];
            for (walls, a, b, front) in edges {
                for wall in walls {
                    stats.walls += 1;
                    let h = wall.heights;
                    let quad = [
                        [a[0], oy + scale(h[0]), a[1]],
                        [b[0], oy + scale(h[1]), b[1]],
                        [b[0], oy + scale(h[2]), b[1]],
                        [a[0], oy + scale(h[3]), a[1]],
                    ];
                    let visible = match wall.normal_mode {
                        NormalMode::Back => front.map(|v| -v),
                        NormalMode::Front | NormalMode::Both => front,
                    };
                    let material = materials.get(project, &wall.texture);
                    push(brushes, stats, slab(&quad, visible, WALL_THICKNESS, material));
                }
            }
        }
    }
}

fn push(brushes: &mut Vec<Brush>, stats: &mut Stats, brush: Option<Brush>) {
    match brush {
        Some(brush) => brushes.push(brush),
        None => stats.rejected += 1,
    }
}

fn coplanar(c: &[[i32; 3]; 4]) -> bool {
    let Some(plane) = Plane::from_points([c[0], c[1], c[2]]) else {
        return false;
    };
    plane.side(c[3]) == 0
}

/// Thicken a planar convex polygon into a brush that lies behind its visible
/// side. Degenerate input (zero-height walls, collinear points) returns None.
fn slab(poly: &[[i32; 3]], visible: [f64; 3], thickness: i32, material: ResourceId) -> Option<Brush> {
    let mut front: Vec<[i32; 3]> = Vec::with_capacity(poly.len());
    for p in poly {
        if front.last() != Some(p) && front.first() != Some(p) {
            front.push(*p);
        }
    }
    if front.len() < 3 {
        return None;
    }
    let offset = visible.map(|v| -(v * f64::from(thickness)).round() as i32);
    let back: Vec<[i32; 3]> = front
        .iter()
        .map(|p| [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]])
        .collect();
    let count = (front.len() * 2) as i64;
    let mut sum = [0i64; 3];
    for p in front.iter().chain(back.iter()) {
        for axis in 0..3 {
            sum[axis] += i64::from(p[axis]);
        }
    }
    let inside = sum.map(|v| (v / count) as i32);

    let mut faces = Vec::new();
    let mut add = |mut points: [[i32; 3]; 3]| -> bool {
        let Some(plane) = Plane::from_points(points) else {
            return false;
        };
        if plane.side(inside) > 0 {
            points.swap(1, 2);
        }
        faces.push(BrushFace {
            points,
            material: Some(material),
            uv: FaceUv {
                scale_q8: [UV_SCALE_Q8, UV_SCALE_Q8],
                ..FaceUv::default()
            },
            edge_repair: None,
        });
        true
    };
    if !add([front[0], front[1], front[2]]) || !add([back[0], back[1], back[2]]) {
        return None;
    }
    for i in 0..front.len() {
        let j = (i + 1) % front.len();
        add([front[i], front[j], back[j]]);
    }
    let brush = Brush {
        faces,
        contents: BrushContents::Solid,
        mover: None,
        group: None,
    };
    let solved = brush.solve();
    (solved.is_valid() && solved.within_extent(psxed_project::brush::BRUSH_EDIT_EXTENT_LIMIT))
        .then_some(brush)
}

/// Spawn on the requested cell, else on the floor cell nearest the centre of
/// the first room's floor area. Returns the cell and its scaled floor height.
fn pick_spawn(level: &Level, requested: Option<(usize, usize)>) -> ((usize, usize), i32) {
    let room = &level.rooms[0];
    let floor_at = |gx: usize, gz: usize| -> Option<i32> {
        let sector = room.sectors.get(gx)?.get(gz)?.as_ref()?;
        let floor = sector.floor.as_ref()?;
        let top = floor.heights.iter().cloned().fold(f32::MIN, f32::max);
        Some(scale(room.position.y) + scale(top))
    };
    if let Some((gx, gz)) = requested {
        let y = floor_at(gx, gz).unwrap_or_else(|| panic!("no floor at spawn cell {gx},{gz}"));
        return ((gx, gz), y);
    }
    let (mut sx, mut sz, mut n) = (0.0, 0.0, 0.0);
    for gx in 0..room.width {
        for gz in 0..room.depth {
            if floor_at(gx, gz).is_some() {
                sx += gx as f64;
                sz += gz as f64;
                n += 1.0;
            }
        }
    }
    let (cx, cz) = (sx / n, sz / n);
    let mut best = None;
    for gx in 0..room.width {
        for gz in 0..room.depth {
            if let Some(y) = floor_at(gx, gz) {
                let d = (gx as f64 - cx).powi(2) + (gz as f64 - cz).powi(2);
                if best.map_or(true, |(bd, _, _)| d < bd) {
                    best = Some((d, (gx, gz), y));
                }
            }
        }
    }
    let (_, cell, y) = best.expect("level has no floor");
    (cell, y)
}

/// Top view of the first room: floor height in 256-unit steps, `#` for
/// cells without a floor, `@` for the spawn cell. Rows are Z, columns X.
fn print_map(level: &Level, spawn: (usize, usize)) {
    let room = &level.rooms[0];
    for gz in 0..room.depth {
        let row: String = (0..room.width)
            .map(|gx| {
                if (gx, gz) == spawn {
                    return '@';
                }
                match room.sectors.get(gx).and_then(|c| c.get(gz)).and_then(|s| s.as_ref()) {
                    Some(Sector {
                        floor: Some(floor), ..
                    }) => {
                        let level = (floor.heights[0] / 256.0).round() as i32;
                        char::from_digit((level + 4).clamp(0, 35) as u32, 36).unwrap_or('?')
                    }
                    Some(_) => '#',
                    None => ' ',
                }
            })
            .collect();
        println!("  {gz:3} {row}");
    }
}
