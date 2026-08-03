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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use psx_engine::{
    cache_room_vertex_lit_surfaces, CachedRoomCell, CachedRoomSurface, RuntimeRoom,
    WorldRenderMaterial, WorldVertex,
};
use psx_level::{
    box_prop_flags, character_action_flags, cloud_layer_flags, far_vista_flags, image_prop_flags,
    model_clip_flags, particle_emitter_flags, room_flags, sky_flags, visibility_cell_flags,
    visibility_edge_flags,
};
use psxed_format::world as psxw;
use psxed_format::{
    texture::{
        Depth as TextureDepth, TextureHeader, MAGIC as TEXTURE_MAGIC, VERSION as TEXTURE_VERSION,
    },
    AssetHeader,
};

use crate::portal_rooms::{extract_portal_room_grid, plan_portal_rooms, PortalRoomConfig};
use crate::world_cook::{
    cook_world_grid, CookedWorldGrid, CookedWorldMaterial, WorldGridCookError,
};
use crate::{
    clamp_ui_font_scale, default_ui_font_scale, default_ui_letter_spacing, spatial, AnimationRole,
    CharacterAnimationAction, CharacterControllerSettings, NodeId, NodeKind, OptionId, OptionKind,
    ParticleEmitterSettings, PhysicsBodySettings, ProjectDocument, PsxBlendMode, ResourceData,
    ResourceId, SceneNode, UiAction, UiAnchor, UiGradient, UiImageEffect, UiNodeId, UiNodeKind,
    UiRect, UiSfxCue, UiTextAlign, UiValueBinding, WorldCameraSettings, WorldGrid,
    WorldStreamingSettings, FAR_VISTA_TEXTURE_PANEL_COUNT, MAX_ROOM_BYTES, MAX_UI_LETTER_SPACING,
    MIN_UI_LETTER_SPACING, PHYSICS_WEIGHT_ONE_Q8,
};

mod assets;
mod cook_ui;
mod manifest;
mod performance;
mod schema;
pub(crate) use cook_ui::*;
mod cook_entities;
pub(crate) use cook_entities::*;
mod cook_props_lights;
pub(crate) use cook_props_lights::*;
mod cook_visibility;
pub(crate) use cook_visibility::*;
mod cook_world;
pub(crate) use cook_world::*;

use assets::{
    expect_room_material_depth, find_resource, load_psxt_bytes, material_texture_bytes,
    resolve_path, sanitise_model_dirname,
};

pub use manifest::{
    cook_to_dir, default_generated_dir, render_manifest_source, streamed_room_chunk_memory_report,
    write_cook_result, write_package,
};
pub use performance::{playtest_performance_envelope, PlaytestPerformanceEnvelope};
pub use schema::*;

#[cfg(test)]
const DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS: u16 = 32;
const ATMOSPHERE_DENSITY_MAX: i32 = 96;
const ATMOSPHERE_FALL_SPEED_MAX_Q4: i32 = 64;
const ATMOSPHERE_WIND_SPEED_MIN_Q4: i32 = -64;
const ATMOSPHERE_WIND_SPEED_MAX_Q4: i32 = 64;
const UI_LARGE_IMAGE_STRIP_WIDTH: u16 = 160;
const UI_LARGE_IMAGE_MAX_DIMENSION: u16 = 256;

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

fn clamp_atmosphere_density(value: i32) -> u8 {
    value.clamp(0, ATMOSPHERE_DENSITY_MAX) as u8
}

fn clamp_atmosphere_fall_speed_q4(value: i32) -> i16 {
    value.clamp(0, ATMOSPHERE_FALL_SPEED_MAX_Q4) as i16
}

fn clamp_atmosphere_wind_speed_q4(value: i32) -> i16 {
    value.clamp(ATMOSPHERE_WIND_SPEED_MIN_Q4, ATMOSPHERE_WIND_SPEED_MAX_Q4) as i16
}

#[derive(Debug, Clone)]
pub(crate) struct AuthoredRoomChunk {
    room_index: u16,
    authored_room: u32,
    chunk_index: u16,
    array_origin: [u16; 2],
    world_origin: [i32; 2],
    size: [u16; 2],
    cells: Vec<[u16; 2]>,
    neighbours: [Option<u16>; 4],
    triangles: usize,
    psxw_bytes: usize,
    static_lit_bytes: usize,
    populated_cells: u16,
    /// Which floor of the room this chunk belongs to (0 = base grid).
    /// Drives auto-stacked vertical links between consecutive floors.
    floor_idx: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingRoomFloorLink {
    room: u16,
    x: u16,
    z: u16,
    world_cell: [i32; 2],
    above_room: Option<NodeId>,
    below_room: Option<NodeId>,
}

#[derive(Debug, Clone)]
pub(crate) struct CookedRoomBakeInput {
    room_index: u16,
    world_asset_index: usize,
    world_origin: [i32; 2],
    /// This chunk's floor elevation in engine units (the room's
    /// `origin_y`). Used to keep light spill from crossing floors:
    /// a light only bleeds onto a chunk on (roughly) its own level.
    origin_y: i32,
    cooked: CookedWorldGrid,
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

/// Build a playtest package from `project`. Validates the scene
/// tree, cooks every Room with non-empty geometry, resolves
/// material textures through `project_root`, and assigns the
/// player spawn.
///
/// On any validation error the returned package is `None`.
pub fn build_package(
    project: &ProjectDocument,
    project_root: &Path,
) -> (Option<PlaytestPackage>, PlaytestValidationReport) {
    let mut report = PlaytestValidationReport::default();
    let scene = project.active_scene();

    // Pass 1: enumerate Room nodes. Index = runtime room id.
    let mut room_nodes: Vec<&SceneNode> = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
        .collect();
    room_nodes.sort_by_key(|node| node.id.raw());

    if room_nodes.is_empty() {
        report.error("playtest needs at least one Room node - none found");
        return (None, report);
    }

    // Pass 2: cook each Room. We need the `CookedWorldGrid` for
    // material slot info; encode straight from it so we don't
    // pay for two cooks. Empty grids skip with a warning.
    let mut assets: Vec<PlaytestAsset> = Vec::new();
    let mut rooms: Vec<PlaytestRoom> = Vec::new();
    let mut materials: Vec<PlaytestMaterial> = Vec::new();
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
    let mut room_chunks_by_node: HashMap<NodeId, Vec<AuthoredRoomChunk>> = HashMap::new();
    let mut room_bake_inputs: Vec<CookedRoomBakeInput> = Vec::new();
    let mut room_visibility: Vec<PlaytestRoomVisibility> = Vec::new();
    let mut visibility_cells: Vec<PlaytestVisibilityCell> = Vec::new();
    let mut visibility_pvs: Vec<PlaytestVisibilityPvs> = Vec::new();
    let mut visibility_pvs_bits: Vec<u8> = Vec::new();
    let mut room_surface_caches: Vec<PlaytestRoomSurfaceCache> = Vec::new();
    let mut room_cache_cells: Vec<PlaytestCachedRoomCell> = Vec::new();
    let mut room_cache_cell_vertices: Vec<u16> = Vec::new();
    let mut room_cache_vertices: Vec<PlaytestCachedRoomVertex> = Vec::new();
    let mut room_cache_surfaces: Vec<PlaytestCachedRoomSurface> = Vec::new();
    let mut room_portals: Vec<PlaytestRoomPortal> = Vec::new();
    let mut pending_floor_links: Vec<PendingRoomFloorLink> = Vec::new();
    let room_near_rooms: Vec<u16> = Vec::new();

    for room_node in &room_nodes {
        let NodeKind::Section { grid: base_grid } = &room_node.kind else {
            continue;
        };
        if base_grid.populated_sector_count() == 0 {
            report.warn(format!(
                "Section '{}' has no geometry - skipped",
                room_node.name
            ));
            continue;
        }
        // Vertical room placement. The room's base Y is the Room node's
        // `Transform3::translation[1]` (in sectors, like X/Z). Each floor
        // then adds its own `elevation` delta on top, so stacked floors
        // cook as independent chunk sets at their own heights. The
        // integer-only runtime never sees the host f32.
        let room_origin_y_base = (f64::from(room_node.transform.translation[1])
            * f64::from(base_grid.sector_size)) as i32;
        let base_elevation = base_grid.elevation;
        // Cook floor 0 (the base grid) plus every floor above it. Each
        // floor runs the full per-floor chunk pipeline below.
        for floor_idx in 0..base_grid.floor_count() {
            let Some(grid) = base_grid.floor(floor_idx) else {
                continue;
            };
            if grid.populated_sector_count() == 0 {
                continue;
            }
            let room_origin_y = room_origin_y_base + (grid.elevation - base_elevation);
            let streaming = scene
                .world_streaming_for_node(room_node.id)
                .unwrap_or_default()
                .normalized();
            let resolved_physics = scene
                .world_physics_for_node(room_node.id)
                .unwrap_or_default()
                .normalized();
            let plan = plan_portal_rooms(scene, room_node.id, grid, PortalRoomConfig::default());
            if plan.over_budget_count() > 0 {
                report.warn(format!(
                    "Section '{}' produced {} runtime room(s) still over budget",
                    room_node.name,
                    plan.over_budget_count()
                ));
            }
            let portal_room_count = plan.room_count();
            let room_index_base = rooms.len();
            let room_portal_base = room_portals.len();
            for portal in &plan.portals {
                let source_room = room_index_base.saturating_add(portal.source_room);
                let destination_room = room_index_base.saturating_add(portal.destination_room);
                room_portals.push(PlaytestRoomPortal {
                    source_room: u16::try_from(source_room).unwrap_or(u16::MAX),
                    destination_room: u16::try_from(destination_room).unwrap_or(u16::MAX),
                    kind: if portal.vertical { 1 } else { 0 },
                    normal: portal.normal_world,
                    vertices: portal.vertices_world,
                });
            }
            for portal_room in plan.rooms {
                let chunk_grid = extract_portal_room_grid(grid, &portal_room);
                if chunk_grid.populated_sector_count() == 0 {
                    continue;
                }
                let cooked = match cook_world_grid(project, &chunk_grid) {
                    Ok(c) => c,
                    Err(e) => {
                        report.error(cook_error_for_node(&room_node.name, e));
                        return (None, report);
                    }
                };
                let room_index = u16::try_from(rooms.len()).unwrap_or(u16::MAX);
                room_chunks_by_node
                    .entry(room_node.id)
                    .or_default()
                    .push(AuthoredRoomChunk {
                        room_index,
                        authored_room: room_node.id.raw() as u32,
                        chunk_index: u16::try_from(portal_room.index).unwrap_or(u16::MAX),
                        array_origin: portal_room.array_origin,
                        world_origin: portal_room.world_origin,
                        size: portal_room.size,
                        cells: portal_room.cells.clone(),
                        neighbours: portal_room.neighbours.map(|neighbour| {
                            neighbour.and_then(|index| {
                                u16::try_from(room_index_base.saturating_add(index)).ok()
                            })
                        }),
                        triangles: portal_room.budget.triangles,
                        psxw_bytes: portal_room.budget.psxw_bytes,
                        static_lit_bytes: portal_room.budget.psxw_static_lit_bytes,
                        populated_cells: u16::try_from(chunk_grid.populated_sector_count())
                            .unwrap_or(u16::MAX),
                        floor_idx,
                    });
                collect_pending_floor_links(room_index, &chunk_grid, &mut pending_floor_links);

                // Room asset goes into the master table first (ahead of
                // any material textures discovered while walking it).
                let world_asset_index = assets.len();
                assets.push(PlaytestAsset {
                    kind: PlaytestAssetKind::RoomWorld,
                    bytes: Vec::new(),
                    filename: format!("room_{:03}.psxw", room_index),
                    source_label: runtime_room_name(
                        &room_node.name,
                        portal_room_count,
                        portal_room.index,
                    ),
                    streamed_class: StreamedClass::None,
                });

                // Walk material slots in slot order. The cooker emits
                // CookedWorldMaterial per resolved slot id; we build
                // PlaytestMaterial mirrors keyed to (room, local_slot)
                // and register each unique texture asset on first use.
                let material_first = u16::try_from(materials.len()).unwrap_or(u16::MAX);
                let mut sorted_materials: Vec<&CookedWorldMaterial> =
                    cooked.materials.iter().collect();
                sorted_materials.sort_by_key(|m| m.slot);

                for cooked_material in sorted_materials {
                    let material_resource = find_resource(project, cooked_material.source);
                    let material_label = material_resource
                        .map(|resource| resource.name.clone())
                        .unwrap_or_else(|| format!("material #{}", cooked_material.source.raw()));
                    let Some(material_resource) = material_resource else {
                        report.error(format!(
                            "Section '{}' material slot {} references missing resource #{}",
                            room_node.name,
                            cooked_material.slot,
                            cooked_material.source.raw(),
                        ));
                        return (None, report);
                    };
                    let (texture_key, texture_bytes) =
                        match material_texture_bytes(project, material_resource, project_root) {
                            Ok(Some(source)) => source,
                            Ok(None) => {
                                report.error(format!(
                                    "Section '{}' material slot {} has no texture (resource #{})",
                                    room_node.name,
                                    cooked_material.slot,
                                    cooked_material.source.raw(),
                                ));
                                return (None, report);
                            }
                            Err(msg) => {
                                report.error(format!(
                                    "Section '{}' material slot {}: {}",
                                    room_node.name, cooked_material.slot, msg,
                                ));
                                return (None, report);
                            }
                        };
                    // Texture pages dedupe by resolved path: materials
                    // sharing one .psxt share one cooked asset.
                    let texture_asset_index =
                        if let Some(&existing) = texture_asset_for_path.get(&texture_key) {
                            existing
                        } else {
                            let bytes = texture_bytes;
                            // Room materials must be 4bpp (16-entry CLUT) --
                            // both the editor preview's material upload
                            // path and the runtime room material slots
                            // assume the 4bpp tpage layout. Loud failure
                            // here beats wrong-colour rendering at runtime.
                            if let Err(msg) = expect_room_material_depth(&material_label, &bytes) {
                                report.error(format!(
                                    "Section '{}' material slot {}: {}",
                                    room_node.name, cooked_material.slot, msg,
                                ));
                                return (None, report);
                            }
                            let texture_index = texture_asset_for_path.len();
                            let new_index = assets.len();
                            assets.push(PlaytestAsset {
                                kind: PlaytestAssetKind::Texture,
                                bytes,
                                filename: format!("texture_{:03}.psxt", texture_index),
                                source_label: material_label.clone(),
                                streamed_class: StreamedClass::None,
                            });
                            texture_asset_for_path.insert(texture_key, new_index);
                            new_index
                        };

                    materials.push(PlaytestMaterial {
                        room: room_index,
                        local_slot: cooked_material.slot,
                        texture_asset_index,
                        tint_rgb: cooked_material.tint,
                        blend_mode: cooked_material.blend_mode,
                        animation: cooked_material.animation,
                        face_sidedness: cooked_material.face_sidedness,
                    });
                }
                let material_count =
                    u16::try_from(materials.len() - material_first as usize).unwrap_or(u16::MAX);
                // The runtime keeps a fixed per-room material table of
                // psx_level::MAX_ROOM_MATERIALS slots and silently drops any
                // material whose local_slot is >= that cap, which makes every
                // surface on the dropped slot invisible at runtime. Fail the cook
                // loudly here instead (this was the demo10 invisible frieze/stairs
                // root cause, which had no signal until the runtime telemetry and
                // this guard were added).
                if material_count as usize > psx_level::MAX_ROOM_MATERIALS {
                    report.error(format!(
                        "Room '{}' uses {} distinct materials but the per-room cap \
                         is {} (psx_level::MAX_ROOM_MATERIALS). Reduce the room's \
                         materials or raise the cap; over-cap materials render \
                         invisible at runtime.",
                        room_node.name,
                        material_count,
                        psx_level::MAX_ROOM_MATERIALS,
                    ));
                    return (None, report);
                }

                let resolved_culling = scene
                    .world_culling_for_node(room_node.id)
                    .unwrap_or_default()
                    .normalized();
                append_room_visibility(
                    room_index,
                    &cooked,
                    resolved_culling.visibility_radius,
                    &mut room_visibility,
                    &mut visibility_cells,
                    &mut visibility_pvs,
                    &mut visibility_pvs_bits,
                );

                let resolved_sky = scene
                    .world_sky_for_node(room_node.id)
                    .unwrap_or_default()
                    .resolved_for_room(chunk_grid.fog_enabled, chunk_grid.fog_color);
                let resolved_far_vista = scene
                    .world_far_vista_for_node(room_node.id)
                    .unwrap_or_default()
                    .resolved_for_room(chunk_grid.fog_enabled, chunk_grid.fog_color);
                let resolved_camera = scene
                    .world_camera_for_node(room_node.id)
                    .unwrap_or_default()
                    .normalized();
                let far_vista_texture_asset_indices = if resolved_far_vista.enabled {
                    let assigned_panels = resolved_far_vista
                        .texture_panels
                        .iter()
                        .any(Option::is_some);
                    if assigned_panels {
                        resolved_far_vista
                            .texture_panels
                            .iter()
                            .take(active_far_vista_panel_count(
                                &resolved_far_vista.texture_panels,
                                resolved_far_vista.segments,
                            ))
                            .enumerate()
                            .map(|(panel_index, texture_id)| {
                                texture_id.and_then(|texture_id| {
                                    let context = format!(
                                        "Room '{}' far vista panel {}",
                                        room_node.name,
                                        panel_index + 1
                                    );
                                    cook_far_vista_texture_asset(
                                        project,
                                        project_root,
                                        texture_id,
                                        &context,
                                        &mut texture_asset_for_path,
                                        &mut assets,
                                        &mut report,
                                    )
                                })
                            })
                            .collect::<Vec<_>>()
                    } else {
                        resolved_far_vista
                            .texture
                            .and_then(|texture_id| {
                                let context = format!("Room '{}' far vista", room_node.name);
                                cook_far_vista_texture_asset(
                                    project,
                                    project_root,
                                    texture_id,
                                    &context,
                                    &mut texture_asset_for_path,
                                    &mut assets,
                                    &mut report,
                                )
                            })
                            .into_iter()
                            .map(Some)
                            .collect::<Vec<_>>()
                    }
                } else {
                    Vec::new()
                };
                let far_vista_has_texture =
                    far_vista_texture_asset_indices.iter().any(Option::is_some);
                let sky_texture_asset_index = cook_sky_panorama_texture_asset(
                    resolved_sky,
                    &mut sky_texture_assets,
                    &mut assets,
                );
                let atmosphere_density = clamp_atmosphere_density(chunk_grid.atmosphere_density);
                let atmosphere_fall_speed_q4 =
                    clamp_atmosphere_fall_speed_q4(chunk_grid.atmosphere_fall_speed_q4);
                let atmosphere_wind_speed_q4 =
                    clamp_atmosphere_wind_speed_q4(chunk_grid.atmosphere_wind_speed_q4);
                let atmosphere_enabled = chunk_grid.atmosphere_enabled && atmosphere_density > 0;
                let reflection_probe_asset_index = if uses_room_reflection_probes {
                    let bytes = match crate::generate_room_reflection_probe_psxt(
                        project,
                        &chunk_grid,
                        project_root,
                    ) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            report.error(format!(
                                "Room '{}' reflection probe could not be baked: {error}",
                                room_node.name
                            ));
                            return (None, report);
                        }
                    };
                    let asset_index = assets.len();
                    assets.push(PlaytestAsset {
                        kind: PlaytestAssetKind::Texture,
                        bytes,
                        filename: format!("room_probe_{room_index:03}.psxt"),
                        source_label: format!(
                            "{} reflection probe",
                            runtime_room_name(
                                &room_node.name,
                                portal_room_count,
                                portal_room.index,
                            )
                        ),
                        streamed_class: StreamedClass::None,
                    });
                    Some(asset_index)
                } else {
                    None
                };

                rooms.push(PlaytestRoom {
                    name: runtime_room_name(&room_node.name, portal_room_count, portal_room.index),
                    world_asset_index,
                    reflection_probe_asset_index,
                    origin_x: chunk_grid.origin[0],
                    origin_z: chunk_grid.origin[1],
                    origin_y: room_origin_y,
                    sector_size: chunk_grid.sector_size,
                    draw_distance: resolved_culling.draw_distance,
                    chunk_activation_radius_sectors: resolved_culling
                        .chunk_activation_radius_sectors,
                    visibility_radius: resolved_culling.visibility_radius,
                    resident_chunk_limit: playtest_streaming_resident_chunk_limit(streaming),
                    visible_chunk_limit: streaming.visible_chunk_limit,
                    gravity_per_tick: resolved_physics.gravity_per_tick,
                    material_first,
                    material_count,
                    portal_first: if portal_room.portal_count == 0 {
                        0
                    } else {
                        u16::try_from(room_portal_base.saturating_add(portal_room.portal_first))
                            .unwrap_or(u16::MAX)
                    },
                    portal_count: u8::try_from(portal_room.portal_count).unwrap_or(u8::MAX),
                    near_room_first: 0,
                    near_room_count: 0,
                    overlapped_room_first: 0,
                    overlapped_room_count: 0,
                    fog_rgb: chunk_grid.fog_color,
                    fog_near: chunk_grid.fog_near,
                    fog_far: chunk_grid.fog_far,
                    atmosphere_rgb: chunk_grid.atmosphere_color,
                    atmosphere_density,
                    atmosphere_fall_speed_q4,
                    atmosphere_wind_speed_q4,
                    sky: PlaytestSky {
                        top_rgb: resolved_sky.top_color,
                        horizon_rgb: resolved_sky.horizon_color,
                        bottom_rgb: resolved_sky.lower_color,
                        horizon_percent: resolved_sky.horizon_percent,
                        horizon_thickness_percent: resolved_sky.horizon_thickness_percent,
                        skybox_columns: resolved_sky.skybox_columns,
                        skybox_rows: resolved_sky.skybox_rows,
                        flags: if resolved_sky.enabled {
                            sky_flags::ENABLED
                        } else {
                            0
                        },
                        cyclorama_quads: Vec::new(),
                        cloud_layer: PlaytestCloudLayer {
                            texture_asset_index: sky_texture_asset_index,
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
                        texture_asset_indices: far_vista_texture_asset_indices,
                        radius: resolved_far_vista.radius,
                        height: resolved_far_vista.height,
                        vertical_offset: resolved_far_vista.vertical_offset,
                        segments: resolved_far_vista.segments,
                        rotation_degrees: resolved_far_vista.rotation_degrees,
                        tint_rgb: resolved_far_vista.tint,
                        flags: if resolved_far_vista.enabled {
                            far_vista_flags::ENABLED
                                | if far_vista_has_texture {
                                    far_vista_flags::TEXTURED
                                } else {
                                    0
                                }
                        } else {
                            0
                        },
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
                    flags: if chunk_grid.fog_enabled {
                        room_flags::FOG_ENABLED
                    } else {
                        0
                    } | if atmosphere_enabled {
                        room_flags::ATMOSPHERE_ENABLED
                    } else {
                        0
                    },
                });
                room_bake_inputs.push(CookedRoomBakeInput {
                    room_index,
                    world_asset_index,
                    world_origin: portal_room.world_origin,
                    origin_y: room_origin_y,
                    cooked,
                });
            }
        }
    }

    if rooms.is_empty() {
        report.error("every Room is empty - cook needs at least one populated room");
        return (None, report);
    }
    let mut chunks = build_playtest_chunks(&room_chunks_by_node, rooms.len());

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
    let mut water_cells: Vec<PlaytestWaterCell> = Vec::new();
    let mut combat_capsules: Vec<PlaytestCombatCapsule> = Vec::new();
    let mut weapon_hitboxes: Vec<PlaytestWeaponHitbox> = Vec::new();
    let mut weapons: Vec<PlaytestWeapon> = Vec::new();
    let mut equipment: Vec<PlaytestEquipment> = Vec::new();
    let mut lights: Vec<PlaytestLight> = Vec::new();
    let mut particle_emitters: Vec<PlaytestParticleEmitter> = Vec::new();
    let mut interactable_messages: Vec<PlaytestInteractableMessage> = Vec::new();
    let mut interactables: Vec<PlaytestInteractable> = Vec::new();
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

    // Water is authored as a sparse world-cell footprint on a Room floor.
    // Resolve each cell through the exact portal-room plan so split rooms and
    // stacked floors inherit the correct runtime room/local coordinates.
    // The compact WATER_CELLS table is both the authoritative gameplay lookup
    // and the stateless render source; water never consumes BoxProp state or
    // collision-blocker capacity.
    let mut occupied_water_cells: HashSet<(u16, u16, u16)> = HashSet::new();
    for node in scene.nodes() {
        let NodeKind::WaterVolume {
            material,
            cells,
            settings,
        } = &node.kind
        else {
            continue;
        };
        let Some(room_node) = enclosing_room(scene, node) else {
            report.warn(format!(
                "Water Volume '{}' has no enclosing Room - skipped",
                node.name
            ));
            continue;
        };
        let NodeKind::Section { grid: base_grid } = &room_node.kind else {
            continue;
        };
        let floor_index = node.floor.min(base_grid.floor_count().saturating_sub(1));
        let Some(grid) = base_grid.floor(floor_index) else {
            continue;
        };
        let normalized = settings.normalized();
        if cells.is_empty() {
            report.warn(format!("Water Volume '{}' has no painted cells", node.name));
            continue;
        }
        let surface = material.and_then(|material_id| {
            resolve_material_texture_asset(
                project,
                project_root,
                &format!("Water Volume '{}' surface", node.name),
                material_id,
                &mut texture_asset_for_path,
                &mut assets,
                &mut report,
            )
            .map(|(texture_asset_index, tint_rgb)| {
                let (blend_mode, animation) = project
                    .resource(material_id)
                    .and_then(|resource| match &resource.data {
                        ResourceData::Material(material) => Some((
                            manifest::model_override_blend_code(material.blend_mode),
                            material.animation,
                        )),
                        _ => None,
                    })
                    .unwrap_or((
                        psx_level::model_override_blend::AVERAGE,
                        crate::MaterialAnimation::default(),
                    ));
                (texture_asset_index, blend_mode, tint_rgb, animation)
            })
        });
        for cell in cells {
            let Some((array_x, array_z)) = grid.world_cell_to_array(cell.x, cell.z) else {
                report.warn(format!(
                    "Water Volume '{}' cell {},{} lies outside floor {} - skipped",
                    node.name,
                    cell.x,
                    cell.z,
                    floor_index + 1,
                ));
                continue;
            };
            let Some(sector) = grid.sector(array_x, array_z) else {
                report.warn(format!(
                    "Water Volume '{}' cell {},{} has no terrain sector - skipped",
                    node.name, cell.x, cell.z,
                ));
                continue;
            };
            let Some(floor) = sector.floor.as_ref() else {
                report.warn(format!(
                    "Water Volume '{}' cell {},{} has no floor - skipped",
                    node.name, cell.x, cell.z,
                ));
                continue;
            };
            let Some(chunk) = room_chunks_by_node.get(&room_node.id).and_then(|chunks| {
                chunks.iter().find(|chunk| {
                    chunk.floor_idx == floor_index && chunk.cells.contains(&[array_x, array_z])
                })
            }) else {
                report.warn(format!(
                    "Water Volume '{}' cell {},{} did not map to a runtime room - skipped",
                    node.name, cell.x, cell.z,
                ));
                continue;
            };
            let local_x = array_x.saturating_sub(chunk.array_origin[0]);
            let local_z = array_z.saturating_sub(chunk.array_origin[1]);
            if !occupied_water_cells.insert((chunk.room_index, local_x, local_z)) {
                report.warn(format!(
                    "Water Volume '{}' overlaps another volume at {},{} - later cell skipped",
                    node.name, cell.x, cell.z,
                ));
                continue;
            }
            let floor_y = floor.lowest_height();
            let depth = normalized.height_above_floor;
            let surface_y = floor_y.saturating_add(i32::from(depth));
            water_cells.push(PlaytestWaterCell {
                room: chunk.room_index,
                x: local_x,
                z: local_z,
                texture_asset_index: surface.map(|surface| surface.0),
                blend_mode: surface
                    .map(|surface| surface.1)
                    .unwrap_or(psx_level::model_override_blend::AVERAGE),
                tint_rgb: surface.map(|surface| surface.2).unwrap_or([128; 3]),
                animation: surface.map(|surface| surface.3).unwrap_or_default(),
                surface_y,
                depth,
                lethal_depth: normalized.lethal_depth,
                movement_percent: normalized.movement_percent,
                death_delay_ticks: normalized.death_delay_ticks,
                death_submerge_depth: normalized.death_submerge_depth,
            });
        }
    }
    water_cells.sort_by_key(|cell| (cell.room, cell.x, cell.z));

    for node in scene.nodes() {
        if node.id == scene.root || matches!(node.kind, NodeKind::Section { .. }) {
            continue;
        }
        if node.kind.is_component() {
            continue;
        }
        let Some(room_node) = enclosing_room(scene, node) else {
            if !matches!(
                node.kind,
                NodeKind::Node | NodeKind::Node3D | NodeKind::Entity | NodeKind::World { .. }
            ) {
                report.warn(format!(
                    "{} '{}' has no enclosing Room - dropped",
                    node.kind.label(),
                    node.name
                ));
            }
            continue;
        };
        let NodeKind::Section { grid } = &room_node.kind else {
            continue;
        };
        let Some(chunk) = room_chunks_by_node
            .get(&room_node.id)
            .and_then(|chunks| chunk_for_node(node, grid, chunks))
        else {
            if !matches!(
                node.kind,
                NodeKind::Node | NodeKind::Node3D | NodeKind::Entity | NodeKind::World { .. }
            ) {
                report.warn(format!(
                    "{} '{}' is outside cooked Room '{}' chunks - dropped",
                    node.kind.label(),
                    node.name,
                    room_node.name
                ));
            }
            continue;
        };
        let room_index = chunk.room_index;
        let raw_pos = node_chunk_local_position(node, grid, chunk);
        let floor_pos = floor_anchored_node_chunk_local_position(node, grid, chunk);
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
                        if !push_model_instance_for_resource(
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
                                report: &mut report,
                            },
                        ) {
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
                            controller_settings: Some(controller.settings),
                            camera: component_camera(scene, node).map(|camera| camera.settings),
                            weight_q8,
                            renderer: component_model_renderer(scene, node),
                            animator: component_animator(scene, node),
                        });
                    } else if component_model_renderer(scene, node).is_none() {
                        let Some(character_id) = controller.character else {
                            if controller.settings.enemy.is_some() {
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
                        if !push_character_controller_idle_instance(
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
                            &mut report,
                        ) {
                            return (None, report);
                        }
                    }
                }

                if let Some(controller) = character_controller {
                    if !controller.player {
                        if let Some(enemy) = controller.settings.enemy {
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
                                    archetype.as_str(),
                                    enemy_character,
                                    model_joint_count,
                                    &mut combat_capsules,
                                    &mut report,
                                )
                            else {
                                return (None, report);
                            };
                            if !push_game_entity(
                                node.name.as_str(),
                                archetype.as_str(),
                                room_index,
                                pos,
                                yaw,
                                &controller.settings,
                                enemy,
                                node_model_instance,
                                state_clips,
                                combat_capsule_first,
                                combat_capsule_count,
                                &mut names,
                                &mut game_entities,
                                &mut report,
                            ) {
                                return (None, report);
                            }
                        }
                    }
                }

                if let Some(equipped) = component_equipment(scene, node) {
                    if let Some(weapon_id) = equipped.weapon {
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
                        equipment.push(PlaytestEquipment {
                            room: room_index,
                            weapon: weapon_index,
                            x: pos[0],
                            y: pos[1],
                            z: pos[2],
                            yaw,
                            character_socket: equipped.character_socket.to_string(),
                            weapon_grip: equipped.weapon_grip.to_string(),
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

                if let Some(interactable) = component_interactable(scene, node) {
                    if !push_interactable(
                        node.name.as_str(),
                        room_index,
                        pos,
                        yaw,
                        interactable,
                        &mut names,
                        &mut interactable_messages,
                        &mut interactables,
                        &mut logic,
                        &mut report,
                    ) {
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
                    if !push_model_instance_for_resource(
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
                            report: &mut report,
                        },
                    ) {
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
                if !push_point_light(
                    node.name.as_str(),
                    grid,
                    room_index,
                    raw_pos,
                    *color,
                    *intensity,
                    *radius,
                    &mut lights,
                    &mut report,
                ) {
                    return (None, report);
                }
            }
            NodeKind::ImageProp {
                material,
                width,
                height,
                cylindrical_billboard,
                collision_enabled: _,
                collision_size: _,
            } => {
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
                if !push_box_prop(
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
                    &mut report,
                ) {
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
                if !push_cylinder_prop(
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
                    &mut report,
                ) {
                    return (None, report);
                }
            }
            NodeKind::ArchProp {
                materials,
                uvs,
                geometry,
                collision_enabled,
            } => {
                if !push_arch_prop(
                    project,
                    project_root,
                    node.name.as_str(),
                    room_index,
                    raw_pos,
                    pitch,
                    yaw,
                    roll,
                    grid.sector_size,
                    materials,
                    uvs,
                    *geometry,
                    *collision_enabled,
                    &mut texture_asset_for_path,
                    &mut assets,
                    &mut arch_props,
                    &mut arch_prop_surfaces,
                    &mut arch_prop_collisions,
                    &mut report,
                ) {
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
                if !push_logic_node(
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
                    &mut names,
                    &mut logic,
                    &mut pending_door_links,
                    &mut report,
                ) {
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
            | NodeKind::Interactable { .. } => {}
        }
    }

    for room_index in 0..rooms.len() {
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
        let total = box_count.saturating_add(arch_count);
        if total > psx_level::MAX_STATIC_PROP_AABB_BLOCKERS {
            report.error(format!(
                "Room '{}' needs {total} static prop collision AABBs ({box_count} box + {arch_count} arch), exceeding the PS1 runtime budget of {}",
                rooms[room_index].name,
                psx_level::MAX_STATIC_PROP_AABB_BLOCKERS,
            ));
        }
    }

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
                        report.error(format!(
                            "Player source '{}' has no Character assigned and no Character resources exist",
                            spawn_node.name
                        ));
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
        _ => None,
    };

    collapse_exclusive_player_material(
        player_controller,
        &mut characters,
        &models,
        &model_instances,
        &weapons,
        &mut assets,
        &mut report,
    );

    if !report.is_ok() {
        return (None, report);
    }

    let mut lights = expand_lights_across_chunks(&room_bake_inputs, &lights);
    bake_static_surface_lights(&mut room_bake_inputs, &lights);
    bake_static_image_prop_lights(&mut image_props, &room_bake_inputs, &lights);
    bake_static_box_prop_lights(
        &mut box_props,
        &mut box_prop_surfaces,
        &room_bake_inputs,
        &lights,
    );
    bake_static_cylinder_prop_lights(
        &cylinder_props,
        &mut cylinder_prop_surfaces,
        &room_bake_inputs,
        &lights,
    );
    bake_static_arch_prop_lights(
        &arch_props,
        &mut arch_prop_surfaces,
        &room_bake_inputs,
        &lights,
    );
    for room in &room_bake_inputs {
        let bytes = match room.cooked.to_psxw_bytes() {
            Ok(b) => b,
            Err(e) => {
                report.error(cook_error_for_node(
                    rooms
                        .get(room.room_index as usize)
                        .map(|room| room.name.as_str())
                        .unwrap_or("<room>"),
                    e,
                ));
                return (None, report);
            }
        };
        if bytes.len() > MAX_ROOM_BYTES {
            let room_name = rooms
                .get(room.room_index as usize)
                .map(|room| room.name.as_str())
                .unwrap_or("<room>");
            report.error(format!(
                "Room '{room_name}' static-lit .psxw is {} bytes; cap is {}",
                bytes.len(),
                MAX_ROOM_BYTES,
            ));
            return (None, report);
        }
        if let Err(msg) = append_room_surface_cache(
            room.room_index,
            &bytes,
            &materials,
            &assets,
            &mut room_surface_caches,
            &mut room_cache_cells,
            &mut room_cache_cell_vertices,
            &mut room_cache_vertices,
            &mut room_cache_surfaces,
        ) {
            report.error(msg);
            return (None, report);
        }
        assign_visibility_cache_cell_indices(
            room.room_index,
            &room_visibility,
            &mut visibility_cells,
            &room_surface_caches,
            &room_cache_cells,
        );
        if let Some(asset) = assets.get_mut(room.world_asset_index) {
            asset.bytes = bytes;
        }
        if let Some(chunk) = chunks.get_mut(room.room_index as usize) {
            chunk.static_lit_bytes = assets
                .get(room.world_asset_index)
                .map(|asset| asset.bytes.len())
                .unwrap_or(chunk.static_lit_bytes);
        }
    }

    let mut room_floor_links = resolve_room_floor_links(&pending_floor_links, &room_chunks_by_node);
    room_floor_links.extend(auto_wire_floor_stack_links(&room_chunks_by_node));
    room_portals.extend(auto_wire_floor_stack_portals(scene, &room_chunks_by_node));
    room_portals.extend(auto_wire_floor_terrace_portals(scene, &room_chunks_by_node));
    // Portals that join two Sections. Until this, every Section cooked as its
    // own island and a level built from more than one of them was disconnected
    // at runtime no matter how the editor drew it.
    let wiring = cross_section_portals(scene, &room_chunks_by_node);
    let (cross_section, cross_issues, wired_edges) = (wiring.portals, wiring.issues, wiring.edges);
    room_portals.extend(cross_section);
    // Sections placed edge to edge with facing openings connect on their own.
    room_portals.extend(auto_adjacent_section_portals(
        scene,
        &room_chunks_by_node,
        &wired_edges,
    ));
    for issue in cross_issues {
        let portal_name = scene
            .node(issue.portal)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("#{}", issue.portal.raw()));
        let section_name = scene
            .node(issue.room)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("#{}", issue.room.raw()));
        let message = format!(
            "Portal '{portal_name}' in Section '{section_name}' is {} - it connects nothing at runtime",
            issue.status.label()
        );
        // An unassigned portal is work in progress; a broken pairing is a door
        // the player can see and never use, so that one fails the build.
        match issue.status {
            crate::room_connections::RoomConnectionStatus::Unassigned => report.warn(message),
            _ => report.error(message),
        }
    }
    let room_overlapped_rooms = assign_floor_stack_overlaps(&mut rooms, &room_chunks_by_node);
    // The runtime visibility BFS reads each room's portals as the
    // contiguous slice [portal_first, portal_first+portal_count). The
    // per-room ranges set during the cook loop only covered the
    // horizontal portals from `plan_portal_rooms`; the vertical
    // floor-stack portals were appended afterwards and would be
    // unreferenced (room.portal_count wouldn't include them, so the BFS
    // never scans them). Regroup the whole table by source_room and
    // rebuild every room's range so both kinds are reachable.
    regroup_room_portals(&mut rooms, &mut room_portals);
    rebuild_portal_connected_visibility_pvs(
        &rooms,
        &chunks,
        &room_portals,
        &mut room_visibility,
        &visibility_cells,
        &mut visibility_pvs,
        &mut visibility_pvs_bits,
    );
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

    (
        Some(PlaytestPackage {
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
            lights,
            particle_emitters,
            interactable_messages,
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

/// Build the same runtime-room/cell/portal topology as [`build_package`]
/// without resolving textures, models, or other play assets. This is cheap
/// enough for editor diagnostics and, importantly, prevents the Room Rings
/// overlay from maintaining a second approximation of the cooker.
pub fn build_debug_topology(project: &ProjectDocument) -> PlaytestDebugTopology {
    let scene = project.active_scene();
    let mut room_nodes: Vec<&SceneNode> = scene
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Section { .. }))
        .collect();
    room_nodes.sort_by_key(|node| node.id.raw());

    let mut cells = Vec::new();
    let mut portals = Vec::new();
    let mut chunks_by_node: HashMap<NodeId, Vec<AuthoredRoomChunk>> = HashMap::new();
    let mut runtime_room_count = 0usize;
    for room_node in room_nodes {
        let NodeKind::Section { grid: base_grid } = &room_node.kind else {
            continue;
        };
        if base_grid.populated_sector_count() == 0 {
            continue;
        }
        let room_origin_y_base = (f64::from(room_node.transform.translation[1])
            * f64::from(base_grid.sector_size)) as i32;
        let base_elevation = base_grid.elevation;
        for floor_index in 0..base_grid.floor_count() {
            let Some(grid) = base_grid.floor(floor_index) else {
                continue;
            };
            if grid.populated_sector_count() == 0 {
                continue;
            }
            let elevation = room_origin_y_base + (grid.elevation - base_elevation);
            let plan = plan_portal_rooms(scene, room_node.id, grid, PortalRoomConfig::default());
            let room_index_base = runtime_room_count;
            for portal in &plan.portals {
                portals.push(PlaytestRoomPortal {
                    source_room: u16::try_from(room_index_base.saturating_add(portal.source_room))
                        .unwrap_or(u16::MAX),
                    destination_room: u16::try_from(
                        room_index_base.saturating_add(portal.destination_room),
                    )
                    .unwrap_or(u16::MAX),
                    kind: if portal.vertical { 1 } else { 0 },
                    normal: portal.normal_world,
                    vertices: portal.vertices_world,
                });
            }
            for portal_room in plan.rooms {
                let chunk_grid = extract_portal_room_grid(grid, &portal_room);
                if chunk_grid.populated_sector_count() == 0 {
                    continue;
                }
                let runtime_room_index = runtime_room_count;
                let node_center = [
                    room_node.transform.translation[0],
                    room_node.transform.translation[2],
                ];
                let room_origin = [
                    node_center[0] + portal_room.array_origin[0] as f32 - grid.width as f32 * 0.5,
                    node_center[1] + portal_room.array_origin[1] as f32 - grid.depth as f32 * 0.5,
                ];
                for array_cell in &portal_room.cells {
                    let local_center = [
                        array_cell[0] as f32 + 0.5 - grid.width as f32 * 0.5,
                        array_cell[1] as f32 + 0.5 - grid.depth as f32 * 0.5,
                    ];
                    cells.push(PlaytestDebugTopologyCell {
                        runtime_room_index,
                        authored_room: room_node.id.raw() as u32,
                        portal_room_index: portal_room.index,
                        floor_index,
                        array_cell: *array_cell,
                        center: [
                            node_center[0] + local_center[0],
                            node_center[1] + local_center[1],
                        ],
                        half: [0.5, 0.5],
                        room_origin,
                        runtime_origin: portal_room.world_origin,
                        elevation,
                        sector_size: grid.sector_size.max(1),
                    });
                }
                let room_index = u16::try_from(runtime_room_index).unwrap_or(u16::MAX);
                chunks_by_node
                    .entry(room_node.id)
                    .or_default()
                    .push(AuthoredRoomChunk {
                        room_index,
                        authored_room: room_node.id.raw() as u32,
                        chunk_index: u16::try_from(portal_room.index).unwrap_or(u16::MAX),
                        array_origin: portal_room.array_origin,
                        world_origin: portal_room.world_origin,
                        size: portal_room.size,
                        cells: portal_room.cells,
                        neighbours: portal_room.neighbours.map(|neighbour| {
                            neighbour.and_then(|index| {
                                u16::try_from(room_index_base.saturating_add(index)).ok()
                            })
                        }),
                        triangles: portal_room.budget.triangles,
                        psxw_bytes: portal_room.budget.psxw_bytes,
                        static_lit_bytes: portal_room.budget.psxw_static_lit_bytes,
                        populated_cells: u16::try_from(chunk_grid.populated_sector_count())
                            .unwrap_or(u16::MAX),
                        floor_idx: floor_index,
                    });
                runtime_room_count += 1;
            }
        }
    }
    portals.extend(auto_wire_floor_stack_portals(scene, &chunks_by_node));
    portals.extend(auto_wire_floor_terrace_portals(scene, &chunks_by_node));
    portals.sort_by_key(|portal| portal.source_room);
    PlaytestDebugTopology {
        cells,
        portals: portals
            .into_iter()
            .enumerate()
            .map(|(portal_index, portal)| PlaytestDebugTopologyPortal {
                portal_index,
                portal,
            })
            .collect(),
    }
}

/// Preserve the exact PS1 blend equation while reducing the common player
/// crystal material from two full model submissions to one. This deliberately
/// stays narrow: sharing the model or using any other blend pairing keeps the
/// authored two-pass path unchanged.
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
