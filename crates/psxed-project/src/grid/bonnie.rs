//! Read a bonnie-32 level and convert it into a [`GridWorld`].
//!
//! bonnie-32 is the sector-grid editor whose data model this one ports, so
//! the conversion is nearly one to one: rooms, sectors with optional floor
//! and ceiling, four corner heights, a diagonal split, stacked walls per edge
//! and two diagonal wall lists. What changes is the representation (integers
//! instead of `f32`, one owner per wall, materials as project resources).
//!
//! Levels are Brotli compressed RON; plain RON is accepted too. Anything the
//! grid document has no field for is dropped and counted in [`ImportStats`]
//! so the report says what the import lost: custom UV corners, vertex
//! colours, blend modes, transparency keys, fog cull offsets and asset
//! instances that need bonnie's asset library to resolve.
//!
//! This module is pure: textures come in through a resolver closure, so it
//! needs no project, files or images. [`super::bonnie_project`] wires it to
//! a real project.

use std::io::Read;

use serde::Deserialize;

use crate::brush::FaceUv;
use crate::ResourceId;

use super::room::{GridFog, GridRoom};
use super::sector::{GridSector, WallEdge};
use super::surface::{GridSplit, GridSurface, GridTri, GridWall, WallFacing};
use super::units::{GridScale, SECTOR_UNITS};
use super::world::GridWorld;

#[derive(Deserialize, Clone, Copy)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

/// A texture reference as bonnie names it: a pack folder and a file stem.
/// Both are empty for the "no texture" checkerboard, and the pack is
/// `_USER` for textures painted inside bonnie.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureRef {
    /// Pack folder name.
    #[serde(default)]
    pub pack: String,
    /// File stem inside the pack.
    #[serde(default)]
    pub name: String,
}

impl TextureRef {
    /// The pack bonnie uses for textures painted in its own editor.
    pub const USER_PACK: &'static str = "_USER";

    /// Whether this is the empty "no texture" reference.
    pub fn is_empty(&self) -> bool {
        self.pack.is_empty() && self.name.is_empty()
    }

    /// Whether this names a bonnie paint texture.
    pub fn is_user(&self) -> bool {
        self.pack == Self::USER_PACK
    }
}

/// Custom UV corner. Read only to know a face had one.
#[derive(Deserialize)]
struct Vec2 {
    #[allow(dead_code)]
    x: f32,
    #[allow(dead_code)]
    y: f32,
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
    texture: TextureRef,
    #[serde(default)]
    split_direction: SplitDirection,
    #[serde(default)]
    heights_2: Option<[f32; 4]>,
    #[serde(default)]
    texture_2: Option<TextureRef>,
    #[serde(default = "yes")]
    walkable: bool,
    #[serde(default)]
    uv: Option<[Vec2; 4]>,
    #[serde(default)]
    uv_2: Option<[Vec2; 4]>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct VFace {
    heights: [f32; 4],
    texture: TextureRef,
    #[serde(default)]
    normal_mode: NormalMode,
    #[serde(default = "yes")]
    solid: bool,
    #[serde(default)]
    uv: Option<[Vec2; 4]>,
}

#[derive(Deserialize, Default)]
struct Sector {
    #[serde(default)]
    floor: Option<HFace>,
    #[serde(default)]
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
enum SpawnKind {
    PlayerStart,
    #[serde(other)]
    Other,
}

/// Pre-asset-library object forms that older levels (Cave, West) still carry.
/// Current-format instances have no such field and read as `Other`.
#[derive(Deserialize, Default)]
enum LegacyObject {
    Light {
        color: Rgb,
        intensity: f32,
        radius: f32,
    },
    Spawn(SpawnKind),
    #[serde(other)]
    #[default]
    Other,
}

#[derive(Deserialize)]
struct Object {
    sector_x: usize,
    sector_z: usize,
    #[serde(default)]
    height: f32,
    #[serde(default)]
    object_type: LegacyObject,
    #[serde(default)]
    asset_id: u64,
    #[serde(default = "yes")]
    enabled: bool,
}

#[derive(Deserialize, Default)]
struct Fog {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    color: (f32, f32, f32),
    #[serde(default)]
    start: f32,
    #[serde(default)]
    falloff: f32,
}

fn default_ambient() -> f32 {
    0.5
}

#[derive(Deserialize)]
struct Room {
    #[serde(default)]
    id: usize,
    position: V3,
    width: usize,
    depth: usize,
    sectors: Vec<Vec<Option<Sector>>>,
    #[serde(default = "default_ambient")]
    ambient: f32,
    #[serde(default)]
    objects: Vec<Object>,
    #[serde(default)]
    fog: Fog,
}

#[derive(Deserialize, Default)]
struct PlayerSettings {
    #[serde(default)]
    height: f32,
}

/// A parsed bonnie-32 level.
#[derive(Deserialize)]
pub struct Level {
    rooms: Vec<Room>,
    #[serde(default)]
    player_settings: PlayerSettings,
}

/// Why a level could not be read or converted.
#[derive(Debug)]
pub enum BonnieError {
    /// The Brotli stream did not decode.
    Decompress(std::io::Error),
    /// The text is not a bonnie-32 level.
    Parse(ron::error::SpannedError),
    /// A room's origin is not on the sector lattice or is not finite.
    UnalignedRoom {
        /// Room index.
        room: usize,
    },
    /// A height is not a finite number.
    BadHeight {
        /// Room index.
        room: usize,
    },
    /// A room's sector columns do not match its declared size.
    BadRoomShape {
        /// Room index.
        room: usize,
    },
}

impl core::fmt::Display for BonnieError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Decompress(e) => write!(f, "level is neither plain RON nor Brotli: {e}"),
            Self::Parse(e) => write!(f, "not a bonnie-32 level: {e}"),
            Self::UnalignedRoom { room } => {
                write!(f, "room {room} is not positioned on whole sectors")
            }
            Self::BadHeight { room } => write!(f, "room {room} has a non-finite height"),
            Self::BadRoomShape { room } => {
                write!(
                    f,
                    "room {room} sector columns do not match its width and depth"
                )
            }
        }
    }
}

impl std::error::Error for BonnieError {}

/// Parse a level from plain RON text.
pub fn parse_level_text(text: &str) -> Result<Level, BonnieError> {
    ron::from_str(text).map_err(BonnieError::Parse)
}

/// Parse a level from file bytes: Brotli compressed RON as bonnie-32 saves
/// it, or plain RON.
pub fn parse_level(bytes: &[u8]) -> Result<Level, BonnieError> {
    let plain = bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'(');
    if plain {
        return parse_level_text(&String::from_utf8_lossy(bytes));
    }
    let mut text = String::new();
    brotli_decompressor::Decompressor::new(bytes, 4096)
        .read_to_string(&mut text)
        .map_err(BonnieError::Decompress)?;
    parse_level_text(&text)
}

impl Level {
    /// Every distinct texture the level names, sorted.
    pub fn textures(&self) -> Vec<TextureRef> {
        let mut found = std::collections::BTreeSet::new();
        for room in &self.rooms {
            for sector in room.sectors.iter().flatten().flatten() {
                for face in [&sector.floor, &sector.ceiling].into_iter().flatten() {
                    found.insert(face.texture.clone());
                    if let Some(second) = &face.texture_2 {
                        found.insert(second.clone());
                    }
                }
                for walls in [
                    &sector.walls_north,
                    &sector.walls_east,
                    &sector.walls_south,
                    &sector.walls_west,
                    &sector.walls_nwse,
                    &sector.walls_nesw,
                ] {
                    for wall in walls {
                        found.insert(wall.texture.clone());
                    }
                }
            }
        }
        found.into_iter().collect()
    }

    /// The level's own player height in bonnie units, when it records one.
    pub fn player_height(&self) -> Option<f32> {
        Some(self.player_settings.height).filter(|h| *h > 0.0)
    }

    /// Number of rooms.
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }
}

/// Scale from bonnie units to editor units for a level whose player is
/// `height` bonnie units tall.
///
/// The Cortex character is 1024 editor units tall, so the scale is about
/// `1024 / height`, kept on sixteenths so every height on bonnie's 16 unit
/// lattice stays whole. The three heights bonnie's sample levels use map to
/// the plan's choices (1800 to 9/16, 762 to 21/16, 1200 to 7/8); other
/// heights round to the nearest sixteenth within 4/16 and 32/16.
pub fn scale_for_player_height(height: f32) -> GridScale {
    match height.round() as i32 {
        1800 => GridScale { num: 9, den: 16 },
        1200 => GridScale { num: 14, den: 16 },
        762 => GridScale { num: 21, den: 16 },
        _ if height.is_finite() && height > 0.0 => GridScale {
            num: ((16.0 * 1024.0 / height).round() as i32).clamp(4, 32),
            den: 16,
        },
        _ => GridScale::DEFAULT,
    }
}

/// A light the level carries, in editor units.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedLight {
    /// Position in editor units.
    pub position: [i32; 3],
    /// Colour.
    pub color: [u8; 3],
    /// Intensity multiplier.
    pub intensity: f32,
    /// Radius in editor units.
    pub radius: i32,
    /// True when generated rather than read from the level.
    pub generated: bool,
}

/// Where the level starts the player, when it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportedSpawn {
    /// Room index.
    pub room: usize,
    /// Local sector.
    pub cell: [usize; 2],
    /// Height above the sector floor, grid units.
    pub height: i32,
}

/// What the conversion kept, generated and dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    /// Floor surfaces.
    pub floors: usize,
    /// Ceiling surfaces.
    pub ceilings: usize,
    /// Wall segments.
    pub walls: usize,
    /// Faces whose custom UV corners were dropped.
    pub custom_uv_faces: usize,
    /// Heights that are not multiples of 16 grid units.
    pub off_lattice_heights: usize,
    /// Objects that need bonnie's asset library and were dropped.
    pub unresolved_objects: usize,
    /// Lights read from the level.
    pub level_lights: usize,
    /// Lights generated because the level has none.
    pub generated_lights: usize,
}

/// A converted level.
#[derive(Clone, Debug)]
pub struct ConvertedLevel {
    /// The grid document.
    pub world: GridWorld,
    /// Lights in editor units, authored or generated.
    pub lights: Vec<ImportedLight>,
    /// The level's player start, when it has one.
    pub spawn: Option<ImportedSpawn>,
    /// What was kept and dropped.
    pub stats: ImportStats,
}

fn to_units(value: f32, room: usize, stats: &mut ImportStats) -> Result<i32, BonnieError> {
    if !value.is_finite() {
        return Err(BonnieError::BadHeight { room });
    }
    let units = value.round() as i32;
    if units % 16 != 0 {
        stats.off_lattice_heights += 1;
    }
    Ok(units)
}

fn heights4(h: [f32; 4], room: usize, stats: &mut ImportStats) -> Result<[i32; 4], BonnieError> {
    Ok([
        to_units(h[0], room, stats)?,
        to_units(h[1], room, stats)?,
        to_units(h[2], room, stats)?,
        to_units(h[3], room, stats)?,
    ])
}

fn convert_surface(
    face: &HFace,
    room: usize,
    resolve: &mut dyn FnMut(&TextureRef) -> Option<ResourceId>,
    stats: &mut ImportStats,
) -> Result<GridSurface, BonnieError> {
    if face.uv.is_some() || face.uv_2.is_some() {
        stats.custom_uv_faces += 1;
    }
    let heights = heights4(face.heights, room, stats)?;
    let heights_b = match face.heights_2 {
        Some(h) => Some(heights4(h, room, stats)?),
        None => None,
    };
    Ok(GridSurface {
        heights,
        split: match face.split_direction {
            SplitDirection::NwSe => GridSplit::NwSe,
            SplitDirection::NeSw => GridSplit::NeSw,
        },
        // An explicit second height set equal to the first is no split level.
        heights_b: heights_b.filter(|h| *h != heights),
        tri_a: GridTri::with_material(resolve(&face.texture)),
        tri_b: face
            .texture_2
            .as_ref()
            .filter(|t| **t != face.texture)
            .map(|t| GridTri::with_material(resolve(t))),
        walkable: face.walkable,
    })
}

fn convert_walls(
    walls: &[VFace],
    room: usize,
    resolve: &mut dyn FnMut(&TextureRef) -> Option<ResourceId>,
    stats: &mut ImportStats,
) -> Result<Vec<GridWall>, BonnieError> {
    walls
        .iter()
        .map(|wall| {
            if wall.uv.is_some() {
                stats.custom_uv_faces += 1;
            }
            stats.walls += 1;
            Ok(GridWall {
                heights: heights4(wall.heights, room, stats)?,
                material: resolve(&wall.texture),
                uv: FaceUv::default(),
                facing: match wall.normal_mode {
                    NormalMode::Front => WallFacing::Front,
                    NormalMode::Back => WallFacing::Back,
                    NormalMode::Both => WallFacing::Both,
                },
                solid: wall.solid,
            })
        })
        .collect()
}

fn convert_room(
    index: usize,
    room: &Room,
    resolve: &mut dyn FnMut(&TextureRef) -> Option<ResourceId>,
    stats: &mut ImportStats,
) -> Result<GridRoom, BonnieError> {
    let lattice = SECTOR_UNITS as f32;
    let aligned = |v: f32| v.is_finite() && v % lattice == 0.0;
    if !aligned(room.position.x) || !aligned(room.position.z) || !room.position.y.is_finite() {
        return Err(BonnieError::UnalignedRoom { room: index });
    }
    if room.sectors.len() != room.width || room.sectors.iter().any(|c| c.len() != room.depth) {
        return Err(BonnieError::BadRoomShape { room: index });
    }
    let mut out = GridRoom::new(format!("Room {}", room.id), room.width, room.depth);
    out.origin = [
        (room.position.x / lattice) as i32,
        (room.position.z / lattice) as i32,
    ];
    out.y_offset = to_units(room.position.y, index, stats)?;
    let ambient = (room.ambient.clamp(0.0, 1.0) * 255.0).round() as u8;
    out.ambient = [ambient; 3];
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    out.fog = GridFog {
        enabled: room.fog.enabled,
        color: [
            channel(room.fog.color.0),
            channel(room.fog.color.1),
            channel(room.fog.color.2),
        ],
        start: room.fog.start.round() as i32,
        falloff: room.fog.falloff.round() as i32,
    };
    for (x, column) in room.sectors.iter().enumerate() {
        for (z, cell) in column.iter().enumerate() {
            let Some(cell) = cell else { continue };
            let mut sector = GridSector::default();
            if let Some(face) = &cell.floor {
                stats.floors += 1;
                sector.floor = Some(convert_surface(face, index, resolve, stats)?);
            }
            if let Some(face) = &cell.ceiling {
                stats.ceilings += 1;
                sector.ceiling = Some(convert_surface(face, index, resolve, stats)?);
            }
            for (edge, walls) in [
                (WallEdge::North, &cell.walls_north),
                (WallEdge::East, &cell.walls_east),
                (WallEdge::South, &cell.walls_south),
                (WallEdge::West, &cell.walls_west),
                (WallEdge::Nwse, &cell.walls_nwse),
                (WallEdge::Nesw, &cell.walls_nesw),
            ] {
                *sector.walls_mut(edge) = convert_walls(walls, index, resolve, stats)?;
            }
            let slot = out.index(x, z).expect("shape checked above");
            out.sectors[slot] = Some(sector);
        }
    }
    Ok(out)
}

/// Mean floor height of a sector, grid units (`None` without a floor).
fn floor_height(world: &GridWorld, room: usize, cell: [usize; 2]) -> Option<i32> {
    let floor = world
        .rooms
        .get(room)?
        .sector(cell[0], cell[1])?
        .floor
        .as_ref()?;
    Some(floor.heights.iter().sum::<i32>() / 4)
}

fn light_position(
    world: &GridWorld,
    room: usize,
    cell: [usize; 2],
    above_floor: i32,
) -> Option<[i32; 3]> {
    let r = world.rooms.get(room)?;
    let floor = floor_height(world, room, cell)?;
    let half = SECTOR_UNITS / 2;
    Some([
        world
            .scale
            .to_editor((r.origin[0] + cell[0] as i32) * SECTOR_UNITS + half),
        world.scale.to_editor(r.y_offset + floor + above_floor),
        world
            .scale
            .to_editor((r.origin[1] + cell[1] as i32) * SECTOR_UNITS + half),
    ])
}

/// Colour, intensity and radius (in sectors) of the generated warm lights,
/// the Cathedral nave setting from the survey.
const GENERATED_COLOR: [u8; 3] = [255, 176, 104];
const GENERATED_INTENSITY: f32 = 1.25;
const GENERATED_RADIUS_SECTORS: i32 = 6;
/// Cells per lattice block edge: one light per about 36 floor cells.
const LATTICE_BLOCK: usize = 6;
/// Fewest floor cells a lattice block needs before it gets a light.
const LATTICE_MIN_CELLS: usize = 6;
/// Lamp height above the floor, and the clearance kept under a ceiling.
const LAMP_HEIGHT: i32 = 2048;
const CEILING_CLEARANCE: i32 = 256;

fn generated_lights(world: &GridWorld) -> Vec<ImportedLight> {
    let mut lights = Vec::new();
    for (room_index, room) in world.rooms.iter().enumerate() {
        let blocks_x = room.width.div_ceil(LATTICE_BLOCK);
        let blocks_z = room.depth.div_ceil(LATTICE_BLOCK);
        for bz in 0..blocks_z {
            for bx in 0..blocks_x {
                let cells: Vec<[usize; 2]> = (bz * LATTICE_BLOCK
                    ..((bz + 1) * LATTICE_BLOCK).min(room.depth))
                    .flat_map(|z| {
                        (bx * LATTICE_BLOCK..((bx + 1) * LATTICE_BLOCK).min(room.width))
                            .map(move |x| [x, z])
                    })
                    .filter(|c| room.sector(c[0], c[1]).is_some_and(|s| s.floor.is_some()))
                    .collect();
                if cells.len() < LATTICE_MIN_CELLS {
                    continue;
                }
                let n = cells.len() as f64;
                let centre = [
                    cells.iter().map(|c| c[0] as f64).sum::<f64>() / n,
                    cells.iter().map(|c| c[1] as f64).sum::<f64>() / n,
                ];
                let Some(cell) = cells.iter().copied().min_by(|a, b| {
                    let da = (a[0] as f64 - centre[0]).powi(2) + (a[1] as f64 - centre[1]).powi(2);
                    let db = (b[0] as f64 - centre[0]).powi(2) + (b[1] as f64 - centre[1]).powi(2);
                    da.total_cmp(&db).then(a.cmp(b))
                }) else {
                    continue;
                };
                let sector = room.sector(cell[0], cell[1]).expect("cell has a floor");
                let floor = floor_height(world, room_index, cell).unwrap_or(0);
                let mut above = LAMP_HEIGHT;
                if let Some(ceiling) = &sector.ceiling {
                    let low = ceiling.height_range().0;
                    above = above.min(low - CEILING_CLEARANCE - floor);
                }
                let Some(position) =
                    light_position(world, room_index, cell, above.max(CEILING_CLEARANCE))
                else {
                    continue;
                };
                lights.push(ImportedLight {
                    position,
                    color: GENERATED_COLOR,
                    intensity: GENERATED_INTENSITY,
                    radius: world
                        .scale
                        .to_editor(GENERATED_RADIUS_SECTORS * SECTOR_UNITS),
                    generated: true,
                });
            }
        }
    }
    lights
}

/// Convert a parsed level.
///
/// `resolve` maps each texture reference to a project material; it is called
/// once per use, so cache inside it. `scale` overrides the scale otherwise
/// derived from the level's player height.
pub fn convert_level(
    level: &Level,
    scale: Option<GridScale>,
    resolve: &mut dyn FnMut(&TextureRef) -> Option<ResourceId>,
) -> Result<ConvertedLevel, BonnieError> {
    let mut stats = ImportStats::default();
    let scale = scale.unwrap_or_else(|| {
        level
            .player_height()
            .map_or(GridScale::DEFAULT, scale_for_player_height)
    });
    let mut world = GridWorld {
        scale,
        rooms: Vec::with_capacity(level.rooms.len()),
    };
    for (index, room) in level.rooms.iter().enumerate() {
        world
            .rooms
            .push(convert_room(index, room, resolve, &mut stats)?);
    }

    let mut lights = Vec::new();
    let mut spawn = None;
    for (room_index, room) in level.rooms.iter().enumerate() {
        for object in room.objects.iter().filter(|o| o.enabled) {
            let cell = [object.sector_x, object.sector_z];
            let height = object.height.round() as i32;
            match &object.object_type {
                LegacyObject::Light {
                    color,
                    intensity,
                    radius,
                } => {
                    if let Some(position) = light_position(&world, room_index, cell, height) {
                        lights.push(ImportedLight {
                            position,
                            color: [color.r, color.g, color.b],
                            intensity: *intensity,
                            radius: world.scale.to_editor(radius.round() as i32),
                            generated: false,
                        });
                    }
                }
                LegacyObject::Spawn(SpawnKind::PlayerStart) if spawn.is_none() => {
                    spawn = Some(ImportedSpawn {
                        room: room_index,
                        cell,
                        height,
                    });
                }
                LegacyObject::Other => {
                    // A current-format instance: what it is (light, spawn,
                    // prop) is decided by bonnie's asset library.
                    if object.asset_id != 0 {
                        stats.unresolved_objects += 1;
                    }
                }
                LegacyObject::Spawn(_) => {}
            }
        }
    }
    stats.level_lights = lights.len();
    if lights.is_empty() {
        lights = generated_lights(&world);
        stats.generated_lights = lights.len();
    }
    Ok(ConvertedLevel {
        world,
        lights,
        spawn,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 3x2 room: a slope, a split-level floor, a ceiling, a back-facing
    /// wall, an `_USER` texture and an empty texture reference.
    const LEVEL: &str = r#"(
        rooms: [(
            id: 4,
            position: (x: -2048.0, y: 512.0, z: 1024.0),
            width: 3, depth: 2,
            ambient: 0.25,
            sectors: [
                [Some((floor: Some((heights: (0.0, -0.0, 256.0, 256.0), texture: (pack: "a", name: "Brick"), split_direction: NeSw)),
                       ceiling: Some((heights: (1024.0, 1024.0, 1024.0, 1024.0), texture: (pack: "a", name: "Brick"))),
                       walls_north: [(heights: (0.0, 0.0, 1024.0, 1024.0), texture: (pack: "a", name: "Brick"), normal_mode: Back, solid: false)],
                       walls_nwse: [(heights: (0.0, 0.0, 512.0, 512.0), texture: (pack: "", name: ""))])),
                 None],
                [Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: "a", name: "Brick"),
                                    heights_2: Some((0.0, 0.0, 512.0, 0.0)), texture_2: Some((pack: "_USER", name: "texture_001")),
                                    uv: Some(((x: 0.0, y: 0.0), (x: 1.0, y: 0.0), (x: 1.0, y: 1.0), (x: 0.0, y: 1.0))))))),
                 Some((floor: Some((heights: (100.0, 0.0, 0.0, 0.0), texture: (pack: "a", name: "Brick")))))],
                [None, None],
            ],
            objects: [
                (sector_x: 0, sector_z: 0, height: 512.0, object_type: Light(color: (r: 10, g: 20, b: 30, blend: Opaque), intensity: 2.0, radius: 2048.0), name: "", enabled: true),
                (sector_x: 1, sector_z: 1, height: 64.0, object_type: Spawn(PlayerStart), name: "", enabled: true),
                (sector_x: 1, sector_z: 0, height: 0.0, asset_id: 77, name: "", enabled: true),
            ],
            fog: (enabled: true, color: (1.0, 0.0, 0.5), start: 100.0, falloff: 200.0, cull_offset: 5.0),
        )],
        player_settings: (height: 762.0, radius: 100.0),
    )"#;

    fn convert(text: &str) -> (ConvertedLevel, Vec<TextureRef>) {
        let level = parse_level_text(text).expect("parse");
        let mut seen = Vec::new();
        let mut project = crate::ProjectDocument::new("t");
        let mut ids = std::collections::BTreeMap::new();
        let converted = convert_level(&level, None, &mut |t| {
            if !seen.contains(t) {
                seen.push(t.clone());
            }
            Some(*ids.entry(t.clone()).or_insert_with(|| {
                project.add_resource(
                    &t.name,
                    crate::ResourceData::Material(crate::MaterialResource::opaque(None)),
                )
            }))
        })
        .expect("convert");
        (converted, seen)
    }

    #[test]
    fn room_geometry_comes_across_in_integers() {
        let (c, _) = convert(LEVEL);
        let room = &c.world.rooms[0];
        assert_eq!(room.name, "Room 4");
        assert_eq!(room.origin, [-2, 1]);
        assert_eq!(room.y_offset, 512);
        assert_eq!((room.width, room.depth), (3, 2));
        assert_eq!(room.ambient, [64; 3]);
        assert_eq!(room.sector_count(), 3);
        // 762 tall player picks 21/16.
        assert_eq!(c.world.scale, GridScale { num: 21, den: 16 });
        let floor = room.sector(0, 0).unwrap().floor.as_ref().unwrap();
        assert_eq!(floor.heights, [0, 0, 256, 256]);
        assert_eq!(floor.split, GridSplit::NeSw);
        assert!(floor.tri_b.is_none() && floor.heights_b.is_none());
        assert!(room.sector(2, 0).is_none());
        // Column-major bonnie storage lands at [x][z].
        assert!(room.sector(1, 1).is_some() && room.sector(1, 0).is_some());
    }

    #[test]
    fn split_level_surfaces_keep_their_second_triangle() {
        let (c, _) = convert(LEVEL);
        let floor = c.world.rooms[0]
            .sector(1, 0)
            .unwrap()
            .floor
            .as_ref()
            .unwrap();
        assert_eq!(floor.heights_b, Some([0, 0, 512, 0]));
        assert!(floor.tri_b.is_some());
        assert_ne!(floor.tri_b.unwrap().material, floor.tri_a.material);
    }

    #[test]
    fn walls_keep_facing_solidity_and_edge() {
        let (c, _) = convert(LEVEL);
        let sector = c.world.rooms[0].sector(0, 0).unwrap();
        assert_eq!(sector.north.len(), 1);
        assert_eq!(sector.north[0].facing, WallFacing::Back);
        assert!(!sector.north[0].solid);
        assert_eq!(sector.nwse.len(), 1);
        assert!(sector.south.is_empty());
        assert_eq!(c.stats.walls, 2);
        assert!(sector.ceiling.is_some());
    }

    #[test]
    fn every_texture_use_goes_through_the_resolver_including_odd_ones() {
        let (_, seen) = convert(LEVEL);
        assert!(seen.iter().any(TextureRef::is_empty));
        assert!(seen.iter().any(TextureRef::is_user));
        assert!(seen.iter().any(|t| t.pack == "a" && t.name == "Brick"));
        let level = parse_level_text(LEVEL).unwrap();
        assert_eq!(level.textures().len(), 3);
    }

    #[test]
    fn level_lights_and_spawn_are_read_in_editor_units() {
        let (c, _) = convert(LEVEL);
        assert_eq!(c.stats.level_lights, 1);
        assert_eq!(c.stats.generated_lights, 0);
        let light = &c.lights[0];
        assert!(!light.generated);
        assert_eq!(light.color, [10, 20, 30]);
        // Sector (0,0) of a room at sector (-2, 1): x centre at -2 sectors
        // plus half, floor mean 128 + 512 lift + 512 above, scaled 21/16.
        assert_eq!(light.position, [-2016, 1512, 1344 + 672]);
        assert_eq!(light.radius, 2688);
        assert_eq!(
            c.spawn,
            Some(ImportedSpawn {
                room: 0,
                cell: [1, 1],
                height: 64
            })
        );
        assert_eq!(c.stats.unresolved_objects, 1);
    }

    #[test]
    fn fog_and_dropped_data_are_reported() {
        let (c, _) = convert(LEVEL);
        let fog = c.world.rooms[0].fog;
        assert!(fog.enabled);
        assert_eq!(fog.color, [255, 0, 128]);
        assert_eq!((fog.start, fog.falloff), (100, 200));
        assert_eq!(c.stats.custom_uv_faces, 1);
        // Only surface heights count towards the lattice check: the 100 unit
        // corner is off it, the fog distances are not heights.
        assert_eq!(c.stats.off_lattice_heights, 1);
    }

    #[test]
    fn levels_without_lights_get_a_generated_lattice() {
        let mut text = String::from(
            "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 6, depth: 6, sectors: [",
        );
        for _ in 0..6 {
            text.push('[');
            for _ in 0..6 {
                text.push_str("Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: \"a\", name: \"b\"))))),");
            }
            text.push_str("],");
        }
        text.push_str("])])");
        let (c, _) = convert(&text);
        assert_eq!(c.stats.generated_lights, 1);
        assert_eq!(c.lights.len(), 1);
        assert!(c.lights[0].generated);
        // Centre-most cell of a 6x6 block, 2048 above its floor.
        assert_eq!(c.lights[0].position[1], c.world.scale.to_editor(2048));
        assert_eq!(c.lights[0].radius, c.world.scale.to_editor(6 * 1024));
        // No player height recorded: the default scale applies.
        assert_eq!(c.world.scale, GridScale::DEFAULT);
    }

    #[test]
    fn lamps_stay_below_the_ceiling() {
        let mut text = String::from(
            "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 6, depth: 6, sectors: [",
        );
        for _ in 0..6 {
            text.push('[');
            for _ in 0..6 {
                text.push_str("Some((floor: Some((heights: (0.0, 0.0, 0.0, 0.0), texture: (pack: \"a\", name: \"b\"))), ceiling: Some((heights: (1024.0, 1024.0, 1024.0, 1024.0), texture: (pack: \"a\", name: \"b\"))))),");
            }
            text.push_str("],");
        }
        text.push_str("])])");
        let (c, _) = convert(&text);
        let ceiling = c.world.scale.to_editor(1024);
        assert!(c.lights[0].position[1] < ceiling, "{:?}", c.lights[0]);
    }

    #[test]
    fn rooms_off_the_sector_lattice_are_rejected() {
        let text = "(rooms: [(position: (x: 100.0, y: 0.0, z: 0.0), width: 1, depth: 1, sectors: [[None]])])";
        let level = parse_level_text(text).unwrap();
        assert!(matches!(
            convert_level(&level, None, &mut |_| None),
            Err(BonnieError::UnalignedRoom { room: 0 })
        ));
    }

    #[test]
    fn mismatched_sector_columns_are_rejected() {
        let text = "(rooms: [(position: (x: 0.0, y: 0.0, z: 0.0), width: 2, depth: 1, sectors: [[None]])])";
        let level = parse_level_text(text).unwrap();
        assert!(matches!(
            convert_level(&level, None, &mut |_| None),
            Err(BonnieError::BadRoomShape { room: 0 })
        ));
    }

    #[test]
    fn plain_and_brotli_levels_both_parse() {
        let plain = "(rooms: [])";
        assert_eq!(parse_level(plain.as_bytes()).unwrap().room_count(), 0);
        // Brotli stream for "(rooms: [])", from the reference `brotli` tool.
        let compressed = [
            0x0f, 0x05, 0x80, 0x28, 0x72, 0x6f, 0x6f, 0x6d, 0x73, 0x3a, 0x20, 0x5b, 0x5d, 0x29,
            0x03,
        ];
        assert_eq!(parse_level(&compressed).unwrap().room_count(), 0);
        assert!(matches!(
            parse_level(b"\x00\x01\x02"),
            Err(BonnieError::Decompress(_)) | Err(BonnieError::Parse(_))
        ));
    }

    #[test]
    fn player_height_picks_the_scale() {
        assert_eq!(
            scale_for_player_height(1800.0),
            GridScale { num: 9, den: 16 }
        );
        assert_eq!(
            scale_for_player_height(762.0),
            GridScale { num: 21, den: 16 }
        );
        assert_eq!(
            scale_for_player_height(1200.0),
            GridScale { num: 14, den: 16 }
        );
        assert_eq!(
            scale_for_player_height(1024.0),
            GridScale { num: 16, den: 16 }
        );
        assert_eq!(scale_for_player_height(0.0), GridScale::DEFAULT);
    }
}
