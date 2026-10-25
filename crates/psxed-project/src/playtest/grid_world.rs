//! Cooking a grid world for Play.
//!
//! A grid project's world is its grid document and nothing else (plan
//! decision D2). Play cooks the document straight into the PGL level format
//! ([`crate::grid::cook`]), with no derived geometry. This
//! module is the glue to the playtest package: the cap check, the material
//! binder that turns document materials into texture assets, the scene's
//! point lights, and the package record the manifest writer emits.

use std::collections::HashMap;
use std::path::Path;

use psx_asset::Texture;
use psx_grid::{MATERIAL_CUTOUT, SECTOR_BYTES};

use crate::grid::cook::{self, BoundMaterial, CookInput, MaterialBinder, RuntimeUnits};
use crate::grid::light::GridLight;
use crate::grid::limits::{self, Severity};
use crate::grid::GridWorld;
use crate::{
    MaterialAnimationMode, MaterialFaceSidedness, ProjectDocument, PsxBlendMode, ResourceData,
    ResourceId, Scene,
};

use super::{
    PackItem, PlaytestAsset, PlaytestAssetKind, PlaytestCards, PlaytestGridWorld, PlaytestPanels,
    PlaytestSkyTables, PlaytestValidationReport, StreamedClass,
};

/// Texel size of the tile the grid renderer maps one material onto.
const TILE_TEXELS: u16 = 64;

/// Check a grid scene against the shared caps and the runtime lattice.
/// Returns `false` when the world cannot be cooked; the reasons are in
/// `report`. A scene without a grid world is refused later, by the cook.
pub(super) fn validate(project: &ProjectDocument, report: &mut PlaytestValidationReport) -> bool {
    let Some(world) = project.active_scene().world.grid() else {
        return true;
    };
    let issues = limits::validate(world);
    for issue in &issues {
        match issue.severity() {
            Severity::Error => report.error(format!("grid world: {}", issue.describe())),
            Severity::Warning => report.warn(format!("grid world: {}", issue.describe())),
        }
    }
    if limits::has_errors(&issues) {
        return false;
    }
    if let Err(error) = RuntimeUnits::new(world.scale) {
        report.error(format!("grid world: {error}"));
        return false;
    }
    true
}

/// Point lights of the scene in the scaled (runtime) project.
fn scene_lights(scene: &Scene) -> Vec<GridLight> {
    crate::grid::lighting::scene_grid_lights(scene, 1.0)
}

/// The 8 by 8 white texture a face without a material is drawn with.
fn flat_white_psxt() -> Vec<u8> {
    psxed_tex::encode_indexed_psxt(
        8,
        8,
        psxed_format::texture::Depth::Bit4,
        &[1; 64],
        &[[0, 0, 0], [255, 255, 255]],
        false,
    )
    .expect("fixed fallback texture is valid")
}

/// Binds document materials to texture assets in the package.
struct Binder<'a> {
    project: &'a ProjectDocument,
    root: &'a Path,
    assets: &'a mut Vec<PlaytestAsset>,
    by_path: &'a mut HashMap<String, usize>,
    /// Asset indices the level's materials use, in first-use order.
    used: Vec<usize>,
    warnings: Vec<String>,
}

impl MaterialBinder for Binder<'_> {
    fn see_through(&self, material: Option<ResourceId>) -> bool {
        let Some(id) = material else {
            return false;
        };
        let Some(ResourceData::Material(data)) = self.project.resource(id).map(|r| &r.data) else {
            return false;
        };
        if data.blend_mode != PsxBlendMode::Opaque {
            return true;
        }
        if data.sky_aperture {
            return false;
        }
        crate::resolve_material_texture_psxt(self.project, id, self.root)
            .ok()
            .flatten()
            .is_some_and(|(_, bytes)| {
                Texture::from_bytes(&bytes).is_ok_and(|t| t.is_index_zero_transparent())
            })
    }

    fn albedo(&mut self, material: Option<ResourceId>) -> [f32; 3] {
        use crate::grid::lighting::BakeMaterials;
        crate::grid::lighting::AlbedoTable::from_project(self.project, self.root, [material])
            .albedo(material)
    }

    fn light_passes(&mut self, material: Option<ResourceId>) -> bool {
        let Some(id) = material else {
            return false;
        };
        let Some(ResourceData::Material(data)) = self.project.resource(id).map(|r| &r.data) else {
            return false;
        };
        data.sky_aperture || data.blend_mode != PsxBlendMode::Opaque
    }

    fn uv_default(&self, material: ResourceId) -> Option<crate::face_uv::UvMode> {
        crate::grid::UvDefaults::default_uv(
            &crate::grid::uv::ProjectUvDefaults(self.project),
            material,
        )
    }

    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        let mut tint = [128; 3];
        let mut blend = PsxBlendMode::Opaque;
        let mut sided = MaterialFaceSidedness::Front;
        let mut textured = false;
        let mut key = "@flat-white".to_string();
        let mut bytes = flat_white_psxt();
        if let Some(id) = material {
            let resource = self
                .project
                .resource(id)
                .ok_or_else(|| format!("material #{} does not exist", id.raw()))?;
            let ResourceData::Material(data) = &resource.data else {
                return Err(format!("resource #{} is not a material", id.raw()));
            };
            tint = data.tint;
            blend = data.blend_mode;
            sided = data.sidedness();
            if data.animation.mode != MaterialAnimationMode::Static {
                self.warnings.push(format!(
                    "material {:?} is animated; the grid renderer draws it still",
                    resource.name
                ));
            }
            if !data.sky_aperture {
                if let Some((found_key, found)) =
                    crate::resolve_material_texture_psxt(self.project, id, self.root)
                        .map_err(|e| format!("material {:?}: {e}", resource.name))?
                {
                    key = found_key;
                    bytes = found;
                    textured = true;
                }
            }
        }
        let texture =
            Texture::from_bytes(&bytes).map_err(|e| format!("invalid PSXT {key}: {e:?}"))?;
        let (width, height) = (texture.width(), texture.height());
        let valid = (8..=128).contains(&width)
            && (8..=128).contains(&height)
            && width.is_power_of_two()
            && height.is_power_of_two();
        if texture.depth() != psxed_format::texture::Depth::Bit4 || !valid {
            return Err(format!(
                "texture {key} must be 4bpp power-of-two 8..128, got {width}x{height}"
            ));
        }
        if textured && !key.contains("#page") && (width, height) != (TILE_TEXELS, TILE_TEXELS) {
            let note = format!(
                "texture {key} is {width}x{height}; the grid renderer tiles 64x64 textures"
            );
            if !self.warnings.contains(&note) {
                self.warnings.push(note);
            }
        }
        let cutout = texture.is_index_zero_transparent();
        let index = match self.by_path.get(&key) {
            Some(&existing) => existing,
            None => {
                let index = self.assets.len();
                self.assets.push(PlaytestAsset {
                    kind: PlaytestAssetKind::Texture,
                    bytes,
                    filename: format!("texture_{index:03}.psxt"),
                    source_label: key.clone(),
                    // Level textures are level data: the executable does not
                    // embed them. They are read off the disc with the other
                    // session-resident assets while the loading screen shows
                    // and uploaded to VRAM from there.
                    streamed_class: StreamedClass::PersistentGameplay,
                });
                self.by_path.insert(key, index);
                index
            }
        };
        if !self.used.contains(&index) {
            self.used.push(index);
        }
        let asset =
            u16::try_from(index).map_err(|_| "texture asset index exceeds u16".to_string())?;
        Ok(BoundMaterial {
            asset,
            flags: match sided {
                MaterialFaceSidedness::Front => 0,
                MaterialFaceSidedness::Back => 1,
                MaterialFaceSidedness::Both => 2,
            } | if cutout { MATERIAL_CUTOUT } else { 0 },
            blend: match blend {
                PsxBlendMode::Opaque => 0,
                PsxBlendMode::Average => 1,
                PsxBlendMode::Add => 2,
                PsxBlendMode::Subtract => 3,
                PsxBlendMode::AddQuarter => 4,
            },
            uv_motion: 0,
            tint,
            textured,
            texels: [width, height],
        })
    }
}

/// Distance LOD for far floors, off by default: `PSXED_GRID_LOD_FAR=<sectors>`
/// turns the merged far quads on beyond that distance (`cook::lod::LOD_FAR_CELLS`
/// is the measured setting). Merged quads cut the Cathedral route's world cost
/// at the tail (p99 and worst frame) but leave one-pixel gaps along some merged
/// edges, so they wait for a crack-free stitch before becoming the default.
fn lod_far_cells() -> u8 {
    std::env::var("PSXED_GRID_LOD_FAR")
        .ok()
        .and_then(|v| v.trim().parse::<u8>().ok())
        .unwrap_or(0)
}

/// Cook the active scene's grid world into a package record, registering the
/// textures it uses in `assets`. `project` is the scaled project (lights and
/// nodes at runtime scale); the document is read as authored.
pub(super) fn cook(
    project: &ProjectDocument,
    world: &GridWorld,
    project_root: &Path,
    assets: &mut Vec<PlaytestAsset>,
    texture_asset_for_path: &mut HashMap<String, usize>,
    report: &mut PlaytestValidationReport,
) -> Option<PlaytestGridWorld> {
    let lights = scene_lights(project.active_scene());
    let mut binder = Binder {
        project,
        root: project_root,
        assets,
        by_path: texture_asset_for_path,
        used: Vec::new(),
        warnings: Vec::new(),
    };
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder: &mut binder,
            lights: &lights,
            first_pack_sector: 0,
            lod_far_cells: lod_far_cells(),
            texture_repeat: cook::texture_repeat_default(),
        },
        None,
    );
    let used = std::mem::take(&mut binder.used);
    for warning in std::mem::take(&mut binder.warnings) {
        report.warn(format!("grid world: {warning}"));
    }
    let mut cooked = match cooked {
        Ok(cooked) => cooked,
        Err(error) => {
            report.error(format!("grid world: {error}"));
            return None;
        }
    };
    for warning in &cooked.report.warnings {
        report.warn(warning.clone());
    }
    if world.stream.flood.on && world.stream.flood.skin {
        if let Err(error) = add_skin(project, project_root, world, &mut cooked, report) {
            report.error(format!("grid world: {error}"));
            return None;
        }
    }
    let mut rooms = Vec::new();
    for chunk in &cooked.chunks {
        rooms.extend_from_slice(chunk);
        while !rooms.len().is_multiple_of(SECTOR_BYTES) {
            rooms.push(0);
        }
    }
    // The pool the guest reserves for the rooms streamed off the disc.
    let chunk_pages: Vec<usize> = cooked
        .chunks
        .iter()
        .map(|chunk| psx_grid::stream::pages_of(chunk.len()))
        .collect();
    let core_words = psx_grid::write::aligned_copy(&cooked.core);
    let pool = match psx_grid::LevelView::parse(psx_grid::write::words_as_bytes(
        &core_words,
        cooked.core.len(),
    )) {
        Ok(level) => psx_grid::stream::plan_pool(&level, &chunk_pages),
        Err(error) => {
            report.error(format!(
                "grid world: the cooked core does not parse: {error:?}"
            ));
            return None;
        }
    };
    let scene = project.active_scene();
    let cube_sky = scene
        .world_sky_for_node(scene.root)
        .is_some_and(|sky| sky.mode == crate::SkyMode::Cube);
    // Far cards and panels take the sky atlas: no chunk sky is baked to stream over them.
    let cards_on = world.stream.flood.on && world.stream.flood.cards;
    let panels_on = world.stream.flood.on && world.stream.flood.panels > 0;
    let (chunk_skies, sky_tables) = if world.stream.ring && cube_sky && !cards_on && !panels_on {
        use crate::grid::lighting::BakeMaterials;
        let materials = cook::sky_bake::WorldColours::materials(world);
        let table = crate::grid::lighting::AlbedoTable::from_project(
            project,
            project_root,
            materials.iter().copied(),
        );
        let albedo = materials.iter().map(|&m| (m, table.albedo(m))).collect();
        let colours =
            cook::sky_bake::WorldColours::new(world, i32::from(cooked.level.sector_units), albedo);
        bake_chunk_skies(&cooked, &colours, report)
    } else {
        (Vec::new(), PlaytestSkyTables::default())
    };
    let cards = if cards_on {
        match bake_cards(project, project_root, world, &cooked, report) {
            Ok(cards) => cards,
            Err(error) => {
                report.error(format!("grid world: {error}"));
                return None;
            }
        }
    } else {
        PlaytestCards::default()
    };
    let panels = if panels_on {
        match bake_panels(project, project_root, world, &cooked, report) {
            Ok(panels) => panels,
            Err(error) => {
                report.error(format!("grid world: {error}"));
                return None;
            }
        }
    } else {
        PlaytestPanels::default()
    };
    let summary = cooked.report.summary();
    report.warn(summary.trim_end().to_string());
    report.warn(format!(
        "grid world: guest room pool {} pages ({} KiB): the wanted set peaks at {} pages in {} rooms (at most {} rooms in any set), the largest room is {} pages; every page costs the guest heap 2 KiB",
        pool.pool_pages,
        pool.pool_pages * 2,
        pool.peak_pages,
        pool.peak_rooms,
        pool.max_rooms,
        pool.max_room_pages
    ));
    // A level with cards lays its pictures out among the room chunks: the
    // rooms' sectors (counted from room 0) follow the new order.
    let rooms = if !panels.is_empty() {
        match relayout_panel_rooms(&mut cooked, &panels) {
            Ok(rooms) => rooms,
            Err(error) => {
                report.error(format!("grid world: {error}"));
                return None;
            }
        }
    } else if cards.interleaved() {
        match relayout_rooms(&mut cooked, &cards) {
            Ok(rooms) => rooms,
            Err(error) => {
                report.error(format!("grid world: {error}"));
                return None;
            }
        }
    } else {
        rooms
    };
    let surfaces = |room: &cook::RoomReport| room.quads + room.triangles;
    Some(PlaytestGridWorld {
        core: cooked.core,
        rooms,
        room_chunks: cooked.chunks,
        records: Vec::new(),
        record_lens: Vec::new(),
        pool,
        texture_asset_indices: used,
        faces: cooked.report.rooms.iter().map(surfaces).sum(),
        triangles: cooked
            .report
            .rooms
            .iter()
            .map(|room| room.quads * 2 + room.triangles)
            .sum(),
        max_room_surfaces: cooked.report.rooms.iter().map(surfaces).max().unwrap_or(0),
        summary,
        chunk_skies,
        sky_tables,
        cards,
        panels,
    })
}

/// A cooked level core and its room chunks, without the package around them.
#[derive(Clone, Debug)]
pub struct CookedLevelBytes {
    pub core: Vec<u8>,
    /// One chunk per room, unpadded.
    pub chunks: Vec<Vec<u8>>,
    /// Cook warnings, one per line.
    pub warnings: Vec<String>,
}

/// Bake the far skin of `cooked` and put it in the core: each level material
/// coloured by its mean texel colour, lit by the baked light of the surface.
fn add_skin(
    project: &ProjectDocument,
    project_root: &Path,
    world: &GridWorld,
    cooked: &mut cook::CookedLevel,
    report: &mut PlaytestValidationReport,
) -> Result<(), String> {
    use crate::grid::lighting::{AlbedoTable, BakeMaterials};
    let ids: Vec<Option<ResourceId>> = cooked.materials.iter().map(|(id, _)| *id).collect();
    let table = AlbedoTable::from_project(project, project_root, ids.iter().copied());
    let albedo: Vec<[f32; 3]> = ids.iter().map(|&id| table.albedo(id)).collect();
    let mut settings = cook::skin::SkinSettings::default();
    if world.stream.flood.skin_tolerance > 0 {
        settings.height_tolerance = i32::from(world.stream.flood.skin_tolerance);
    }
    let (block, summary) = cook::skin::bake(cooked, &albedo, settings)?;
    cooked.level.skin = Some(block);
    cooked.core = cooked.level.try_to_bytes()?;
    cooked.report.core_bytes = cooked.core.len();
    report.warn(format!(
        "skin: {} sectors in {} leaves (1, 2, 4, 8 sectors across: {:?}), {} bytes in the core, height step {} units",
        summary.sectors, summary.leaves, summary.by_size, summary.bytes, summary.height_step
    ));
    Ok(())
}

/// Set each room's sector for the pack order of a level with cards
/// ([`PlaytestCards::pack_order`]), rewrite the core with them, and return
/// the rooms as one run of sectors in that order, the tiles' pictures as
/// zeroed sectors.
fn relayout_rooms(
    cooked: &mut cook::CookedLevel,
    cards: &PlaytestCards,
) -> Result<Vec<u8>, String> {
    let rooms = cooked.chunks.len();
    let mut blob = Vec::new();
    let mut next = 0u32;
    for item in cards.pack_order(rooms) {
        match item {
            PackItem::Room(room) => {
                let room = room as usize;
                let chunk = &cooked.chunks[room];
                let sectors = chunk.len().div_ceil(SECTOR_BYTES).max(1);
                cooked.level.rooms[room].pack_sector = next;
                cooked.level.rooms[room].pack_sectors = u16::try_from(sectors)
                    .map_err(|_| format!("room {room} chunk is too large"))?;
                next += sectors as u32;
                blob.extend_from_slice(chunk);
                blob.resize(blob.len().next_multiple_of(SECTOR_BYTES), 0);
            }
            PackItem::Tile(_) => {
                next += cards.tile_sectors();
                blob.resize(blob.len() + cards.tile_sectors() as usize * SECTOR_BYTES, 0);
            }
            PackItem::Block(x) => {
                let sectors = (cards.blocks[x as usize].len() / SECTOR_BYTES) as u32;
                next += sectors;
                blob.resize(blob.len() + sectors as usize * SECTOR_BYTES, 0);
            }
            PackItem::Eye(_) => {}
        }
    }
    cooked.core = cooked.level.try_to_bytes()?;
    Ok(blob)
}

/// [`relayout_rooms`] for a level with panels: the rooms and the eyes' blocks in
/// [`PlaytestPanels::order`], the blocks as zeroed sectors.
fn relayout_panel_rooms(
    cooked: &mut cook::CookedLevel,
    panels: &PlaytestPanels,
) -> Result<Vec<u8>, String> {
    let mut blob = Vec::new();
    let mut next = 0u32;
    for item in &panels.order {
        match *item {
            PackItem::Room(room) => {
                let room = room as usize;
                let chunk = &cooked.chunks[room];
                let sectors = chunk.len().div_ceil(SECTOR_BYTES).max(1);
                cooked.level.rooms[room].pack_sector = next;
                cooked.level.rooms[room].pack_sectors = u16::try_from(sectors)
                    .map_err(|_| format!("room {room} chunk is too large"))?;
                next += sectors as u32;
                blob.extend_from_slice(chunk);
                blob.resize(blob.len().next_multiple_of(SECTOR_BYTES), 0);
            }
            PackItem::Eye(eye) => {
                let bytes = panels.blocks[eye as usize].len();
                next += (bytes / SECTOR_BYTES) as u32;
                blob.resize(blob.len() + bytes, 0);
            }
            PackItem::Tile(_) | PackItem::Block(_) => {}
        }
    }
    cooked.core = cooked.level.try_to_bytes()?;
    Ok(blob)
}

/// Where the player starts, in runtime units, when the scene places one (the cook has already
/// brought the scene to runtime units).
fn player_start(scene: &crate::Scene) -> Option<[i32; 3]> {
    scene.nodes().iter().find_map(|node| {
        let is_player = match node.kind {
            crate::NodeKind::SpawnPoint { player: true, .. } => true,
            crate::NodeKind::Entity => {
                super::cook_entities::component_character_controller(scene, node)
                    .is_some_and(|controller| controller.player)
            }
            _ => false,
        };
        is_player.then(|| node.transform.translation.map(|v| v.round() as i32))
    })
}

/// Eyes, nearest the start first, whose blocks the level load reads at once.
const FIRST_EYES: usize = 3;

/// Sectors beyond the radius the rooms are drawn whole to that the level load may read rooms
/// and eyes from (a room is placed by its centre; rooms are wide).
const START_MARGIN_SECTORS: i32 = 10;

/// Height of the bake eyes of the panels above the floor they stand over, in sectors: the
/// follow camera's, as for the room cards.
const PANEL_EYE_SECTORS: f32 = 1.45;

/// Floors this many sectors apart (or more) in height get bake eyes of their own where they
/// stand over the same lattice cell: a bridge over the ground.
const PANEL_STOREY_SECTORS: f32 = 2.5;

/// The bake eyes of the panels: the floors a body can stand on and reach, grouped by a
/// lattice of `spacing` sectors (and by storey), each group one eye at the walkable sector
/// nearest the group's mean, a person's height over the floor.
pub fn panel_eyes(
    project: &ProjectDocument,
    world: &GridWorld,
    cooked: &cook::CookedLevel,
    spacing: i32,
) -> Vec<cook::panels::EyeSpec> {
    use crate::grid::walk::{analyse, player_spawns};
    let sector = cooked.level.sector_units as i32;
    let Ok(units) = RuntimeUnits::new(world.scale) else {
        return Vec::new();
    };
    let spawns = player_spawns(project, world);
    let analysis = analyse(world, &spawns, &psx_engine::WalkLimits::DEFAULT);
    // (lattice x, lattice z, storey) -> sectors: (room, x, floor y, z).
    type Floors = Vec<(usize, f32, f32, f32)>;
    let mut groups: std::collections::BTreeMap<(i32, i32, i32), Floors> = Default::default();
    for (ri, room) in world.rooms.iter().enumerate() {
        let record = &cooked.level.rooms[ri];
        for (lx, lz, grid_sector) in room.iter() {
            let Some(floor) = grid_sector.ground_floor() else {
                continue;
            };
            let walk = analysis
                .as_ref()
                .and_then(|a| a.rooms.get(ri))
                .and_then(|r| r.cell([room.origin[0] + lx as i32, room.origin[1] + lz as i32]));
            if let Some(walk) = walk {
                let reach = analysis
                    .as_ref()
                    .is_none_or(|a| !a.reach_ran || !walk.unreachable);
                if !walk.standable || !reach {
                    continue;
                }
            } else if !floor.walkable {
                continue;
            }
            let mean = floor.heights.iter().sum::<i32>() / 4;
            let y = units.rt(room.y_offset + mean) as f32;
            let x = (record.origin[0] + lx as i32 * sector + sector / 2) as f32;
            let z = (record.origin[2] + lz as i32 * sector + sector / 2) as f32;
            let cell = (
                (x / (spacing * sector) as f32).floor() as i32,
                (z / (spacing * sector) as f32).floor() as i32,
                (y / (PANEL_STOREY_SECTORS * sector as f32)).floor() as i32,
            );
            groups.entry(cell).or_default().push((ri, x, y, z));
        }
    }
    let height = PANEL_EYE_SECTORS * sector as f32;
    groups
        .values()
        .map(|cells| {
            let n = cells.len() as f32;
            let (mx, mz) = (
                cells.iter().map(|c| c.1).sum::<f32>() / n,
                cells.iter().map(|c| c.3).sum::<f32>() / n,
            );
            let best = cells
                .iter()
                .min_by(|a, b| {
                    let da = (a.1 - mx).powi(2) + (a.3 - mz).powi(2);
                    let db = (b.1 - mx).powi(2) + (b.3 - mz).powi(2);
                    da.total_cmp(&db)
                })
                .expect("a group has a sector");
            cook::panels::EyeSpec {
                room: best.0,
                eye: [best.1, best.2 + height, best.3],
            }
        })
        .collect()
}

/// Bake the far panels of `cooked` for a tool (the probe examples): `project` and `world` are
/// as [`cook_for_cards`] cooked them.
pub fn bake_panels_for_tools(
    project: &ProjectDocument,
    project_root: &Path,
    world: &GridWorld,
    cooked: &cook::CookedLevel,
) -> Result<PlaytestPanels, String> {
    bake_panels(
        project,
        project_root,
        world,
        cooked,
        &mut PlaytestValidationReport::default(),
    )
}

/// Bake the far panels of `cooked`.
fn bake_panels(
    project: &ProjectDocument,
    project_root: &Path,
    world: &GridWorld,
    cooked: &cook::CookedLevel,
    report: &mut PlaytestValidationReport,
) -> Result<PlaytestPanels, String> {
    use crate::grid::lighting::{AlbedoTable, BakeMaterials};
    use psx_grid::flood::{Flood, FloodQuery, NO_PAGE_BUDGET};
    let started = std::time::Instant::now();
    let ids: Vec<Option<ResourceId>> = cooked.materials.iter().map(|(id, _)| *id).collect();
    let table = AlbedoTable::from_project(project, project_root, ids.iter().copied());
    let albedo: Vec<[f32; 3]> = ids.iter().map(|&id| table.albedo(id)).collect();
    let sector = cooked.level.sector_units as i32;
    let flood = &world.stream.flood;
    let spacing = i32::from(flood.panels);
    let eyes = panel_eyes(project, world, cooked, spacing);
    if eyes.is_empty() {
        return Err("panels: the level has no floor to put a bake eye on".into());
    }
    let limit = i32::from(i16::MAX);
    if eyes
        .iter()
        .any(|e| e.eye.iter().any(|v| v.abs() as i32 > limit))
    {
        return Err(
            "panels: a bake eye lies past 32767 runtime units from the world origin".into(),
        );
    }
    let tris = cook::panels::World::new(
        cooked,
        &albedo,
        Some(panel_textures(project, project_root, cooked)),
    );
    let blobs = crate::grid::census::LevelBlobs::new(&cooked.core, &cooked.chunks);
    let view = blobs.view();
    // The rooms the flood draws whole from anywhere within a lattice cell of an eye: the eye's
    // own flood with the radius cut by the distance the eye can be from its lattice vertex
    // (the flood's distance is within 7 percent of straight).
    let reach = spacing * sector * 4 / 5;
    let radius = (i32::from(flood.near_radius) * sector - reach * 107 / 100).max(0);
    let rooms = cooked.level.rooms.len();
    let exclude = |spec: &cook::panels::EyeSpec| -> Vec<bool> {
        let mut flood = Flood::new(&view);
        flood.solve(
            &view,
            FloodQuery {
                room: spec.room,
                at: spec.eye.map(|v| v as i32),
                radius,
                page_budget: NO_PAGE_BUDGET,
            },
        );
        let mut drawn = vec![false; rooms];
        for entry in flood.rooms() {
            drawn[usize::from(entry.room)] = true;
        }
        drawn
    };
    let focal = PANEL_FOCAL;
    let baked = cook::panels::bake_eyes(&tris, &eyes, focal as f32, &exclude);
    let blocks = cook::panels::eye_blocks(&baked);
    let count: usize = baked.iter().map(|set| set.iter().flatten().count()).sum();
    // Where the player starts, in plan: the pack ends around it.
    let start = player_start(project.active_scene()).map(|point| {
        // What the first frames read: the rooms the streamer wants with the player there, and
        // the eyes nearest.
        let mut exact = Vec::new();
        let mut window = psx_grid::stream::StreamWindow::new(&view);
        let room = psx_grid::stream::room_at(&view, 0, point);
        let mut wanted = Vec::new();
        window.wanted(
            &view,
            room.map(|r| (r, point)),
            room.map(|r| (r, point)),
            &mut wanted,
        );
        // Need rooms first, nearest first: the order the streamer asks for them in.
        wanted.sort_by_key(|w| (w.want != psx_grid::stream::Want::Need, w.distance));
        exact.extend(wanted.iter().map(|w| (false, usize::from(w.room))));
        let at = [point[0], point[2]];
        let mut by_distance: Vec<(i64, usize)> = eyes
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let (dx, dz) = (
                    i64::from(e.eye[0] as i32 - at[0]),
                    i64::from(e.eye[2] as i32 - at[1]),
                );
                (dx * dx + dz * dz, i)
            })
            .collect();
        by_distance.sort();
        exact.extend(by_distance.iter().take(FIRST_EYES).map(|&(_, i)| (true, i)));
        cook::panels::PackStart {
            at,
            radius: (i32::from(flood.near_radius) + START_MARGIN_SECTORS) * sector,
            exact,
        }
    });
    // Rooms and eyes along one Hilbert curve; an eye with nothing to show is left out.
    let room_centres: Vec<Option<[i32; 2]>> = tris
        .centres
        .iter()
        .map(|c| c.map(|c| [c[0] as i32, c[1] as i32]))
        .collect();
    let eye_centres: Vec<Option<[i32; 2]>> = eyes
        .iter()
        .zip(&blocks)
        .map(|(e, block)| (!block.is_empty()).then(|| [e.eye[0] as i32, e.eye[2] as i32]))
        .collect();
    let mut order = vec![PackItem::Room(0)];
    order.extend(
        cook::panels::pack_order(&room_centres, &eye_centres, start.as_ref())
            .into_iter()
            .map(|(is_eye, i)| {
                if is_eye {
                    PackItem::Eye(i as u32)
                } else {
                    PackItem::Room(i as u32)
                }
            }),
    );
    if let Some(start) = &start {
        let (at, radius) = (start.at, start.radius);
        let within = room_centres
            .iter()
            .chain(&eye_centres)
            .flatten()
            .filter(|c| {
                let (dx, dz) = (i64::from(c[0] - at[0]), i64::from(c[1] - at[1]));
                dx * dx + dz * dz <= i64::from(radius) * i64::from(radius)
            })
            .count();
        report.warn(format!(
            "panels: the pack ends around the start at {at:?} (radius {radius}): {within} rooms and eyes of {} placed, {} read at once",
            room_centres.iter().chain(&eye_centres).flatten().count(),
            start.exact.len()
        ));
    } else {
        report.warn("panels: the scene has no player start the pack could end around".to_string());
    }
    let kib: usize = blocks.iter().map(Vec::len).sum::<usize>() / 1024;
    if std::env::var_os("PANEL_TILE_STATS").is_some() {
        let (mut tiles, mut bytes, mut stored) = (0usize, 0usize, 0usize);
        for panel in baked.iter().flat_map(|set| set.iter().flatten()) {
            tiles += panel.tile_count();
            bytes += panel.tile_bytes();
            stored += panel
                .tiles
                .iter()
                .flatten()
                .filter(|t| t.len() >= psx_grid::tile_lz::TILE_BYTES)
                .count();
        }
        report.warn(format!(
            "panel tiles: {tiles} stored in {bytes} bytes (mean {:.0} B, {stored} kept whole), {} bytes uncompressed",
            bytes as f64 / tiles.max(1) as f64,
            tiles * psx_grid::tile_lz::TILE_BYTES
        ));
    }
    report.warn(format!(
        "panels: {} eyes {} sectors apart, {} panels, {} KiB of pictures, drawn whole radius {} sectors (cut to {:.1} for the eyes' reach), baked in {:.1} s",
        eyes.len(),
        spacing,
        count,
        kib,
        flood.near_radius,
        radius as f32 / sector as f32,
        started.elapsed().as_secs_f64()
    ));
    let scene = project.active_scene();
    let sky = scene.world_sky_for_node(scene.root);
    let backdrop = sky.map_or([[0; 3]; 3], |s| {
        [s.lower_color, s.horizon_color, s.top_color]
    });
    Ok(PlaytestPanels {
        backdrop,
        eyes: eyes
            .iter()
            .map(|e| [e.eye[0] as i16, e.eye[1] as i16, e.eye[2] as i16])
            .collect(),
        blocks,
        order,
        focal,
        spacing: spacing * sector,
        sector,
        real: i32::from(flood.near_radius) * sector,
    })
}

/// The textures of the level's materials as the panel bake samples them, by material slot.
pub fn panel_textures(
    project: &ProjectDocument,
    project_root: &Path,
    cooked: &cook::CookedLevel,
) -> cook::panels::TextureSet {
    let mut set = cook::panels::TextureSet::default();
    for (id, bound) in &cooked.materials {
        let image = bound
            .textured
            .then(|| {
                let id = (*id)?;
                let (_, bytes) = crate::resolve_material_texture_psxt(project, id, project_root)
                    .ok()
                    .flatten()?;
                cook::panels::TextureImage::from_psxt(&bytes)
            })
            .flatten();
        set.images.push(image);
        set.tint.push(bound.tint.map(|c| f32::from(c) / 128.0));
    }
    set
}

/// The projection plane the panels are baked for: the follow camera's, at 43 degrees of
/// vertical field of view on a 240 row screen.
const PANEL_FOCAL: i32 = 305 * (psx_grid::render::COLS as i32 * 64) / 320;

/// Height of a person's eye above the floor, in sectors, that the room cards are drawn
/// from: the follow camera's (its boom and a person's height).
const CARD_EYE_HEIGHT_SECTORS: f32 = 1.45;

/// Height of the floor the player spawns on, runtime units, when the scene places one.
fn player_floor_y(scene: &crate::Scene) -> Option<f32> {
    scene.nodes().iter().find_map(|node| match node.kind {
        crate::NodeKind::SpawnPoint { player: true, .. } => Some(node.transform.translation[1]),
        crate::NodeKind::Entity
            if super::cook_entities::component_character_controller(scene, node)
                .is_some_and(|controller| controller.player) =>
        {
            Some(node.transform.translation[1])
        }
        _ => None,
    })
}

/// The eye the room cards are baked from: out along each heading by the radius the rooms
/// are drawn whole to, where the game hands a room's geometry over to its card, at a
/// person's height above the floor the player spawns on (a room far below or above that
/// floor is seen from where the player sees it), or above the room's own lowest geometry
/// for a scene with no spawn.
pub fn room_card_eye(
    flood: &crate::grid::world::FloodSettings,
    sector: i32,
    spawn_floor_y: Option<f32>,
) -> cook::cards::RoomEye {
    let eye = CARD_EYE_HEIGHT_SECTORS * sector as f32;
    cook::cards::RoomEye {
        distance: (i32::from(flood.near_radius) * sector) as f32,
        height: spawn_floor_y.map_or(cook::cards::EyeHeight::AboveRoom(eye), |y| {
            cook::cards::EyeHeight::World(y + eye)
        }),
    }
}

/// What the cards are baked from.
pub struct CardInputs {
    pub cooked: cook::CookedLevel,
    /// The mean colour of each material of the level.
    pub albedo: Vec<[f32; 3]>,
    /// Height of the floor the player spawns on, runtime units.
    pub spawn_floor_y: Option<f32>,
}

/// Cook `world` as [`cook_level_bytes`] does and return what the cards are baked from: for the
/// card fit example and tests. `project` is as authored; it is scaled to runtime units here,
/// as the package build does.
pub fn cook_for_cards(
    project: &ProjectDocument,
    world: &GridWorld,
    project_root: &Path,
) -> Result<CardInputs, String> {
    use crate::grid::lighting::{AlbedoTable, BakeMaterials};
    let mut scaled = project.clone();
    crate::units::scale_project_to_engine_units(&mut scaled);
    let project = &scaled;
    let lights = scene_lights(project.active_scene());
    let mut assets = Vec::new();
    let mut by_path = HashMap::new();
    let mut binder = Binder {
        project,
        root: project_root,
        assets: &mut assets,
        by_path: &mut by_path,
        used: Vec::new(),
        warnings: Vec::new(),
    };
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder: &mut binder,
            lights: &lights,
            first_pack_sector: 0,
            lod_far_cells: 0,
            texture_repeat: cook::texture_repeat_default(),
        },
        None,
    )?;
    let ids: Vec<Option<ResourceId>> = cooked.materials.iter().map(|(id, _)| *id).collect();
    let table = AlbedoTable::from_project(project, project_root, ids.iter().copied());
    let albedo = ids.iter().map(|&id| table.albedo(id)).collect();
    Ok(CardInputs {
        cooked,
        albedo,
        spawn_floor_y: player_floor_y(project.active_scene()),
    })
}

/// Bake the far cards of `cooked`: each level material coloured by its mean
/// texel colour, lit by the baked light of the surface.
fn bake_cards(
    project: &ProjectDocument,
    project_root: &Path,
    world: &GridWorld,
    cooked: &cook::CookedLevel,
    report: &mut PlaytestValidationReport,
) -> Result<PlaytestCards, String> {
    use crate::grid::lighting::{AlbedoTable, BakeMaterials};
    use psx_grid::render::{SLOT_BYTES, VIEWS};
    let started = std::time::Instant::now();
    let ids: Vec<Option<ResourceId>> = cooked.materials.iter().map(|(id, _)| *id).collect();
    let table = AlbedoTable::from_project(project, project_root, ids.iter().copied());
    let albedo: Vec<[f32; 3]> = ids.iter().map(|&id| table.albedo(id)).collect();
    let sector = i32::from(cooked.level.sector_units);
    let flood = &world.stream.flood;
    let (rooms_baked, summary) = cook::cards::bake(
        cooked,
        &albedo,
        // PSXED_CARDS_SHEAR=1 bakes the shear orthographic room cards the game had before,
        // to compare the two (the gap scan).
        std::env::var_os("PSXED_CARDS_SHEAR")
            .is_none()
            .then(|| room_card_eye(flood, sector, player_floor_y(project.active_scene()))),
    );
    // The layers of cards beyond the room cards: the guest works their bands out the same way
    // (`psx_grid::render::outer_bands`), so the level carries only the layer of each card.
    let room_far = flood.card_far_sectors() * sector;
    let room_fade = 3 * sector;
    let bands = psx_grid::render::outer_bands(room_far, room_fade);
    let mut layers: Vec<[i32; 3]> = vec![[flood.card_near_sectors() * sector, room_far, room_fade]];
    let mut specs = Vec::new();
    let mut prev_far = room_far;
    for (band, layer) in bands.iter().zip(flood.card_layer_bands()) {
        layers.push([band.near, band.far, band.fade]);
        specs.push(cook::cards::LayerSpec {
            near: prev_far,
            far: (band.far < psx_grid::render::CARD_UNLIMITED).then_some(band.far),
            cell: layer.cell * sector,
            depth: layer.depth.map(|d| d * sector),
        });
        prev_far = band.far;
    }
    let layer_started = std::time::Instant::now();
    let layer_cards = cook::cards::bake_layers(cooked, &albedo, &rooms_baked, &specs);
    let rooms = rooms_baked.len();
    let mut baked = rooms_baked;
    let layer_counts: Vec<usize> = layer_cards.iter().map(Vec::len).collect();
    baked.extend(layer_cards.into_iter().flatten().map(Some));
    let limit = i32::from(i16::MAX);
    let mut rows = Vec::with_capacity(baked.len());
    for (room, card) in baked.iter().enumerate() {
        let Some(card) = card else {
            rows.push([0; 6]);
            continue;
        };
        let reach = [
            card.centre[0] - card.half_width,
            card.centre[0] + card.half_width,
            card.centre[1] - card.half_width,
            card.centre[1] + card.half_width,
            card.base,
            card.base + card.height,
        ];
        if reach.iter().any(|v| v.abs() > limit) {
            return Err(format!(
                "the card of room {room} reaches past 32767 runtime units from the world origin"
            ));
        }
        rows.push([
            card.centre[0] as i16,
            card.centre[1] as i16,
            card.base as i16,
            card.height as i16,
            card.half_width as i16,
            i16::from(card.layer),
        ]);
    }
    // Tiles of eight rooms that stand together, each its eight views one
    // after another: see `cook::cards::disc_layout`. The layers' cards go in
    // tiles of their own after the rooms'.
    let centres: Vec<Option<[i32; 2]>> = baked
        .iter()
        .take(rooms)
        .map(|card| card.as_ref().map(|card| card.centre))
        .collect();
    let layout = cook::cards::disc_layout(&centres);
    let layer_centres: Vec<Option<[i32; 2]>> = baked
        .iter()
        .skip(rooms)
        .map(|card| card.as_ref().map(|card| card.centre))
        .collect();
    let layer_layout = cook::cards::disc_layout(&layer_centres);
    let room_tiles = (layout.sectors / (cook::cards::TILE_ROOMS * VIEWS as u32)) as usize;
    let mut layout = layout;
    layout.offsets.extend(layer_layout.offsets.iter().map(|&o| {
        if o == u32::MAX {
            o
        } else {
            o + layout.sectors
        }
    }));
    layout.sectors += layer_layout.sectors;
    // The copies are worked out for the rooms' cards alone.
    let centres: Vec<Option<[i32; 2]>> = centres
        .into_iter()
        .chain(std::iter::repeat_n(None, baked.len() - rooms))
        .collect();
    let tile = cook::cards::TILE_ROOMS as usize;
    let mut pictures = vec![0u8; layout.sectors as usize * SLOT_BYTES];
    for (card, &offset) in baked.iter().zip(&layout.offsets) {
        let Some(card) = card.as_ref().filter(|_| offset != u32::MAX) else {
            continue;
        };
        for view in 0..VIEWS {
            let to = (offset as usize + view * tile) * SLOT_BYTES;
            pictures[to..to + SLOT_BYTES]
                .copy_from_slice(&card.sectors[view * SLOT_BYTES..(view + 1) * SLOT_BYTES]);
        }
    }
    // Copies of the pictures each tile's neighbourhood asks for, laid out
    // beside it: see `cook::cards::local_copies`.
    let far = room_far;
    let mut local = cook::cards::local_copies(
        &layout,
        &centres,
        flood.card_copy_near_sectors() * sector,
        far + 9 * sector,
        sector,
    );
    // The tiles of the layers' cards stand nowhere the eye is in.
    for centre in local.centres.iter_mut().skip(room_tiles) {
        *centre = [i16::MAX as i32, i16::MAX as i32];
    }
    let tiles = local.centres.len();
    let tile_bytes = tile * VIEWS * SLOT_BYTES;
    let mut blocks = Vec::new();
    let mut copies = vec![0u32; tiles * tiles];
    let mut local_bytes = 0usize;
    for (x, entries) in local.entries.iter().enumerate() {
        let mut block = Vec::new();
        for &(y, mask) in entries {
            copies[x * tiles + y as usize] =
                (block.len() / SLOT_BYTES) as u32 | (u32::from(mask) << 24);
            for view in 0..VIEWS {
                if mask & (1 << view) != 0 {
                    let from = y as usize * tile_bytes + view * tile * SLOT_BYTES;
                    block.extend_from_slice(&pictures[from..from + tile * SLOT_BYTES]);
                }
            }
        }
        // A chunk is at least a sector.
        block.resize(block.len().max(SLOT_BYTES), 0);
        local_bytes += block.len();
        blocks.push(block);
    }
    let room_tile: Vec<u8> = layout
        .offsets
        .iter()
        .map(|&o| {
            if o == u32::MAX {
                0
            } else {
                (o as usize / (tile * VIEWS)) as u8
            }
        })
        .collect();
    let room_place: Vec<u8> = layout
        .offsets
        .iter()
        .map(|&o| {
            if o == u32::MAX {
                0
            } else {
                (o as usize % tile) as u8
            }
        })
        .collect();
    if tiles > 0 {
        report.warn(format!(
            "cards: {tiles} tiles of {tile} rooms, copies near each tile: {} KiB ({} KiB a tile), tables {} B",
            local_bytes / 1024,
            local_bytes / 1024 / tiles.max(1),
            tiles * tiles * 4 + tiles * 4 + 2 * room_tile.len()
        ));
    }
    let scene = project.active_scene();
    let sky = scene.world_sky_for_node(scene.root);
    let backdrop = sky.map_or([[0; 3]; 3], |s| {
        [s.lower_color, s.horizon_color, s.top_color]
    });
    if !layer_counts.is_empty() {
        report.warn(format!(
            "cards: layers beyond the rooms' {} sectors: {} cards ({} pictures, {} KiB) in {:.1} s; bands (near, far, fade, in sectors) {:?}",
            flood.card_far_sectors(),
            layer_counts.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" + "),
            layer_counts.iter().sum::<usize>() * VIEWS,
            layer_counts.iter().sum::<usize>() * VIEWS * SLOT_BYTES / 1024,
            layer_started.elapsed().as_secs_f64(),
            layers
                .iter()
                .map(|l| (l[0] / sector, if l[1] >= psx_grid::render::CARD_UNLIMITED { -1 } else { l[1] / sector }, l[2] / sector))
                .collect::<Vec<_>>()
        ));
    }
    report.warn(format!(
        "cards: {} rooms in {:.1} s ({} empty), {} KiB of pictures, heights {} to {} units, half widths {} to {}",
        summary.cards,
        started.elapsed().as_secs_f64(),
        summary.empty,
        summary.bytes / 1024,
        summary.height.0,
        summary.height.1,
        summary.half_width.0,
        summary.half_width.1,
    ));
    Ok(PlaytestCards {
        rows,
        pictures,
        offsets: layout.offsets,
        stride: cook::cards::TILE_ROOMS,
        tile_centres: local
            .centres
            .iter()
            .map(|c| [c[0] as i16, c[1] as i16])
            .collect(),
        blocks: if tiles > 0 { blocks } else { Vec::new() },
        copies: if tiles > 0 { copies } else { Vec::new() },
        room_tile,
        room_place,
        far: world.stream.flood.card_far_sectors() * sector,
        fade: 3 * sector,
        near: flood.card_near_sectors() * sector,
        backdrop,
    })
}

/// How [`cook_level_bytes`] binds materials.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MaterialBinding {
    /// The project's own materials and textures, as Play binds them.
    Project,
    /// Every material a 64 by 64 texture of its own id, the project's
    /// textures and the scene's lights ignored, far LOD quads on: the stand-in
    /// the flood design note was measured with, for reproducing its tables.
    StandIn,
}

/// Binds every material as an untextured-size 64 by 64 stand-in.
struct StandInBinder;

impl MaterialBinder for StandInBinder {
    fn bind(&mut self, material: Option<ResourceId>) -> Result<BoundMaterial, String> {
        Ok(BoundMaterial {
            asset: material.map_or(0, |m| m.raw() as u16),
            flags: 0,
            blend: 0,
            uv_motion: 0,
            tint: [128; 3],
            textured: material.is_some(),
            texels: [TILE_TEXELS, TILE_TEXELS],
        })
    }
}

/// Cook `world` into its level core and room chunks, for tools that read the
/// shape of a level (page counts, portals, quads per sector) without
/// building a disc: no sky is baked and no asset is packaged, so it takes
/// seconds, not a minute.
pub fn cook_level_bytes(
    project: &ProjectDocument,
    world: &GridWorld,
    project_root: &Path,
    binding: MaterialBinding,
) -> Result<CookedLevelBytes, String> {
    let lights = match binding {
        MaterialBinding::Project => scene_lights(project.active_scene()),
        MaterialBinding::StandIn => Vec::new(),
    };
    let mut assets = Vec::new();
    let mut by_path = HashMap::new();
    let mut project_binder = Binder {
        project,
        root: project_root,
        assets: &mut assets,
        by_path: &mut by_path,
        used: Vec::new(),
        warnings: Vec::new(),
    };
    let mut stand_in = StandInBinder;
    let binder: &mut dyn MaterialBinder = match binding {
        MaterialBinding::Project => &mut project_binder,
        MaterialBinding::StandIn => &mut stand_in,
    };
    let cooked = cook::cook_level(
        CookInput {
            world,
            binder,
            lights: &lights,
            first_pack_sector: 0,
            lod_far_cells: match binding {
                MaterialBinding::Project => lod_far_cells(),
                MaterialBinding::StandIn => cook::lod::LOD_FAR_CELLS,
            },
            texture_repeat: cook::texture_repeat_default(),
        },
        None,
    )?;
    let mut warnings = std::mem::take(&mut project_binder.warnings);
    warnings.extend(cooked.report.warnings);
    Ok(CookedLevelBytes {
        core: cooked.core,
        chunks: cooked.chunks,
        warnings,
    })
}

/// Bake every room's sky ([`cook::sky_bake`], hits in their materials'
/// colours) across the host's cores, on palettes shared by all of them (so
/// the guest streams one over another in place), each stored sparsely against
/// the level's background. A failure leaves the world with its one authored
/// sky.
fn bake_chunk_skies(
    cooked: &cook::CookedLevel,
    colours: &cook::sky_bake::WorldColours<'_>,
    report: &mut PlaytestValidationReport,
) -> (Vec<Vec<u8>>, PlaytestSkyTables) {
    let started = std::time::Instant::now();
    let rooms = cooked.level.rooms.len();
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(rooms.max(1));
    // Run `job` for every room across the cores, results in room order.
    let across = |job: &(dyn Fn(usize) -> Result<Vec<u8>, String> + Sync)| {
        std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|worker| {
                    scope.spawn(move || {
                        (worker..rooms)
                            .step_by(threads)
                            .map(|room| job(room).map(|out| (room, out)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let mut out = vec![Vec::new(); rooms];
            for worker in workers {
                for result in worker
                    .join()
                    .map_err(|_| "a bake thread panicked".to_string())?
                {
                    let (room, bytes) = result?;
                    out[room] = bytes;
                }
            }
            Ok::<_, String>(out)
        })
    };
    let skies = std::sync::Mutex::new(Vec::new());
    let baked = across(&|room| {
        let sky = cook::sky_bake::bake_chunk_sky_with(cooked, room, Some(colours))?;
        skies.lock().map_err(|_| "poisoned")?.push(sky);
        Ok(Vec::new())
    })
    .and_then(|_| {
        use psx_level::sky_tiles as st;
        let mut skies = skies.into_inner().map_err(|_| "poisoned".to_string())?;
        skies.sort_by_key(|sky| sky.chunk);
        let palettes = cook::sky_bake::shared_palettes(&skies)?;
        // Every room fogs alike in a hex level; a room that did not would only
        // store more tiles, never different texels.
        let fog = skies.first().map_or([0.0; 3], |sky| sky.fog);
        let background = cook::sky_bake::SkyBackground::new(fog, &palettes);
        let entries = across(&|room| skies[room].to_sparse(&palettes, &background))?;
        let initial = skies[0].to_psxt_with(&palettes)?;
        let initial_mask = entries[0][st::HEADER_BYTES..st::TILES_OFFSET]
            .chunks_exact(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        Ok((
            entries,
            PlaytestSkyTables {
                initial,
                initial_mask,
                background_rows: background.rows,
                background_runs: background.runs,
                palettes: palettes.to_vec(),
            },
        ))
    });
    match baked {
        Ok((entries, tables)) => {
            let bytes: usize = entries.iter().map(Vec::len).sum();
            report.warn(format!(
                "grid world: baked {rooms} chunk skies in {:.1} s: {} KiB stored sparsely \
                 (dense would be {} KiB), background {} runs",
                started.elapsed().as_secs_f64(),
                bytes / 1024,
                rooms * (tables.initial.len().div_ceil(2048) * 2048) / 1024,
                tables.background_runs.len() / 2,
            ));
            (entries, tables)
        }
        Err(error) => {
            report.warn(format!(
                "grid world: chunk sky bake failed, keeping the authored sky: {error}"
            ));
            (Vec::new(), PlaytestSkyTables::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::starter;

    /// A starter project made beside its base so shared asset paths resolve.
    fn starter_project(tag: &str) -> (std::path::PathBuf, ProjectDocument) {
        let projects = crate::projects_dir();
        let out = projects.join(format!("test-starter-{tag}-{}", std::process::id()));
        let path = starter::create_project(&projects.join(starter::STARTER_BASE), &out, "Test")
            .expect("the starter project is created");
        (
            out,
            ProjectDocument::load_from_path(path).expect("the starter loads"),
        )
    }

    #[test]
    fn the_starter_room_cooks_for_play_as_a_grid_level() {
        let (dir, project) = starter_project("cook");
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let package = package.expect("a package");
        let grid = &package.world_geometry;
        assert!(grid.core.starts_with(b"PGL1"));
        assert!(grid.rooms.starts_with(b"PGRM"));
        assert_eq!(grid.rooms.len() % SECTOR_BYTES, 0);
        assert!(grid.faces > 0 && grid.max_room_surfaces <= grid.faces);
    }

    #[test]
    fn a_grid_world_that_breaks_the_caps_fails_with_a_readable_reason() {
        let (dir, mut project) = starter_project("caps");
        if let Some(world) = project.active_scene_mut().world.grid_mut() {
            // A leaf outside the rectangle, which the document cannot hold.
            let far = [world.rooms[0].origin[0] + 100, world.rooms[0].origin[1]];
            world.rooms[0]
                .leaves
                .insert(
                    crate::grid::leaf::LeafKey::sector(far),
                    crate::grid::GridSector::default(),
                )
                .unwrap();
        }
        let (package, report) = super::super::build_package(&project, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(package.is_none());
        let message = report.error_messages().join("; ");
        assert!(message.contains("grid world: room 0"), "{message}");
        assert!(message.contains("outside the room rectangle"), "{message}");
    }

    /// FNV-1a over a byte string.
    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// Cooks the project in `PSXED_SKY_PROJECT` (a `project.ron`) and holds
    /// every chunk's sparse sky to the dense atlas the dense cook produced:
    /// `PSXED_SKY_HASHES` lists one FNV hash per room of that dense PSXT.
    /// Rebuilding the PSXT from the sparse entry over the shared palettes
    /// must give the same bytes, so not one texel differs.
    #[test]
    #[ignore = "needs PSXED_SKY_PROJECT and PSXED_SKY_HASHES"]
    fn sparse_skies_rebuild_the_dense_atlases_texel_for_texel() {
        use psx_level::sky_tiles as st;
        let project_path = std::env::var("PSXED_SKY_PROJECT").expect("PSXED_SKY_PROJECT");
        let hashes = std::fs::read_to_string(std::env::var("PSXED_SKY_HASHES").unwrap()).unwrap();
        let project = ProjectDocument::load_from_path(&project_path).unwrap();
        let root = std::path::Path::new(&project_path).parent().unwrap();
        let (package, report) = super::super::build_package(&project, root);
        let package = package.expect("a package");
        assert!(report.is_ok(), "{:?}", report.error_messages());
        let world = &package.world_geometry;
        let tables = &world.sky_tables;
        let background = st::Background::new(&tables.background_rows, &tables.background_runs);
        let palettes: Vec<Vec<[u8; 3]>> = tables.palettes.clone();
        let wanted: Vec<&str> = hashes.lines().collect();
        assert_eq!(world.chunk_skies.len(), wanted.len());
        let mut bad = Vec::new();
        let mut total = 0;
        for (room, entry) in world.chunk_skies.iter().enumerate() {
            total += entry.len();
            let mut indices = vec![0u8; 6 * 256 * 256];
            st::decode_indices(entry, &background, &mut indices).expect("a readable entry");
            assert!(st::entry_sectors(st::header_tile_count(entry).unwrap()) <= 26);
            let psxt = psxed_tex::encode_indexed_psxt_with_clut_rows(
                1536,
                256,
                psxed_format::texture::Depth::Bit4,
                &indices,
                palettes.as_slice(),
                false,
            )
            .unwrap();
            if format!("{:016x}", fnv(&psxt)) != wanted[room] {
                bad.push(room);
            }
        }
        std::eprintln!(
            "{} sparse skies, {} KiB; background {} rows runs {}",
            world.chunk_skies.len(),
            total / 1024,
            tables.background_rows.len(),
            tables.background_runs.len() / 2
        );
        assert!(
            bad.is_empty(),
            "atlases that differ from the dense cook: {bad:?}"
        );
        // `PSXED_SKY_ATLAS_DIR`: every room's atlas as the VRAM words it
        // should be (four texels a word), for comparing with a VRAM dump.
        if let Ok(dir) = std::env::var("PSXED_SKY_ATLAS_DIR") {
            for (room, entry) in world.chunk_skies.iter().enumerate() {
                let mut indices = vec![0u8; 6 * 256 * 256];
                st::decode_indices(entry, &background, &mut indices).unwrap();
                let words: Vec<u8> = indices
                    .chunks_exact(4)
                    .flat_map(|t| {
                        (u16::from(t[0])
                            | u16::from(t[1]) << 4
                            | u16::from(t[2]) << 8
                            | u16::from(t[3]) << 12)
                            .to_le_bytes()
                    })
                    .collect();
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("atlas-{room}.bin")),
                    words,
                )
                .unwrap();
            }
        }
        // `PSXED_SKY_EXPECT=tiles,wipes`: which walks 0 -> a -> b of two skies
        // upload and wipe that many tiles (to match a run's counters).
        if let Ok(expect) = std::env::var("PSXED_SKY_EXPECT") {
            let (tiles, wipes) = expect.split_once(',').expect("tiles,wipes");
            let (tiles, wipes): (usize, usize) = (tiles.parse().unwrap(), wipes.parse().unwrap());
            let mask = |room: usize| -> Vec<u32> {
                world.chunk_skies[room][st::HEADER_BYTES..st::TILES_OFFSET]
                    .chunks_exact(4)
                    .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
                    .collect()
            };
            let ones = |m: &[u32]| m.iter().map(|w| w.count_ones() as usize).sum::<usize>();
            let minus = |a: &[u32], b: &[u32]| {
                a.iter()
                    .zip(b)
                    .map(|(a, b)| (a & !b).count_ones() as usize)
                    .sum::<usize>()
            };
            let rooms = world.chunk_skies.len();
            for a in 0..rooms {
                for b in 0..rooms {
                    if a != b
                        && ones(&mask(a)) + ones(&mask(b)) == tiles
                        && minus(&mask(0), &mask(a)) + minus(&mask(a), &mask(b)) == wipes
                    {
                        std::eprintln!("walk 0 -> {a} -> {b} gives {tiles} tiles, {wipes} wipes");
                    }
                }
            }
        }
        // Room 0's mask is what the entry's header says.
        assert_eq!(tables.initial_mask.len(), st::MASK_WORDS);
    }
}
