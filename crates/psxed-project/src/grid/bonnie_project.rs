//! Build a grid project from a bonnie-32 level.
//!
//! The project is based on an existing one (Graybox Reach by default) so it
//! carries the current Cortex character, camera, sky and UI: the base's
//! brushes and world nodes are dropped, its player and a few enemies stay and
//! are moved to the level's spawn. The level becomes the scene's
//! [`GridWorld`]; its lights become ordinary `PointLight` nodes.
//!
//! Textures register as normal project materials, converted from the level's
//! texture packs with the survey's fixes: the lookup is `<pack>/<name>` (any
//! extension case), an empty reference gets the checkerboard bonnie draws, a
//! bonnie paint texture (`_USER`) or a missing file gets that checkerboard
//! too (and is listed in the report), and a texture keeps its own size so the
//! 32 texels per sector mapping tiles exactly as it did in bonnie.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use psxed_tex::{convert, Config, CropMode, PsxtDepth, Resampler};

use crate::import_util::sanitize_name;
use crate::{MaterialResource, NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, Scene};

use super::bonnie::{
    convert_level, parse_level, BonnieError, ConvertedLevel, ImportStats, TextureRef,
};
use super::limits::{self, GridIssue, Severity};
use super::units::{GridScale, SECTOR_UNITS};
use super::world::{GridWorld, SceneWorld};

/// Folder inside the output project that holds the converted textures.
pub const LEVEL_TEXTURES: &str = "level_textures";
/// Height above the floor the spawn is placed at, editor units, so the
/// character never starts inside the floor.
const SPAWN_LIFT: i32 = 64;
/// Name of the fallback checkerboard material.
pub const FALLBACK_MATERIAL: &str = "Grid Fallback";

/// Where to put the spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnRequest {
    /// Room index.
    pub room: usize,
    /// Local sector `[x, z]`.
    pub cell: [usize; 2],
}

/// Inputs of [`import_project`].
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Project to base the output on (its `project.ron` and `assets`).
    pub base_dir: PathBuf,
    /// Folder holding the texture packs, one sub-folder per pack.
    pub packs_dir: PathBuf,
    /// Output project folder. Should sit beside the base so shared asset
    /// paths such as `../default/...` still resolve.
    pub out_dir: PathBuf,
    /// Project name.
    pub name: String,
    /// Scale override; default is derived from the level's player height.
    pub scale: Option<GridScale>,
    /// Spawn override; default is the level's player start, else the floor
    /// sector nearest the centre of the first room.
    pub spawn: Option<SpawnRequest>,
    /// Player yaw in degrees.
    pub yaw_degrees: f32,
    /// Enemies kept from the base project.
    pub enemies: usize,
}

/// Why the import failed.
#[derive(Debug)]
pub enum ImportError {
    /// The level did not read or convert.
    Level(BonnieError),
    /// The base project did not load.
    Base(String),
    /// A file operation failed.
    Io(String, std::io::Error),
    /// The converted world breaks a cap.
    Invalid(Vec<GridIssue>),
    /// The level has no floor to stand on.
    NoFloor,
    /// The requested spawn sector has no floor.
    BadSpawn(SpawnRequest),
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Level(e) => write!(f, "{e}"),
            Self::Base(e) => write!(f, "base project: {e}"),
            Self::Io(what, e) => write!(f, "{what}: {e}"),
            Self::Invalid(issues) => {
                write!(f, "the level breaks the grid caps:")?;
                for issue in issues.iter().filter(|i| i.severity() == Severity::Error) {
                    write!(f, " {};", issue.describe())?;
                }
                Ok(())
            }
            Self::NoFloor => write!(f, "the level has no floor sector to spawn on"),
            Self::BadSpawn(s) => write!(
                f,
                "room {} sector {},{} has no floor to spawn on",
                s.room, s.cell[0], s.cell[1]
            ),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<BonnieError> for ImportError {
    fn from(error: BonnieError) -> Self {
        Self::Level(error)
    }
}

/// What the import did.
#[derive(Clone, Debug)]
pub struct ImportReport {
    /// Written `project.ron`.
    pub project_path: PathBuf,
    /// Kept, generated and dropped counts.
    pub stats: ImportStats,
    /// Scale the world uses.
    pub scale: GridScale,
    /// Materials the import added to the project, fallback included.
    pub materials: usize,
    /// Texture references whose file was not found.
    pub missing_textures: Vec<TextureRef>,
    /// Texture references replaced by the fallback on purpose (empty and
    /// bonnie paint textures).
    pub substituted_textures: Vec<TextureRef>,
    /// Spawn used, editor units.
    pub spawn: [i32; 3],
    /// Non-fatal findings from validation.
    pub warnings: Vec<GridIssue>,
    /// Drawable faces in the converted world.
    pub faces: usize,
}

struct MaterialTable<'a> {
    packs: &'a Path,
    out: &'a Path,
    by_ref: BTreeMap<TextureRef, ResourceId>,
    names: BTreeMap<String, TextureRef>,
    fallback: Option<ResourceId>,
    /// Materials this table added to the project.
    created: usize,
    missing: Vec<TextureRef>,
    substituted: Vec<TextureRef>,
    error: Option<ImportError>,
}

/// A 64x64 black and magenta checkerboard, the "no texture" look.
fn checker_png() -> Vec<u8> {
    let mut image = image::RgbaImage::new(64, 64);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let on = ((x / 8) + (y / 8)) % 2 == 0;
        *pixel = if on {
            image::Rgba([200, 40, 200, 255])
        } else {
            image::Rgba([24, 24, 24, 255])
        };
    }
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .expect("encode checkerboard");
    bytes
}

/// Find `<packs>/<pack>/<name>.png`, matching the extension case-blind.
fn find_texture(packs: &Path, texture: &TextureRef) -> Option<PathBuf> {
    let dir = packs.join(&texture.pack);
    ["png", "PNG", "Png"]
        .iter()
        .map(|ext| dir.join(format!("{}.{ext}", texture.name)))
        .find(|p| p.is_file())
}

/// A texture size the PS1 tiles cleanly: both sides a power of two from 8
/// to 128, else 64 square.
fn texture_size(png: &[u8]) -> (u16, u16) {
    let dims = image::ImageReader::new(Cursor::new(png))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    match dims {
        Some((w, h))
            if [w, h]
                .iter()
                .all(|v| v.is_power_of_two() && (8..=128).contains(v)) =>
        {
            (w as u16, h as u16)
        }
        _ => (64, 64),
    }
}

impl MaterialTable<'_> {
    fn write_texture(&mut self, relative: &str, png: &[u8]) -> bool {
        let (width, height) = texture_size(png);
        let psxt = match convert(
            png,
            &Config {
                width,
                height,
                depth: PsxtDepth::Bit4,
                crop: CropMode::None,
                resampler: Resampler::Lanczos3,
                transparent_index_zero: false,
                clut_rows: 1,
            },
        ) {
            Ok(psxt) => psxt,
            Err(_) => return false,
        };
        if let Err(e) = std::fs::write(self.out.join(relative), psxt) {
            self.error
                .get_or_insert(ImportError::Io(format!("write {relative}"), e));
            return false;
        }
        true
    }

    fn fallback(&mut self, project: &mut ProjectDocument) -> ResourceId {
        if let Some(id) = self.fallback {
            return id;
        }
        let relative = format!("{LEVEL_TEXTURES}/grid_fallback.psxt");
        let path = self
            .write_texture(&relative, &checker_png())
            .then_some(relative);
        let id = project.add_resource(
            FALLBACK_MATERIAL,
            ResourceData::Material(MaterialResource::opaque(path)),
        );
        self.fallback = Some(id);
        self.created += 1;
        id
    }

    fn material(&mut self, project: &mut ProjectDocument, texture: &TextureRef) -> ResourceId {
        if let Some(id) = self.by_ref.get(texture) {
            return *id;
        }
        let id = self.create(project, texture);
        self.by_ref.insert(texture.clone(), id);
        id
    }

    fn create(&mut self, project: &mut ProjectDocument, texture: &TextureRef) -> ResourceId {
        if texture.is_empty() || texture.is_user() {
            self.substituted.push(texture.clone());
            return self.fallback(project);
        }
        let Some(source) = find_texture(self.packs, texture) else {
            self.missing.push(texture.clone());
            return self.fallback(project);
        };
        let Ok(png) = std::fs::read(&source) else {
            self.missing.push(texture.clone());
            return self.fallback(project);
        };
        let stem = sanitize_name(&format!("{}__{}", texture.pack, texture.name), "texture");
        let relative = format!("{LEVEL_TEXTURES}/{stem}.psxt");
        if !self.write_texture(&relative, &png) {
            self.missing.push(texture.clone());
            return self.fallback(project);
        }
        // Two packs may share a texture name: keep the first plain, suffix
        // the pack on later ones.
        let name = match self.names.get(&texture.name) {
            Some(first) if first != texture => format!("{} ({})", texture.name, texture.pack),
            _ => {
                self.names.insert(texture.name.clone(), texture.clone());
                texture.name.clone()
            }
        };
        self.created += 1;
        project.add_resource(
            name,
            ResourceData::Material(MaterialResource::opaque(Some(relative))),
        )
    }
}

fn entity_role(scene: &Scene, id: NodeId) -> Option<bool> {
    let node = scene.node(id)?;
    if !matches!(node.kind, NodeKind::Entity) {
        return None;
    }
    node.children
        .iter()
        .find_map(|child| match &scene.node(*child)?.kind {
            NodeKind::CharacterController { player, .. } => Some(*player),
            _ => None,
        })
}

/// The base project minus its level: brushes, lights, props and pickups go;
/// the player and the first `enemies` enemies stay so frame costs match a
/// real playthrough. The once-per-launch world message is dropped.
fn strip_base(project: &mut ProjectDocument, enemies: usize) {
    let scene = project.active_scene_mut();
    scene.brushes.clear();
    let top: Vec<NodeId> = scene
        .node(NodeId::ROOT)
        .map(|root| root.children.clone())
        .unwrap_or_default();
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
}

/// Highest floor corner of a sector, grid units.
fn floor_top(world: &GridWorld, room: usize, cell: [usize; 2]) -> Option<i32> {
    let floor = world
        .rooms
        .get(room)?
        .sector(cell[0], cell[1])?
        .floor
        .as_ref()?;
    floor.heights.iter().copied().max()
}

/// The floor sector nearest the centre of room 0's floor area.
fn central_floor_cell(world: &GridWorld) -> Option<SpawnRequest> {
    let room = world.rooms.first()?;
    let floors: Vec<[usize; 2]> = room
        .iter()
        .filter(|(_, _, s)| s.floor.is_some())
        .map(|(x, z, _)| [x, z])
        .collect();
    let n = floors.len() as f64;
    if floors.is_empty() {
        return None;
    }
    let centre = [
        floors.iter().map(|c| c[0] as f64).sum::<f64>() / n,
        floors.iter().map(|c| c[1] as f64).sum::<f64>() / n,
    ];
    floors
        .into_iter()
        .min_by(|a, b| {
            let d = |c: &[usize; 2]| {
                (c[0] as f64 - centre[0]).powi(2) + (c[1] as f64 - centre[1]).powi(2)
            };
            d(a).total_cmp(&d(b)).then(a.cmp(b))
        })
        .map(|cell| SpawnRequest { room: 0, cell })
}

/// Editor-space spawn point for a sector: its centre, just above the
/// highest floor corner.
fn spawn_point(world: &GridWorld, spawn: SpawnRequest) -> Option<[i32; 3]> {
    let room = world.rooms.get(spawn.room)?;
    let top = floor_top(world, spawn.room, spawn.cell)?;
    let half = SECTOR_UNITS / 2;
    Some([
        world
            .scale
            .to_editor((room.origin[0] + spawn.cell[0] as i32) * SECTOR_UNITS + half),
        world.scale.to_editor(room.y_offset + top) + SPAWN_LIFT,
        world
            .scale
            .to_editor((room.origin[1] + spawn.cell[1] as i32) * SECTOR_UNITS + half),
    ])
}

/// How far above the spawn an actor may still be dropped onto a floor.
const ACTOR_FLOOR_REACH: i32 = 512;

fn place_actors(
    project: &mut ProjectDocument,
    world: &GridWorld,
    at: [i32; 3],
    yaw: f32,
    step: i32,
) {
    let scene = project.active_scene_mut();
    let mut actors: Vec<(bool, NodeId)> = scene
        .node(NodeId::ROOT)
        .map(|root| root.children.clone())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|id| entity_role(scene, id).map(|player| (!player, id)))
        .collect();
    actors.sort_by_key(|(enemy, _)| *enemy);
    if !actors.iter().any(|(enemy, _)| !enemy) {
        // The base had no player: fall back to a plain spawn point.
        let id = scene.add_node(
            NodeId::ROOT,
            "Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        actors.insert(0, (false, id));
    }
    let (forward_x, forward_z) = (yaw.to_radians().sin(), yaw.to_radians().cos());
    for (index, (_, id)) in actors.iter().enumerate() {
        let ahead = index as f32 * 4.0 * step as f32;
        let side = if index == 2 { -1.5 } else { 1.5 }
            * step as f32
            * (index.min(1) as f32 + (index / 2) as f32);
        // Stand each actor on a floor: if the spot has none (past the edge of
        // the level), pull it back towards the spawn until it does.
        let mut pull = 1.0f32;
        let (x, y, z) = loop {
            let x = at[0] + ((forward_x * ahead + forward_z * side) * pull) as i32;
            let z = at[2] + ((forward_z * ahead - forward_x * side) * pull) as i32;
            match world.floor_height_at(x, z, at[1] + ACTOR_FLOOR_REACH) {
                Some(floor) => break (x, floor + SPAWN_LIFT, z),
                None if pull > 0.1 => pull *= 0.5,
                None => break (at[0], at[1], at[2]),
            }
        };
        let position = [x, y, z];
        if let Some(node) = scene.node_mut(*id) {
            node.transform.translation = position.map(|v| v as f32);
            node.transform.rotation_degrees = [0.0, yaw, 0.0];
        }
    }
}

fn add_lights(project: &mut ProjectDocument, converted: &ConvertedLevel) {
    let scene = project.active_scene_mut();
    // Radii are stored in sectors of the World node's `sector_size` (1024
    // by default), not of the grid, so convert from editor units.
    let unit = scene
        .world_sector_size_for_node(NodeId::ROOT)
        .unwrap_or(crate::DEFAULT_WORLD_SECTOR_SIZE)
        .max(1) as f32;
    let (mut level, mut generated) = (0, 0);
    for light in &converted.lights {
        let name = if light.generated {
            generated += 1;
            format!("Generated Light {generated}")
        } else {
            level += 1;
            format!("Level Light {level}")
        };
        let id = scene.add_node(
            NodeId::ROOT,
            name,
            NodeKind::PointLight {
                color: light.color,
                intensity: light.intensity,
                radius: light.radius as f32 / unit,
            },
        );
        if let Some(node) = scene.node_mut(id) {
            node.transform.translation = light.position.map(|v| v as f32);
        }
    }
}

#[cfg(unix)]
fn link_assets(base: &Path, out: &Path) -> std::io::Result<()> {
    let link = out.join("assets");
    if link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)?;
    }
    // A sibling base links relatively so the pair can move together.
    let base_real = std::fs::canonicalize(base)?;
    let out_parent = std::fs::canonicalize(out.parent().unwrap_or(out))?;
    let target = if base_real.parent() == Some(out_parent.as_path()) {
        PathBuf::from("..")
            .join(base_real.file_name().unwrap_or_default())
            .join("assets")
    } else {
        base_real.join("assets")
    };
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link_assets(base: &Path, out: &Path) -> std::io::Result<()> {
    // No symlinks here: copy the shared assets instead.
    let _ = (base, out);
    Err(std::io::Error::other(
        "linking the base assets needs a unix host",
    ))
}

/// Import a bonnie-32 level (Brotli or plain RON bytes) as a grid project.
pub fn import_project(
    level_bytes: &[u8],
    options: &ImportOptions,
) -> Result<ImportReport, ImportError> {
    let io = |what: &str| {
        let what = what.to_string();
        move |e: std::io::Error| ImportError::Io(what.clone(), e)
    };
    let level = parse_level(level_bytes)?;

    let mut project = ProjectDocument::load_from_path(options.base_dir.join("project.ron"))
        .map_err(|e| ImportError::Base(e.to_string()))?;
    strip_base(&mut project, options.enemies);
    project.name = options.name.clone();

    std::fs::create_dir_all(options.out_dir.join(LEVEL_TEXTURES))
        .map_err(io("create output folders"))?;
    link_assets(&options.base_dir, &options.out_dir).map_err(io("link base assets"))?;

    let mut table = MaterialTable {
        packs: &options.packs_dir,
        out: &options.out_dir,
        by_ref: BTreeMap::new(),
        names: BTreeMap::new(),
        fallback: None,
        created: 0,
        missing: Vec::new(),
        substituted: Vec::new(),
        error: None,
    };
    let converted = convert_level(&level, options.scale, &mut |t| {
        Some(table.material(&mut project, t))
    })?;
    if let Some(error) = table.error.take() {
        return Err(error);
    }

    let issues = limits::validate(&converted.world);
    if limits::has_errors(&issues) {
        return Err(ImportError::Invalid(issues));
    }

    let spawn = options
        .spawn
        .or_else(|| {
            converted.spawn.map(|s| SpawnRequest {
                room: s.room,
                cell: s.cell,
            })
        })
        .or_else(|| central_floor_cell(&converted.world))
        .ok_or(ImportError::NoFloor)?;
    let spawn_at = spawn_point(&converted.world, spawn).ok_or(ImportError::BadSpawn(spawn))?;

    let scale = converted.world.scale;
    let faces = super::faces::face_count(&converted.world);
    let step = scale.sector_editor_units();
    add_lights(&mut project, &converted);
    place_actors(
        &mut project,
        &converted.world,
        spawn_at,
        options.yaw_degrees,
        step,
    );
    project.active_scene_mut().world = SceneWorld::Grid(converted.world);
    // Drop the base project's authored views: the editor frames the whole
    // world, in 3D and in the Top view, the first time the project opens.
    project.editor_camera = crate::EditorCameraState::default();
    project.editor_viewport = crate::EditorViewportState::default();

    let project_path = options.out_dir.join("project.ron");
    project
        .save_to_path(&project_path)
        .map_err(|e| ImportError::Base(e.to_string()))?;

    Ok(ImportReport {
        project_path,
        stats: converted.stats,
        scale,
        materials: table.created,
        missing_textures: table.missing,
        substituted_textures: table.substituted,
        spawn: spawn_at,
        warnings: issues,
        faces,
    })
}
