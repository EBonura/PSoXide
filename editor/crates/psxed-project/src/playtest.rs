//! Playtest pipeline: scene-tree → cooked rooms + master asset
//! table + per-room residency lists, written as a Rust-source
//! manifest the `engine/examples/editor-playtest` example
//! `include!`s.
//!
//! # Why a Rust-source manifest?
//!
//! The runtime example is `no_std` and PSX-target only. It can't
//! deserialize RON / parse RAM-resident config without dragging in
//! crates the cooked path doesn't want. A generated Rust source
//! file with `include_bytes!` references is the lightest contract:
//! the runtime sees `static ASSETS: &[LevelAssetRecord]` /
//! `static ROOM_RESIDENCY: &[RoomResidencyRecord]` and the bytes
//! are baked into the EXE at build time.
//!
//! # Schema lives in `psx-level`
//!
//! The record types ([`psx_level::LevelAssetRecord`] and friends)
//! live in the shared `no_std` `psx-level` crate so the writer
//! here and the reader in the runtime example reference one
//! definition. Whenever a record's shape changes, both ends
//! pick up the change at compile time.
//!
//! # Backing store
//!
//! Today every asset is `include_bytes!`-baked. Tomorrow assets
//! may be paged in from a stream pack on CD; the schema doesn't
//! care. The residency manager already tracks RAM/VRAM membership
//! independently of where bytes live.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use psx_bsp::collision_provider::select_body_hull;
use psx_level::{
    box_prop_flags, character_action_flags, cloud_layer_flags, image_prop_flags, model_clip_flags,
    particle_emitter_flags, sky_flags,
};
use psxed_format::{
    texture::{
        Depth as TextureDepth, TextureHeader, MAGIC as TEXTURE_MAGIC, VERSION as TEXTURE_VERSION,
    },
    AssetHeader,
};

use crate::{
    clamp_ui_font_scale, default_ui_font_scale, default_ui_letter_spacing, AnimationRole,
    CharacterAnimationAction, CharacterControllerSettings, NodeKind, OptionId, OptionKind,
    ParticleEmitterSettings, PhysicsBodySettings, ProjectDocument, PsxBlendMode, ResourceData,
    ResourceId, SceneNode, UiAction, UiAnchor, UiGradient, UiImageEffect, UiNodeId, UiNodeKind,
    UiRect, UiSfxCue, UiTextAlign, UiValueBinding, WorldCameraSettings, WorldStreamingSettings,
    MAX_UI_LETTER_SPACING, MIN_UI_LETTER_SPACING, PHYSICS_WEIGHT_ONE_Q8,
};

mod assets;
mod budget;
pub use budget::*;
mod cook_ui;
mod manifest;
mod performance;
mod schema;
pub(crate) use cook_ui::*;
mod cook_entities;
pub(crate) use cook_entities::*;
pub use cook_entities::{
    bake_model_clip_frame_bounds, bake_model_frame_pair_bounds, model_bounds_joint_transform,
    transform_model_bounds_vertex, ModelBoundsJointTransform, MODEL_FRAME_BOUNDS_PAD_UNITS,
};
mod cook_props_lights;
pub(crate) use cook_props_lights::*;

use assets::{
    expect_room_material_depth, find_resource, load_psxt_bytes, material_texture_bytes,
    resolve_path, sanitise_model_dirname,
};

type CookOutputSink = Arc<dyn Fn(String) + Send + Sync>;

static COOK_OUTPUT_CAPTURE: OnceLock<Mutex<()>> = OnceLock::new();
static COOK_OUTPUT_SINK: OnceLock<Mutex<Option<CookOutputSink>>> = OnceLock::new();

struct CookOutputCaptureGuard;

impl Drop for CookOutputCaptureGuard {
    fn drop(&mut self) {
        *COOK_OUTPUT_SINK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

/// Run one cook operation while mirroring its terminal progress lines.
///
/// Brush compilation uses worker threads, so the temporary sink is
/// process-wide rather than thread-local. Captures are serialized and the
/// sink is removed by an RAII guard even if the cook unwinds.
pub fn capture_cook_output<T>(work: impl FnOnce() -> T) -> (T, Vec<String>) {
    let _capture = COOK_OUTPUT_CAPTURE
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lines = Arc::new(Mutex::new(Vec::new()));
    let captured_lines = Arc::clone(&lines);
    let sink: CookOutputSink = Arc::new(move |line| {
        captured_lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(line);
    });
    *COOK_OUTPUT_SINK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
    let guard = CookOutputCaptureGuard;
    let result = work();
    drop(guard);
    let output = lines
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    (result, output)
}

/// Preserve a cooker diagnostic on stderr and forward it to the active host
/// capture, when the editor launched the cook in-process.
pub(crate) fn emit_cook_output(arguments: std::fmt::Arguments<'_>) {
    let line = arguments.to_string();
    eprintln!("{line}");
    let sink = COOK_OUTPUT_SINK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(sink) = sink {
        sink(line);
    }
}

/// Map a normalized authored POI identity to one fixed read/reward bit pair.
/// The full save bitmap is always declared, so reordering or inserting scene
/// nodes cannot invalidate the card or shift another POI's state. Hash-pair
/// collisions are rejected during cook with an actionable ID change.
fn stable_poi_flag_pair(persistence_id: &str) -> (u16, u16) {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    const PAIR_CAPACITY: usize = psx_level::POI_PERSISTENT_FLAG_CAPACITY / 2;
    let mut hash = FNV_OFFSET;
    for byte in persistence_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let read = ((hash as usize % PAIR_CAPACITY) * 2) as u16;
    (read, read + 1)
}

/// Map one stable scene-node identity to a persistent world-state bit.
/// Destructibles use their authored node id rather than cook order so inserting
/// another object cannot make an existing memory-card flag refer to a new wall.
fn stable_destructible_flag(node_id: crate::NodeId) -> u16 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET;
    for byte in b"destructible:"
        .iter()
        .chain(node_id.raw().to_le_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash as usize % psx_level::POI_PERSISTENT_FLAG_CAPACITY) as u16
}

fn project_save_identity(project_name: &str) -> (String, String) {
    const FNV_OFFSET: u32 = 0x811c9dc5;
    const FNV_PRIME: u32 = 0x01000193;
    let mut hash = FNV_OFFSET;
    for byte in project_name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let save_name = format!("BESLES-{hash:08X}");
    let mut save_title: String = project_name
        .chars()
        .filter(|ch| ch.is_ascii_graphic() || *ch == ' ')
        .map(|ch| ch.to_ascii_uppercase())
        .take(32)
        .collect();
    if save_title.trim().is_empty() {
        save_title = "PSOXIDE PROJECT".to_string();
    }
    (save_name, save_title)
}

#[cfg(test)]
mod poi_persistence_tests {
    use super::*;

    #[test]
    fn persistent_flag_pair_depends_only_on_normalized_id() {
        let alpha = stable_poi_flag_pair("archive-alpha");
        assert_eq!(alpha, stable_poi_flag_pair("archive-alpha"));
        assert_ne!(alpha, stable_poi_flag_pair("archive-beta"));
        assert_eq!(alpha.0 % 2, 0);
        assert_eq!(alpha.1, alpha.0 + 1);
        assert!(usize::from(alpha.1) < psx_level::POI_PERSISTENT_FLAG_CAPACITY);
    }

    #[test]
    fn destructible_flag_is_stable_and_inside_the_save_bitmap() {
        let mut project = ProjectDocument::new("destructible-flags");
        let scene = project.active_scene_mut();
        let first = scene.add_node(scene.root, "first", crate::NodeKind::Node);
        let second = scene.add_node(scene.root, "second", crate::NodeKind::Node);
        assert_eq!(
            stable_destructible_flag(first),
            stable_destructible_flag(first)
        );
        assert_ne!(
            stable_destructible_flag(first),
            stable_destructible_flag(second)
        );
        assert!(
            usize::from(stable_destructible_flag(first)) < psx_level::POI_PERSISTENT_FLAG_CAPACITY
        );
    }

    #[test]
    fn memory_card_identity_is_stable_project_specific_and_bounded() {
        let first = project_save_identity("Cortex Ignition");
        assert_eq!(first, project_save_identity("Cortex Ignition"));
        assert_ne!(first.0, project_save_identity("Quake PSX").0);
        assert!(first.0.len() <= 20);
        assert!(!first.1.is_empty());
        assert!(first.1.len() <= 32);
        assert!(first.0.is_ascii() && first.1.is_ascii());
    }
}

pub use manifest::{
    cook_to_dir, default_generated_dir, render_manifest_source, streamed_room_chunk_memory_report,
    write_cook_result, write_package,
};
pub use performance::{playtest_performance_envelope, PlaytestPerformanceEnvelope};
pub use schema::*;

fn runtime_sky_flags(sky: crate::ResolvedSkySettings) -> u16 {
    if !sky.sky_enabled() {
        return 0;
    }
    let projection = match sky.mode {
        crate::SkyMode::Off => 0,
        crate::SkyMode::Panorama => sky_flags::PANORAMA,
        crate::SkyMode::QuakeLayered => sky_flags::QUAKE_LAYERED,
        crate::SkyMode::Cube => sky_flags::CUBE,
    };
    sky_flags::ENABLED
        | projection
        | if sky.visibility == crate::SkyVisibility::ThroughSkySurfaces {
            sky_flags::THROUGH_SKY_SURFACES
        } else {
            0
        }
}

const UI_LARGE_IMAGE_STRIP_WIDTH: u16 = 160;
const UI_LARGE_IMAGE_MAX_DIMENSION: u16 = 256;

fn validate_pxbsp_body_hulls(
    project: &ProjectDocument,
    body_hulls: &[psx_bsp::collision_provider::CookedBodyHull],
    characters: &[PlaytestCharacter],
    game_entities: &[PlaytestGameEntity],
    report: &mut PlaytestValidationReport,
) {
    let largest_radius = body_hulls.iter().map(|hull| hull.radius).max().unwrap_or(0);
    let largest_height = body_hulls.iter().map(|hull| hull.height).max().unwrap_or(0);
    for character in characters {
        if select_body_hull(
            body_hulls,
            i32::from(character.radius),
            i32::from(character.height),
        )
        .is_none()
        {
            let name = project
                .resource(character.source_resource)
                .map(|resource| resource.name.as_str())
                .unwrap_or("<missing Character>");
            report.error(format!(
                "Character '{name}' body {}x{} exceeds every cooked PXBSP hull (largest is {}x{})",
                character.radius, character.height, largest_radius, largest_height,
            ));
        }
    }
    for (index, entity) in game_entities.iter().enumerate() {
        if select_body_hull(
            body_hulls,
            i32::from(entity.radius),
            i32::from(entity.height),
        )
        .is_none()
        {
            report.error(format!(
                "Game entity {index} body {}x{} exceeds every cooked PXBSP hull (largest is {}x{})",
                entity.radius, entity.height, largest_radius, largest_height,
            ));
        }
    }
}

fn brush_world_validation_target(
    error: &crate::brush_world::BrushWorldCookError,
) -> Option<PlaytestValidationTarget> {
    use crate::brush_world::BrushWorldCookError;

    match error {
        BrushWorldCookError::InvalidBrush { brush, face } => {
            Some(PlaytestValidationTarget::Brush {
                brush: *brush,
                face: *face,
            })
        }
        BrushWorldCookError::MissingMover { brush, .. }
        | BrushWorldCookError::BrushOwnerIsNotDoor { brush, .. }
        | BrushWorldCookError::LiquidMover { brush, .. } => Some(PlaytestValidationTarget::Brush {
            brush: *brush,
            face: None,
        }),
        BrushWorldCookError::UnsupportedMoverTransform(node)
        | BrushWorldCookError::MoverOriginOutOfRange(node)
        | BrushWorldCookError::MoverOriginInSolid(node)
        | BrushWorldCookError::InvalidPlayerSpawnTransform(node)
        | BrushWorldCookError::PlayerSpawnInSolid(node)
        | BrushWorldCookError::InvalidDestructibleHealth(node)
        | BrushWorldCookError::InvalidDoorMotion { node, .. } => {
            Some(PlaytestValidationTarget::Node(*node))
        }
        BrushWorldCookError::MissingMaterial(resource)
        | BrushWorldCookError::ResourceIsNotMaterial(resource)
        | BrushWorldCookError::MaterialTexture {
            material: resource, ..
        }
        | BrushWorldCookError::InvalidTexture {
            material: Some(resource),
            ..
        } => Some(PlaytestValidationTarget::Resource(*resource)),
        // Capacity overflows blame the object being compiled when it ran out
        // of room, not the map as a whole.
        BrushWorldCookError::ModelIndexOverflow(node) => {
            Some(PlaytestValidationTarget::Node(*node))
        }
        BrushWorldCookError::TextureAssetOverflow {
            material: Some(resource),
        } => Some(PlaytestValidationTarget::Resource(*resource)),
        BrushWorldCookError::Light {
            node: Some(node), ..
        } => Some(PlaytestValidationTarget::Node(*node)),
        BrushWorldCookError::Collision(
            crate::brush_collision_hulls::CollisionHullCompileError::InvalidPlane(Some(brush)),
        ) => Some(PlaytestValidationTarget::Brush {
            brush: *brush,
            face: None,
        }),
        // Genuinely underivable, each for its own reason:
        // - EmptyStaticWorld / InvalidWorldTree: whole-map conditions.
        // - InvalidTexture{material: None}: the built-in default brush
        //   texture, which is not a project resource.
        // - TextureAssetOverflow{material: None}: same, the default texture.
        // - Pack: BrushPackError indexes SURFACES, leaves and planes of the
        //   compiled BSP. CSG splitting means one authored brush becomes many
        //   surfaces and one surface can come from several brushes, and no
        //   surface-to-brush provenance table is kept, so a surface index
        //   cannot be honestly resolved back to a brush the author can select.
        // - Collision other arms: InvalidBounds indexes the requested hull
        //   size list (a cook constant, not authored content) and
        //   LimitExceeded is a whole-map cap.
        // - Light without a node: the reported light index fell outside the
        //   light list the cook built, so no PointLight node matches it.
        // - Pxbsp: whole-map record counts, alignment and reference checks.
        //   Its InvalidMaterial names a compiled material SLOT, which is a
        //   cook-side index into the merged slot table rather than a resource.
        BrushWorldCookError::EmptyStaticWorld
        | BrushWorldCookError::InvalidWorldTree
        | BrushWorldCookError::InvalidTexture { material: None, .. }
        | BrushWorldCookError::TextureAssetOverflow { material: None }
        | BrushWorldCookError::Pack(_)
        | BrushWorldCookError::Collision(_)
        | BrushWorldCookError::Light { node: None, .. }
        | BrushWorldCookError::Pxbsp(_) => None,
    }
}

#[derive(Clone)]
pub(crate) struct CookedUiImageTexture {
    width: u16,
    fragments: Vec<CookedUiImageFragment>,
}

#[derive(Clone)]
pub(crate) struct CookedUiImageFragment {
    asset_index: usize,
    width: u16,
}

pub(crate) struct PlayerSpawnCandidate<'a> {
    node: &'a SceneNode,
    room_index: u16,
    position: [i32; 3],
    character: Option<ResourceId>,
    controller_settings: Option<CharacterControllerSettings>,
    camera: Option<WorldCameraSettings>,
    weight_q8: u16,
    renderer: Option<ModelRendererComponent>,
    animator: Option<AnimatorComponent<'a>>,
}

fn playtest_streaming_resident_chunk_limit(streaming: WorldStreamingSettings) -> u8 {
    std::env::var("PSXED_PLAYTEST_RESIDENT_CHUNK_LIMIT")
        .ok()
        .and_then(|raw| raw.trim().parse::<u8>().ok())
        .map(|limit| {
            limit.clamp(
                crate::MIN_WORLD_STREAMING_RESIDENT_CHUNKS,
                crate::MAX_WORLD_STREAMING_RESIDENT_CHUNKS,
            )
        })
        .unwrap_or(streaming.resident_chunk_limit)
}

pub(crate) fn yaw_from_degrees(degrees: f32) -> i16 {
    angle_from_degrees(degrees)
}

pub(crate) fn angle_from_degrees(degrees: f32) -> i16 {
    crate::spatial::euler_degrees_to_q12(degrees) as i16
}

pub(crate) fn checked_u32(value: usize, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} exceeds u32"))
}

/// Build a playtest package from `project`. Validates the scene
/// tree, cooks every Room with non-empty geometry, resolves
/// material textures through `project_root`, and assigns the
/// player spawn.
///
/// On any validation error the returned package is `None`.
/// The enemy tuning this controller actually runs with: its own override if it
/// has one, otherwise the Character's. Keeping this in one place is what lets a
/// placed enemy inherit changes made to its type.
fn controller_enemy_behavior(
    project: &ProjectDocument,
    controller: &CharacterControllerComponent,
) -> Option<crate::EnemyBehaviorSettings> {
    if let Some(settings) = controller.settings {
        return settings.enemy;
    }
    controller
        .character
        .and_then(|id| project.resource(id))
        .and_then(|resource| match &resource.data {
            ResourceData::Character(character) => character.enemy_behavior,
            _ => None,
        })
}

pub fn build_package(
    project: &ProjectDocument,
    project_root: &Path,
) -> (Option<PlaytestPackage>, PlaytestValidationReport) {
    // BSP projects cook at engine (Quake) scale: divide every authored
    // length by WORLD_UNIT_DIVISOR on an in-memory clone. Authored
    // files and the editor's preview stay at the historical scale.
    let mut scaled_project = project.clone();
    crate::units::scale_project_to_engine_units(&mut scaled_project);
    let project = &scaled_project;
    let mut report = PlaytestValidationReport::default();
    let scene = project.active_scene();

    // Pass 1: enumerate Room nodes. Index = runtime room id.
    let mut room_nodes: Vec<&SceneNode> = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
        .collect();
    room_nodes.sort_by_key(|node| node.id.raw());
    let uses_pxbsp = true;
    if scene.brushes.is_empty() {
        report.error("the active scene holds no brushes; BSP is the sole world source");
        return (None, report);
    }

    if !room_nodes.is_empty() {
        // Previously these were dropped in silence and the cook went green
        // with the authored Sections missing from the level. A BSP project
        // has exactly one spatial authority, so a Section is a contradiction
        // to resolve in the editor, not a preference to apply here.
        let node = room_nodes[0];
        report.error_at(
            PlaytestValidationTarget::Node(node.id),
            format!(
                "the scene holds {} removed grid Section node(s), starting \
                 with '{}'; delete the obsolete nodes before building",
                room_nodes.len(),
                node.name
            ),
        );
        return (None, report);
    }

    // Pass 2: cook each Room. We need the `CookedWorldGrid` for
    // material slot info; encode straight from it so we don't
    // pay for two cooks. Empty grids skip with a warning.
    let mut assets: Vec<PlaytestAsset> = Vec::new();
    let mut rooms: Vec<PlaytestRoom> = Vec::new();
    let materials: Vec<PlaytestMaterial> = Vec::new();
    // Resolved psxt path → index into `assets` for texture page
    // deduplication (materials sharing one image share one page).
    // First-use order is deterministic because we walk rooms +
    // material slots in deterministic order and assign the
    // texture's compact "texture index" via `texture_asset_for_path.len()`
    // at first insertion (never removed). HashMap is fine -- we
    // only use it for presence tests.
    let mut used_ui_source_paths: Vec<String> = Vec::new();
    let mut texture_asset_for_path: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut sky_texture_assets: Vec<(crate::ResolvedSkySettings, usize)> = Vec::new();
    let uses_room_reflection_probes = project.resources.iter().any(|resource| {
        matches!(
            &resource.data,
            ResourceData::Material(material)
                if material.texture_mode == crate::MaterialTextureMode::ReflectiveProbe
                    || material.enabled_secondary_layer().is_some_and(|layer| {
                        layer.texture_mode == crate::MaterialTextureMode::ReflectiveProbe
                    })
        )
    });
    if uses_room_reflection_probes {
        report.error(
            "room reflection-probe materials belonged to the removed grid renderer; use an image or generated material",
        );
        return (None, report);
    }
    let room_visibility: Vec<PlaytestRoomVisibility> = Vec::new();
    let visibility_cells: Vec<PlaytestVisibilityCell> = Vec::new();
    let visibility_pvs: Vec<PlaytestVisibilityPvs> = Vec::new();
    let visibility_pvs_bits: Vec<u8> = Vec::new();
    let room_surface_caches: Vec<PlaytestRoomSurfaceCache> = Vec::new();
    let room_cache_cells: Vec<PlaytestCachedRoomCell> = Vec::new();
    let room_cache_cell_vertices: Vec<u16> = Vec::new();
    let room_cache_vertices: Vec<PlaytestCachedRoomVertex> = Vec::new();
    let room_cache_surfaces: Vec<PlaytestCachedRoomSurface> = Vec::new();
    let room_portals: Vec<PlaytestRoomPortal> = Vec::new();
    let room_near_rooms: Vec<u16> = Vec::new();

    // The singleton record retains shared world/camera/sky settings for
    // the ordinary gameplay pipeline. Geometry, collision, visibility,
    // and spatial activation come exclusively from resident PXBSP; there
    // is deliberately no synthetic WorldGrid, PSXW asset, room chunk, or
    // grid-PVS bake hiding behind this metadata anchor.
    let world_sector_size = scene
        .world_sector_size_for_node(scene.root)
        .unwrap_or(1024)
        .max(1);
    let resolved_camera = scene
        .world_camera_for_node(scene.root)
        .unwrap_or_default()
        .normalized();
    let resolved_culling = scene
        .world_culling_for_node(scene.root)
        .unwrap_or_default()
        .normalized();
    let streaming = scene
        .world_streaming_for_node(scene.root)
        .unwrap_or_default()
        .normalized();
    let resolved_physics = scene
        .world_physics_for_node(scene.root)
        .unwrap_or_default()
        .normalized();
    let resolved_sky = scene
        .world_sky_for_node(scene.root)
        .unwrap_or_default()
        .resolved_for_room(false, [0; 3]);
    let resolved_far_vista = scene
        .world_far_vista_for_node(scene.root)
        .unwrap_or_default()
        .resolved_for_room(false, [0; 3]);
    rooms.push(PlaytestRoom {
        name: "PXBSP World".to_string(),
        world_asset_index: None,
        reflection_probe_asset_index: None,
        origin_x: 0,
        origin_z: 0,
        origin_y: 0,
        sector_size: world_sector_size,
        draw_distance: resolved_culling.draw_distance,
        chunk_activation_radius_sectors: resolved_culling.chunk_activation_radius_sectors,
        visibility_radius: resolved_culling.visibility_radius,
        resident_chunk_limit: playtest_streaming_resident_chunk_limit(streaming),
        visible_chunk_limit: streaming.visible_chunk_limit,
        gravity_per_tick: resolved_physics.gravity_per_tick,
        material_first: 0,
        material_count: 0,
        portal_first: 0,
        portal_count: 0,
        near_room_first: 0,
        near_room_count: 0,
        overlapped_room_first: 0,
        overlapped_room_count: 0,
        fog_rgb: [0; 3],
        fog_near: 0,
        fog_far: resolved_culling.draw_distance,
        atmosphere_rgb: [0; 3],
        atmosphere_density: 0,
        atmosphere_fall_speed_q4: 0,
        atmosphere_wind_speed_q4: 0,
        sky: PlaytestSky {
            top_rgb: resolved_sky.top_color,
            horizon_rgb: resolved_sky.horizon_color,
            bottom_rgb: resolved_sky.lower_color,
            horizon_percent: resolved_sky.horizon_percent,
            horizon_thickness_percent: resolved_sky.horizon_thickness_percent,
            skybox_columns: resolved_sky.skybox_columns,
            skybox_rows: resolved_sky.skybox_rows,
            flags: runtime_sky_flags(resolved_sky),
            texture_asset_index: None,
            cyclorama_quads: Vec::new(),
            cloud_layer: PlaytestCloudLayer {
                texture_asset_index: None,
                color_rgb: resolved_sky.cloud_layer.color,
                density: resolved_sky.cloud_layer.density,
                altitude: resolved_sky.cloud_layer.altitude,
                extent: resolved_sky.cloud_layer.extent,
                tile_count: resolved_sky.cloud_layer.tile_count,
                scroll_speed: resolved_sky.cloud_layer.scroll_speed,
                noise_seed: resolved_sky.cloud_layer.noise_seed,
                flags: if resolved_sky.cloud_layer.enabled && resolved_sky.enabled {
                    cloud_layer_flags::ENABLED
                } else {
                    0
                },
            },
        },
        far_vista: PlaytestFarVista {
            texture_asset_indices: Vec::new(),
            radius: resolved_far_vista.radius,
            height: resolved_far_vista.height,
            vertical_offset: resolved_far_vista.vertical_offset,
            segments: resolved_far_vista.segments,
            rotation_degrees: resolved_far_vista.rotation_degrees,
            tint_rgb: resolved_far_vista.tint,
            flags: 0,
        },
        camera: PlaytestCamera {
            distance: resolved_camera.distance,
            height: resolved_camera.height,
            target_height: resolved_camera.target_height,
            lock_rise_percent: resolved_camera.lock_rise_percent,
            min_floor_clearance: resolved_camera.min_floor_clearance,
            orbit_speed_level: resolved_camera.orbit_speed_level,
            position_lag_shift: resolved_camera.position_lag_shift,
            focus_lag_shift: resolved_camera.focus_lag_shift,
            distance_lag_shift: resolved_camera.distance_lag_shift,
        },
        flags: 0,
    });

    let texture_asset_base = match u16::try_from(assets.len()) {
        Ok(base) => base,
        Err(_) => {
            report.error("PXBSP texture asset base exceeds u16");
            return (None, report);
        }
    };
    let compiled = match crate::brush_world::compile_brush_world(
        project,
        crate::brush_world::BrushWorldCookOptions {
            project_root,
            mode: project.bsp_cook_mode,
            ambient: [32; 3],
            texture_asset_base,
        },
    ) {
        Ok(compiled) => compiled,
        Err(error) => {
            report.error_maybe_at(
                brush_world_validation_target(&error),
                format!("brush world compile failed: {error}"),
            );
            return (None, report);
        }
    };
    for texture in &compiled.textures {
        let expected = assets.len();
        if usize::from(texture.asset_id) != expected {
            report.error(format!(
                "PXBSP texture asset {} does not match master asset slot {expected}",
                texture.asset_id
            ));
            return (None, report);
        }
        texture_asset_for_path.insert(texture.key.clone(), expected);
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes: texture.bytes.clone(),
            filename: format!("texture_{:03}.psxt", texture.asset_id),
            source_label: texture.key.clone(),
            streamed_class: StreamedClass::None,
        });
    }
    if !compiled.leak_path.is_empty() {
        report.warn(format!(
                "BSP leak reaches the infinite exterior through {} pointfile points; portal PVS includes the opening and writes {}",
                compiled.leak_path.len(),
                crate::brush_playtest::BRUSH_LEAK_FILENAME,
            ));
    }
    let authored_leak_path: Vec<_> = compiled
        .leak_path
        .iter()
        .map(|point| {
            point.map(|coordinate| coordinate.saturating_mul(crate::units::WORLD_UNIT_DIVISOR))
        })
        .collect();
    // PXBSP rooms use the same authored panorama and gameplay-transient
    // lifetime as grid rooms. Register it after the brush textures so
    // enabling or disabling a sky cannot renumber PXBSP material assets
    // or perturb the resident world bytes.
    let sky_texture_asset_index = cook_scene_sky_texture_asset(
        project,
        project_root,
        resolved_sky,
        &mut sky_texture_assets,
        &mut assets,
        &mut report,
    );
    rooms
        .last_mut()
        .expect("PXBSP metadata room was just appended")
        .sky
        .texture_asset_index = sky_texture_asset_index;
    rooms
        .last_mut()
        .expect("PXBSP metadata room was just appended")
        .sky
        .cloud_layer
        .texture_asset_index = sky_texture_asset_index;
    let world_geometry = PlaytestWorldGeometry::Pxbsp(PlaytestPxbspWorld {
        bytes: compiled.pxbsp.bytes,
        max_visible_faces: compiled.pxbsp.max_visible_faces,
        body_hulls: compiled.body_hulls,
        texture_asset_indices: compiled
            .textures
            .iter()
            .map(|texture| usize::from(texture.asset_id))
            .collect(),
        movers: compiled
            .movers
            .iter()
            .map(|mover| PlaytestPxbspMover {
                node: mover.node.raw() as u32,
                model_index: mover.model_index,
            })
            .collect(),
        leak_path: authored_leak_path,
    });

    if rooms.is_empty() {
        report.error("BSP cook did not produce its world metadata record");
        return (None, report);
    }
    let chunks = Vec::new();

    // Pass 3: spawn + entities + model instances + lights.
    let mut player_spawns: Vec<PlayerSpawnCandidate<'_>> = Vec::new();
    let mut entities: Vec<PlaytestEntity> = Vec::new();
    let mut models: Vec<PlaytestModel> = Vec::new();
    let mut model_clips: Vec<PlaytestModelClip> = Vec::new();
    let mut model_clip_bounds: Vec<PlaytestModelClipBounds> = Vec::new();
    let mut model_frame_bounds: Vec<PlaytestModelFrameBounds> = Vec::new();
    let mut model_sockets: Vec<PlaytestModelSocket> = Vec::new();
    let mut model_instances: Vec<PlaytestModelInstance> = Vec::new();
    let mut image_props: Vec<PlaytestImageProp> = Vec::new();
    let mut box_props: Vec<PlaytestBoxProp> = Vec::new();
    let mut box_prop_surfaces: Vec<PlaytestBoxPropSurface> = Vec::new();
    let mut cylinder_props: Vec<PlaytestCylinderProp> = Vec::new();
    let mut cylinder_prop_surfaces: Vec<PlaytestCylinderPropSurface> = Vec::new();
    let mut arch_props: Vec<PlaytestArchProp> = Vec::new();
    let mut arch_prop_surfaces: Vec<PlaytestArchPropSurface> = Vec::new();
    let mut arch_prop_collisions: Vec<PlaytestArchPropCollision> = Vec::new();
    let water_cells: Vec<PlaytestWaterCell> = Vec::new();
    let mut combat_capsules: Vec<PlaytestCombatCapsule> = Vec::new();
    let mut weapon_hitboxes: Vec<PlaytestWeaponHitbox> = Vec::new();
    let mut weapons: Vec<PlaytestWeapon> = Vec::new();
    let mut equipment: Vec<PlaytestEquipment> = Vec::new();
    let mut lights: Vec<PlaytestLight> = Vec::new();
    let mut particle_emitters: Vec<PlaytestParticleEmitter> = Vec::new();
    let mut interactable_messages: Vec<PlaytestInteractableMessage> = Vec::new();
    let mut interactable_message_pages: Vec<String> = Vec::new();
    let mut interactables: Vec<PlaytestInteractable> = Vec::new();
    let mut destructibles: Vec<PlaytestDestructible> = Vec::new();
    let mut destructible_for_node: HashMap<crate::NodeId, u16> = HashMap::new();
    let mut boost_modules: Vec<PlaytestBoostModule> = Vec::new();
    let mut poi_persistence_ids: HashSet<String> = HashSet::new();
    let mut persistence_slots: HashMap<u16, String> = HashMap::new();
    let persistent_flag_count = psx_level::POI_PERSISTENT_FLAG_CAPACITY as u16;
    // Phase-3 gameplay records: interned names, the logic event graph,
    // and placed souls-like game entities. Door links resolve against
    // the box-prop name table after the walk (a door may name a box
    // that cooks later in scene order).
    let mut names = NameInterner::default();
    let mut logic: Vec<PlaytestLogic> = Vec::new();
    let mut game_entities: Vec<PlaytestGameEntity> = Vec::new();
    let mut box_prop_indices_by_name: HashMap<String, Vec<u16>> = HashMap::new();
    let mut pending_door_links: Vec<PendingDoorLink> = Vec::new();
    // ResourceId → index into `models` for instance dedup.
    let runtime_model_clips = collect_runtime_model_clip_requirements(project, scene);
    let mut model_for_resource: HashMap<ResourceId, u16> = HashMap::new();
    let mut model_clip_remaps: HashMap<ResourceId, Vec<Option<u16>>> = HashMap::new();
    let mut weapon_for_resource: HashMap<ResourceId, u16> = HashMap::new();
    let mut warned_unsupported: HashSet<&'static str> = HashSet::new();

    for node in scene.nodes() {
        let NodeKind::Destructible {
            max_health,
            damage_affinity,
            enabled,
        } = &node.kind
        else {
            continue;
        };
        if *max_health == 0 {
            report.error_at(
                PlaytestValidationTarget::Node(node.id),
                format!(
                    "Destructible '{}' must have at least one health point",
                    node.name
                ),
            );
            continue;
        }
        if destructibles.len() >= psx_level::MAX_DESTRUCTIBLES {
            report.error(format!(
                "Project exceeds the {} shared-destructible runtime limit",
                psx_level::MAX_DESTRUCTIBLES,
            ));
            break;
        }
        let index = destructibles.len() as u16;
        let persistent_flag = stable_destructible_flag(node.id);
        let persistent_name = format!("destructible '{}' (node {})", node.name, node.id.raw());
        if let Some(existing) = persistence_slots.insert(persistent_flag, persistent_name.clone()) {
            report.error_at(
                PlaytestValidationTarget::Node(node.id),
                format!(
                    "{} and {} map to the same persistent save slot; recreate one node to give it a new stable id",
                    existing, persistent_name
                ),
            );
            continue;
        }
        destructible_for_node.insert(node.id, index);
        destructibles.push(PlaytestDestructible {
            max_health: *max_health,
            persistent_flag,
            damage_affinity: match damage_affinity {
                crate::DestructibleDamageAffinity::Horizon => {
                    psx_level::destructible_affinity::HORIZON
                }
                crate::DestructibleDamageAffinity::Zenith => {
                    psx_level::destructible_affinity::ZENITH
                }
                crate::DestructibleDamageAffinity::Both => psx_level::destructible_affinity::BOTH,
            },
            flags: if *enabled {
                psx_level::destructible_flags::ENABLED
            } else {
                0
            },
        });
    }

    let world_message = match scene.node(scene.root).map(|node| &node.kind) {
        Some(NodeKind::World {
            world_message: Some(message),
            ..
        }) => {
            if !validate_message_pages(
                "World message",
                &message.pages,
                3,
                runtime_message_font(project),
                &mut report,
            ) {
                return (None, report);
            }
            if message.pages.len() > u16::MAX as usize {
                report.error("World message page table exceeds 65535 entries");
                return (None, report);
            }
            let page_first = interactable_message_pages.len() as u16;
            interactable_message_pages.extend(message.pages.iter().cloned());
            Some(PlaytestInteractableMessage {
                title: String::new(),
                body: message.pages[0].clone(),
                page_first,
                page_count: message.pages.len() as u16,
            })
        }
        _ => None,
    };

    // Water is authored as BSP liquid brushes and is compiled with the world geometry.

    for node in scene.nodes() {
        if node.id == scene.root || matches!(node.kind, NodeKind::Section { .. }) {
            continue;
        }
        if node.kind.is_component() {
            continue;
        }
        // BSP point entities author transforms directly in world units.
        let pos = node.transform.translation.map(|value| value.round() as i32);
        let (room_index, raw_pos, floor_pos, placement_sector_size) = (
            0,
            pos,
            pos,
            rooms.first().map(|room| room.sector_size).unwrap_or(1024),
        );
        let pitch = angle_from_degrees(node.transform.rotation_degrees[0]);
        let yaw = yaw_from_degrees(node.transform.rotation_degrees[1]);
        let roll = angle_from_degrees(node.transform.rotation_degrees[2]);

        match &node.kind {
            NodeKind::Entity => {
                let pos = floor_pos;
                let character_controller = component_character_controller(scene, node);
                let weight_q8 = physics_body_weight_q8(scene, node);
                let is_player_controlled =
                    character_controller.is_some_and(|controller| controller.player);
                // Any model instance pushed while cooking THIS node (a
                // ModelRenderer component or a non-player controller's
                // idle instance) is the node's visual; a game entity
                // record links it by index.
                let model_instances_before = model_instances.len();
                if !is_player_controlled {
                    if let Some((model_resource_id, renderer)) =
                        component_model_renderer(scene, node).and_then(|renderer| {
                            renderer
                                .model
                                .and_then(|id| {
                                    project
                                        .resource(id)
                                        .filter(|r| matches!(r.data, ResourceData::Model(_)))
                                        .map(|_| id)
                                })
                                .map(|id| (id, renderer))
                        })
                    {
                        let animator = component_animator(scene, node);
                        let clip = animator.as_ref().and_then(|anim| anim.clip);
                        let pose_frame = match &animator {
                            Some(anim) if !anim.autoplay => anim.pose_frame,
                            _ => MODEL_INSTANCE_POSE_ANIMATE,
                        };
                        let material_override = renderer.material.and_then(|material_id| {
                            resolve_model_material_override(
                                project,
                                project_root,
                                &format!("Model Renderer '{}'", node.name),
                                material_id,
                                &mut texture_asset_for_path,
                                &mut assets,
                                &mut report,
                            )
                        });
                        let ok =
                            report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                                push_model_instance_for_resource(
                                    project,
                                    project_root,
                                    node.name.as_str(),
                                    model_resource_id,
                                    ModelInstancePlacement {
                                        clip_override: clip,
                                        pose_frame,
                                        room_index,
                                        pos,
                                        yaw,
                                        visual_yaw: renderer.visual_yaw,
                                        pitch,
                                        roll,
                                        visual_offset: renderer.visual_offset,
                                        visual_scale_q8: renderer.visual_scale_q8,
                                        material_override,
                                    },
                                    ModelCookTables {
                                        assets: &mut assets,
                                        models: &mut models,
                                        model_clips: &mut model_clips,
                                        model_clip_bounds: &mut model_clip_bounds,
                                        model_frame_bounds: &mut model_frame_bounds,
                                        model_sockets: &mut model_sockets,
                                        model_instances: &mut model_instances,
                                        model_for_resource: &mut model_for_resource,
                                        runtime_model_clips: &runtime_model_clips,
                                        model_clip_remaps: &mut model_clip_remaps,
                                        report,
                                    },
                                )
                            });
                        if !ok {
                            return (None, report);
                        }
                    }
                }

                if let Some(controller) = character_controller {
                    if controller.player {
                        player_spawns.push(PlayerSpawnCandidate {
                            node,
                            room_index,
                            position: pos,
                            character: controller.character,
                            controller_settings: controller.settings,
                            camera: component_camera(scene, node).map(|camera| camera.settings),
                            weight_q8,
                            renderer: component_model_renderer(scene, node),
                            animator: component_animator(scene, node),
                        });
                    } else if component_model_renderer(scene, node).is_none() {
                        let Some(character_id) = controller.character else {
                            if controller_enemy_behavior(project, &controller).is_some() {
                                report.error(format!(
                                    "Enemy on '{}' has no Character - the archetype \
                                     tag is the Character resource name",
                                    node.name
                                ));
                                return (None, report);
                            }
                            report.warn(format!(
                                "Non-player Character Controller on '{}' has no Character - skipped",
                                node.name
                            ));
                            continue;
                        };
                        let ok =
                            report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                                push_character_controller_idle_instance(
                                    project,
                                    project_root,
                                    node.name.as_str(),
                                    character_id,
                                    room_index,
                                    pos,
                                    yaw,
                                    &mut texture_asset_for_path,
                                    &mut assets,
                                    &mut models,
                                    &mut model_clips,
                                    &mut model_clip_bounds,
                                    &mut model_frame_bounds,
                                    &mut model_sockets,
                                    &mut model_instances,
                                    &mut model_for_resource,
                                    &runtime_model_clips,
                                    &mut model_clip_remaps,
                                    report,
                                )
                            });
                        if !ok {
                            return (None, report);
                        }
                    }
                }

                if let Some(controller) = character_controller {
                    if !controller.player {
                        if let Some(enemy) = controller_enemy_behavior(project, &controller) {
                            let Some(character_id) = controller.character else {
                                report.error(format!(
                                    "Enemy on '{}' has no Character - the archetype \
                                     tag is the Character resource name",
                                    node.name
                                ));
                                return (None, report);
                            };
                            let (archetype, enemy_character) = match project.resource(character_id)
                            {
                                Some(resource) => match &resource.data {
                                    ResourceData::Character(character) => {
                                        (resource.name.clone(), character)
                                    }
                                    _ => {
                                        report.error(format!(
                                            "Enemy on '{}' references resource '{}' which is \
                                             not a Character",
                                            node.name, resource.name
                                        ));
                                        return (None, report);
                                    }
                                },
                                None => {
                                    report.error(format!(
                                        "Enemy on '{}' references Character #{} which \
                                         doesn't exist",
                                        node.name,
                                        character_id.raw()
                                    ));
                                    return (None, report);
                                }
                            };
                            let node_model_instance =
                                (model_instances.len() > model_instances_before).then(|| {
                                    u16::try_from(model_instances.len() - 1).unwrap_or(u16::MAX)
                                });
                            let Some(state_clips) = game_entity_state_clips(
                                project,
                                archetype.as_str(),
                                enemy_character,
                                node_model_instance,
                                &model_instances,
                                &models,
                                &model_for_resource,
                                &model_clip_remaps,
                                &mut report,
                            ) else {
                                return (None, report);
                            };
                            let model_joint_count = node_model_instance
                                .and_then(|instance| model_instances.get(instance as usize))
                                .and_then(|instance| models.get(instance.model as usize))
                                .and_then(|model| assets.get(model.mesh_asset_index))
                                .and_then(|asset| psx_asset::Model::from_bytes(&asset.bytes).ok())
                                .map(|model| model.joint_count())
                                .unwrap_or(0);
                            let Some((combat_capsule_first, combat_capsule_count)) =
                                cook_character_combat_capsules(
                                    project,
                                    archetype.as_str(),
                                    enemy_character,
                                    model_joint_count,
                                    &mut combat_capsules,
                                    &mut report,
                                )
                            else {
                                return (None, report);
                            };
                            let projectile_attack_range = enemy_character
                                .combat_capsules
                                .iter()
                                .find_map(|volume| match volume.role {
                                    crate::CombatCapsuleRole::ProjectileEmitter {
                                        action: CharacterAnimationAction::LightAttack,
                                        min_range,
                                        max_range,
                                        ..
                                    } => Some((min_range, max_range)),
                                    _ => None,
                                });
                            let attack_event_end = combat_capsules
                                .get(combat_capsule_first as usize..combat_capsule_first as usize
                                    + usize::from(combat_capsule_count))
                                .unwrap_or(&[])
                                .iter()
                                .filter(|capsule| {
                                    capsule.action
                                        == CharacterAnimationAction::LightAttack.to_index() as u8
                                        && capsule.flags
                                            & (psx_level::combat_capsule_flags::HITBOX
                                                | psx_level::combat_capsule_flags::PROJECTILE_EMITTER)
                                            != 0
                                })
                                .map(|capsule| capsule.active_end_frame)
                                .max();
                            let attack_sample_rate_hz = node_model_instance
                                .and_then(|instance| model_instances.get(instance as usize))
                                .and_then(|instance| models.get(instance.model as usize))
                                .and_then(|model| {
                                    model_clips.get(
                                        usize::from(model.clip_first)
                                            .saturating_add(usize::from(state_clips.attack)),
                                    )
                                })
                                .and_then(|clip| assets.get(clip.animation_asset_index))
                                .and_then(|asset| {
                                    psx_asset::Animation::from_bytes(&asset.bytes).ok()
                                })
                                .map(|animation| animation.sample_rate_hz());
                            let attack_active_ticks = cooked_game_entity_attack_active_ticks(
                                enemy.windup_ticks,
                                attack_event_end,
                                attack_sample_rate_hz,
                            );
                            let ok =
                                report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                                    push_game_entity(
                                        node.name.as_str(),
                                        archetype.as_str(),
                                        room_index,
                                        pos,
                                        yaw,
                                        // Override if the placement has one,
                                        // otherwise the Character's own tuning.
                                        &controller.settings.unwrap_or_else(|| {
                                            crate::CharacterControllerSettings::from_character(
                                                enemy_character,
                                            )
                                        }),
                                        enemy,
                                        node_model_instance,
                                        state_clips,
                                        combat_capsule_first,
                                        combat_capsule_count,
                                        attack_active_ticks,
                                        projectile_attack_range,
                                        &mut names,
                                        &mut game_entities,
                                        report,
                                    )
                                });
                            if !ok {
                                return (None, report);
                            }
                        }
                    }
                }

                // Start from the reusable Character profile, then let an
                // Equipment component on this placement replace the default
                // occupying the same socket. This preserves scene-specific
                // overrides while making equipped enemy variants portable.
                let mut equipped_bindings = character_controller
                    .and_then(|controller| controller.character)
                    .and_then(|character_id| project.resource(character_id))
                    .and_then(|resource| match &resource.data {
                        ResourceData::Character(character) => Some(
                            character
                                .default_equipment
                                .iter()
                                .map(|binding| {
                                    (
                                        binding.weapon,
                                        binding.character_socket.clone(),
                                        binding.weapon_grip.clone(),
                                    )
                                })
                                .collect::<Vec<_>>(),
                        ),
                        _ => None,
                    })
                    .unwrap_or_default();
                for explicit in component_equipment(scene, node) {
                    equipped_bindings.retain(|(_, socket, _)| {
                        socket.as_str() != explicit.character_socket
                    });
                    equipped_bindings.push((
                        explicit.weapon,
                        explicit.character_socket.to_string(),
                        explicit.weapon_grip.to_string(),
                    ));
                }

                for (weapon, character_socket, weapon_grip) in equipped_bindings {
                    if let Some(weapon_id) = weapon {
                        let Some(weapon_index) = register_weapon_for_equipment(
                            project,
                            project_root,
                            weapon_id,
                            &mut assets,
                            &mut models,
                            &mut model_clips,
                            &mut model_clip_bounds,
                            &mut model_frame_bounds,
                            &mut model_sockets,
                            &mut model_for_resource,
                            &runtime_model_clips,
                            &mut model_clip_remaps,
                            &mut weapon_hitboxes,
                            &mut weapons,
                            &mut weapon_for_resource,
                            &mut report,
                        ) else {
                            return (None, report);
                        };
                        // The host's visual instance (pushed earlier in
                        // this node's arm, same last-pushed convention
                        // as game entities), so the runtime can ride
                        // the weapon on the LIVE entity pose. The
                        // player carries no instance; its live pose
                        // comes from the player context.
                        let host_instance = if !is_player_controlled
                            && model_instances.len() > model_instances_before
                        {
                            u16::try_from(model_instances.len() - 1).unwrap_or(u16::MAX)
                        } else {
                            u16::MAX
                        };
                        equipment.push(PlaytestEquipment {
                            room: room_index,
                            weapon: weapon_index,
                            x: pos[0],
                            y: pos[1],
                            z: pos[2],
                            yaw,
                            character_socket,
                            weapon_grip,
                            model_instance: host_instance,
                            flags: if is_player_controlled {
                                psx_level::equipment_flags::PLAYER
                            } else {
                                0
                            },
                        });
                    } else if warned_unsupported.insert("UnboundEquipment") {
                        report.warn("Equipment components with no Weapon are skipped");
                    }
                }

                let has_legacy_interaction = component_interactable(scene, node).is_some();
                let point_of_interest_node = component_children(scene, node)
                    .find(|child| matches!(child.kind, NodeKind::PointOfInterest { .. }))
                    .map(|child| child.id);
                let has_point_of_interest = point_of_interest_node.is_some();
                if has_legacy_interaction && has_point_of_interest {
                    report.error_at(
                        PlaytestValidationTarget::Node(node.id),
                        format!(
                            "Entity '{}' cannot have both Interactable and Point of Interest components",
                            node.name
                        ),
                    );
                    return (None, report);
                }

                if let Some(interactable) = component_interactable(scene, node) {
                    let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                        push_interactable(
                            node.name.as_str(),
                            room_index,
                            pos,
                            yaw,
                            interactable,
                            &mut names,
                            &mut interactable_messages,
                            &mut interactable_message_pages,
                            &mut interactables,
                            &mut logic,
                            report,
                        )
                    });
                    if !ok {
                        return (None, report);
                    }
                }

                if let Some(point) = component_point_of_interest(scene, node) {
                    let validation_target =
                        PlaytestValidationTarget::Node(point_of_interest_node.unwrap_or(node.id));
                    let persistence_id = if point.persistence_id.trim().is_empty() {
                        format!("poi_{}", node.id.raw())
                    } else {
                        point.persistence_id.trim().to_string()
                    };
                    if !poi_persistence_ids.insert(persistence_id.clone()) {
                        report.error_at(
                            validation_target,
                            format!(
                                "Point of Interest on '{}' reuses persistence id '{}'",
                                node.name, persistence_id
                            ),
                        );
                        return (None, report);
                    }
                    let (read_flag, reward_flag) = stable_poi_flag_pair(&persistence_id);
                    if let Some(existing) = persistence_slots.get(&read_flag) {
                        report.error_at(
                            validation_target,
                            format!(
                                "Persistent object '{}' and point-of-interest '{}' map to the same save slot; rename the POI persistence id",
                                existing, persistence_id
                            ),
                        );
                        return (None, report);
                    }
                    persistence_slots
                        .insert(read_flag, format!("point of interest '{persistence_id}'"));
                    if let Some(existing) = persistence_slots.insert(
                        reward_flag,
                        format!("point of interest reward '{persistence_id}'"),
                    ) {
                        report.error_at(
                            validation_target,
                            format!(
                                "Persistent object '{}' and point-of-interest reward '{}' map to the same save slot; rename the POI persistence id",
                                existing, persistence_id
                            ),
                        );
                        return (None, report);
                    }
                    let point = PointOfInterestComponent {
                        persistence_id: &persistence_id,
                        ..point
                    };
                    let ok = report.blaming(validation_target, |report| {
                        push_point_of_interest(
                            project,
                            node.name.as_str(),
                            room_index,
                            pos,
                            yaw,
                            point,
                            read_flag,
                            reward_flag,
                            &mut interactable_messages,
                            &mut interactable_message_pages,
                            &mut interactables,
                            &mut boost_modules,
                            report,
                        )
                    });
                    if !ok {
                        return (None, report);
                    }
                }
            }
            NodeKind::SpawnPoint { player: true, .. } => {
                let pos = floor_pos;
                let NodeKind::SpawnPoint { character, .. } = &node.kind else {
                    unreachable!();
                };
                player_spawns.push(PlayerSpawnCandidate {
                    node,
                    room_index,
                    position: pos,
                    character: *character,
                    controller_settings: None,
                    camera: None,
                    weight_q8: PHYSICS_WEIGHT_ONE_Q8,
                    renderer: None,
                    animator: None,
                });
            }
            NodeKind::SpawnPoint { player: false, .. } => {
                let pos = floor_pos;
                entities.push(PlaytestEntity {
                    room: room_index,
                    kind: PlaytestEntityKind::Marker,
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                    yaw,
                    resource_slot: 0,
                    flags: 0,
                });
            }
            NodeKind::MeshInstance {
                mesh,
                animation_clip,
                ..
            } => {
                // Two cases:
                // (a) `mesh` is `Some(_)` and resolves to a
                //     `ResourceData::Model` → real model
                //     instance, register the model bundle on
                //     first sight and emit a model instance.
                // (b) `mesh` is `None` or points at a non-Model
                //     resource → falls through to a legacy
                //     entity marker so authored placements
                //     don't disappear silently.
                let pos = floor_pos;
                let model_id = mesh.and_then(|id| {
                    project
                        .resource(id)
                        .filter(|r| matches!(r.data, ResourceData::Model(_)))
                        .map(|_| id)
                });
                if let Some(model_resource_id) = model_id {
                    let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                        push_model_instance_for_resource(
                            project,
                            project_root,
                            node.name.as_str(),
                            model_resource_id,
                            ModelInstancePlacement {
                                clip_override: *animation_clip,
                                pose_frame: MODEL_INSTANCE_POSE_ANIMATE,
                                room_index,
                                pos,
                                yaw,
                                visual_yaw: 0,
                                pitch,
                                roll,
                                visual_offset: [0; 3],
                                visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
                                material_override: None,
                            },
                            ModelCookTables {
                                assets: &mut assets,
                                models: &mut models,
                                model_clips: &mut model_clips,
                                model_clip_bounds: &mut model_clip_bounds,
                                model_frame_bounds: &mut model_frame_bounds,
                                model_sockets: &mut model_sockets,
                                model_instances: &mut model_instances,
                                model_for_resource: &mut model_for_resource,
                                runtime_model_clips: &runtime_model_clips,
                                model_clip_remaps: &mut model_clip_remaps,
                                report,
                            },
                        )
                    });
                    if !ok {
                        return (None, report);
                    }
                } else {
                    // Legacy / unbound MeshInstance → marker
                    // (matches the pre-Model-resource behaviour).
                    entities.push(PlaytestEntity {
                        room: room_index,
                        kind: PlaytestEntityKind::Marker,
                        x: pos[0],
                        y: pos[1],
                        z: pos[2],
                        yaw,
                        resource_slot: 0,
                        flags: 0,
                    });
                }
            }
            NodeKind::PointLight {
                color,
                intensity,
                radius,
            } => {
                let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                    push_point_light(
                        node.name.as_str(),
                        placement_sector_size,
                        room_index,
                        raw_pos,
                        *color,
                        *intensity,
                        *radius,
                        &mut lights,
                        report,
                    )
                });
                if !ok {
                    return (None, report);
                }
            }
            NodeKind::ImageProp {
                material,
                width,
                height,
                cylindrical_billboard,
                collision_enabled,
                collision_size,
                destructible,
            } => {
                let destructible = match destructible {
                    Some(owner_id) => match destructible_for_node.get(owner_id).copied() {
                        Some(index) => index,
                        None => {
                            report.error_at(
                                PlaytestValidationTarget::Node(node.id),
                                format!(
                                    "Image Prop '{}' references missing/non-Destructible node {:?}",
                                    node.name, owner_id
                                ),
                            );
                            return (None, report);
                        }
                    },
                    None => psx_level::WORLD_OBJECT_DESTRUCTIBLE_NONE,
                };
                if !push_image_prop(
                    project,
                    project_root,
                    node.name.as_str(),
                    room_index,
                    raw_pos,
                    pitch,
                    yaw,
                    roll,
                    *material,
                    *width,
                    *height,
                    *cylindrical_billboard,
                    *collision_enabled,
                    *collision_size,
                    destructible,
                    &mut texture_asset_for_path,
                    &mut assets,
                    &mut image_props,
                    &mut report,
                ) {
                    return (None, report);
                }
            }
            NodeKind::BoxProp {
                materials,
                uvs,
                vertices,
                collision_enabled,
                break_flags,
                erosion,
            } => {
                // Box props are scenery that can sit at any height (e.g.
                // stacked on another box), so they honor the authored Y
                // like the editor preview's raw origin. They are NOT
                // floor-anchored: snapping to the floor grid ignores a box
                // stacked beneath and would collapse the stack to the floor.
                let box_props_before = box_props.len();
                let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                    push_box_prop(
                        project,
                        project_root,
                        node.name.as_str(),
                        room_index,
                        raw_pos,
                        // Ground beneath the box (floor-anchored Y): where its
                        // fragments settle and where it falls to if unsupported.
                        floor_pos[1],
                        pitch,
                        yaw,
                        roll,
                        materials,
                        uvs,
                        *vertices,
                        *collision_enabled,
                        *break_flags,
                        *erosion,
                        &mut texture_asset_for_path,
                        &mut assets,
                        &mut box_props,
                        &mut box_prop_surfaces,
                        report,
                    )
                });
                if !ok {
                    return (None, report);
                }
                // Record the cooked index under the node name so Door
                // logic nodes can link this box. A material-less box
                // is skipped by the push (warned), so only a box that
                // actually cooked registers -- a door naming a skipped
                // box fails the link resolution loudly.
                if box_props.len() > box_props_before {
                    let cooked_index = box_props_before.min(u16::MAX as usize) as u16;
                    box_prop_indices_by_name
                        .entry(node.name.clone())
                        .or_default()
                        .push(cooked_index);
                }
            }
            NodeKind::CylinderProp {
                materials,
                uvs,
                geometry,
                collision_enabled,
            } => {
                let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                    push_cylinder_prop(
                        project,
                        project_root,
                        node.name.as_str(),
                        room_index,
                        raw_pos,
                        pitch,
                        yaw,
                        roll,
                        materials,
                        uvs,
                        *geometry,
                        *collision_enabled,
                        &mut texture_asset_for_path,
                        &mut assets,
                        &mut cylinder_props,
                        &mut cylinder_prop_surfaces,
                        report,
                    )
                });
                if !ok {
                    return (None, report);
                }
            }
            NodeKind::ArchProp {
                materials,
                uvs,
                geometry,
                collision_enabled,
            } => {
                let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                    push_arch_prop(
                        project,
                        project_root,
                        node.name.as_str(),
                        room_index,
                        raw_pos,
                        pitch,
                        yaw,
                        roll,
                        placement_sector_size,
                        materials,
                        uvs,
                        *geometry,
                        *collision_enabled,
                        &mut texture_asset_for_path,
                        &mut assets,
                        &mut arch_props,
                        &mut arch_prop_surfaces,
                        &mut arch_prop_collisions,
                        report,
                    )
                });
                if !ok {
                    return (None, report);
                }
            }
            NodeKind::Logic {
                kind,
                target,
                killtarget,
                master,
                delay_ticks,
                wait_ticks,
                enabled,
            } => {
                let bsp_door_link = match &world_geometry {
                    PlaytestWorldGeometry::Pxbsp(world) => world
                        .movers
                        .iter()
                        .position(|mover| mover.node == node.id.raw() as u32)
                        .and_then(|index| u16::try_from(index).ok()),
                    PlaytestWorldGeometry::Grid => None,
                };
                let ok = report.blaming(PlaytestValidationTarget::Node(node.id), |report| {
                    push_logic_node(
                        node.name.as_str(),
                        room_index,
                        floor_pos,
                        kind,
                        target,
                        killtarget,
                        master,
                        *delay_ticks,
                        *wait_ticks,
                        *enabled,
                        bsp_door_link,
                        &mut names,
                        &mut logic,
                        &mut pending_door_links,
                        report,
                    )
                });
                if !ok {
                    return (None, report);
                }
            }
            NodeKind::Portal { .. } => {
                if warned_unsupported.insert("Portal") {
                    report
                        .warn("Portal markers define runtime-room seams; not emitted as entities");
                }
            }
            NodeKind::ParticleEmitter { settings } => {
                if !push_particle_emitter(
                    node.name.as_str(),
                    room_index,
                    raw_pos,
                    settings,
                    &mut particle_emitters,
                    &mut report,
                ) {
                    return (None, report);
                }
            }
            NodeKind::Node
            | NodeKind::Group
            | NodeKind::Node3D
            | NodeKind::World { .. }
            | NodeKind::Section { .. }
            | NodeKind::WaterVolume { .. }
            | NodeKind::ModelRenderer { .. }
            | NodeKind::Animator { .. }
            | NodeKind::Collider { .. }
            | NodeKind::CharacterController { .. }
            | NodeKind::Camera { .. }
            | NodeKind::Equipment { .. }
            | NodeKind::PhysicsBody { .. }
            | NodeKind::Interactable { .. }
            | NodeKind::PointOfInterest { .. }
            | NodeKind::Destructible { .. } => {}
        }
    }

    for (room_index, room) in rooms.iter().enumerate() {
        let image_count = image_props
            .iter()
            .filter(|prop| {
                usize::from(prop.room) == room_index
                    && prop.flags & psx_level::image_prop_flags::COLLISION_ENABLED != 0
            })
            .count();
        let box_count = box_props
            .iter()
            .filter(|prop| {
                usize::from(prop.room) == room_index
                    && prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED != 0
            })
            .count();
        let arch_count: usize = arch_props
            .iter()
            .filter(|prop| usize::from(prop.room) == room_index)
            .map(|prop| usize::from(prop.collision_count))
            .sum();
        let total = image_count
            .saturating_add(box_count)
            .saturating_add(arch_count);
        if total > psx_level::MAX_STATIC_PROP_AABB_BLOCKERS {
            report.error(format!(
                "Room '{}' needs {total} static prop collision AABBs ({image_count} image + {box_count} box + {arch_count} arch), exceeding the PS1 runtime budget of {}",
                room.name,
                psx_level::MAX_STATIC_PROP_AABB_BLOCKERS,
            ));
        }
    }

    let world_objects = cook_world_objects(
        &image_props,
        &box_props,
        &cylinder_props,
        &arch_props,
        &arch_prop_surfaces,
        &interactables,
        &mut report,
    );

    // Door links resolve now that every Box Prop has cooked: an
    // unresolved or ambiguous box name is a hard cook error.
    if !resolve_door_links(
        &pending_door_links,
        &box_prop_indices_by_name,
        &mut logic,
        &mut report,
    ) {
        return (None, report);
    }

    // Contract caps (psx-level is the single source, like
    // MAX_ROOM_MATERIALS): the runtime sizes its SoA state from these,
    // so over-cap content must fail loudly here instead of silently
    // never spawning.
    if game_entities.len() > psx_level::MAX_GAME_ENTITY_RECORDS {
        report.error(format!(
            "Project places {} game entities but the runtime contract cap \
             is {} (psx_level::MAX_GAME_ENTITY_RECORDS)",
            game_entities.len(),
            psx_level::MAX_GAME_ENTITY_RECORDS,
        ));
        return (None, report);
    }
    if logic.len() > psx_level::MAX_LOGIC_RECORDS {
        report.error(format!(
            "Project cooks {} logic records but the runtime contract cap \
             is {} (psx_level::MAX_LOGIC_RECORDS)",
            logic.len(),
            psx_level::MAX_LOGIC_RECORDS,
        ));
        return (None, report);
    }
    if names.len() >= usize::from(u16::MAX) {
        report.error("Interned gameplay name table overflowed u16 ids");
        return (None, report);
    }

    let spawn = match player_spawns.len() {
        0 => {
            report.error(
                "No player source. Place one Player Spawn, or select a Character Controller and enable Player controlled.",
            );
            None
        }
        1 => {
            let candidate = &player_spawns[0];
            let node = candidate.node;
            let room_index = candidate.room_index;
            let pos = candidate.position;
            Some(PlaytestSpawn {
                room: room_index,
                x: pos[0],
                y: pos[1],
                z: pos[2],
                yaw: yaw_from_degrees(node.transform.rotation_degrees[1]),
                flags: 1,
            })
        }
        n => {
            report.error(format!(
                "Expected exactly one player source, found {n}. Keep one Player Spawn or one Player controlled Character Controller enabled."
            ));
            None
        }
    };
    if let [candidate] = &player_spawns[..] {
        if let Some(camera) = candidate.camera {
            let camera = camera.normalized();
            for room in &mut rooms {
                room.camera = PlaytestCamera {
                    distance: camera.distance,
                    height: camera.height,
                    target_height: camera.target_height,
                    lock_rise_percent: camera.lock_rise_percent,
                    min_floor_clearance: camera.min_floor_clearance,
                    orbit_speed_level: camera.orbit_speed_level,
                    position_lag_shift: camera.position_lag_shift,
                    focus_lag_shift: camera.focus_lag_shift,
                    distance_lag_shift: camera.distance_lag_shift,
                };
            }
        }
    }

    // Pass 4: resolve the player's Character, register its
    // model (deduped against MeshInstance-bound models above),
    // and emit a PlaytestCharacter + PlaytestPlayerController.
    //
    // Character resources unrelated to the player aren't cooked
    // in this pass -- only the player slot consumes them. Once
    // enemies / NPCs surface, the same `register_model_for_instance`
    // dedupe path handles their backing models too.
    let mut characters: Vec<PlaytestCharacter> = Vec::new();
    let player_controller = match (spawn, &player_spawns[..]) {
        (Some(spawn_record), [candidate]) => {
            let spawn_node = candidate.node;
            let renderer_model = candidate.renderer.and_then(|renderer| renderer.model);
            let resolved = if renderer_model.is_some() {
                candidate.character
            } else {
                match crate::resolve::resolve_spawn_character(project, candidate.character) {
                    Ok(resolved) => {
                        if resolved.auto_picked {
                            report.warn(format!(
                                "Player source '{}' had no Character -- auto-picked the only one defined",
                                spawn_node.name,
                            ));
                        }
                        Some(resolved.id)
                    }
                    Err(crate::resolve::SpawnCharacterResolutionError::MissingExplicit(id)) => {
                        report.error(format!(
                            "Player source '{}' references Character #{} which doesn't exist",
                            spawn_node.name,
                            id.raw()
                        ));
                        None
                    }
                    Err(crate::resolve::SpawnCharacterResolutionError::ExplicitNotCharacter(
                        id,
                    )) => {
                        let name = project
                            .resource(id)
                            .map(|r| r.name.as_str())
                            .unwrap_or("<missing>");
                        report.error(format!(
                            "Player source '{}' references resource '{}' which is not a Character",
                            spawn_node.name, name
                        ));
                        None
                    }
                    Err(crate::resolve::SpawnCharacterResolutionError::NoCharacters) => {
                        if uses_pxbsp {
                            report.warn(format!(
                                "Player source '{}' has no Character; using the BSP debug controller",
                                spawn_node.name
                            ));
                        } else {
                            report.error(format!(
                                "Player source '{}' has no Character assigned and no Character resources exist",
                                spawn_node.name
                            ));
                        }
                        None
                    }
                    Err(crate::resolve::SpawnCharacterResolutionError::AmbiguousCharacters {
                        count,
                    }) => {
                        report.error(format!(
                            "Player source '{}' has no Character assigned and {count} Characters are defined -- pick one explicitly",
                            spawn_node.name
                        ));
                        None
                    }
                }
            };
            if uses_pxbsp && resolved.is_none() && renderer_model.is_none() {
                None
            } else {
                let profile_material = resolved.and_then(|character_id| {
                    project.resource(character_id).and_then(|resource| {
                        if let ResourceData::Character(character) = &resource.data {
                            character.material
                        } else {
                            None
                        }
                    })
                });
                let renderer_material_override = candidate
                    .renderer
                    .and_then(|renderer| renderer.material)
                    .or(profile_material)
                    .and_then(|material_id| {
                        resolve_model_material_override(
                            project,
                            project_root,
                            &format!("Model Renderer '{}'", spawn_node.name),
                            material_id,
                            &mut texture_asset_for_path,
                            &mut assets,
                            &mut report,
                        )
                    });
                cook_player_character(
                    project,
                    project_root,
                    spawn_node,
                    resolved,
                    renderer_model,
                    renderer_material_override,
                    candidate
                        .renderer
                        .map(|renderer| renderer.visual_offset)
                        .unwrap_or([0; 3]),
                    candidate
                        .renderer
                        .map(|renderer| renderer.visual_yaw)
                        .unwrap_or(0),
                    candidate
                        .renderer
                        .map(|renderer| renderer.visual_scale_q8)
                        .unwrap_or(crate::MODEL_SCALE_ONE_Q8),
                    candidate
                        .animator
                        .map(|animator| animator.action_clips)
                        .unwrap_or(&[]),
                    candidate.controller_settings,
                    candidate.camera,
                    candidate.weight_q8,
                    &mut assets,
                    &mut models,
                    &mut model_clips,
                    &mut model_clip_bounds,
                    &mut model_frame_bounds,
                    &mut model_sockets,
                    &mut model_for_resource,
                    &runtime_model_clips,
                    &mut model_clip_remaps,
                    &mut combat_capsules,
                    &mut characters,
                    &mut report,
                )
                .map(|character_index| PlaytestPlayerController {
                    spawn: spawn_record,
                    character: character_index,
                })
            }
        }
        _ => None,
    };

    // Animation Studio weapon tracks are part of the player's authored action
    // presentation. Provision every distinct weapon/socket pair they use so
    // the runtime draws the same loadout the Studio previews. Explicit scene
    // Equipment remains authoritative for an already-present pair; this only
    // fills the pairs that would otherwise be silently absent at runtime.
    if let Some(controller) = player_controller {
        let appearance_equipment = characters
            .get(controller.character as usize)
            .and_then(|character| project.resource(character.source_resource))
            .and_then(|resource| match &resource.data {
                ResourceData::Character(character) => character.animation_set,
                _ => None,
            })
            .and_then(|set_id| project.resource(set_id))
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationSet(set) => Some(
                    set.weapon_appearance_tracks
                        .iter()
                        .filter(|track| {
                            project.resource(track.weapon).is_some_and(|resource| {
                                matches!(resource.data, ResourceData::Weapon(_))
                            })
                        })
                        .map(|track| (track.weapon, track.character_socket.clone()))
                        .fold(Vec::new(), |mut pairs, pair| {
                            if !pairs.contains(&pair) {
                                pairs.push(pair);
                            }
                            pairs
                        }),
                ),
                _ => None,
            })
            .unwrap_or_default();

        for (weapon_id, character_socket) in appearance_equipment {
            let weapon_grip = project
                .resource(weapon_id)
                .and_then(|resource| match &resource.data {
                    ResourceData::Weapon(weapon) => Some(weapon.grip.name.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "grip".to_string());
            let Some(weapon_index) = register_weapon_for_equipment(
                project,
                project_root,
                weapon_id,
                &mut assets,
                &mut models,
                &mut model_clips,
                &mut model_clip_bounds,
                &mut model_frame_bounds,
                &mut model_sockets,
                &mut model_for_resource,
                &runtime_model_clips,
                &mut model_clip_remaps,
                &mut weapon_hitboxes,
                &mut weapons,
                &mut weapon_for_resource,
                &mut report,
            ) else {
                return (None, report);
            };
            if equipment.iter().any(|record| {
                record.flags & psx_level::equipment_flags::PLAYER != 0
                    && record.weapon == weapon_index
                    && record.character_socket == character_socket
            }) {
                continue;
            }
            equipment.push(PlaytestEquipment {
                room: controller.spawn.room,
                weapon: weapon_index,
                x: controller.spawn.x,
                y: controller.spawn.y,
                z: controller.spawn.z,
                yaw: controller.spawn.yaw,
                character_socket,
                weapon_grip,
                model_instance: u16::MAX,
                flags: psx_level::equipment_flags::PLAYER,
            });
        }
    }

    let weapon_appearances = cook_weapon_appearances(
        project,
        &characters,
        &models,
        &model_clips,
        &weapon_for_resource,
        &equipment,
        &mut report,
    );

    collapse_exclusive_player_material(
        player_controller,
        &mut characters,
        &models,
        &model_instances,
        &weapons,
        &mut assets,
        &mut report,
    );

    if let PlaytestWorldGeometry::Pxbsp(world) = &world_geometry {
        validate_pxbsp_body_hulls(
            project,
            &world.body_hulls,
            &characters,
            &game_entities,
            &mut report,
        );
    }

    if !report.is_ok() {
        return (None, report);
    }

    let mut lights = lights;
    let room_floor_links = Vec::new();
    let room_overlapped_rooms = Vec::new();
    let (ui_nodes, ui_paints, ui_scenes, ui_sfx_samples, ui_sfx_cues, game_flow, cdda_tracks) =
        cook_ui_nodes(
            project,
            project_root,
            &mut texture_asset_for_path,
            &mut assets,
            &mut used_ui_source_paths,
            &mut report,
        );
    if !report.is_ok() {
        return (None, report);
    }
    // Runtime lighting shades in room-local space. Keep each room's lights
    // contiguous so a room view can binary-slice once instead of filtering
    // the complete level table for every material shade.
    lights.sort_by_key(|light| light.room);
    let options = cook_options(project);

    // Fail closed on the boundary itself. Every grid spatial producer above
    // is reachable only through `room_nodes`, which is provably empty for a
    // BSP project - but "provably" decays as the cook grows, and the failure
    // mode it decays into is silent: a BSP level that quietly streams grid
    // rooms nobody authored. Re-check the outputs instead of trusting the
    // guards, so a future leak is a named cook error on the first run.
    //
    // Two deliberate exceptions, per docs/quake-psoxide-convergence-handoff.md
    // section 0.10, are NOT leaks and are excluded by name below:
    //   * the singleton `PlaytestRoom` ("PXBSP World"), which is non-spatial
    //     metadata (gravity, camera, sky, fog) with `world_asset_index: None`;
    //   * the header-only WORLD.PAK the manifest writes from an empty world
    //     pack order, kept so the disc layout and loader contract stay stable.
    let leaks: [(&str, usize); 9] = [
        ("room chunks", chunks.len()),
        ("room visibility rows", room_visibility.len()),
        ("visibility cells", visibility_cells.len()),
        ("visibility PVS rows", visibility_pvs.len()),
        ("room surface caches", room_surface_caches.len()),
        ("room portals", room_portals.len()),
        ("room floor links", room_floor_links.len()),
        ("water cells", water_cells.len()),
        (
            "PSXW world assets",
            assets
                .iter()
                .filter(|asset| asset.kind == PlaytestAssetKind::RoomWorld)
                .count(),
        ),
    ];
    for (what, count) in leaks {
        if count != 0 {
            report.error(format!(
                "BSP project cooked {count} {what}; PXBSP is the only \
                     spatial authority and grid spatial state must never be \
                     produced for a BSP project"
            ));
        }
    }
    if rooms.len() != 1 {
        report.error(format!(
            "BSP project cooked {} room records; exactly one non-spatial \
                 metadata room is expected",
            rooms.len()
        ));
    }
    if rooms.iter().any(|room| room.world_asset_index.is_some()) {
        report.error(
            "BSP project cooked a room referencing a PSXW world asset; the \
                 metadata room must carry no world geometry",
        );
    }
    if !matches!(world_geometry, PlaytestWorldGeometry::Pxbsp(_)) {
        report.error(
            "BSP project produced a grid world geometry payload; the cook \
                 would ship a level with no world",
        );
    }
    if !report.is_ok() {
        return (None, report);
    }

    let (save_name, save_title) = project_save_identity(&project.name);
    (
        Some(PlaytestPackage {
            save_name,
            save_title,
            bsp_cook_mode: project.bsp_cook_mode,
            world_geometry,
            // Same map the cook uses to dedupe texture references, so the
            // reachable set cannot drift from what was actually cooked.
            used_texture_paths: {
                let mut paths: Vec<String> = texture_asset_for_path.keys().cloned().collect();
                paths.sort();
                paths
            },
            // `model_for_resource` holds exactly the model resources that were
            // registered, and `model_clip_remaps` records which of a model's
            // clips survived the runtime-clip filter, so a clip the cook skipped
            // is not shipped.
            used_ui_paths: used_ui_source_paths,
            used_model_paths: {
                let mut paths: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for resource_id in model_for_resource.keys() {
                    let Some(resource) = project.resource(*resource_id) else {
                        continue;
                    };
                    let crate::ResourceData::Model(model) = &resource.data else {
                        continue;
                    };
                    paths.insert(model.model_path.clone());
                    if let Some(atlas) = &model.texture_path {
                        paths.insert(atlas.clone());
                    }
                    // Same accessor the cook uses to enumerate a model's clips,
                    // filtered by the remap it produced, so a clip dropped by the
                    // runtime-clip filter is not shipped either.
                    let remap = model_clip_remaps.get(resource_id);
                    for (index, clip) in project
                        .resolved_model_animation_clips(*resource_id)
                        .iter()
                        .enumerate()
                    {
                        let kept = remap
                            .map(|r| r.get(index).is_some_and(Option::is_some))
                            .unwrap_or(true);
                        if kept {
                            paths.insert(clip.psxanim_path.clone());
                        }
                    }
                }
                paths.into_iter().collect()
            },
            runtime_depth_sort_mode: project.runtime_depth_sort_mode,
            runtime_texture_split_mode: project.runtime_texture_split_mode,
            runtime_room_draw_order_mode: project.runtime_room_draw_order_mode,
            runtime_texture_split_max_edge: project.runtime_texture_split_max_edge,
            assets,
            rooms,
            chunks,
            room_portals,
            room_floor_links,
            water_cells,
            room_near_rooms,
            room_overlapped_rooms,
            materials,
            room_visibility,
            visibility_cells,
            visibility_pvs,
            visibility_pvs_bits,
            room_surface_caches,
            room_cache_cells,
            room_cache_cell_vertices,
            room_cache_vertices,
            room_cache_surfaces,
            models,
            model_clips,
            model_clip_bounds,
            model_frame_bounds,
            model_sockets,
            model_instances,
            destructibles,
            world_objects,
            image_props,
            box_props,
            box_prop_surfaces,
            cylinder_props,
            cylinder_prop_surfaces,
            arch_props,
            arch_prop_surfaces,
            arch_prop_collisions,
            ui_nodes,
            ui_paints,
            ui_scenes,
            ui_sfx_samples,
            ui_sfx_cues,
            game_flow,
            options,
            cdda_tracks,
            combat_capsules,
            weapon_hitboxes,
            weapons,
            equipment,
            weapon_appearances,
            lights,
            particle_emitters,
            interactable_messages,
            interactable_message_pages,
            world_message,
            persistent_flag_count,
            boost_modules,
            interactables,
            logic,
            game_entities,
            spawn,
            characters,
            player_controller,
            entities,
        }),
        report,
    )
}

fn collapse_exclusive_player_material(
    player_controller: Option<PlaytestPlayerController>,
    characters: &mut [PlaytestCharacter],
    models: &[PlaytestModel],
    model_instances: &[PlaytestModelInstance],
    weapons: &[PlaytestWeapon],
    assets: &mut [PlaytestAsset],
    report: &mut PlaytestValidationReport,
) {
    let Some(player_controller) = player_controller else {
        return;
    };
    let character_index = usize::from(player_controller.character);
    let Some(character) = characters.get(character_index) else {
        return;
    };
    let model_index = character.model;
    let Some(mut material) = character.material_override else {
        return;
    };
    let Some(secondary) = material.secondary_layer else {
        return;
    };
    if material.texture_asset_index.is_some()
        || material.blend_mode != PsxBlendMode::Average
        || secondary.blend_mode != PsxBlendMode::AddQuarter
        || secondary.motion.enabled
        || material.reflection_probe.is_some()
    {
        return;
    }
    let shared_by_character = characters
        .iter()
        .enumerate()
        .any(|(index, other)| index != character_index && other.model == model_index);
    let shared_by_instance = model_instances
        .iter()
        .any(|instance| instance.model == model_index);
    let shared_by_weapon = weapons
        .iter()
        .any(|weapon| weapon.model == Some(model_index));
    if shared_by_character || shared_by_instance || shared_by_weapon {
        return;
    }

    let Some(atlas_asset_index) = models
        .get(usize::from(model_index))
        .and_then(|model| model.texture_asset_index)
    else {
        return;
    };
    let Some(primary_bytes) = assets
        .get(atlas_asset_index)
        .map(|asset| asset.bytes.clone())
    else {
        return;
    };
    let Some(secondary_asset_index) = secondary.texture_asset_index else {
        return;
    };
    let Some(secondary_bytes) = assets
        .get(secondary_asset_index)
        .map(|asset| asset.bytes.clone())
    else {
        return;
    };
    let fused = match crate::fuse_average_add_quarter_psxt(
        &primary_bytes,
        material.tint_rgb,
        &secondary_bytes,
        secondary.tint_rgb,
    ) {
        Ok(fused) => fused,
        Err(error) => {
            report.warn(format!(
                "Player material passes could not be collapsed ({error}); keeping the authored two-pass path"
            ));
            return;
        }
    };
    let Some(atlas_asset) = assets.get_mut(atlas_asset_index) else {
        return;
    };
    atlas_asset.bytes = fused;
    atlas_asset.source_label = format!("{} (fused player material)", atlas_asset.source_label);
    material.tint_rgb = crate::fused_material_neutral_tint();
    material.secondary_layer = None;
    if let Some(character) = characters.get_mut(character_index) {
        character.material_override = Some(material);
    }
}

#[cfg(test)]
mod tests;
