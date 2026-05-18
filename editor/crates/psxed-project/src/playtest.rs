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

use psx_engine::{
    cache_room_vertex_lit_surfaces, CachedRoomCell, CachedRoomSurface, RuntimeRoom,
    WorldRenderMaterial, WorldVertex,
};
use psx_level::{
    character_action_flags, cloud_layer_flags, far_vista_flags, image_prop_flags, model_clip_flags,
    sky_flags, visibility_cell_flags, visibility_edge_flags,
};
use psxed_format::world as psxw;

use crate::streaming::{plan_generated_chunks, StreamingChunkConfig};
use crate::world_cook::{
    cook_world_grid, CookedWorldGrid, CookedWorldMaterial, WorldGridCookError,
};
use crate::{
    spatial, AnimationRole, CharacterAnimationAction, CharacterControllerSettings, GridDirection,
    NodeId, NodeKind, ProjectDocument, ResourceData, ResourceId, SceneNode, WorldGrid,
    FAR_VISTA_TEXTURE_PANEL_COUNT, MAX_ROOM_BYTES,
};

mod assets;
mod manifest;
mod schema;

use assets::{
    expect_room_material_depth, find_resource, load_texture_bytes, resolve_path,
    sanitise_model_dirname,
};

pub use manifest::{
    cook_to_dir, default_generated_dir, render_manifest_source, streamed_room_chunk_memory_report,
    write_package,
};
pub use schema::*;

const PLAYTEST_VISIBILITY_CELL_RADIUS: u16 = 32;

struct PlayerSpawnCandidate<'a> {
    node: &'a SceneNode,
    room_index: u16,
    position: [i32; 3],
    character: Option<ResourceId>,
    controller_settings: Option<CharacterControllerSettings>,
    renderer: Option<ModelRendererComponent>,
    animator: Option<AnimatorComponent<'a>>,
}

#[derive(Debug, Clone, Copy)]
struct AuthoredRoomChunk {
    room_index: u16,
    authored_room: u32,
    chunk_index: u16,
    array_origin: [u16; 2],
    world_origin: [i32; 2],
    size: [u16; 2],
    triangles: usize,
    psxw_bytes: usize,
    static_lit_bytes: usize,
    populated_cells: u16,
}

#[derive(Debug, Clone)]
struct CookedRoomBakeInput {
    room_index: u16,
    world_asset_index: usize,
    world_origin: [i32; 2],
    cooked: CookedWorldGrid,
}

/// Chunking policy used by Embedded Play. Authored Rooms may grow
/// beyond the runtime room cap, but generated `.psxw` chunks should be
/// sized for render cost before the hard runtime limits are reached.
pub fn playtest_streaming_chunk_config() -> StreamingChunkConfig {
    StreamingChunkConfig::default()
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
        .filter(|node| matches!(node.kind, NodeKind::Room { .. }))
        .collect();
    room_nodes.sort_by_key(|node| node.id.raw());

    if room_nodes.is_empty() {
        report.error("playtest needs at least one Room node — none found");
        return (None, report);
    }

    // Pass 2: cook each Room. We need the `CookedWorldGrid` for
    // material slot info; encode straight from it so we don't
    // pay for two cooks. Empty grids skip with a warning.
    let mut assets: Vec<PlaytestAsset> = Vec::new();
    let mut rooms: Vec<PlaytestRoom> = Vec::new();
    let mut materials: Vec<PlaytestMaterial> = Vec::new();
    // ResourceId → index into `assets` for texture deduplication.
    // First-use order is deterministic because we walk rooms +
    // material slots in deterministic order and assign the
    // texture's compact "texture index" via `texture_asset_for_resource.len()`
    // at first insertion (never removed). HashMap is fine -- we
    // only use it for presence tests.
    let mut texture_asset_for_resource: std::collections::HashMap<ResourceId, usize> =
        std::collections::HashMap::new();
    let mut sky_texture_assets: Vec<(crate::ResolvedSkySettings, usize)> = Vec::new();
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

    for room_node in &room_nodes {
        let NodeKind::Room { grid } = &room_node.kind else {
            continue;
        };
        if grid.populated_sector_count() == 0 {
            report.warn(format!(
                "Room '{}' has no geometry — skipped",
                room_node.name
            ));
            continue;
        }
        let plan = plan_generated_chunks(grid, playtest_streaming_chunk_config());
        let chunk_count = plan.chunk_count();
        for chunk in plan.chunks {
            let Some(chunk_grid) = grid_rect(grid, chunk.array_origin, chunk.size) else {
                continue;
            };
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
                    chunk_index: u16::try_from(chunk.index).unwrap_or(u16::MAX),
                    array_origin: chunk.array_origin,
                    world_origin: chunk.world_origin,
                    size: chunk.size,
                    triangles: chunk.budget.triangles,
                    psxw_bytes: chunk.budget.psxw_bytes,
                    static_lit_bytes: chunk.budget.psxw_static_lit_bytes,
                    populated_cells: u16::try_from(chunk_grid.populated_sector_count())
                        .unwrap_or(u16::MAX),
                });

            // Room asset goes into the master table first (ahead of
            // any material textures discovered while walking it).
            let world_asset_index = assets.len();
            assets.push(PlaytestAsset {
                kind: PlaytestAssetKind::RoomWorld,
                bytes: Vec::new(),
                filename: format!("room_{:03}.psxw", room_index),
                source_label: chunk_room_name(&room_node.name, chunk_count, chunk.index),
            });

            // Walk material slots in slot order. The cooker emits
            // CookedWorldMaterial per resolved slot id; we build
            // PlaytestMaterial mirrors keyed to (room, local_slot)
            // and register each unique texture asset on first use.
            let material_first = u16::try_from(materials.len()).unwrap_or(u16::MAX);
            let mut sorted_materials: Vec<&CookedWorldMaterial> = cooked.materials.iter().collect();
            sorted_materials.sort_by_key(|m| m.slot);

            for cooked_material in sorted_materials {
                let texture_id = match cooked_material.texture {
                    Some(id) => id,
                    None => {
                        report.error(format!(
                            "Room '{}' material slot {} has no texture (resource #{})",
                            room_node.name,
                            cooked_material.slot,
                            cooked_material.source.raw(),
                        ));
                        return (None, report);
                    }
                };
                let texture_resource = match find_resource(project, texture_id) {
                    Some(r) => r,
                    None => {
                        report.error(format!(
                            "Room '{}' material slot {} references missing texture resource #{}",
                            room_node.name,
                            cooked_material.slot,
                            texture_id.raw(),
                        ));
                        return (None, report);
                    }
                };
                let texture_asset_index =
                    if let Some(&existing) = texture_asset_for_resource.get(&texture_id) {
                        existing
                    } else {
                        let bytes = match load_texture_bytes(texture_resource, project_root) {
                            Ok(b) => b,
                            Err(msg) => {
                                report.error(format!(
                                    "Room '{}' material slot {}: {}",
                                    room_node.name, cooked_material.slot, msg,
                                ));
                                return (None, report);
                            }
                        };
                        // Room materials must be 4bpp (16-entry CLUT) --
                        // both the editor preview's material upload
                        // path and the runtime room material slots
                        // assume the 4bpp tpage layout. Loud failure
                        // here beats wrong-colour rendering at runtime.
                        if let Err(msg) = expect_room_material_depth(texture_resource, &bytes) {
                            report.error(format!(
                                "Room '{}' material slot {}: {}",
                                room_node.name, cooked_material.slot, msg,
                            ));
                            return (None, report);
                        }
                        let texture_index = texture_asset_for_resource.len();
                        let new_index = assets.len();
                        assets.push(PlaytestAsset {
                            kind: PlaytestAssetKind::Texture,
                            bytes,
                            filename: format!("texture_{:03}.psxt", texture_index),
                            source_label: texture_resource.name.clone(),
                        });
                        texture_asset_for_resource.insert(texture_id, new_index);
                        new_index
                    };

                materials.push(PlaytestMaterial {
                    room: room_index,
                    local_slot: cooked_material.slot,
                    texture_asset_index,
                    tint_rgb: cooked_material.tint,
                    face_sidedness: cooked_material.face_sidedness,
                });
            }
            let material_count =
                u16::try_from(materials.len() - material_first as usize).unwrap_or(u16::MAX);

            append_room_visibility(
                room_index,
                &cooked,
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
                                    &mut texture_asset_for_resource,
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
                                &mut texture_asset_for_resource,
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
            let far_vista_has_texture = far_vista_texture_asset_indices.iter().any(Option::is_some);
            let sky_texture_asset_index =
                cook_sky_panorama_texture_asset(resolved_sky, &mut sky_texture_assets, &mut assets);

            rooms.push(PlaytestRoom {
                name: chunk_room_name(&room_node.name, chunk_count, chunk.index),
                world_asset_index,
                origin_x: chunk_grid.origin[0],
                origin_z: chunk_grid.origin[1],
                sector_size: chunk_grid.sector_size,
                material_first,
                material_count,
                fog_rgb: chunk_grid.fog_color,
                fog_near: chunk_grid.fog_near,
                fog_far: chunk_grid.fog_far,
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
                    min_floor_clearance: resolved_camera.min_floor_clearance,
                },
                flags: if chunk_grid.fog_enabled {
                    psx_level::room_flags::FOG_ENABLED
                } else {
                    0
                },
            });
            room_bake_inputs.push(CookedRoomBakeInput {
                room_index,
                world_asset_index,
                world_origin: chunk.world_origin,
                cooked,
            });
        }
    }

    if rooms.is_empty() {
        report.error("every Room is empty — cook needs at least one populated room");
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
    let mut weapon_hitboxes: Vec<PlaytestWeaponHitbox> = Vec::new();
    let mut weapons: Vec<PlaytestWeapon> = Vec::new();
    let mut equipment: Vec<PlaytestEquipment> = Vec::new();
    let mut lights: Vec<PlaytestLight> = Vec::new();
    // ResourceId → index into `models` for instance dedup.
    let runtime_model_clips = collect_runtime_model_clip_requirements(project, scene);
    let mut model_for_resource: HashMap<ResourceId, u16> = HashMap::new();
    let mut model_clip_remaps: HashMap<ResourceId, Vec<Option<u16>>> = HashMap::new();
    let mut weapon_for_resource: HashMap<ResourceId, u16> = HashMap::new();
    let mut warned_unsupported: HashSet<&'static str> = HashSet::new();

    for node in scene.nodes() {
        if node.id == scene.root || matches!(node.kind, NodeKind::Room { .. }) {
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
                    "{} '{}' has no enclosing Room — dropped",
                    node.kind.label(),
                    node.name
                ));
            }
            continue;
        };
        let NodeKind::Room { grid } = &room_node.kind else {
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
                    "{} '{}' is outside cooked Room '{}' chunks — dropped",
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
                let is_player_controlled =
                    character_controller.is_some_and(|controller| controller.player);
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
                        let clip = component_animator(scene, node).and_then(|anim| anim.clip);
                        if !push_model_instance_for_resource(
                            project,
                            project_root,
                            node.name.as_str(),
                            model_resource_id,
                            clip,
                            room_index,
                            pos,
                            yaw,
                            renderer.visual_yaw,
                            renderer.visual_offset,
                            renderer.visual_scale_q8,
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
                    if controller.player {
                        player_spawns.push(PlayerSpawnCandidate {
                            node,
                            room_index,
                            position: pos,
                            character: controller.character,
                            controller_settings: Some(controller.settings),
                            renderer: component_model_renderer(scene, node),
                            animator: component_animator(scene, node),
                        });
                    } else if component_model_renderer(scene, node).is_none() {
                        let Some(character_id) = controller.character else {
                            report.warn(format!(
                                "Non-player Character Controller on '{}' has no Character — skipped",
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
                        *animation_clip,
                        room_index,
                        pos,
                        yaw,
                        0,
                        [0; 3],
                        crate::MODEL_SCALE_ONE_Q8,
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
                    &mut texture_asset_for_resource,
                    &mut assets,
                    &mut image_props,
                    &mut report,
                ) {
                    return (None, report);
                }
            }
            NodeKind::Trigger { .. } => {
                if warned_unsupported.insert("Trigger") {
                    report.warn("Trigger volumes are skipped in this pass");
                }
            }
            NodeKind::AudioSource { .. } => {
                if warned_unsupported.insert("AudioSource") {
                    report.warn("AudioSource nodes are skipped in this pass");
                }
            }
            NodeKind::Portal { .. } => {
                if warned_unsupported.insert("Portal") {
                    report.warn("Portal nodes are skipped (no streaming yet)");
                }
            }
            NodeKind::Node
            | NodeKind::Node3D
            | NodeKind::World { .. }
            | NodeKind::Room { .. }
            | NodeKind::ModelRenderer { .. }
            | NodeKind::Animator { .. }
            | NodeKind::Collider { .. }
            | NodeKind::Interactable { .. }
            | NodeKind::CharacterController { .. }
            | NodeKind::AiController { .. }
            | NodeKind::Combat { .. }
            | NodeKind::Equipment { .. } => {}
        }
    }

    let spawn = match player_spawns.len() {
        0 => {
            report.error(
                "playtest needs exactly one player source — mark a SpawnPoint as player or enable Player controlled on a Character Controller",
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
                "playtest needs exactly one player source, found {n}"
            ));
            None
        }
    };

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
            cook_player_character(
                project,
                project_root,
                spawn_node,
                resolved,
                renderer_model,
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
                &mut assets,
                &mut models,
                &mut model_clips,
                &mut model_clip_bounds,
                &mut model_frame_bounds,
                &mut model_sockets,
                &mut model_for_resource,
                &runtime_model_clips,
                &mut model_clip_remaps,
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

    if !report.is_ok() {
        return (None, report);
    }

    let lights = expand_lights_across_chunks(&room_bake_inputs, &lights);
    bake_static_surface_lights(&mut room_bake_inputs, &lights);
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

    (
        Some(PlaytestPackage {
            assets,
            rooms,
            chunks,
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
            weapon_hitboxes,
            weapons,
            equipment,
            lights,
            spawn,
            characters,
            player_controller,
            entities,
        }),
        report,
    )
}

fn active_far_vista_panel_count(
    texture_panels: &[Option<ResourceId>; FAR_VISTA_TEXTURE_PANEL_COUNT],
    segments: u8,
) -> usize {
    texture_panels
        .iter()
        .rposition(Option::is_some)
        .map(|index| index + 1)
        .unwrap_or(0)
        .min(segments as usize)
        .min(FAR_VISTA_TEXTURE_PANEL_COUNT)
}

fn cook_sky_panorama_texture_asset(
    sky: crate::ResolvedSkySettings,
    sky_texture_assets: &mut Vec<(crate::ResolvedSkySettings, usize)>,
    assets: &mut Vec<PlaytestAsset>,
) -> Option<usize> {
    if !sky.enabled {
        return None;
    }
    if let Some((_, existing)) = sky_texture_assets
        .iter()
        .find(|(existing_sky, _)| *existing_sky == sky)
    {
        return Some(*existing);
    }
    let bytes = crate::generate_sky_panorama_psxt(sky)?;
    let sky_index = sky_texture_assets.len();
    let asset_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("sky/sky_{sky_index:03}.psxt"),
        source_label: format!("Cooked Sky Panorama {sky_index}"),
    });
    sky_texture_assets.push((sky, asset_index));
    Some(asset_index)
}

fn cook_far_vista_texture_asset(
    project: &ProjectDocument,
    project_root: &Path,
    texture_id: ResourceId,
    context: &str,
    texture_asset_for_resource: &mut HashMap<ResourceId, usize>,
    assets: &mut Vec<PlaytestAsset>,
    report: &mut PlaytestValidationReport,
) -> Option<usize> {
    if let Some(existing) = texture_asset_for_resource.get(&texture_id).copied() {
        return Some(existing);
    }
    let Some(texture_resource) = find_resource(project, texture_id) else {
        report.warn(format!(
            "{context}: texture resource #{} is missing; using placeholder",
            texture_id.raw()
        ));
        return None;
    };
    let bytes = match load_texture_bytes(texture_resource, project_root) {
        Ok(bytes) => bytes,
        Err(msg) => {
            report.warn(format!("{context}: {msg}; using placeholder"));
            return None;
        }
    };
    if let Err(msg) = expect_room_material_depth(texture_resource, &bytes) {
        report.warn(format!("{context}: {msg}; using placeholder"));
        return None;
    }

    let texture_index = texture_asset_for_resource.len();
    let new_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("texture_{texture_index:03}.psxt"),
        source_label: texture_resource.name.clone(),
    });
    texture_asset_for_resource.insert(texture_id, new_index);
    Some(new_index)
}

fn collect_runtime_model_clip_requirements(
    project: &ProjectDocument,
    scene: &crate::Scene,
) -> HashMap<ResourceId, BTreeSet<u16>> {
    let mut out = HashMap::new();

    for resource in &project.resources {
        match &resource.data {
            ResourceData::Model(_) => {
                add_model_clip_requirement(project, &mut out, resource.id, None);
            }
            ResourceData::Character(character) => {
                add_character_clip_requirements(project, &mut out, character);
            }
            ResourceData::Weapon(weapon) => {
                if let Some(model) = weapon.model {
                    add_model_clip_requirement(project, &mut out, model, None);
                }
            }
            _ => {}
        }
    }

    for node in scene.nodes() {
        match &node.kind {
            NodeKind::Entity => {
                if let Some(model) = component_model_renderer(scene, node).and_then(|r| r.model) {
                    if let Some(animator) = component_animator(scene, node) {
                        add_model_clip_requirement(project, &mut out, model, animator.clip);
                        for binding in animator.action_clips {
                            add_model_clip_requirement(
                                project,
                                &mut out,
                                model,
                                Some(binding.clip),
                            );
                        }
                    } else {
                        add_model_clip_requirement(project, &mut out, model, None);
                    }
                }
                if let Some(controller) = component_character_controller(scene, node) {
                    if let Some(character_id) = controller.character {
                        if let Some(ResourceData::Character(character)) =
                            project.resource(character_id).map(|r| &r.data)
                        {
                            add_character_clip_requirements(project, &mut out, character);
                        }
                    }
                }
                if let Some(equipment) = component_equipment(scene, node) {
                    if let Some(weapon_id) = equipment.weapon {
                        if let Some(ResourceData::Weapon(weapon)) =
                            project.resource(weapon_id).map(|r| &r.data)
                        {
                            if let Some(model) = weapon.model {
                                add_model_clip_requirement(project, &mut out, model, None);
                            }
                        }
                    }
                }
            }
            NodeKind::MeshInstance {
                mesh: Some(model),
                animation_clip,
                ..
            } => {
                if project
                    .resource(*model)
                    .is_some_and(|r| matches!(r.data, ResourceData::Model(_)))
                {
                    add_model_clip_requirement(project, &mut out, *model, *animation_clip);
                }
            }
            NodeKind::SpawnPoint {
                character: Some(character_id),
                ..
            }
            | NodeKind::CharacterController {
                character: Some(character_id),
                ..
            } => {
                if let Some(ResourceData::Character(character)) =
                    project.resource(*character_id).map(|r| &r.data)
                {
                    add_character_clip_requirements(project, &mut out, character);
                }
            }
            _ => {}
        }
    }

    out
}

fn add_character_clip_requirements(
    project: &ProjectDocument,
    out: &mut HashMap<ResourceId, BTreeSet<u16>>,
    character: &crate::CharacterResource,
) {
    let Some(model) = character.model else {
        return;
    };
    add_model_clip_requirement(project, out, model, None);

    for legacy in [
        character.idle_clip,
        character.walk_clip,
        character.run_clip,
        character.turn_clip,
        character.roll_clip,
        character.backstep_clip,
    ]
    .into_iter()
    .flatten()
    {
        add_model_clip_requirement(project, out, model, Some(legacy));
    }
    for binding in &character.action_clips {
        add_model_clip_requirement(project, out, model, Some(binding.clip));
    }

    let Some(set) = character.animation_set.and_then(|id| {
        project
            .resource(id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationSet(set) => Some(set),
                _ => None,
            })
    }) else {
        return;
    };

    for action in CharacterAnimationAction::ALL {
        if let Some(animation_id) = animation_set_action_clip(project, set, action) {
            if let Some(index) = project.resolved_model_animation_index(model, animation_id) {
                add_model_clip_requirement(project, out, model, Some(index));
            }
        }
    }
}

fn animation_set_action_clip(
    project: &ProjectDocument,
    set: &crate::AnimationSetResource,
    action: CharacterAnimationAction,
) -> Option<ResourceId> {
    if let Some(id) = set.action_clip(action) {
        return Some(id);
    }
    set.clips.iter().copied().find(|id| {
        project
            .resource(*id)
            .and_then(|resource| match &resource.data {
                ResourceData::AnimationClip(clip) => {
                    let role_matches = match action {
                        CharacterAnimationAction::HeavyAttack
                        | CharacterAnimationAction::ComboAttack
                        | CharacterAnimationAction::Block => false,
                        _ => action.role_hint().is_some_and(|role| {
                            clip.role == role
                                || AnimationRole::guess_from_name(&resource.name) == role
                        }),
                    };
                    let action_matches =
                        CharacterAnimationAction::guess_from_name(&resource.name) == Some(action);
                    Some(role_matches || action_matches)
                }
                _ => None,
            })
            .unwrap_or(false)
    })
}

fn character_action_flags_for(
    action: CharacterAnimationAction,
    options: Option<crate::CharacterActionOptions>,
) -> u8 {
    let mut flags = 0;
    if options
        .map(|options| options.looping)
        .unwrap_or_else(|| action.loops_by_default())
    {
        flags |= character_action_flags::LOOPING;
    }
    if let Some(options) = options {
        flags |= character_action_flags::IN_PLACE_OVERRIDE;
        if options.in_place {
            flags |= character_action_flags::IN_PLACE;
        }
    }
    flags
}

fn add_model_clip_requirement(
    project: &ProjectDocument,
    out: &mut HashMap<ResourceId, BTreeSet<u16>>,
    model: ResourceId,
    clip: Option<u16>,
) {
    let resolved_len = project.resolved_model_animation_clips(model).len();
    if resolved_len == 0 {
        return;
    }
    let index = clip
        .or_else(|| {
            project
                .resource(model)
                .and_then(|resource| match &resource.data {
                    ResourceData::Model(model) => model.default_clip,
                    _ => None,
                })
        })
        .unwrap_or(0);
    if (index as usize) < resolved_len {
        out.entry(model).or_default().insert(index);
    }
}

fn runtime_model_clip_indices(
    resolved_len: usize,
    required: Option<&BTreeSet<u16>>,
    default_clip: u16,
) -> Vec<u16> {
    let mut selected = BTreeSet::new();
    if (default_clip as usize) < resolved_len {
        selected.insert(default_clip);
    }
    if let Some(required) = required {
        for index in required {
            if (*index as usize) < resolved_len {
                selected.insert(*index);
            }
        }
    }
    if selected.is_empty() && resolved_len > 0 {
        selected.insert(0);
    }
    selected.into_iter().collect()
}

fn remap_runtime_model_clip(
    remaps: &HashMap<ResourceId, Vec<Option<u16>>>,
    model: ResourceId,
    authored_index: u16,
) -> Option<u16> {
    remaps
        .get(&model)
        .and_then(|model_remap| model_remap.get(authored_index as usize))
        .copied()
        .flatten()
}

/// Cook one Character resource into a [`PlaytestCharacter`],
/// registering its backing model on first sight (deduped against
/// MeshInstance placements). Validates clip indices land inside
/// the resolved model's clip slice; the runtime trusts the
/// contract.
#[allow(clippy::too_many_arguments)]
fn cook_player_character(
    project: &ProjectDocument,
    project_root: &Path,
    spawn_node: &SceneNode,
    character_id: Option<ResourceId>,
    model_override: Option<ResourceId>,
    visual_offset: [i16; 3],
    visual_yaw: i16,
    visual_scale_q8: u16,
    action_overrides: &[crate::CharacterActionClip],
    controller_settings: Option<CharacterControllerSettings>,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut std::collections::HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    characters: &mut Vec<PlaytestCharacter>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    let default_character = crate::CharacterResource::defaults();
    let (character, character_name) = match character_id {
        Some(character_id) => {
            let resource = match project.resource(character_id) {
                Some(r) => r,
                None => {
                    report.error(format!(
                        "Player Spawn '{}' references Character #{} which doesn't exist",
                        spawn_node.name,
                        character_id.raw()
                    ));
                    return None;
                }
            };
            match &resource.data {
                ResourceData::Character(c) => (c, resource.name.as_str()),
                _ => {
                    report.error(format!(
                        "Player Spawn '{}' references resource '{}' which is not a Character",
                        spawn_node.name, resource.name
                    ));
                    return None;
                }
            }
        }
        None => (&default_character, spawn_node.name.as_str()),
    };
    let settings = controller_settings
        .unwrap_or_else(|| CharacterControllerSettings::from_character(character));

    let model_resource_id = match model_override.or(character.model) {
        Some(id) => id,
        None => {
            report.error(format!(
                "Character '{}' has no Model assigned -- add a Model Renderer or set a profile Model",
                character_name
            ));
            return None;
        }
    };
    let model_index = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    )?;
    let model = &models[model_index as usize];

    let model_skeleton =
        project
            .resource(model_resource_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => model.skeleton,
                _ => None,
            });
    let animation_set = character.animation_set.and_then(|id| {
        let resource = project.resource(id)?;
        match &resource.data {
            ResourceData::AnimationSet(set) => Some((id, resource.name.as_str(), set)),
            _ => None,
        }
    });
    if let Some((_, set_name, set)) = animation_set {
        if set.skeleton.is_some() && model_skeleton.is_some() && set.skeleton != model_skeleton {
            report.error(format!(
                "Character '{}' clip role map '{}' targets a different skeleton than its model",
                character_name, set_name
            ));
            return None;
        }
    }

    let resolve_action = |action: CharacterAnimationAction,
                          required: bool,
                          project: &ProjectDocument,
                          report: &mut PlaytestValidationReport|
     -> Option<(u16, u8)> {
        let action_label = action.label().to_ascii_lowercase();
        if let Some(binding) = action_overrides
            .iter()
            .find(|binding| binding.action == action)
        {
            let idx = binding.clip;
            return match remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx) {
                Some(local) => Some((local, character_action_flags_for(action, binding.options))),
                None => {
                    report.error(format!(
                        "Animator on '{}' maps {action_label} to clip {idx}, but that clip was not packaged for runtime",
                        spawn_node.name
                    ));
                    None
                }
            };
        }

        if let Some((_, set_name, set)) = animation_set {
            if let Some(animation_id) = animation_set_action_clip(project, set, action) {
                let options = set
                    .action_binding(action)
                    .filter(|binding| binding.clip == animation_id)
                    .and_then(|binding| binding.options);
                match project.resolved_model_animation_index(model_resource_id, animation_id) {
                    Some(index) => {
                        if let Some(local) =
                            remap_runtime_model_clip(model_clip_remaps, model_resource_id, index)
                        {
                            return Some((local, character_action_flags_for(action, options)));
                        }
                        report.error(format!(
                            "Character '{}' {action_label} clip resolves to {index}, but that clip was not packaged for runtime",
                            character_name
                        ));
                        return None;
                    }
                    None => {
                        report.error(format!(
                            "Character '{}' action map '{}' {action_label} clip is not compatible with model '{}'",
                            character_name, set_name, model.name
                        ));
                        return None;
                    }
                }
            }
        }

        let character_binding = character.action_binding(action);
        match character_binding
            .map(|binding| binding.clip)
            .or_else(|| character.action_clip(action))
        {
            Some(idx) => {
                match remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx) {
                    Some(local) => Some((
                        local,
                        character_action_flags_for(
                            action,
                            character_binding.and_then(|binding| binding.options),
                        ),
                    )),
                    None => {
                        report.error(format!(
                            "Character '{}' {action_label} clip {idx} was not packaged for runtime",
                            character_name
                        ));
                        None
                    }
                }
            }
            None if required => {
                report.error(format!(
                    "Character '{}' has no {action_label} clip assigned",
                    character_name
                ));
                None
            }
            None => Some((
                CHARACTER_CLIP_NONE,
                character_action_flags_for(action, None),
            )),
        }
    };

    let mut action_clips = [CHARACTER_CLIP_NONE; PLAYTEST_CHARACTER_ACTION_COUNT];
    let mut action_flags = [0u8; PLAYTEST_CHARACTER_ACTION_COUNT];
    for action in CharacterAnimationAction::ALL {
        let (clip, flags) = resolve_action(action, action.required_for_player(), project, report)?;
        action_clips[action.to_index()] = clip;
        action_flags[action.to_index()] = flags;
    }

    if settings.radius == 0 {
        report.error(format!("Character '{character_name}' radius must be > 0"));
        return None;
    }
    if settings.height == 0 {
        report.error(format!("Character '{character_name}' height must be > 0"));
        return None;
    }
    if settings.walk_speed <= 0 || settings.run_speed <= 0 {
        report.error(format!(
            "Character Controller for '{}' walk/run speeds must be > 0",
            character_name
        ));
        return None;
    }
    if settings.turn_speed_degrees_per_second == 0 {
        report.error(format!(
            "Character Controller for '{}' turn_speed must be > 0",
            character_name
        ));
        return None;
    }
    if settings.stamina_max_q12 <= 0 {
        report.error(format!(
            "Character Controller for '{}' stamina_max must be > 0",
            character_name
        ));
        return None;
    }
    if settings.sprint_min_q12 < 0
        || settings.sprint_drain_q12 < 0
        || settings.stamina_recover_q12 < 0
        || settings.roll_cost_q12 < 0
        || settings.backstep_cost_q12 < 0
    {
        report.error(format!(
            "Character Controller for '{}' stamina costs and recovery must be >= 0",
            character_name
        ));
        return None;
    }
    if settings.roll_speed <= 0 || settings.backstep_speed <= 0 {
        report.error(format!(
            "Character Controller for '{}' evade speeds must be > 0",
            character_name
        ));
        return None;
    }
    if settings.roll_active_frames == 0 || settings.backstep_active_frames == 0 {
        report.error(format!(
            "Character Controller for '{}' evade active frames must be > 0",
            character_name
        ));
        return None;
    }
    if character.camera_distance <= 0 {
        report.error(format!(
            "Character '{}' camera_distance must be > 0",
            character_name
        ));
        return None;
    }
    if character.camera_height < 0 || character.camera_target_height < 0 {
        report.error(format!(
            "Character '{}' camera offsets must be >= 0",
            character_name
        ));
        return None;
    }

    if action_clips[CharacterAnimationAction::Run.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no run clip -- runtime will fall back to walk for run input",
        ));
    }
    if action_clips[CharacterAnimationAction::Turn.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!("Character '{character_name}' has no turn clip"));
    }
    if action_clips[CharacterAnimationAction::Roll.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no roll clip -- runtime will fall back to run/walk",
        ));
    }
    if action_clips[CharacterAnimationAction::Backstep.to_index()] == CHARACTER_CLIP_NONE {
        report.warn(format!(
            "Character '{character_name}' has no backstep clip -- runtime will fall back to walk",
        ));
    }

    let character_index = u16::try_from(characters.len()).unwrap_or(u16::MAX);
    characters.push(PlaytestCharacter {
        source_resource: character_id.unwrap_or(model_resource_id),
        model: model_index,
        action_clips,
        action_flags,
        visual_offset,
        visual_yaw,
        visual_scale_q8,
        radius: settings.radius,
        height: settings.height,
        walk_speed: settings.walk_speed,
        run_speed: settings.run_speed,
        turn_speed_degrees_per_second: settings.turn_speed_degrees_per_second,
        stamina_max_q12: settings.stamina_max_q12,
        sprint_min_q12: settings.sprint_min_q12,
        sprint_drain_q12: settings.sprint_drain_q12,
        stamina_recover_q12: settings.stamina_recover_q12,
        roll_cost_q12: settings.roll_cost_q12,
        roll_speed: settings.roll_speed,
        roll_active_frames: settings.roll_active_frames,
        roll_recovery_frames: settings.roll_recovery_frames,
        roll_invulnerable_frames: settings.roll_invulnerable_frames,
        backstep_cost_q12: settings.backstep_cost_q12,
        backstep_speed: settings.backstep_speed,
        backstep_active_frames: settings.backstep_active_frames,
        backstep_recovery_frames: settings.backstep_recovery_frames,
        backstep_invulnerable_frames: settings.backstep_invulnerable_frames,
        camera_distance: character.camera_distance,
        camera_height: character.camera_height,
        camera_target_height: character.camera_target_height,
    });
    Some(character_index)
}

/// Register a `ResourceData::Model` into the playtest package
/// on first sight; reuse the cached index otherwise. On
/// success, returns the model's index in `models`.
///
/// Failures (missing files, invalid blobs, joint-count
/// mismatches) push to `report.errors` and return `None`; the
/// caller turns that into a hard cook failure.
#[allow(clippy::too_many_arguments)]
fn register_model_for_instance(
    project: &ProjectDocument,
    project_root: &Path,
    model_resource_id: ResourceId,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut std::collections::HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    if let Some(&existing) = model_for_resource.get(&model_resource_id) {
        return Some(existing);
    }
    let resource = project.resource(model_resource_id)?;
    let ResourceData::Model(model) = &resource.data else {
        report.error(format!(
            "MeshInstance references resource #{} which is not a Model",
            model_resource_id.raw()
        ));
        return None;
    };

    // Runtime contract: a placed model must carry an atlas
    // (the runtime renders textured) and at least one clip
    // (the runtime renders animated). Bind-pose / untextured
    // rendering would need engine-side work the current pass
    // doesn't ship -- fail loud at cook so the editor surfaces
    // it rather than silently dropping the instance at runtime.
    if model.texture_path.is_none() {
        report.error(format!(
            "Model '{}' has no atlas; the runtime can't render untextured models in this pass",
            resource.name
        ));
        return None;
    }
    let resolved_clips = project.resolved_model_animation_clips(model_resource_id);
    if resolved_clips.is_empty() {
        report.error(format!(
            "Model '{}' has no animation clips; the runtime requires at least one clip",
            resource.name
        ));
        return None;
    }
    if model.collision_radius == 0 {
        report.error(format!(
            "Model '{}' has zero collision radius; actor blockers must be at least 1 engine unit",
            resource.name
        ));
        return None;
    }

    let model_index = u16::try_from(models.len()).unwrap_or(u16::MAX);
    let safe = sanitise_model_dirname(&resource.name);
    let folder = format!("{MODELS_DIRNAME}/model_{:03}_{safe}", model_index);

    // Mesh asset.
    let mesh_path = resolve_path(&model.model_path, project_root);
    let mesh_bytes = match std::fs::read(&mesh_path) {
        Ok(b) => b,
        Err(e) => {
            report.error(format!(
                "Model '{}' mesh {}: {e}",
                resource.name,
                mesh_path.display()
            ));
            return None;
        }
    };
    let parsed_model = match psx_asset::Model::from_bytes(&mesh_bytes) {
        Ok(m) => m,
        Err(e) => {
            report.error(format!(
                "Model '{}' mesh parse failed: {e:?}",
                resource.name
            ));
            return None;
        }
    };
    let model_joint_count = parsed_model.joint_count();
    let mesh_asset_index = assets.len();
    assets.push(PlaytestAsset {
        kind: PlaytestAssetKind::ModelMesh,
        bytes: mesh_bytes.clone(),
        filename: format!("{folder}/mesh.psxmdl"),
        source_label: resource.name.clone(),
    });

    // Atlas asset (optional).
    let texture_asset_index = if let Some(tex_path) = &model.texture_path {
        let abs = resolve_path(tex_path, project_root);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                report.error(format!(
                    "Model '{}' atlas {}: {e}",
                    resource.name,
                    abs.display()
                ));
                return None;
            }
        };
        let parsed_atlas = match psx_asset::Texture::from_bytes(&bytes) {
            Ok(t) => t,
            Err(e) => {
                report.error(format!(
                    "Model '{}' atlas parse failed: {e:?}",
                    resource.name
                ));
                return None;
            }
        };
        // Model atlases must be 8bpp (256-entry CLUT) -- the
        // runtime model atlas region uses an 8bpp tpage and a
        // 256-entry CLUT row per atlas. Other depths render with
        // wrong colours, so reject loud at cook time.
        if parsed_atlas.clut_entries() != 256 {
            report.error(format!(
                "Model '{}' atlas must be 8bpp (256-entry CLUT); found {} entries",
                resource.name,
                parsed_atlas.clut_entries(),
            ));
            return None;
        }
        let idx = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: format!("{folder}/atlas.psxt"),
            source_label: format!("{} atlas", resource.name),
        });
        Some(idx)
    } else {
        None
    };

    let authored_default_clip = match model.default_clip {
        Some(idx) if (idx as usize) < resolved_clips.len() => idx,
        Some(idx) => {
            report.error(format!(
                "Model '{}' default_clip {idx} is out of range ({} clips)",
                resource.name,
                resolved_clips.len()
            ));
            return None;
        }
        None => 0,
    };
    let selected_clip_indices = runtime_model_clip_indices(
        resolved_clips.len(),
        runtime_model_clips.get(&model_resource_id),
        authored_default_clip,
    );

    // Clip assets -- one .psxanim per runtime-needed clip. The
    // editor may keep a much larger animation library on the model;
    // the PS1 package only needs defaults, placed overrides, and
    // character role clips.
    let clip_first = u16::try_from(model_clips.len()).unwrap_or(u16::MAX);
    let mut clip_remap = vec![None; resolved_clips.len()];
    for (local_i, resolved_i) in selected_clip_indices.iter().copied().enumerate() {
        let Some(clip) = resolved_clips.get(resolved_i as usize) else {
            continue;
        };
        let abs = resolve_path(&clip.psxanim_path, project_root);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                report.error(format!(
                    "Model '{}' clip '{}' {}: {e}",
                    resource.name,
                    clip.name,
                    abs.display()
                ));
                return None;
            }
        };
        let parsed_anim = match psx_asset::Animation::from_bytes(&bytes) {
            Ok(a) => a,
            Err(e) => {
                report.error(format!(
                    "Model '{}' clip '{}' parse failed: {e:?}",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        if parsed_anim.joint_count() != model_joint_count {
            report.error(format!(
                "Model '{}' clip '{}': animation has {} joints, model has {}",
                resource.name,
                clip.name,
                parsed_anim.joint_count(),
                model_joint_count
            ));
            return None;
        }
        let frame_first = match u16::try_from(model_frame_bounds.len()) {
            Ok(index) => index,
            Err(_) => {
                report.error(format!(
                    "Model '{}' clip '{}': too many baked model-bound frames",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        let clip_index = match u16::try_from(model_clips.len()) {
            Ok(index) => index,
            Err(_) => {
                report.error(format!(
                    "Model '{}' has too many animation clips for the playtest manifest",
                    resource.name
                ));
                return None;
            }
        };
        let baked_bounds = bake_model_clip_frame_bounds(&parsed_model, &parsed_anim);
        let frame_count = match u16::try_from(baked_bounds.len()) {
            Ok(count) => count,
            Err(_) => {
                report.error(format!(
                    "Model '{}' clip '{}': too many baked model-bound frames",
                    resource.name, clip.name
                ));
                return None;
            }
        };
        let floor_y = baked_bounds
            .first()
            .map(|bounds| bounds.floor_y)
            .unwrap_or(0);
        model_frame_bounds.extend(baked_bounds);
        model_clip_bounds.push(PlaytestModelClipBounds {
            model: model_index,
            clip: clip_index,
            first_frame: frame_first,
            frame_count,
            floor_y,
            pose_offset: clip.calibration.offset,
            flags: if clip.calibration.in_place {
                model_clip_flags::IN_PLACE
            } else {
                0
            },
        });
        let asset_index = assets.len();
        let safe_clip = sanitise_model_dirname(&clip.name);
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::ModelAnimation,
            bytes,
            filename: format!("{folder}/clip_{:02}_{safe_clip}.psxanim", local_i),
            source_label: format!("{} / {}", resource.name, clip.name),
        });
        clip_remap[resolved_i as usize] = u16::try_from(local_i).ok();
        model_clips.push(PlaytestModelClip {
            model: model_index,
            name: clip.name.clone(),
            animation_asset_index: asset_index,
        });
    }
    let clip_count = u16::try_from(model_clips.len() - clip_first as usize).unwrap_or(u16::MAX);

    // Resolve the model's default clip. Validation rules:
    //   - explicit `model.default_clip = Some(idx)` MUST be in
    //     range; out-of-range is a hard cook error so the user
    //     fixes the resource rather than a runtime instance
    //     silently pointing at clip 0.
    //   - `None` falls back to clip 0. Cooker has already
    //     refused empty-clip placed models, so `clip_count >= 1`.
    let Some(default_clip) = clip_remap
        .get(authored_default_clip as usize)
        .copied()
        .flatten()
    else {
        report.error(format!(
            "Model '{}' default_clip {authored_default_clip} was not packaged for runtime",
            resource.name
        ));
        return None;
    };

    let socket_first = u16::try_from(model_sockets.len()).unwrap_or(u16::MAX);
    let mut seen_sockets: Vec<&str> = Vec::new();
    for socket in &model.attachments {
        if socket.name.trim().is_empty() {
            report.error(format!(
                "Model '{}' has an attachment socket with no name",
                resource.name
            ));
            return None;
        }
        if socket.joint >= model_joint_count {
            report.error(format!(
                "Model '{}' socket '{}' references joint {}, but the model has {} joints",
                resource.name, socket.name, socket.joint, model_joint_count
            ));
            return None;
        }
        if seen_sockets
            .iter()
            .any(|name| *name == socket.name.as_str())
        {
            report.error(format!(
                "Model '{}' has duplicate attachment socket '{}'",
                resource.name, socket.name
            ));
            return None;
        }
        seen_sockets.push(socket.name.as_str());
        model_sockets.push(PlaytestModelSocket {
            model: model_index,
            name: socket.name.clone(),
            joint: socket.joint,
            translation: socket.translation,
            rotation_q12: socket.rotation_q12,
        });
    }
    let socket_count =
        u16::try_from(model_sockets.len() - socket_first as usize).unwrap_or(u16::MAX);

    models.push(PlaytestModel {
        name: resource.name.clone(),
        source_resource: model_resource_id,
        mesh_asset_index,
        texture_asset_index,
        clip_first,
        clip_count,
        default_clip,
        socket_first,
        socket_count,
        world_height: model.world_height,
        collision_radius: model.collision_radius,
    });
    model_for_resource.insert(model_resource_id, model_index);
    model_clip_remaps.insert(model_resource_id, clip_remap);
    Some(model_index)
}

const MODEL_FRAME_BOUNDS_PAD_UNITS: i32 = 64;

#[derive(Clone, Copy)]
struct ModelBoundsJointTransform {
    matrix: [[i16; 3]; 3],
    translation: [i32; 3],
}

fn bake_model_clip_frame_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
) -> Vec<PlaytestModelFrameBounds> {
    let frame_count = animation.frame_count();
    let cycle_frames = frame_count.saturating_sub(1).max(1);
    let mut out = Vec::with_capacity(cycle_frames as usize);
    let mut frame = 0u16;
    while frame < cycle_frames {
        let next = if cycle_frames <= 1 || frame + 1 >= cycle_frames {
            0
        } else {
            frame + 1
        };
        out.push(bake_model_frame_pair_bounds(model, animation, frame, next));
        frame += 1;
    }
    out
}

fn bake_model_frame_pair_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
    a: u16,
    b: u16,
) -> PlaytestModelFrameBounds {
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    let mut floor_y = i32::MIN;
    accumulate_model_frame_bounds(model, animation, a, &mut min, &mut max, &mut floor_y);
    if b != a {
        accumulate_model_frame_bounds(model, animation, b, &mut min, &mut max, &mut floor_y);
    }

    if min[0] == i32::MAX {
        return PlaytestModelFrameBounds {
            center: [0, 0, 0],
            radius: MODEL_FRAME_BOUNDS_PAD_UNITS,
            floor_y: 0,
        };
    }

    let center = [
        average_i32(min[0], max[0]),
        average_i32(min[1], max[1]),
        average_i32(min[2], max[2]),
    ];
    let radius = aabb_radius(min, max).saturating_add(MODEL_FRAME_BOUNDS_PAD_UNITS);
    PlaytestModelFrameBounds {
        center,
        radius,
        floor_y,
    }
}

fn accumulate_model_frame_bounds(
    model: &psx_asset::Model<'_>,
    animation: &psx_asset::Animation<'_>,
    frame: u16,
    min: &mut [i32; 3],
    max: &mut [i32; 3],
    floor_y: &mut i32,
) {
    let joint_count = model.joint_count().min(animation.joint_count());
    let mut joints = Vec::with_capacity(joint_count as usize);
    let mut raw_joints = Vec::with_capacity(joint_count as usize);
    let mut joint = 0u16;
    while joint < joint_count {
        if let Some(pose) = animation.pose(frame, joint) {
            joints.push(model_bounds_joint_transform(
                pose,
                model.local_to_world_q12(),
            ));
            raw_joints.push(model_bounds_joint_transform(pose, 0x1000));
        }
        joint += 1;
    }

    let mut part_index = 0u16;
    while part_index < model.part_count() {
        let Some(part) = model.part(part_index) else {
            part_index += 1;
            continue;
        };
        let primary_joint = part.joint_index() as usize;
        let Some(primary) = joints.get(primary_joint).copied() else {
            part_index += 1;
            continue;
        };
        let Some(raw_primary) = raw_joints.get(primary_joint).copied() else {
            part_index += 1;
            continue;
        };
        let first = part.first_vertex();
        let end = first
            .saturating_add(part.vertex_count())
            .min(model.vertex_count());
        let mut vertex_index = first;
        while vertex_index < end {
            if let Some(vertex) = model.vertex(vertex_index) {
                let mut point = transform_model_bounds_vertex(primary, vertex);
                let mut raw_point = transform_model_bounds_vertex(raw_primary, vertex);
                if vertex.is_blend() {
                    if let Some(secondary) = joints.get(vertex.joint1 as usize).copied() {
                        let secondary_point = transform_model_bounds_vertex(secondary, vertex);
                        point = lerp_bounds_point(point, secondary_point, vertex.blend);
                    }
                    if let Some(raw_secondary) = raw_joints.get(vertex.joint1 as usize).copied() {
                        let raw_secondary_point =
                            transform_model_bounds_vertex(raw_secondary, vertex);
                        raw_point = lerp_bounds_point(raw_point, raw_secondary_point, vertex.blend);
                    }
                }
                include_bounds_point(point, min, max);
                *floor_y = (*floor_y).max(raw_point[1]);
            }
            vertex_index += 1;
        }
        part_index += 1;
    }
}

fn model_bounds_joint_transform(
    pose: psx_asset::JointPose,
    local_to_world_q12: u16,
) -> ModelBoundsJointTransform {
    let mut matrix = [[0i16; 3]; 3];
    let mut row = 0usize;
    while row < 3 {
        let mut col = 0usize;
        while col < 3 {
            matrix[row][col] =
                clamp_i16_i64(((pose.matrix[col][row] as i64) * (local_to_world_q12 as i64)) >> 12);
            col += 1;
        }
        row += 1;
    }
    ModelBoundsJointTransform {
        matrix,
        translation: [
            apply_q12_i32(pose.translation.x, local_to_world_q12),
            apply_q12_i32(pose.translation.y, local_to_world_q12),
            apply_q12_i32(pose.translation.z, local_to_world_q12),
        ],
    }
}

fn transform_model_bounds_vertex(
    transform: ModelBoundsJointTransform,
    vertex: psx_asset::ModelVertex,
) -> [i32; 3] {
    let vx = vertex.position.x as i64;
    let vy = vertex.position.y as i64;
    let vz = vertex.position.z as i64;
    let row = |row: [i16; 3], translation: i32| -> i32 {
        let value = ((row[0] as i64) * vx + (row[1] as i64) * vy + (row[2] as i64) * vz) >> 12;
        clamp_i32_i64(value.saturating_add(translation as i64))
    };
    [
        row(transform.matrix[0], transform.translation[0]),
        row(transform.matrix[1], transform.translation[1]),
        row(transform.matrix[2], transform.translation[2]),
    ]
}

fn lerp_bounds_point(a: [i32; 3], b: [i32; 3], t: u8) -> [i32; 3] {
    let t = t as i64;
    let inv = 256 - t;
    [
        clamp_i32_i64(((a[0] as i64) * inv + (b[0] as i64) * t) >> 8),
        clamp_i32_i64(((a[1] as i64) * inv + (b[1] as i64) * t) >> 8),
        clamp_i32_i64(((a[2] as i64) * inv + (b[2] as i64) * t) >> 8),
    ]
}

fn include_bounds_point(point: [i32; 3], min: &mut [i32; 3], max: &mut [i32; 3]) {
    let mut axis = 0usize;
    while axis < 3 {
        min[axis] = min[axis].min(point[axis]);
        max[axis] = max[axis].max(point[axis]);
        axis += 1;
    }
}

fn average_i32(a: i32, b: i32) -> i32 {
    (((a as i64) + (b as i64)) / 2).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn aabb_radius(min: [i32; 3], max: [i32; 3]) -> i32 {
    let half_x = half_extent_u128(min[0], max[0]);
    let half_y = half_extent_u128(min[1], max[1]);
    let half_z = half_extent_u128(min[2], max[2]);
    let square = half_x
        .saturating_mul(half_x)
        .saturating_add(half_y.saturating_mul(half_y))
        .saturating_add(half_z.saturating_mul(half_z));
    ceil_sqrt_u128(square).min(i32::MAX as u128) as i32
}

fn half_extent_u128(min: i32, max: i32) -> u128 {
    let extent = (max as i64).saturating_sub(min as i64).unsigned_abs() as u128;
    (extent + 1) / 2
}

fn ceil_sqrt_u128(value: u128) -> u128 {
    if value <= 1 {
        return value;
    }
    let mut hi = 1u128;
    while hi.saturating_mul(hi) < value {
        hi = hi.saturating_mul(2);
    }
    let mut lo = hi / 2;
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if mid.saturating_mul(mid) >= value {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

fn apply_q12_i32(value: i32, q12: u16) -> i32 {
    clamp_i32_i64(((value as i64) * (q12 as i64)) >> 12)
}

fn clamp_i16_i64(value: i64) -> i16 {
    value.clamp(i16::MIN as i64, i16::MAX as i64) as i16
}

fn clamp_i32_i64(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

fn push_model_instance_for_resource(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    model_resource_id: ResourceId,
    clip_override: Option<u16>,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    visual_yaw: i16,
    visual_offset: [i16; 3],
    visual_scale_q8: u16,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_instances: &mut Vec<PlaytestModelInstance>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(model_index) = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    let clip = match clip_override {
        Some(idx) => {
            let authored_clip_count = project
                .resolved_model_animation_clips(model_resource_id)
                .len();
            if idx as usize >= authored_clip_count {
                report.error(format!(
                    "Model instance '{node_name}' clip override {idx} out of range (model has {authored_clip_count})"
                ));
                return false;
            }
            let Some(local) = remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx)
            else {
                report.error(format!(
                    "Model instance '{node_name}' clip override {idx} was not packaged for runtime"
                ));
                return false;
            };
            local
        }
        None => MODEL_CLIP_INHERIT,
    };
    model_instances.push(PlaytestModelInstance {
        room: room_index,
        model: model_index,
        clip,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        visual_yaw,
        visual_offset,
        visual_scale_q8,
        flags: 0,
    });
    true
}

#[allow(clippy::too_many_arguments)]
fn push_character_controller_idle_instance(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    character_id: ResourceId,
    room_index: u16,
    pos: [i32; 3],
    yaw: i16,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_instances: &mut Vec<PlaytestModelInstance>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(resource) = project.resource(character_id) else {
        report.error(format!(
            "Non-player Character Controller '{node_name}' references Character #{} which doesn't exist",
            character_id.raw()
        ));
        return false;
    };
    let ResourceData::Character(character) = &resource.data else {
        report.error(format!(
            "Non-player Character Controller '{node_name}' references resource '{}' which is not a Character",
            resource.name
        ));
        return false;
    };
    let Some(model_resource_id) = character.model else {
        report.error(format!(
            "Character '{}' has no Model assigned — required for non-player Entity '{}'",
            resource.name, node_name
        ));
        return false;
    };
    let Some(model_index) = register_model_for_instance(
        project,
        project_root,
        model_resource_id,
        assets,
        models,
        model_clips,
        model_clip_bounds,
        model_frame_bounds,
        model_sockets,
        model_for_resource,
        runtime_model_clips,
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    let Some(clip) = character_idle_clip_for_model_instance(
        project,
        resource.name.as_str(),
        character,
        model_resource_id,
        &models[model_index as usize],
        model_clip_remaps,
        report,
    ) else {
        return false;
    };
    model_instances.push(PlaytestModelInstance {
        room: room_index,
        model: model_index,
        clip,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        yaw,
        visual_yaw: 0,
        visual_offset: [0; 3],
        visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
        flags: 0,
    });
    true
}

fn character_idle_clip_for_model_instance(
    project: &ProjectDocument,
    character_name: &str,
    character: &crate::CharacterResource,
    model_resource_id: ResourceId,
    model: &PlaytestModel,
    model_clip_remaps: &HashMap<ResourceId, Vec<Option<u16>>>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    let model_skeleton =
        project
            .resource(model_resource_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Model(model) => model.skeleton,
                _ => None,
            });
    let animation_set = character.animation_set.and_then(|id| {
        let resource = project.resource(id)?;
        match &resource.data {
            ResourceData::AnimationSet(set) => Some((resource.name.as_str(), set)),
            _ => None,
        }
    });
    if let Some((set_name, set)) = animation_set {
        if set.skeleton.is_some() && model_skeleton.is_some() && set.skeleton != model_skeleton {
            report.error(format!(
                "Character '{character_name}' clip role map '{set_name}' targets a different skeleton than its model"
            ));
            return None;
        }
        if let Some(animation_id) = set.role_clip(AnimationRole::Idle) {
            return match project.resolved_model_animation_index(model_resource_id, animation_id) {
                Some(index) => {
                    if let Some(local) =
                        remap_runtime_model_clip(model_clip_remaps, model_resource_id, index)
                    {
                        return Some(local);
                    }
                    report.error(format!(
                        "Character '{character_name}' idle clip resolves to {index}, but that clip was not packaged for runtime"
                    ));
                    None
                }
                None => {
                    report.error(format!(
                        "Character '{character_name}' clip role map '{set_name}' idle clip is not compatible with model '{}'",
                        model.name
                    ));
                    None
                }
            };
        }
    }

    match character.idle_clip {
        Some(idx) => {
            if let Some(local) = remap_runtime_model_clip(model_clip_remaps, model_resource_id, idx)
            {
                return Some(local);
            }
            report.error(format!(
                "Character '{character_name}' idle clip {idx} was not packaged for runtime"
            ));
            None
        }
        None => Some(model.default_clip),
    }
}

#[allow(clippy::too_many_arguments)]
fn register_weapon_for_equipment(
    project: &ProjectDocument,
    project_root: &Path,
    weapon_resource_id: ResourceId,
    assets: &mut Vec<PlaytestAsset>,
    models: &mut Vec<PlaytestModel>,
    model_clips: &mut Vec<PlaytestModelClip>,
    model_clip_bounds: &mut Vec<PlaytestModelClipBounds>,
    model_frame_bounds: &mut Vec<PlaytestModelFrameBounds>,
    model_sockets: &mut Vec<PlaytestModelSocket>,
    model_for_resource: &mut HashMap<ResourceId, u16>,
    runtime_model_clips: &HashMap<ResourceId, BTreeSet<u16>>,
    model_clip_remaps: &mut HashMap<ResourceId, Vec<Option<u16>>>,
    weapon_hitboxes: &mut Vec<PlaytestWeaponHitbox>,
    weapons: &mut Vec<PlaytestWeapon>,
    weapon_for_resource: &mut HashMap<ResourceId, u16>,
    report: &mut PlaytestValidationReport,
) -> Option<u16> {
    if let Some(&existing) = weapon_for_resource.get(&weapon_resource_id) {
        return Some(existing);
    }
    let Some(resource) = project.resource(weapon_resource_id) else {
        report.error(format!(
            "Equipment references missing Weapon resource #{}",
            weapon_resource_id.raw()
        ));
        return None;
    };
    let ResourceData::Weapon(weapon) = &resource.data else {
        report.error(format!(
            "Equipment references resource '{}' which is not a Weapon",
            resource.name
        ));
        return None;
    };

    let model = match weapon.model {
        Some(model_resource_id) => Some(register_model_for_instance(
            project,
            project_root,
            model_resource_id,
            assets,
            models,
            model_clips,
            model_clip_bounds,
            model_frame_bounds,
            model_sockets,
            model_for_resource,
            runtime_model_clips,
            model_clip_remaps,
            report,
        )?),
        None => None,
    };

    let hitbox_first = u16::try_from(weapon_hitboxes.len()).unwrap_or(u16::MAX);
    for hitbox in &weapon.hitboxes {
        weapon_hitboxes.push(PlaytestWeaponHitbox {
            name: hitbox.name.clone(),
            shape: playtest_weapon_shape(&hitbox.shape),
            active_start_frame: hitbox.active_start_frame,
            active_end_frame: hitbox.active_end_frame.max(hitbox.active_start_frame),
        });
    }
    let hitbox_count =
        u16::try_from(weapon_hitboxes.len() - hitbox_first as usize).unwrap_or(u16::MAX);
    let weapon_index = u16::try_from(weapons.len()).unwrap_or(u16::MAX);
    weapons.push(PlaytestWeapon {
        name: resource.name.clone(),
        source_resource: weapon_resource_id,
        model,
        default_character_socket: weapon.default_character_socket.clone(),
        grip_name: weapon.grip.name.clone(),
        grip_translation: weapon.grip.translation,
        grip_rotation_q12: weapon.grip.rotation_q12,
        hitbox_first,
        hitbox_count,
    });
    weapon_for_resource.insert(weapon_resource_id, weapon_index);
    Some(weapon_index)
}

fn playtest_weapon_shape(shape: &crate::WeaponHitShape) -> PlaytestWeaponHitShape {
    match shape {
        crate::WeaponHitShape::Box {
            center,
            half_extents,
        } => PlaytestWeaponHitShape::Box {
            center: *center,
            half_extents: *half_extents,
        },
        crate::WeaponHitShape::Capsule { start, end, radius } => PlaytestWeaponHitShape::Capsule {
            start: *start,
            end: *end,
            radius: *radius,
        },
    }
}

#[derive(Clone, Copy)]
struct ModelRendererComponent {
    model: Option<ResourceId>,
    visual_offset: [i16; 3],
    visual_yaw: i16,
    visual_scale_q8: u16,
}

#[derive(Clone, Copy)]
struct AnimatorComponent<'a> {
    clip: Option<u16>,
    action_clips: &'a [crate::CharacterActionClip],
}

#[derive(Clone, Copy)]
struct CharacterControllerComponent {
    character: Option<ResourceId>,
    settings: CharacterControllerSettings,
    player: bool,
}

struct EquipmentComponent<'a> {
    weapon: Option<ResourceId>,
    character_socket: &'a str,
    weapon_grip: &'a str,
}

fn component_model_renderer(
    scene: &crate::Scene,
    host: &SceneNode,
) -> Option<ModelRendererComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::ModelRenderer {
            model,
            material: _,
            visual_offset,
            visual_scale_q8,
        } => Some(ModelRendererComponent {
            model: *model,
            visual_offset: *visual_offset,
            visual_yaw: yaw_from_degrees(node.transform.rotation_degrees[1]),
            visual_scale_q8: *visual_scale_q8,
        }),
        _ => None,
    })
}

fn component_animator<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> Option<AnimatorComponent<'a>> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Animator {
            clip, action_clips, ..
        } => Some(AnimatorComponent {
            clip: *clip,
            action_clips,
        }),
        _ => None,
    })
}

fn component_character_controller(
    scene: &crate::Scene,
    host: &SceneNode,
) -> Option<CharacterControllerComponent> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::CharacterController {
            character,
            settings,
            player,
        } => Some(CharacterControllerComponent {
            character: *character,
            settings: *settings,
            player: *player,
        }),
        _ => None,
    })
}

fn component_equipment<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> Option<EquipmentComponent<'a>> {
    component_children(scene, host).find_map(|node| match &node.kind {
        NodeKind::Equipment {
            weapon,
            character_socket,
            weapon_grip,
        } => Some(EquipmentComponent {
            weapon: *weapon,
            character_socket,
            weapon_grip,
        }),
        _ => None,
    })
}

fn component_children<'a>(
    scene: &'a crate::Scene,
    host: &'a SceneNode,
) -> impl Iterator<Item = &'a SceneNode> + 'a {
    host.children
        .iter()
        .filter_map(|id| scene.node(*id))
        .filter(|node| node.kind.is_component())
}

#[allow(clippy::too_many_arguments)]
fn push_image_prop(
    project: &ProjectDocument,
    project_root: &Path,
    node_name: &str,
    room_index: u16,
    pos: [i32; 3],
    pitch: i16,
    yaw: i16,
    roll: i16,
    material: Option<ResourceId>,
    width: u16,
    height: u16,
    cylindrical_billboard: bool,
    texture_asset_for_resource: &mut HashMap<ResourceId, usize>,
    assets: &mut Vec<PlaytestAsset>,
    image_props: &mut Vec<PlaytestImageProp>,
    report: &mut PlaytestValidationReport,
) -> bool {
    let Some(material_id) = material else {
        report.warn(format!(
            "Image Prop '{node_name}' has no Material — skipped"
        ));
        return true;
    };
    let Some(material_resource) = project.resource(material_id) else {
        report.warn(format!(
            "Image Prop '{node_name}' references missing Material #{} — skipped",
            material_id.raw()
        ));
        return true;
    };
    let ResourceData::Material(material) = &material_resource.data else {
        report.warn(format!(
            "Image Prop '{node_name}' references '{}' but it is not a Material — skipped",
            material_resource.name
        ));
        return true;
    };
    let Some(texture_id) = material.texture else {
        report.warn(format!(
            "Image Prop '{node_name}' material '{}' has no Texture — skipped",
            material_resource.name
        ));
        return true;
    };
    let Some(texture_resource) = find_resource(project, texture_id) else {
        report.warn(format!(
            "Image Prop '{node_name}' material '{}' references missing Texture #{} — skipped",
            material_resource.name,
            texture_id.raw()
        ));
        return true;
    };
    let texture_asset_index = if let Some(&existing) = texture_asset_for_resource.get(&texture_id) {
        existing
    } else {
        let bytes = match load_texture_bytes(texture_resource, project_root) {
            Ok(bytes) => bytes,
            Err(msg) => {
                report.warn(format!("Image Prop '{node_name}': {msg} — skipped"));
                return true;
            }
        };
        if let Err(msg) = expect_room_material_depth(texture_resource, &bytes) {
            report.warn(format!("Image Prop '{node_name}': {msg} — skipped"));
            return true;
        }
        let texture_index = texture_asset_for_resource.len();
        let new_index = assets.len();
        assets.push(PlaytestAsset {
            kind: PlaytestAssetKind::Texture,
            bytes,
            filename: format!("texture_{texture_index:03}.psxt"),
            source_label: texture_resource.name.clone(),
        });
        texture_asset_for_resource.insert(texture_id, new_index);
        new_index
    };
    image_props.push(PlaytestImageProp {
        room: room_index,
        texture_asset_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        pitch,
        yaw,
        roll,
        width: width.max(1),
        height: height.max(1),
        tint_rgb: material.tint,
        flags: if cylindrical_billboard {
            image_prop_flags::CYLINDRICAL_BILLBOARD
        } else {
            0
        },
    });
    true
}

fn push_point_light(
    node_name: &str,
    grid: &crate::WorldGrid,
    room_index: u16,
    pos: [i32; 3],
    color: [u8; 3],
    intensity: f32,
    radius: f32,
    lights: &mut Vec<PlaytestLight>,
    report: &mut PlaytestValidationReport,
) -> bool {
    // Reject obviously broken lights at cook time -- radius 0
    // contributes nothing, negative intensity is meaningless.
    // Clamp the rest into the wire format's u16 ranges.
    if radius <= 0.0 {
        report.error(format!(
            "Light '{node_name}' has radius {radius} (must be > 0)"
        ));
        return false;
    }
    if !intensity.is_finite() || intensity < 0.0 {
        report.error(format!(
            "Light '{node_name}' has invalid intensity {intensity}"
        ));
        return false;
    }
    // Editor radius is in *sector units* -- convert to world
    // units (engine units) at cook time so the runtime record
    // stays in one canonical unit regardless of room sector size.
    let radius_world = spatial::light_radius_record_units(grid, radius);
    let intensity_q8 = (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u16;
    lights.push(PlaytestLight {
        room: room_index,
        x: pos[0],
        y: pos[1],
        z: pos[2],
        radius: radius_world,
        intensity_q8,
        color,
    });
    true
}

fn expand_lights_across_chunks(
    rooms: &[CookedRoomBakeInput],
    lights: &[PlaytestLight],
) -> Vec<PlaytestLight> {
    let mut out = Vec::new();
    for light in lights {
        let Some(source_room) = rooms.iter().find(|room| room.room_index == light.room) else {
            out.push(*light);
            continue;
        };
        let source_origin = room_origin_units(source_room);
        let global_x = source_origin[0].saturating_add(light.x);
        let global_z = source_origin[1].saturating_add(light.z);
        let mut emitted = false;
        for target_room in rooms {
            if !light_overlaps_room_chunk(global_x, global_z, light.radius, target_room) {
                continue;
            }
            let target_origin = room_origin_units(target_room);
            out.push(PlaytestLight {
                room: target_room.room_index,
                x: global_x.saturating_sub(target_origin[0]),
                y: light.y,
                z: global_z.saturating_sub(target_origin[1]),
                radius: light.radius,
                intensity_q8: light.intensity_q8,
                color: light.color,
            });
            emitted = true;
        }
        if !emitted {
            out.push(*light);
        }
    }
    out
}

fn light_overlaps_room_chunk(
    global_x: i32,
    global_z: i32,
    radius: u16,
    room: &CookedRoomBakeInput,
) -> bool {
    let origin = room_origin_units(room);
    let min_x = origin[0] as i64;
    let min_z = origin[1] as i64;
    let max_x =
        origin[0].saturating_add((room.cooked.width as i32) * room.cooked.sector_size) as i64;
    let max_z =
        origin[1].saturating_add((room.cooked.depth as i32) * room.cooked.sector_size) as i64;
    let x = global_x as i64;
    let z = global_z as i64;
    let closest_x = x.clamp(min_x, max_x);
    let closest_z = z.clamp(min_z, max_z);
    let dx = x - closest_x;
    let dz = z - closest_z;
    let radius = radius as i64;
    dx.saturating_mul(dx).saturating_add(dz.saturating_mul(dz)) <= radius.saturating_mul(radius)
}

fn room_origin_units(room: &CookedRoomBakeInput) -> [i32; 2] {
    [
        room.world_origin[0].saturating_mul(room.cooked.sector_size),
        room.world_origin[1].saturating_mul(room.cooked.sector_size),
    ]
}

fn bake_static_surface_lights(rooms: &mut [CookedRoomBakeInput], lights: &[PlaytestLight]) {
    for room in rooms {
        room.cooked.static_vertex_lighting = true;
        let room_lights: Vec<&PlaytestLight> = lights
            .iter()
            .filter(|light| light.room == room.room_index)
            .collect();
        let depth = room.cooked.depth as usize;
        let sector_size = room.cooked.sector_size;
        let ambient = room.cooked.ambient_color;
        let materials = room.cooked.materials.clone();
        for (idx, sector) in room.cooked.sectors.iter_mut().enumerate() {
            let Some(sector) = sector else {
                continue;
            };
            let sx = (idx / depth) as u16;
            let sz = (idx % depth) as u16;
            if let Some(face) = &mut sector.floor {
                let verts = horizontal_vertices(sx, sz, sector_size, face.heights);
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }
            if let Some(face) = &mut sector.ceiling {
                let verts =
                    reverse_quad_vertices(horizontal_vertices(sx, sz, sector_size, face.heights));
                face.baked_vertex_rgb = bake_surface_vertex_rgb(
                    &materials,
                    ambient,
                    verts,
                    face.material,
                    &room_lights,
                );
            }

            for (direction, walls) in [
                (psxw::direction::NORTH, sector.walls.north.as_mut_slice()),
                (psxw::direction::EAST, sector.walls.east.as_mut_slice()),
                (psxw::direction::SOUTH, sector.walls.south.as_mut_slice()),
                (psxw::direction::WEST, sector.walls.west.as_mut_slice()),
                (
                    psxw::direction::NORTH_WEST_SOUTH_EAST,
                    sector.walls.north_west_south_east.as_mut_slice(),
                ),
                (
                    psxw::direction::NORTH_EAST_SOUTH_WEST,
                    sector.walls.north_east_south_west.as_mut_slice(),
                ),
            ] {
                for wall in walls {
                    if let Some(verts) = wall_vertices(sx, sz, sector_size, direction, wall.heights)
                    {
                        wall.baked_vertex_rgb = bake_surface_vertex_rgb(
                            &materials,
                            ambient,
                            verts,
                            wall.material,
                            &room_lights,
                        );
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn bake_surface_vertex_rgb(
    materials: &[CookedWorldMaterial],
    ambient: [u8; 3],
    vertices: [[i32; 3]; 4],
    material_slot: u16,
    lights: &[&PlaytestLight],
) -> [[u8; 3]; 4] {
    let base = cooked_material_tint(materials, material_slot);
    [
        bake_static_vertex_rgb(vertices[0], base, ambient, lights),
        bake_static_vertex_rgb(vertices[1], base, ambient, lights),
        bake_static_vertex_rgb(vertices[2], base, ambient, lights),
        bake_static_vertex_rgb(vertices[3], base, ambient, lights),
    ]
}

fn cooked_material_tint(materials: &[CookedWorldMaterial], slot: u16) -> [u8; 3] {
    materials
        .iter()
        .find(|material| material.slot == slot)
        .map(|material| material.tint)
        .unwrap_or([128, 128, 128])
}

fn horizontal_vertices(sx: u16, sz: u16, sector_size: i32, heights: [i32; 4]) -> [[i32; 3]; 4] {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    [
        [x0, heights[0], z0],
        [x1, heights[1], z0],
        [x1, heights[2], z1],
        [x0, heights[3], z1],
    ]
}

fn reverse_quad_vertices(vertices: [[i32; 3]; 4]) -> [[i32; 3]; 4] {
    [vertices[3], vertices[2], vertices[1], vertices[0]]
}

fn wall_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<[[i32; 3]; 4]> {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    match direction {
        psxw::direction::NORTH => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z0],
            [x1, heights[2], z0],
            [x0, heights[3], z0],
        ]),
        psxw::direction::EAST => Some([
            [x1, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        psxw::direction::SOUTH => Some([
            [x1, heights[0], z1],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z1],
        ]),
        psxw::direction::WEST => Some([
            [x0, heights[0], z1],
            [x0, heights[1], z0],
            [x0, heights[2], z0],
            [x0, heights[3], z1],
        ]),
        psxw::direction::NORTH_WEST_SOUTH_EAST => Some([
            [x0, heights[0], z0],
            [x1, heights[1], z1],
            [x1, heights[2], z1],
            [x0, heights[3], z0],
        ]),
        psxw::direction::NORTH_EAST_SOUTH_WEST => Some([
            [x1, heights[0], z0],
            [x0, heights[1], z1],
            [x0, heights[2], z1],
            [x1, heights[3], z0],
        ]),
        _ => None,
    }
}

fn bake_static_vertex_rgb(
    point: [i32; 3],
    base: [u8; 3],
    ambient: [u8; 3],
    lights: &[&PlaytestLight],
) -> [u8; 3] {
    const LIGHTING_NEUTRAL: u32 = 128;
    const LIGHTING_MAX: u32 = 255;
    let mut accum = [ambient[0] as u32, ambient[1] as u32, ambient[2] as u32];
    for light in lights {
        let Some(weight_q8) =
            point_light_weight_q8(point, [light.x, light.y, light.z], light.radius)
        else {
            continue;
        };
        for (channel, color) in accum.iter_mut().zip(light.color) {
            let weighted = (color as u32).saturating_mul(light.intensity_q8 as u32);
            *channel = channel.saturating_add(weighted.saturating_mul(weight_q8) >> 16);
        }
    }
    [
        ((base[0] as u32 * accum[0].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[1] as u32 * accum[1].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
        ((base[2] as u32 * accum[2].min(LIGHTING_MAX)) / LIGHTING_NEUTRAL).min(255) as u8,
    ]
}

fn point_light_weight_q8(point: [i32; 3], light_position: [i32; 3], radius: u16) -> Option<u32> {
    let radius = radius as u32;
    if radius == 0 {
        return None;
    }
    let dx = point[0].abs_diff(light_position[0]);
    let dy = point[1].abs_diff(light_position[1]);
    let dz = point[2].abs_diff(light_position[2]);
    if dx >= radius || dy >= radius || dz >= radius {
        return None;
    }
    let d2 = dx
        .checked_mul(dx)?
        .checked_add(dy.checked_mul(dy)?)?
        .checked_add(dz.checked_mul(dz)?)?;
    let r2 = radius.checked_mul(radius)?;
    if d2 >= r2 {
        return None;
    }
    Some((radius - isqrt_u32(d2)).saturating_mul(256) / radius)
}

fn isqrt_u32(value: u32) -> u32 {
    let mut x = value;
    let mut r = 0u32;
    let mut bit = 1u32 << 30;
    while bit > x {
        bit >>= 2;
    }
    while bit != 0 {
        if x >= r + bit {
            x -= r + bit;
            r = (r >> 1) + bit;
        } else {
            r >>= 1;
        }
        bit >>= 2;
    }
    r
}

const FULL_HEIGHT_BLOCKER_TOLERANCE: i32 = 32;

fn append_room_visibility(
    room_index: u16,
    cooked: &CookedWorldGrid,
    room_visibility: &mut Vec<PlaytestRoomVisibility>,
    visibility_cells: &mut Vec<PlaytestVisibilityCell>,
    visibility_pvs: &mut Vec<PlaytestVisibilityPvs>,
    visibility_pvs_bits: &mut Vec<u8>,
) {
    let cell_first = u16::try_from(visibility_cells.len()).unwrap_or(u16::MAX);
    let mut local_cells = build_visibility_cells(room_index, cooked);
    let cell_count = u16::try_from(local_cells.len()).unwrap_or(u16::MAX);
    let index_by_coord = visibility_index_by_coord(cooked.width, cooked.depth, &local_cells);
    assign_visibility_portals(
        cooked.width,
        cooked.depth,
        &index_by_coord,
        &mut local_cells,
    );
    let pvs_first = u32::try_from(visibility_pvs.len()).unwrap_or(u32::MAX);
    append_visibility_pvs(
        cooked.width,
        cooked.depth,
        &local_cells,
        &index_by_coord,
        visibility_pvs,
        visibility_pvs_bits,
    );
    let pvs_count =
        u16::try_from(visibility_pvs.len().saturating_sub(pvs_first as usize)).unwrap_or(u16::MAX);

    visibility_cells.extend(local_cells);
    room_visibility.push(PlaytestRoomVisibility {
        room: room_index,
        cell_first,
        cell_count,
        pvs_first,
        pvs_count,
    });
}

#[allow(clippy::too_many_arguments)]
fn append_room_surface_cache(
    room_index: u16,
    room_bytes: &[u8],
    materials: &[PlaytestMaterial],
    assets: &[PlaytestAsset],
    room_surface_caches: &mut Vec<PlaytestRoomSurfaceCache>,
    room_cache_cells: &mut Vec<PlaytestCachedRoomCell>,
    room_cache_cell_vertices: &mut Vec<u16>,
    room_cache_vertices: &mut Vec<PlaytestCachedRoomVertex>,
    room_cache_surfaces: &mut Vec<PlaytestCachedRoomSurface>,
) -> Result<(), String> {
    let room = RuntimeRoom::from_bytes(room_bytes)
        .map_err(|e| format!("Room #{room_index} generated cache parse failed: {e:?}"))?;
    let cache_materials = cache_materials_for_room(room_index, materials, assets)?;
    let surface_capacity = (room.width() as usize)
        .saturating_mul(room.depth() as usize)
        .saturating_mul(4)
        .saturating_add(room.world().wall_count() as usize)
        .max(1);
    let cell_capacity = (room.width() as usize)
        .saturating_mul(room.depth() as usize)
        .max(1);
    let vertex_capacity = surface_capacity.saturating_mul(4).max(1);
    let mut cells = vec![CachedRoomCell::EMPTY; cell_capacity];
    let mut vertices = vec![WorldVertex::ZERO; vertex_capacity];
    let mut surfaces = vec![CachedRoomSurface::EMPTY; surface_capacity];
    let stats = cache_room_vertex_lit_surfaces(
        room.render(),
        &cache_materials,
        &mut cells,
        &mut vertices,
        &mut surfaces,
    );
    if stats.overflow {
        return Err(format!(
            "Room #{room_index} generated surface cache overflowed its computed capacity"
        ));
    }
    let cell_first = checked_u32(room_cache_cells.len(), "room cache cell start")?;
    let vertex_first = checked_u32(room_cache_vertices.len(), "room cache vertex start")?;
    let surface_first = checked_u32(room_cache_surfaces.len(), "room cache surface start")?;
    let cell_vertex_first = checked_u32(
        room_cache_cell_vertices.len(),
        "room cache cell vertex start",
    )?;
    let cell_count = checked_u16(stats.cell_count, "room cache cell count")?;
    let vertex_count = checked_u16(stats.vertex_count, "room cache vertex count")?;
    let surface_count = checked_u16(stats.surface_count, "room cache surface count")?;
    let mut local_cell_vertices = Vec::new();
    let mut playtest_cells = Vec::with_capacity(stats.cell_count);
    for cell in &cells[..stats.cell_count] {
        let local_vertex_first = checked_u16(
            local_cell_vertices.len(),
            "room cache local cell vertex start",
        )?;
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(stats.surface_count);
        let mut unique = Vec::new();
        for surface in &surfaces[first..end] {
            for vertex_index in surface.vertex_indices {
                if (vertex_index as usize) < stats.vertex_count && !unique.contains(&vertex_index) {
                    unique.push(vertex_index);
                }
            }
        }
        let local_vertex_count = checked_u16(unique.len(), "room cache local cell vertex count")?;
        local_cell_vertices.extend(unique);
        playtest_cells.push(playtest_cached_room_cell(
            *cell,
            local_vertex_first,
            local_vertex_count,
        ));
    }
    let cell_vertex_count = checked_u16(local_cell_vertices.len(), "room cache cell vertex count")?;

    room_cache_cells.extend(playtest_cells);
    room_cache_cell_vertices.extend(local_cell_vertices);
    room_cache_vertices.extend(
        vertices[..stats.vertex_count]
            .iter()
            .copied()
            .map(playtest_cached_room_vertex),
    );
    room_cache_surfaces.extend(
        surfaces[..stats.surface_count]
            .iter()
            .copied()
            .map(playtest_cached_room_surface),
    );
    room_surface_caches.push(PlaytestRoomSurfaceCache {
        room: room_index,
        cell_first,
        cell_count,
        cell_vertex_first,
        cell_vertex_count,
        vertex_first,
        vertex_count,
        surface_first,
        surface_count,
    });
    Ok(())
}

fn assign_visibility_cache_cell_indices(
    room_index: u16,
    room_visibility: &[PlaytestRoomVisibility],
    visibility_cells: &mut [PlaytestVisibilityCell],
    room_surface_caches: &[PlaytestRoomSurfaceCache],
    room_cache_cells: &[PlaytestCachedRoomCell],
) {
    let Some(visibility) = room_visibility
        .iter()
        .find(|visibility| visibility.room == room_index)
    else {
        return;
    };
    let Some(cache) = room_surface_caches
        .iter()
        .find(|cache| cache.room == room_index)
    else {
        return;
    };
    let visible_first = visibility.cell_first as usize;
    let visible_end = visible_first.saturating_add(visibility.cell_count as usize);
    let cache_first = cache.cell_first as usize;
    let cache_end = cache_first.saturating_add(cache.cell_count as usize);
    let Some(visible_cells) = visibility_cells.get_mut(visible_first..visible_end) else {
        return;
    };
    let Some(cache_cells) = room_cache_cells.get(cache_first..cache_end) else {
        return;
    };
    for cell in visible_cells {
        cell.cache_cell_index =
            cached_room_cell_index_for_coord(cache_cells, cell.x, cell.z).unwrap_or(u16::MAX);
    }
}

fn cached_room_cell_index_for_coord(
    cells: &[PlaytestCachedRoomCell],
    x: u16,
    z: u16,
) -> Option<u16> {
    let key = cached_room_cell_key(x, z);
    let mut low = 0usize;
    let mut high = cells.len();
    while low < high {
        let mid = (low + high) / 2;
        let cell = cells[mid];
        let cell_key = cached_room_cell_key(cell.x, cell.z);
        if cell_key < key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let cell = cells.get(low)?;
    (cached_room_cell_key(cell.x, cell.z) == key)
        .then(|| u16::try_from(low).ok())
        .flatten()
}

const fn cached_room_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
}

fn cache_materials_for_room(
    room_index: u16,
    materials: &[PlaytestMaterial],
    assets: &[PlaytestAsset],
) -> Result<Vec<WorldRenderMaterial>, String> {
    let mut out = Vec::new();
    for material in materials
        .iter()
        .filter(|material| material.room == room_index)
    {
        let slot = material.local_slot as usize;
        if out.len() <= slot {
            out.resize(slot + 1, WorldRenderMaterial::cache_only(64, 64));
        }
        let texture_asset = assets.get(material.texture_asset_index).ok_or_else(|| {
            format!(
                "Room #{room_index} material slot {} references missing texture asset {}",
                material.local_slot, material.texture_asset_index
            )
        })?;
        let texture = psx_asset::Texture::from_bytes(&texture_asset.bytes).map_err(|e| {
            format!(
                "Room #{room_index} material slot {} texture '{}' parse failed while building generated cache: {e:?}",
                material.local_slot, texture_asset.source_label
            )
        })?;
        out[slot] = WorldRenderMaterial::cache_only(
            room_cache_texture_size(texture.width()),
            room_cache_texture_size(texture.height()),
        );
    }
    Ok(out)
}

fn room_cache_texture_size(size: u16) -> u8 {
    if size < 8 || size > 64 || !size.is_power_of_two() || size % 8 != 0 {
        64
    } else {
        size as u8
    }
}

fn checked_u32(value: usize, what: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("{what} {value} exceeds u32::MAX"))
}

fn checked_u16(value: usize, what: &str) -> Result<u16, String> {
    u16::try_from(value).map_err(|_| format!("{what} {value} exceeds u16::MAX"))
}

fn playtest_cached_room_cell(
    cell: CachedRoomCell,
    vertex_first: u16,
    vertex_count: u16,
) -> PlaytestCachedRoomCell {
    PlaytestCachedRoomCell {
        x: cell.x,
        z: cell.z,
        min_y: cell.min_y,
        max_y: cell.max_y,
        visibility_center: cell.visibility_center,
        visibility_radius: cell.visibility_radius,
        surface_first: cell.surface_first,
        surface_count: cell.surface_count,
        vertex_first,
        vertex_count,
    }
}

fn playtest_cached_room_vertex(vertex: WorldVertex) -> PlaytestCachedRoomVertex {
    PlaytestCachedRoomVertex {
        x: vertex.x,
        y: vertex.y,
        z: vertex.z,
    }
}

fn playtest_cached_room_surface(surface: CachedRoomSurface) -> PlaytestCachedRoomSurface {
    PlaytestCachedRoomSurface {
        material_slot: surface.material_slot,
        vertex_indices: surface.vertex_indices,
        sample_sx: surface.sample_sx,
        sample_sz: surface.sample_sz,
        sample_ordinal: surface.sample_ordinal,
        uv_words: surface.uv_words,
        baked_vertex_rgb: surface.baked_vertex_rgb,
        kind_flags: surface.kind_flags,
        wall_direction: surface.wall_direction,
        split: surface.split,
        triangle_index: surface.triangle_index,
    }
}

fn build_visibility_cells(
    room_index: u16,
    cooked: &CookedWorldGrid,
) -> Vec<PlaytestVisibilityCell> {
    let mut out = Vec::new();
    for x in 0..cooked.width {
        for z in 0..cooked.depth {
            let Some(sector) = cooked_sector(cooked, x, z) else {
                continue;
            };
            let (min_y, max_y) = cooked_sector_y_bounds(sector, cooked.sector_size);
            out.push(PlaytestVisibilityCell {
                room: room_index,
                x,
                z,
                min_y,
                max_y,
                portal_mask: 0,
                blocker_mask: blocker_mask_for_sector(sector, cooked.sector_size),
                cache_cell_index: u16::MAX,
                flags: visibility_cell_flags::HAS_GEOMETRY,
            });
        }
    }
    out
}

fn visibility_index_by_coord(
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
) -> Vec<Option<usize>> {
    let mut out = vec![None; (width as usize).saturating_mul(depth as usize)];
    for (index, cell) in cells.iter().enumerate() {
        if let Some(flat) = visibility_flat_index(depth, cell.x, cell.z) {
            if let Some(slot) = out.get_mut(flat) {
                *slot = Some(index);
            }
        }
    }
    out
}

fn assign_visibility_portals(
    width: u16,
    depth: u16,
    index_by_coord: &[Option<usize>],
    cells: &mut [PlaytestVisibilityCell],
) {
    for index in 0..cells.len() {
        let x = cells[index].x;
        let z = cells[index].z;
        let mut mask = 0u8;
        for edge in VISIBILITY_EDGES {
            let Some((nx, nz)) = neighbour_cell(width, depth, x, z, edge.dx, edge.dz) else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            let this_blocked = cells[index].blocker_mask & edge.bit != 0;
            let neighbour_blocked = cells[neighbour_index].blocker_mask & edge.opposite_bit != 0;
            if !this_blocked && !neighbour_blocked {
                mask |= edge.bit;
            }
        }
        cells[index].portal_mask = mask;
    }
}

fn append_visibility_pvs(
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
    visibility_pvs: &mut Vec<PlaytestVisibilityPvs>,
    visibility_pvs_bits: &mut Vec<u8>,
) {
    let bitset_bytes = visibility_pvs_bitset_bytes(cells.len());
    let mut bits = vec![0u8; bitset_bytes];
    for anchor_index in 0..cells.len() {
        bits.fill(0);
        fill_visibility_pvs_bits(anchor_index, width, depth, cells, index_by_coord, &mut bits);
        let byte_first =
            find_existing_visibility_pvs_bits(visibility_pvs, visibility_pvs_bits, &bits)
                .unwrap_or_else(|| {
                    let byte_first = u32::try_from(visibility_pvs_bits.len()).unwrap_or(u32::MAX);
                    visibility_pvs_bits.extend_from_slice(&bits);
                    byte_first
                });
        visibility_pvs.push(PlaytestVisibilityPvs {
            byte_first,
            byte_count: u16::try_from(bitset_bytes).unwrap_or(u16::MAX),
        });
    }
}

fn find_existing_visibility_pvs_bits(
    visibility_pvs: &[PlaytestVisibilityPvs],
    visibility_pvs_bits: &[u8],
    bits: &[u8],
) -> Option<u32> {
    for pvs in visibility_pvs {
        if pvs.byte_count as usize != bits.len() {
            continue;
        }
        let start = pvs.byte_first as usize;
        let Some(end) = start.checked_add(bits.len()) else {
            continue;
        };
        if visibility_pvs_bits.get(start..end) == Some(bits) {
            return Some(pvs.byte_first);
        }
    }
    None
}

fn visibility_pvs_bitset_bytes(cell_count: usize) -> usize {
    cell_count.saturating_add(7) / 8
}

fn fill_visibility_pvs_bits(
    anchor_index: usize,
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
    bits: &mut [u8],
) -> Vec<usize> {
    let visible = visibility_indices_for_anchor(anchor_index, width, depth, cells, index_by_coord);
    for &index in &visible {
        set_visibility_pvs_bit(bits, index);
    }
    visible
}

fn visibility_indices_for_anchor(
    anchor_index: usize,
    width: u16,
    depth: u16,
    cells: &[PlaytestVisibilityCell],
    index_by_coord: &[Option<usize>],
) -> Vec<usize> {
    if anchor_index >= cells.len() {
        return Vec::new();
    }
    let anchor = cells[anchor_index];
    let mut visible = Vec::new();
    let mut visited = vec![false; cells.len()];
    let mut selected = vec![false; cells.len()];
    let mut queue = Vec::new();
    visited[anchor_index] = true;
    queue.push((anchor_index, 0u16));

    let mut cursor = 0usize;
    while let Some(&(cell_index, distance)) = queue.get(cursor) {
        cursor += 1;
        visible.push(cell_index);
        if distance >= PLAYTEST_VISIBILITY_CELL_RADIUS {
            continue;
        }

        let cell = cells[cell_index];
        for edge in VISIBILITY_EDGES {
            if cell.portal_mask & edge.bit == 0 {
                continue;
            }
            let Some((nx, nz)) = neighbour_cell(width, depth, cell.x, cell.z, edge.dx, edge.dz)
            else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            if visited[neighbour_index] {
                continue;
            }
            visited[neighbour_index] = true;
            queue.push((neighbour_index, distance + 1));
        }
    }

    for &(index, _) in &queue {
        selected[index] = true;
    }
    let mut i = queue.len();
    while i != 0 {
        i -= 1;
        let cell = cells[queue[i].0];
        for edge in VISIBILITY_EDGES {
            let Some((nx, nz)) = neighbour_cell(width, depth, cell.x, cell.z, edge.dx, edge.dz)
            else {
                continue;
            };
            let Some(neighbour_index) = visibility_cell_index(index_by_coord, depth, nx, nz) else {
                continue;
            };
            if !selected[neighbour_index] {
                selected[neighbour_index] = true;
                visible.push(neighbour_index);
            }
        }
    }

    visible.sort_by(|&a, &b| {
        let ca = cells[a];
        let cb = cells[b];
        let da = chebyshev_distance(anchor, ca);
        let db = chebyshev_distance(anchor, cb);
        db.cmp(&da).then(ca.x.cmp(&cb.x)).then(ca.z.cmp(&cb.z))
    });
    visible
}

fn set_visibility_pvs_bit(bits: &mut [u8], index: usize) {
    let byte = index / 8;
    let bit = index % 8;
    if let Some(slot) = bits.get_mut(byte) {
        *slot |= 1 << bit;
    }
}

#[derive(Clone, Copy)]
struct VisibilityEdge {
    bit: u8,
    opposite_bit: u8,
    dx: i32,
    dz: i32,
}

const VISIBILITY_EDGES: [VisibilityEdge; 4] = [
    VisibilityEdge {
        bit: visibility_edge_flags::NORTH,
        opposite_bit: visibility_edge_flags::SOUTH,
        dx: 0,
        dz: -1,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::EAST,
        opposite_bit: visibility_edge_flags::WEST,
        dx: 1,
        dz: 0,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::SOUTH,
        opposite_bit: visibility_edge_flags::NORTH,
        dx: 0,
        dz: 1,
    },
    VisibilityEdge {
        bit: visibility_edge_flags::WEST,
        opposite_bit: visibility_edge_flags::EAST,
        dx: -1,
        dz: 0,
    },
];

fn chebyshev_distance(anchor: PlaytestVisibilityCell, cell: PlaytestVisibilityCell) -> i32 {
    (cell.x as i32 - anchor.x as i32)
        .abs()
        .max((cell.z as i32 - anchor.z as i32).abs())
}

fn visibility_cell_index(
    index_by_coord: &[Option<usize>],
    depth: u16,
    x: u16,
    z: u16,
) -> Option<usize> {
    let flat = visibility_flat_index(depth, x, z)?;
    index_by_coord.get(flat).copied().flatten()
}

fn visibility_flat_index(depth: u16, x: u16, z: u16) -> Option<usize> {
    (x as usize)
        .checked_mul(depth as usize)?
        .checked_add(z as usize)
}

fn neighbour_cell(width: u16, depth: u16, x: u16, z: u16, dx: i32, dz: i32) -> Option<(u16, u16)> {
    let nx = x as i32 + dx;
    let nz = z as i32 + dz;
    if nx < 0 || nz < 0 || nx > u16::MAX as i32 || nz > u16::MAX as i32 {
        return None;
    }
    let nx = nx as u16;
    let nz = nz as u16;
    if nx >= width || nz >= depth {
        return None;
    }
    Some((nx, nz))
}

fn cooked_sector(
    cooked: &CookedWorldGrid,
    x: u16,
    z: u16,
) -> Option<&crate::world_cook::CookedGridSector> {
    let index = (x as usize)
        .checked_mul(cooked.depth as usize)?
        .checked_add(z as usize)?;
    cooked.sectors.get(index)?.as_ref()
}

fn cooked_sector_y_bounds(
    sector: &crate::world_cook::CookedGridSector,
    sector_size: i32,
) -> (i32, i32) {
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    let mut any = false;
    if let Some(face) = sector.floor {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, face.heights);
    }
    if let Some(face) = sector.ceiling {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, face.heights);
    }
    for wall in sector
        .walls
        .north
        .iter()
        .chain(sector.walls.east.iter())
        .chain(sector.walls.south.iter())
        .chain(sector.walls.west.iter())
    {
        include_cooked_heights(&mut min_y, &mut max_y, &mut any, wall.heights);
    }
    if any {
        (min_y, max_y)
    } else {
        (0, sector_size)
    }
}

fn include_cooked_heights(min_y: &mut i32, max_y: &mut i32, any: &mut bool, heights: [i32; 4]) {
    for height in heights {
        *min_y = (*min_y).min(height);
        *max_y = (*max_y).max(height);
        *any = true;
    }
}

fn blocker_mask_for_sector(sector: &crate::world_cook::CookedGridSector, sector_size: i32) -> u8 {
    let mut mask = 0u8;
    if has_full_height_solid_wall(&sector.walls.north, sector_size) {
        mask |= visibility_edge_flags::NORTH;
    }
    if has_full_height_solid_wall(&sector.walls.east, sector_size) {
        mask |= visibility_edge_flags::EAST;
    }
    if has_full_height_solid_wall(&sector.walls.south, sector_size) {
        mask |= visibility_edge_flags::SOUTH;
    }
    if has_full_height_solid_wall(&sector.walls.west, sector_size) {
        mask |= visibility_edge_flags::WEST;
    }
    mask
}

fn has_full_height_solid_wall(
    walls: &[crate::world_cook::CookedGridVerticalFace],
    sector_size: i32,
) -> bool {
    walls.iter().any(|wall| {
        if !wall.solid {
            return false;
        }
        let bottom = wall.heights[0].min(wall.heights[1]);
        let top = wall.heights[2].max(wall.heights[3]);
        top.saturating_sub(bottom)
            >= sector_size
                .saturating_sub(FULL_HEIGHT_BLOCKER_TOLERANCE)
                .max(sector_size / 2)
    })
}

fn grid_rect(grid: &WorldGrid, origin: [u16; 2], size: [u16; 2]) -> Option<WorldGrid> {
    if size[0] == 0 || size[1] == 0 {
        return None;
    }
    let end_x = origin[0].checked_add(size[0])?;
    let end_z = origin[1].checked_add(size[1])?;
    if end_x > grid.width || end_z > grid.depth {
        return None;
    }

    let mut out = WorldGrid::empty(size[0], size[1], grid.sector_size);
    out.origin = [
        grid.origin[0] + origin[0] as i32,
        grid.origin[1] + origin[1] as i32,
    ];
    out.ambient_color = grid.ambient_color;
    out.fog_enabled = grid.fog_enabled;
    out.fog_color = grid.fog_color;
    out.fog_near = grid.fog_near;
    out.fog_far = grid.fog_far;

    for x in 0..size[0] {
        for z in 0..size[1] {
            let src = grid.sector_index(origin[0] + x, origin[1] + z)?;
            let dst = out.sector_index(x, z)?;
            out.sectors[dst] = grid.sectors[src].clone();
        }
    }
    append_chunk_boundary_floor_transition_walls(grid, &mut out, origin, size);
    Some(out)
}

fn append_chunk_boundary_floor_transition_walls(
    source: &WorldGrid,
    chunk: &mut WorldGrid,
    origin: [u16; 2],
    size: [u16; 2],
) {
    for x in 0..size[0] {
        append_boundary_floor_transition_wall(
            source,
            chunk,
            origin,
            x,
            size[1] - 1,
            GridDirection::North,
        );
    }
    for z in 0..size[1] {
        append_boundary_floor_transition_wall(
            source,
            chunk,
            origin,
            size[0] - 1,
            z,
            GridDirection::East,
        );
    }
}

fn append_boundary_floor_transition_wall(
    source: &WorldGrid,
    chunk: &mut WorldGrid,
    origin: [u16; 2],
    local_x: u16,
    local_z: u16,
    direction: GridDirection,
) {
    let source_x = origin[0].saturating_add(local_x);
    let source_z = origin[1].saturating_add(local_z);
    let Some(wall) = source.floor_transition_wall_for_edge(source_x, source_z, direction) else {
        return;
    };
    let Some(sector) = chunk.ensure_sector(local_x, local_z) else {
        return;
    };
    sector.walls.get_mut(direction).push(wall);
}

fn chunk_room_name(room_name: &str, chunk_count: usize, chunk_index: usize) -> String {
    if chunk_count <= 1 {
        room_name.to_string()
    } else {
        format!("{room_name} / Chunk {chunk_index}")
    }
}

fn enclosing_room<'a>(scene: &'a crate::Scene, node: &'a SceneNode) -> Option<&'a SceneNode> {
    let mut current = node.parent;
    while let Some(parent_id) = current {
        let parent = scene.node(parent_id)?;
        if matches!(parent.kind, NodeKind::Room { .. }) {
            return Some(parent);
        }
        current = parent.parent;
    }
    None
}

fn chunk_for_node<'a>(
    node: &SceneNode,
    grid: &WorldGrid,
    chunks: &'a [AuthoredRoomChunk],
) -> Option<&'a AuthoredRoomChunk> {
    let world_cells =
        grid.editor_to_world_cells([node.transform.translation[0], node.transform.translation[2]]);
    let wcx = world_cells[0].floor() as i32;
    let wcz = world_cells[1].floor() as i32;
    let (sx, sz) = grid.world_cell_to_array(wcx, wcz)?;
    chunks.iter().find(|chunk| {
        let x0 = chunk.array_origin[0];
        let z0 = chunk.array_origin[1];
        let x1 = x0.saturating_add(chunk.size[0]);
        let z1 = z0.saturating_add(chunk.size[1]);
        sx >= x0 && sx < x1 && sz >= z0 && sz < z1
    })
}

fn build_playtest_chunks(
    room_chunks_by_node: &HashMap<NodeId, Vec<AuthoredRoomChunk>>,
    room_count: usize,
) -> Vec<PlaytestChunk> {
    let mut chunks = vec![
        PlaytestChunk {
            room: 0,
            authored_room: 0,
            chunk_index: 0,
            origin_x: 0,
            origin_z: 0,
            width: 0,
            depth: 0,
            neighbours: [None; 4],
            triangles: 0,
            psxw_bytes: 0,
            static_lit_bytes: 0,
            populated_cells: 0,
            flags: 0,
        };
        room_count
    ];

    for node_chunks in room_chunks_by_node.values() {
        for chunk in node_chunks {
            let Some(out) = chunks.get_mut(chunk.room_index as usize) else {
                continue;
            };
            *out = PlaytestChunk {
                room: chunk.room_index,
                authored_room: chunk.authored_room,
                chunk_index: chunk.chunk_index,
                origin_x: chunk.world_origin[0],
                origin_z: chunk.world_origin[1],
                width: chunk.size[0],
                depth: chunk.size[1],
                neighbours: chunk_neighbours(chunk, node_chunks),
                triangles: chunk.triangles,
                psxw_bytes: chunk.psxw_bytes,
                static_lit_bytes: chunk.static_lit_bytes,
                populated_cells: chunk.populated_cells,
                flags: 0,
            };
        }
    }

    chunks
}

fn chunk_neighbours(chunk: &AuthoredRoomChunk, chunks: &[AuthoredRoomChunk]) -> [Option<u16>; 4] {
    let mut neighbours = [None; 4];
    let mut scores = [0u16; 4];
    for candidate in chunks {
        if candidate.room_index == chunk.room_index {
            continue;
        }
        if let Some((direction, score)) = chunk_cardinal_touch_score(chunk, candidate) {
            if score > scores[direction] {
                neighbours[direction] = Some(candidate.room_index);
                scores[direction] = score;
            }
        }
    }
    neighbours
}

fn chunk_cardinal_touch_score(
    a: &AuthoredRoomChunk,
    b: &AuthoredRoomChunk,
) -> Option<(usize, u16)> {
    let ax0 = a.array_origin[0];
    let az0 = a.array_origin[1];
    let ax1 = ax0.saturating_add(a.size[0]);
    let az1 = az0.saturating_add(a.size[1]);
    let bx0 = b.array_origin[0];
    let bz0 = b.array_origin[1];
    let bx1 = bx0.saturating_add(b.size[0]);
    let bz1 = bz0.saturating_add(b.size[1]);
    let x_overlap = overlap_len(ax0, ax1, bx0, bx1);
    let z_overlap = overlap_len(az0, az1, bz0, bz1);

    if x_overlap > 0 && az0 == bz1 {
        return Some((0, x_overlap));
    }
    if z_overlap > 0 && ax1 == bx0 {
        return Some((1, z_overlap));
    }
    if x_overlap > 0 && az1 == bz0 {
        return Some((2, x_overlap));
    }
    if z_overlap > 0 && ax0 == bx1 {
        return Some((3, z_overlap));
    }
    None
}

fn overlap_len(a0: u16, a1: u16, b0: u16, b1: u16) -> u16 {
    a1.min(b1).saturating_sub(a0.max(b0))
}

/// Convert a node's editor-space transform to its generated
/// runtime chunk-local coordinates. The authored Room may be
/// arbitrary-size; the cooked `.psxw` for one chunk is still
/// array-rooted at that chunk's origin.
fn node_chunk_local_position(
    node: &SceneNode,
    grid: &WorldGrid,
    chunk: &AuthoredRoomChunk,
) -> [i32; 3] {
    let world_cells =
        grid.editor_to_world_cells([node.transform.translation[0], node.transform.translation[2]]);
    let s = grid.sector_size as f32;
    [
        ((world_cells[0] - chunk.world_origin[0] as f32) * s) as i32,
        (node.transform.translation[1] * s) as i32,
        ((world_cells[1] - chunk.world_origin[1] as f32) * s) as i32,
    ]
}

/// Like [`node_chunk_local_position`], but treats the node as a
/// floor anchor: X/Z come from the authored transform and Y is
/// sampled from the floor directly underneath when possible.
fn floor_anchored_node_chunk_local_position(
    node: &SceneNode,
    grid: &WorldGrid,
    chunk: &AuthoredRoomChunk,
) -> [i32; 3] {
    let mut pos = node_chunk_local_position(node, grid, chunk);
    let world =
        grid.editor_to_room_local([node.transform.translation[0], node.transform.translation[2]]);
    if let Some(floor_y) = grid.floor_height_at_room_local(world[0] as i32, world[2] as i32) {
        pos[1] = floor_y;
    }
    pos
}

/// Convert an editor euler-degrees-Y rotation to a PSX angle
/// unit (`0..4096`).
fn yaw_from_degrees(degrees: f32) -> i16 {
    angle_from_degrees(degrees)
}

/// Convert editor Euler degrees to PSX angle units (`0..4096`).
fn angle_from_degrees(degrees: f32) -> i16 {
    let normalised = degrees.rem_euclid(360.0);
    let units = normalised * (4096.0 / 360.0);
    units as i16
}

fn cook_error_for_node(name: &str, err: WorldGridCookError) -> String {
    format!("Room '{name}' failed cook: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NodeKind, ProjectDocument, DEFAULT_WORLD_CAMERA_DISTANCE, DEFAULT_WORLD_CAMERA_HEIGHT,
        DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE, DEFAULT_WORLD_CAMERA_TARGET_HEIGHT,
    };

    fn starter_project_root() -> PathBuf {
        crate::default_project_dir()
    }

    fn visibility_test_cell(x: u16, z: u16, blocker_mask: u8) -> PlaytestVisibilityCell {
        PlaytestVisibilityCell {
            room: 0,
            x,
            z,
            min_y: 0,
            max_y: crate::DEFAULT_WORLD_SECTOR_SIZE,
            portal_mask: 0,
            blocker_mask,
            cache_cell_index: u16::MAX,
            flags: visibility_cell_flags::HAS_GEOMETRY,
        }
    }

    fn project_with_one_room() -> ProjectDocument {
        let project = ProjectDocument::starter();
        let scene = project.active_scene();
        let has_room = scene
            .nodes()
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Room { .. }));
        let has_player_spawn = scene.nodes().iter().any(|n| is_player_spawn_node(scene, n));
        assert!(has_room, "starter must contain a Room");
        assert!(
            has_player_spawn,
            "starter must contain a player spawn entity"
        );
        project
    }

    fn is_player_spawn_node(scene: &crate::Scene, node: &SceneNode) -> bool {
        match &node.kind {
            NodeKind::SpawnPoint { player: true, .. } => true,
            NodeKind::Entity => node.children.iter().any(|id| {
                scene.node(*id).is_some_and(|child| {
                    matches!(
                        child.kind,
                        NodeKind::CharacterController { player: true, .. }
                    )
                })
            }),
            _ => false,
        }
    }

    fn player_spawn_node_id(project: &ProjectDocument) -> NodeId {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find(|node| is_player_spawn_node(scene, node))
            .expect("starter has a player spawn entity")
            .id
    }

    fn player_controller_component_id(project: &ProjectDocument) -> NodeId {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find(|node| {
                matches!(
                    node.kind,
                    NodeKind::CharacterController { player: true, .. }
                )
            })
            .expect("starter has a player CharacterController")
            .id
    }

    fn player_character_resource_id(project: &ProjectDocument) -> ResourceId {
        let scene = project.active_scene();
        scene
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::CharacterController {
                    player: true,
                    character: Some(character),
                    ..
                } => Some(*character),
                _ => None,
            })
            .expect("starter has an assigned player Character")
    }

    fn player_model_resource_id(project: &ProjectDocument) -> ResourceId {
        let character_id = player_character_resource_id(project);
        project
            .resource(character_id)
            .and_then(|resource| match &resource.data {
                ResourceData::Character(character) => character.model,
                _ => None,
            })
            .expect("starter player Character has a Model")
    }

    fn demote_player_spawns(project: &mut ProjectDocument) {
        let scene = project.active_scene_mut();
        let ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| {
                matches!(
                    node.kind,
                    NodeKind::SpawnPoint { player: true, .. }
                        | NodeKind::CharacterController { player: true, .. }
                )
            })
            .map(|node| node.id)
            .collect();
        for id in ids {
            let Some(node) = scene.node_mut(id) else {
                continue;
            };
            match &mut node.kind {
                NodeKind::SpawnPoint { player, character } if *player => {
                    *player = false;
                    *character = None;
                }
                NodeKind::CharacterController {
                    player, character, ..
                } if *player => {
                    *player = false;
                    *character = None;
                }
                _ => {}
            }
        }
    }

    fn starter_light_color(project: &ProjectDocument) -> [u8; 3] {
        project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::PointLight { color, .. } => Some(*color),
                _ => None,
            })
            .expect("starter contains one light")
    }

    fn starter_light_intensity_q8(project: &ProjectDocument) -> u16 {
        let intensity = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::PointLight { intensity, .. } => Some(*intensity),
                _ => None,
            })
            .expect("starter contains one light");
        (intensity * 256.0).clamp(0.0, u16::MAX as f32) as u16
    }

    fn starter_light_ids(project: &ProjectDocument) -> Vec<NodeId> {
        project
            .active_scene()
            .nodes()
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::PointLight { .. }))
            .map(|n| n.id)
            .collect()
    }

    fn remove_model_renderer_components(project: &mut ProjectDocument) {
        let scene = project.active_scene_mut();
        let ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::ModelRenderer { model: Some(_), .. }))
            .map(|node| node.id)
            .collect();
        for id in ids {
            scene.remove_node(id);
        }
    }

    fn set_first_model_instance_clip(project: &mut ProjectDocument, clip_index: u16) {
        let model_id = player_model_resource_id(project);
        let scene = project.active_scene_mut();
        let ids: Vec<NodeId> = scene
            .nodes()
            .iter()
            .filter(|node| matches!(node.kind, NodeKind::MeshInstance { .. }))
            .map(|node| node.id)
            .collect();
        for id in ids {
            let Some(node) = scene.node_mut(id) else {
                continue;
            };
            if let NodeKind::MeshInstance { animation_clip, .. } = &mut node.kind {
                *animation_clip = Some(clip_index);
                return;
            }
        }
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .expect("starter has Room");
        scene.add_node(
            room_id,
            "Invalid Clip Model",
            NodeKind::MeshInstance {
                mesh: Some(model_id),
                material: None,
                animation_clip: Some(clip_index),
            },
        );
    }

    #[test]
    fn tracked_editor_playtest_manifest_is_placeholder() {
        let manifest = std::fs::read_to_string(default_generated_dir().join(MANIFEST_FILENAME))
            .expect("read tracked editor-playtest manifest");
        assert!(
            !manifest.contains("include_bytes!"),
            "tracked placeholder manifest must not reference ignored cooked blobs"
        );
        assert!(manifest.contains("pub static ASSETS: &[LevelAssetRecord] = &[];"));
        assert!(manifest.contains("pub static ROOMS: &[LevelRoomRecord] = &[];"));
        assert!(manifest.contains("pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[];"));
        assert!(manifest.contains("pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[];"));
        assert!(manifest.contains("pub static VISIBILITY_PVS_BITS: &[u8] = &[];"));
        assert!(manifest
            .contains("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];"));
        assert!(
            manifest.contains("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];")
        );
        assert!(manifest
            .contains("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];"));
        assert!(manifest
            .contains("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];"));
        assert!(manifest.contains("pub static MODEL_SOCKETS: &[LevelModelSocketRecord] = &[];"));
        assert!(manifest.contains("pub static WEAPONS: &[LevelWeaponRecord] = &[];"));
        assert!(manifest.contains("pub static EQUIPMENT: &[EquipmentRecord] = &[];"));
        assert!(manifest.contains("pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[];"));
    }

    #[test]
    fn starter_project_validates_and_cooks() {
        let project = project_with_one_room();
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("package returned on ok report");
        assert_eq!(package.rooms.len(), 1);
        assert_eq!(package.room_asset_count(), 1);
        assert_eq!(
            package.rooms[0].sky.flags & sky_flags::ENABLED,
            sky_flags::ENABLED
        );
        assert_eq!(package.rooms[0].sky.horizon_percent, 58);
        assert_eq!(
            package.rooms[0].far_vista.flags & far_vista_flags::TEXTURED,
            0
        );
        assert_eq!(
            package.rooms[0].far_vista.flags & far_vista_flags::ENABLED,
            0
        );
        assert_eq!(package.rooms[0].far_vista.segments, 12);
        assert!(package.rooms[0].far_vista.texture_asset_indices.is_empty());
        assert_eq!(
            package.rooms[0].camera.distance,
            DEFAULT_WORLD_CAMERA_DISTANCE
        );
        assert_eq!(package.rooms[0].camera.height, DEFAULT_WORLD_CAMERA_HEIGHT);
        assert_eq!(
            package.rooms[0].camera.target_height,
            DEFAULT_WORLD_CAMERA_TARGET_HEIGHT
        );
        assert_eq!(
            package.rooms[0].camera.min_floor_clearance,
            DEFAULT_WORLD_CAMERA_MIN_FLOOR_CLEARANCE
        );
        assert_eq!(package.room_visibility.len(), 1);
        assert!(!package.visibility_cells.is_empty());
        assert!(!package.visibility_pvs.is_empty());
        assert!(!package.visibility_pvs_bits.is_empty());
        assert_eq!(package.room_surface_caches.len(), package.rooms.len());
        assert!(!package.room_cache_cells.is_empty());
        assert!(!package.room_cache_vertices.is_empty());
        assert!(!package.room_cache_surfaces.is_empty());
        let cache = package.room_surface_caches[0];
        let cache_first = cache.cell_first as usize;
        let cache_end = cache_first + cache.cell_count as usize;
        let cache_cells = &package.room_cache_cells[cache_first..cache_end];
        for cell in package
            .visibility_cells
            .iter()
            .filter(|cell| cell.room == cache.room)
        {
            assert_ne!(cell.cache_cell_index, u16::MAX);
            let cached = cache_cells[cell.cache_cell_index as usize];
            assert_eq!((cached.x, cached.z), (cell.x, cell.z));
        }
        assert!(package.spawn.is_some());
    }

    #[test]
    fn generated_room_cache_counts_match_runtime_builder() {
        let project = project_with_one_room();
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "{report:?}");
        let package = package.expect("package");
        let cache = package.room_surface_caches[0];
        assert!(cache.cell_vertex_count > 0);
        assert!(!package.room_cache_cell_vertices.is_empty());
        let room_record = &package.rooms[cache.room as usize];
        let room_asset = &package.assets[room_record.world_asset_index];
        let room = RuntimeRoom::from_bytes(&room_asset.bytes).expect("room parses");
        let materials =
            cache_materials_for_room(cache.room, &package.materials, &package.assets).unwrap();
        let mut cells = vec![CachedRoomCell::EMPTY; cache.cell_count as usize];
        let mut vertices = vec![WorldVertex::ZERO; cache.vertex_count as usize];
        let mut surfaces = vec![CachedRoomSurface::EMPTY; cache.surface_count as usize];
        let stats = cache_room_vertex_lit_surfaces(
            room.render(),
            &materials,
            &mut cells,
            &mut vertices,
            &mut surfaces,
        );
        assert!(!stats.overflow);
        assert_eq!(stats.cell_count, cache.cell_count as usize);
        assert_eq!(stats.vertex_count, cache.vertex_count as usize);
        assert_eq!(stats.surface_count, cache.surface_count as usize);
        assert_eq!(
            package.room_cache_cells[cache.cell_first as usize],
            playtest_cached_room_cell(
                cells[0],
                package.room_cache_cells[cache.cell_first as usize].vertex_first,
                package.room_cache_cells[cache.cell_first as usize].vertex_count,
            )
        );
        assert_eq!(
            package.room_cache_vertices[cache.vertex_first as usize],
            playtest_cached_room_vertex(vertices[0])
        );
        assert_eq!(
            package.room_cache_surfaces[cache.surface_first as usize],
            playtest_cached_room_surface(surfaces[0])
        );
    }

    #[test]
    fn visibility_pvs_adds_one_cell_boundary_shell() {
        let width = 1;
        let depth = PLAYTEST_VISIBILITY_CELL_RADIUS + 6;
        let mut cells: Vec<PlaytestVisibilityCell> =
            (0..depth).map(|z| visibility_test_cell(0, z, 0)).collect();
        let index_by_coord = visibility_index_by_coord(width, depth, &cells);
        assign_visibility_portals(width, depth, &index_by_coord, &mut cells);

        let visible = visibility_indices_for_anchor(0, width, depth, &cells, &index_by_coord);

        assert_eq!(visible.len(), PLAYTEST_VISIBILITY_CELL_RADIUS as usize + 2);
        assert!(visible.contains(&0));
        assert!(visible.contains(&(PLAYTEST_VISIBILITY_CELL_RADIUS as usize)));
        assert!(visible.contains(&(PLAYTEST_VISIBILITY_CELL_RADIUS as usize + 1)));
        assert!(!visible.contains(&(PLAYTEST_VISIBILITY_CELL_RADIUS as usize + 2)));
    }

    #[test]
    fn visibility_pvs_keeps_blocked_boundary_shell_without_traversing() {
        let width = 2;
        let depth = 1;
        let mut cells = vec![
            visibility_test_cell(0, 0, visibility_edge_flags::EAST),
            visibility_test_cell(1, 0, visibility_edge_flags::WEST),
        ];
        let index_by_coord = visibility_index_by_coord(width, depth, &cells);
        assign_visibility_portals(width, depth, &index_by_coord, &mut cells);

        let visible = visibility_indices_for_anchor(0, width, depth, &cells, &index_by_coord);

        assert_eq!(visible, vec![1, 0]);
    }

    #[test]
    fn visibility_pvs_reuses_identical_bitsets() {
        let width = 2;
        let depth = 1;
        let mut cells = vec![visibility_test_cell(0, 0, 0), visibility_test_cell(1, 0, 0)];
        let index_by_coord = visibility_index_by_coord(width, depth, &cells);
        assign_visibility_portals(width, depth, &index_by_coord, &mut cells);
        let mut pvs = Vec::new();
        let mut bits = Vec::new();

        append_visibility_pvs(width, depth, &cells, &index_by_coord, &mut pvs, &mut bits);

        assert_eq!(pvs.len(), 2);
        assert_eq!(bits.len(), 1);
        assert_eq!(pvs[0].byte_first, pvs[1].byte_first);
        assert_eq!(pvs[0].byte_count, 1);
        assert_eq!(bits[0], 0b0000_0011);
    }

    #[test]
    fn oversized_authored_room_cooks_into_runtime_chunks() {
        let mut project = project_with_one_room();
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let room_id = {
            let scene = project.active_scene();
            scene
                .nodes()
                .iter()
                .find(|n| matches!(n.kind, NodeKind::Room { .. }))
                .expect("starter has a room")
                .id
        };
        if let Some(room) = project.active_scene_mut().node_mut(room_id) {
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("starter room is a room");
            };
            *grid = crate::WorldGrid::empty(
                1,
                crate::MAX_ROOM_DEPTH + 8,
                crate::DEFAULT_WORLD_SECTOR_SIZE,
            );
            for z in 0..grid.depth {
                grid.set_floor(0, z, 0, Some(floor_material));
            }
        }
        let spawn_id = player_spawn_node_id(&project);
        if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
            spawn.transform.translation = [0.0, 0.0, 0.0];
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("package returned on ok report");
        assert_eq!(package.rooms.len(), 4);
        assert_eq!(package.room_asset_count(), 4);
        assert!(package
            .spawn
            .is_some_and(|spawn| (spawn.room as usize) < package.rooms.len()));
    }

    #[test]
    fn chunked_rooms_emit_warm_residency_hints() {
        let mut project = project_with_one_room();
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let room_id = {
            let scene = project.active_scene();
            scene
                .nodes()
                .iter()
                .find(|n| matches!(n.kind, NodeKind::Room { .. }))
                .expect("starter has a room")
                .id
        };
        if let Some(room) = project.active_scene_mut().node_mut(room_id) {
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("starter room is a room");
            };
            *grid = crate::WorldGrid::empty(1, 40, crate::DEFAULT_WORLD_SECTOR_SIZE);
            for z in 0..grid.depth {
                grid.set_floor(0, z, 0, Some(floor_material));
            }
        }
        let spawn_id = player_spawn_node_id(&project);
        if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
            spawn.transform.translation = [0.0, 0.0, -19.0];
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        let src = render_manifest_source(&package);

        let warm_ram_line = src
            .lines()
            .find(|line| line.contains("pub static ROOM_0_WARM_RAM"))
            .expect("room 0 warm RAM static emitted");
        assert!(
            warm_ram_line.contains("AssetId("),
            "room 0 should warm at least one neighbouring room asset: {warm_ram_line}"
        );
        assert!(src.contains("warm_ram: ROOM_0_WARM_RAM"));
        assert!(src.contains("warm_vram: ROOM_0_WARM_VRAM"));
    }

    #[test]
    fn starter_project_emits_player_controller_and_character() {
        let project = project_with_one_room();
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("package returned on ok report");
        assert_eq!(
            package.characters.len(),
            1,
            "starter ships exactly one player Character"
        );
        let pc = package
            .player_controller
            .expect("player controller emitted");
        assert_eq!(pc.character, 0);
        assert_eq!(pc.spawn, package.spawn.unwrap());
        let character = &package.characters[0];
        // Starter characters use the shared standalone FBX
        // library rather than model-local Meshy clips.
        assert_eq!(
            character.action_clips[CharacterAnimationAction::Idle.to_index()],
            0
        );
        assert_eq!(
            character.action_clips[CharacterAnimationAction::Walk.to_index()],
            1
        );
        assert_eq!(
            character.action_clips[CharacterAnimationAction::Run.to_index()],
            1
        );
        assert_eq!(
            character.action_clips[CharacterAnimationAction::Turn.to_index()],
            CHARACTER_CLIP_NONE
        );
    }

    #[test]
    fn animation_set_infers_evade_roles_from_extra_clip_names() {
        let mut project = ProjectDocument::new("role inference");
        let skeleton = project.add_resource(
            "Skeleton",
            ResourceData::Skeleton(crate::SkeletonResource {
                joint_count: 1,
                parents: vec![None],
                signature: "test".to_string(),
                note: String::new(),
            }),
        );
        let roll = project.add_resource(
            "Meshy Gold / roll dodge",
            ResourceData::AnimationClip(crate::AnimationClipResource {
                psxanim_path: "roll.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: crate::AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Generic,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
            }),
        );
        let backstep = project.add_resource(
            "Meshy Gold / step back",
            ResourceData::AnimationClip(crate::AnimationClipResource {
                psxanim_path: "backstep.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: crate::AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Generic,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
            }),
        );
        let light_attack = project.add_resource(
            "Standalone FBX / sword attack",
            ResourceData::AnimationClip(crate::AnimationClipResource {
                psxanim_path: "attack.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: crate::AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Attack,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
            }),
        );
        let heavy_attack = project.add_resource(
            "Custom flourish",
            ResourceData::AnimationClip(crate::AnimationClipResource {
                psxanim_path: "heavy.psxanim".to_string(),
                skeleton: Some(skeleton),
                source: None,
                target_model: None,
                bake: crate::AnimationClipBakeKind::ModelNative,
                role: AnimationRole::Generic,
                looping: false,
                tags: Vec::new(),
                calibration: Default::default(),
            }),
        );
        let mut set = crate::AnimationSetResource {
            skeleton: Some(skeleton),
            clips: vec![roll, backstep, light_attack],
            ..crate::AnimationSetResource::default()
        };
        set.set_action_clip(CharacterAnimationAction::HeavyAttack, Some(heavy_attack));

        assert_eq!(
            animation_set_action_clip(&project, &set, CharacterAnimationAction::Roll),
            Some(roll)
        );
        assert_eq!(
            animation_set_action_clip(&project, &set, CharacterAnimationAction::Backstep),
            Some(backstep)
        );
        assert_eq!(
            animation_set_action_clip(&project, &set, CharacterAnimationAction::LightAttack),
            Some(light_attack)
        );
        assert_eq!(
            animation_set_action_clip(&project, &set, CharacterAnimationAction::HeavyAttack),
            Some(heavy_attack)
        );
        assert_eq!(
            animation_set_action_clip(&project, &set, CharacterAnimationAction::ComboAttack),
            None,
            "generic attack clips must not fill every combat action"
        );
    }

    #[test]
    fn player_character_controller_settings_drive_cooked_character() {
        let mut project = project_with_one_room();
        let controller_id = player_controller_component_id(&project);
        let scene = project.active_scene_mut();
        let controller = scene.node_mut(controller_id).unwrap();
        let NodeKind::CharacterController { settings, .. } = &mut controller.kind else {
            panic!("starter player controller must be a Character Controller");
        };
        settings.walk_speed = 61;
        settings.run_speed = 133;
        settings.turn_speed_degrees_per_second = 270;
        settings.stamina_max_q12 = 2048;
        settings.roll_speed = 144;

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let character = &package.expect("package returned on ok report").characters[0];
        assert_eq!(character.walk_speed, 61);
        assert_eq!(character.run_speed, 133);
        assert_eq!(character.turn_speed_degrees_per_second, 270);
        assert_eq!(character.stamina_max_q12, 2048);
        assert_eq!(character.roll_speed, 144);
    }

    #[test]
    fn player_model_renderer_visual_transform_drives_cooked_character() {
        let mut project = project_with_one_room();
        let spawn_id = player_spawn_node_id(&project);
        let scene = project.active_scene_mut();
        let renderer_id = scene
            .node(spawn_id)
            .and_then(|node| {
                node.children.iter().find_map(|child| {
                    scene.node(*child).and_then(|node| {
                        matches!(node.kind, NodeKind::ModelRenderer { .. }).then_some(node.id)
                    })
                })
            })
            .expect("starter player has a model renderer");
        let renderer = scene.node_mut(renderer_id).unwrap();
        let NodeKind::ModelRenderer {
            visual_offset,
            visual_scale_q8,
            ..
        } = &mut renderer.kind
        else {
            panic!("expected model renderer");
        };
        *visual_offset = [32, -16, 48];
        *visual_scale_q8 = crate::MODEL_SCALE_ONE_Q8 + 64;
        renderer.transform.rotation_degrees[1] = 45.0;

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let character = &package.expect("package returned on ok report").characters[0];
        assert_eq!(character.visual_offset, [32, -16, 48]);
        assert_eq!(character.visual_yaw, 512);
        assert_eq!(character.visual_scale_q8, crate::MODEL_SCALE_ONE_Q8 + 64);
    }

    #[test]
    fn player_character_model_is_deduplicated_with_renderer_component() {
        // Starter includes both a ModelRenderer component and a
        // Character resource on the player
        // entity. The cooker must register the model once, but
        // must not also emit a static model instance for the
        // player-controlled renderer.
        let project = project_with_one_room();
        let (package, _report) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");
        assert_eq!(
            package.models.len(),
            1,
            "shared model should be registered once across ModelRenderer + Character"
        );
        // The player character references the model; the authored
        // renderer component is consumed by the player path, not
        // emitted as a second static draw.
        assert_eq!(package.characters[0].model, 0);
        assert!(package.model_instances.is_empty());
    }

    #[test]
    fn player_character_model_lands_in_room_residency_without_placed_meshinstance() {
        // Simulate a project where the player Character points
        // at a Model that *isn't* also placed as a MeshInstance.
        // The starter has both, so we delete the placed renderer
        // before cooking and assert residency still picks up the
        // Wraith mesh + atlas + clips via the player path.
        let mut project = project_with_one_room();
        remove_model_renderer_components(&mut project);
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("package returned on ok report");
        // Only the player path should have registered the model
        // -- there's no MeshInstance left to pull it in.
        assert!(package.model_instances.is_empty());
        assert_eq!(package.models.len(), 1);
        assert_eq!(package.characters.len(), 1);

        let manifest = render_manifest_source(&package);
        // Asset indexes for the player mesh, atlas, and clips
        // come straight from `package.assets` -- every one of
        // them must show up in ROOM_0_REQUIRED_RAM/VRAM.
        let wraith = &package.models[0];
        let mesh_token = format!("AssetId({})", wraith.mesh_asset_index);
        assert!(
            manifest_contains_required(&manifest, "RAM", 0, &mesh_token),
            "RAM missing player mesh: {mesh_token}"
        );
        let atlas_token = format!(
            "AssetId({})",
            wraith
                .texture_asset_index
                .expect("starter wraith has atlas")
        );
        assert!(
            manifest_contains_required(&manifest, "VRAM", 0, &atlas_token),
            "VRAM missing player atlas: {atlas_token}"
        );
        let cf = wraith.clip_first as usize;
        let cc = wraith.clip_count as usize;
        for clip in &package.model_clips[cf..cf + cc] {
            let tok = format!("AssetId({})", clip.animation_asset_index);
            assert!(
                manifest_contains_required(&manifest, "RAM", 0, &tok),
                "RAM missing clip {}: {tok}",
                clip.name
            );
        }
    }

    #[test]
    fn player_character_model_assets_dedupe_with_placed_meshinstance() {
        // Starter's player model is referenced twice: by the
        // placed renderer and by the Character. Each asset still
        // shows up exactly once in the manifest's residency
        // slice -- the player path mustn't double-add.
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");
        let manifest = render_manifest_source(&package);
        let wraith = &package.models[0];

        let mesh_token = format!("AssetId({})", wraith.mesh_asset_index);
        assert_eq!(
            count_required_occurrences(&manifest, "RAM", 0, &mesh_token),
            1,
            "player mesh appears more than once in RAM residency"
        );
        let atlas = wraith.texture_asset_index.unwrap();
        let atlas_token = format!("AssetId({atlas})");
        assert_eq!(
            count_required_occurrences(&manifest, "VRAM", 0, &atlas_token),
            1,
            "wraith atlas appears more than once in VRAM residency"
        );
    }

    /// `true` when `ROOM_<idx>_REQUIRED_<kind>` contains `token`.
    fn manifest_contains_required(manifest: &str, kind: &str, idx: u16, token: &str) -> bool {
        count_required_occurrences(manifest, kind, idx, token) > 0
    }

    /// Count occurrences of `token` inside the
    /// `ROOM_<idx>_REQUIRED_<kind>` slice declaration. Robust
    /// enough for residency assertions; not a full Rust parser.
    fn count_required_occurrences(manifest: &str, kind: &str, idx: u16, token: &str) -> usize {
        let header = format!("ROOM_{idx}_REQUIRED_{kind}: &[AssetId] = &[");
        let Some(start) = manifest.find(&header) else {
            return 0;
        };
        let body = &manifest[start + header.len()..];
        let Some(end) = body.find("];") else {
            return 0;
        };
        body[..end].matches(token).count()
    }

    #[test]
    fn rendered_manifest_includes_characters_and_player_controller() {
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let manifest = render_manifest_source(&package.unwrap());
        assert!(manifest.contains("pub static CHARACTERS:"));
        assert!(manifest.contains("LevelCharacterRecord"));
        assert!(manifest.contains("pub static PLAYER_CONTROLLER:"));
        assert!(manifest.contains("Some(PlayerControllerRecord"));
        assert!(manifest.contains("CHARACTER_CLIP_NONE"));
    }

    #[test]
    fn player_spawn_with_invalid_idle_clip_fails_validation() {
        let mut project = project_with_one_room();
        let scene = project.active_scene();
        let character_id = scene
            .nodes()
            .iter()
            .find_map(|_| {
                project.resources.iter().find_map(|r| match &r.data {
                    crate::ResourceData::Character(_) => Some(r.id),
                    _ => None,
                })
            })
            .expect("starter has a Character");
        // Bump idle clip past the model's clip count so cook
        // validation must reject.
        if let Some(resource) = project.resource_mut(character_id) {
            if let crate::ResourceData::Character(c) = &mut resource.data {
                c.animation_set = None;
                c.idle_clip = Some(99);
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(report.errors.iter().any(|e| e.contains("idle clip 99")));
    }

    #[test]
    fn legacy_spawn_without_character_assignment_auto_picks_when_one_exists() {
        // Keep exactly one Character. Component-authored players use
        // their Model Renderer directly, so this legacy auto-pick path
        // is only for SpawnPoint-authored projects.
        let mut project = project_with_one_room();
        let player_character = player_character_resource_id(&project);
        project.resources.retain(|resource| {
            !matches!(resource.data, ResourceData::Character(_)) || resource.id == player_character
        });
        demote_player_spawns(&mut project);
        let room_id = project
            .active_scene()
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        let spawn_id = project.active_scene_mut().add_node(
            room_id,
            "Legacy Player Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        if let Some(node) = project.active_scene_mut().node_mut(spawn_id) {
            node.transform.translation = [0.0, 0.0, 0.0];
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(
            report.is_ok(),
            "errors: {:?}; warnings: {:?}",
            report.errors,
            report.warnings
        );
        let package = package.expect("auto-pick should succeed");
        assert!(package.player_controller.is_some());
        assert!(report.warnings.iter().any(|w| w.contains("auto-picked")));
    }

    #[test]
    fn component_player_without_profile_uses_model_renderer_and_animator() {
        let mut project = project_with_one_room();
        let model = player_model_resource_id(&project);
        let controller_id = player_controller_component_id(&project);
        let scene = project.active_scene_mut();
        if let Some(controller) = scene.node_mut(controller_id) {
            let NodeKind::CharacterController {
                character,
                settings,
                ..
            } = &mut controller.kind
            else {
                panic!("starter player controller must be a Character Controller");
            };
            *character = None;
            settings.walk_speed = 77;
        }
        let player = player_spawn_node_id(&project);
        let animator_id = project
            .active_scene()
            .node(player)
            .and_then(|node| {
                node.children.iter().find_map(|id| {
                    project.active_scene().node(*id).and_then(|child| {
                        matches!(child.kind, NodeKind::Animator { .. }).then_some(child.id)
                    })
                })
            })
            .expect("starter player has Animator component");
        if let Some(animator) = project.active_scene_mut().node_mut(animator_id) {
            let NodeKind::Animator { action_clips, .. } = &mut animator.kind else {
                panic!("expected Animator component");
            };
            action_clips.push(crate::CharacterActionClip {
                action: CharacterAnimationAction::Idle,
                clip: 0,
                options: None,
            });
            action_clips.push(crate::CharacterActionClip {
                action: CharacterAnimationAction::Walk,
                clip: 0,
                options: None,
            });
            action_clips.push(crate::CharacterActionClip {
                action: CharacterAnimationAction::Backstep,
                clip: 0,
                options: Some(crate::CharacterActionOptions {
                    looping: true,
                    in_place: false,
                }),
            });
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        assert!(
            !report.warnings.iter().any(|w| w.contains("auto-picked")),
            "component player should not secretly auto-pick a profile: {:?}",
            report.warnings
        );
        let package = package.expect("component player cooks");
        let character = &package.characters[0];
        assert_eq!(character.walk_speed, 77);
        assert_eq!(
            package.models[character.model as usize].source_resource,
            model
        );
        assert_eq!(
            character.action_clips[CharacterAnimationAction::Backstep.to_index()],
            0
        );
        assert_eq!(
            character.action_flags[CharacterAnimationAction::Backstep.to_index()],
            character_action_flags::LOOPING | character_action_flags::IN_PLACE_OVERRIDE
        );
    }

    #[test]
    fn character_controller_with_zero_radius_fails_validation() {
        let mut project = project_with_one_room();
        let controller_id = player_controller_component_id(&project);
        if let Some(node) = project.active_scene_mut().node_mut(controller_id) {
            if let NodeKind::CharacterController { settings, .. } = &mut node.kind {
                settings.radius = 0;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(report
            .errors
            .iter()
            .any(|e| e.contains("radius must be > 0")));
    }

    #[test]
    fn legacy_spawn_without_character_field_still_loads() {
        // Older project.ron files lacked `character` on
        // SpawnPoint. `#[serde(default)]` should fill it with
        // `None` so they keep deserializing.
        let ron = r#"(
            name: "Legacy",
            scenes: [(
                name: "Main",
                root: (1),
                next_node_id: 3,
                nodes: [
                    (id: (1), name: "Root", kind: Node3D, transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)), parent: None, children: [(2)]),
                    (id: (2), name: "Spawn", kind: SpawnPoint(player: true), transform: (translation: (0.0, 0.0, 0.0), rotation_degrees: (0.0, 0.0, 0.0), scale: (1.0, 1.0, 1.0)), parent: Some((1)), children: []),
                ],
            )],
            resources: [],
            next_resource_id: 1,
        )"#;
        let project = ProjectDocument::from_ron_str(ron).expect("legacy spawn deserializes");
        let scene = project.active_scene();
        let spawn = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::SpawnPoint { player: true, .. }))
            .expect("spawn round-tripped");
        if let NodeKind::SpawnPoint { character, .. } = &spawn.kind {
            assert!(character.is_none(), "missing field should default to None");
        }
    }

    #[test]
    fn character_resource_roundtrips_through_ron() {
        use crate::CharacterResource;
        let mut project = ProjectDocument::starter();
        let id = project.add_resource(
            "Test Character",
            crate::ResourceData::Character(CharacterResource {
                model: None,
                animation_set: None,
                idle_clip: Some(0),
                walk_clip: Some(1),
                run_clip: None,
                turn_clip: None,
                radius: 200,
                height: 1024,
                walk_speed: 50,
                run_speed: 100,
                turn_speed_degrees_per_second: 240,
                camera_distance: 1500,
                camera_height: 800,
                camera_target_height: 600,
                ..CharacterResource::defaults()
            }),
        );
        let serialized = project.to_ron_string().expect("serializes");
        let reloaded = ProjectDocument::from_ron_str(&serialized).expect("deserializes");
        let resource = reloaded.resource(id).expect("character preserved");
        match &resource.data {
            crate::ResourceData::Character(c) => {
                assert_eq!(c.idle_clip, Some(0));
                assert_eq!(c.walk_clip, Some(1));
                assert_eq!(c.radius, 200);
                assert_eq!(c.walk_speed, 50);
                assert_eq!(
                    c.roll_active_frames,
                    CharacterResource::defaults().roll_active_frames
                );
                assert_eq!(c.camera_target_height, 600);
            }
            _ => panic!("character resource lost its variant after round-trip"),
        }
    }

    #[test]
    fn starter_project_emits_expected_texture_assets() {
        // Starter cooks one room texture, one sky panorama, and the player atlas.
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");
        assert_eq!(package.texture_asset_count(), 3);
        assert!(package.rooms[0]
            .sky
            .cloud_layer
            .texture_asset_index
            .is_some());
    }

    #[test]
    fn starter_project_emits_one_model_with_clips() {
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");
        assert_eq!(package.models.len(), 1);
        assert_eq!(
            package.models[0].collision_radius,
            crate::default_model_collision_radius_for_height(package.models[0].world_height)
        );
        assert_eq!(package.model_instances.len(), 0);
        assert!(!package.model_clips.is_empty());
        assert_eq!(package.model_mesh_asset_count(), 1);
        assert_eq!(
            package.model_animation_asset_count(),
            package.model_clips.len()
        );
        assert_eq!(package.model_clip_bounds.len(), package.model_clips.len());
        assert!(!package.model_frame_bounds.is_empty());
        for bounds in &package.model_clip_bounds {
            let first = bounds.first_frame as usize;
            let count = bounds.frame_count as usize;
            assert!(count > 0);
            assert!(first + count <= package.model_frame_bounds.len());
            assert!(package.model_frame_bounds[first].radius > 0);
            assert_eq!(
                bounds.floor_y, package.model_frame_bounds[first].floor_y,
                "clip floor anchor should use its first cooked frame floor"
            );
            assert_ne!(package.model_frame_bounds[first].floor_y, i32::MIN);
        }
    }

    #[test]
    fn starter_room_material_slice_matches_cook() {
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");
        let room = &package.rooms[0];
        // Slice indices are valid.
        let first = room.material_first as usize;
        let count = room.material_count as usize;
        assert!(first + count <= package.materials.len());
        // Each material in the slice belongs to room 0 and has a
        // unique local_slot.
        let slice = &package.materials[first..first + count];
        let mut slots: Vec<u16> = slice.iter().map(|m| m.local_slot).collect();
        slots.sort();
        let mut dedup = slots.clone();
        dedup.dedup();
        assert_eq!(slots, dedup, "duplicate local_slot in room slice");
        for material in slice {
            assert_eq!(material.room, 0);
        }
    }

    #[test]
    fn starter_residency_includes_world_and_textures() {
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("starter cooks");

        let room = &package.rooms[0];
        let first = room.material_first as usize;
        let count = room.material_count as usize;
        let mut texture_ids: Vec<usize> = package.materials[first..first + count]
            .iter()
            .map(|m| m.texture_asset_index)
            .collect();
        texture_ids.sort();
        texture_ids.dedup();

        // Sanity: every texture asset index is a Texture asset.
        for &i in &texture_ids {
            assert_eq!(package.assets[i].kind, PlaytestAssetKind::Texture);
        }
        // Room asset is a RoomWorld at the recorded index.
        assert_eq!(
            package.assets[room.world_asset_index].kind,
            PlaytestAssetKind::RoomWorld,
        );
    }

    #[test]
    fn empty_project_fails_validation() {
        let mut project = ProjectDocument::starter();
        project.scenes[0] = crate::Scene::new("Empty");
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(!report.is_ok());
        assert!(report.errors.iter().any(|e| e.contains("Room")));
    }

    #[test]
    fn project_with_no_player_spawn_fails_validation() {
        let mut project = ProjectDocument::starter();
        demote_player_spawns(&mut project);
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(report.errors.iter().any(|e| e.contains("player")));
    }

    #[test]
    fn project_with_multiple_player_spawns_fails_validation() {
        let mut project = ProjectDocument::starter();
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .expect("starter has a room");
        scene.add_node(
            room_id,
            "Spawn 2",
            NodeKind::SpawnPoint {
                player: true,
                character: None,
            },
        );
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(report.errors.iter().any(|e| e.contains("exactly one")));
    }

    #[test]
    fn rendered_manifest_imports_psx_level_and_static_blocks() {
        let project = project_with_one_room();
        let (package, _) = build_package(&project, &starter_project_root());
        let src = render_manifest_source(&package.expect("starter cooks"));
        assert!(src.contains("use psx_level::"));
        assert!(src.contains("pub static ASSETS"));
        assert!(src.contains("pub static MATERIALS"));
        assert!(src.contains("pub static ROOMS"));
        assert!(src.contains("pub static ROOM_CHUNKS"));
        assert!(src.contains("LevelSkyRecord"));
        assert!(src.contains("sky: LevelSkyRecord"));
        assert!(src.contains("LevelFarVistaRecord"));
        assert!(src.contains("far_vista: LevelFarVistaRecord"));
        assert!(src.contains("LevelCameraRecord"));
        assert!(src.contains("camera: LevelCameraRecord"));
        assert!(src.contains("pub static ROOM_VISIBILITY"));
        assert!(src.contains("pub static VISIBILITY_PVS"));
        assert!(src.contains("pub static VISIBILITY_PVS_BITS"));
        assert!(src.contains("pub static VISIBILITY_CELLS"));
        assert!(src.contains("pub static ROOM_SURFACE_CACHES"));
        assert!(src.contains("pub static ROOM_CACHE_CELLS"));
        assert!(src.contains("pub static ROOM_CACHE_VERTICES"));
        assert!(src.contains("pub static ROOM_CACHE_SURFACES"));
        assert!(src.contains("pub static ROOM_RESIDENCY"));
        assert!(src.contains("pub static PLAYER_SPAWN"));
        assert!(src.contains("pub static ENTITIES"));
        assert!(src.contains("include_bytes!(\"rooms/"));
        assert!(src.contains("include_bytes!(\"textures/"));
    }

    #[test]
    fn cook_to_dir_writes_manifest_rooms_and_textures() {
        let project = ProjectDocument::starter();
        let dir = std::env::temp_dir().join(format!(
            "psxed-playtest-cook-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let report = cook_to_dir(&project, &starter_project_root(), &dir).expect("cook IO");
        assert!(report.is_ok(), "errors: {:?}", report.errors);

        let manifest = std::fs::read_to_string(dir.join(COOKED_MANIFEST_FILENAME))
            .expect("cooked manifest written");
        assert!(manifest.contains("rooms/room_000.psxw"));
        assert!(manifest.contains("textures/texture_000.psxt"));
        assert!(
            !dir.join(MANIFEST_FILENAME).exists(),
            "cook should not overwrite the tracked placeholder manifest"
        );
        let world_pack_order = std::fs::read_to_string(dir.join(WORLD_PACK_ORDER_FILENAME))
            .expect("world pack order written");
        assert!(world_pack_order.lines().any(|line| line.trim() == "0"));

        let blob = std::fs::read(dir.join(ROOMS_DIRNAME).join("room_000.psxw"))
            .expect("room blob written");
        assert_eq!(&blob[0..4], b"PSXW");

        // Room texture blobs land in generated/textures. Model
        // atlases are stored under generated/models/<model>/.
        assert!(dir
            .join(TEXTURES_DIRNAME)
            .join("texture_000.psxt")
            .is_file());
        assert!(dir
            .join(MODELS_DIRNAME)
            .join("model_000_crimson_cross_knight")
            .join("atlas.psxt")
            .is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cook_to_dir_purges_stale_assets() {
        // Drop a fake stale file in textures/ before cooking;
        // the writer should remove it so the generated tree only
        // references files that survive this run.
        let project = ProjectDocument::starter();
        let dir = std::env::temp_dir().join(format!(
            "psxed-playtest-purge-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let textures_dir = dir.join(TEXTURES_DIRNAME);
        std::fs::create_dir_all(&textures_dir).unwrap();
        let stale = textures_dir.join("texture_999.psxt");
        std::fs::write(&stale, b"stale").unwrap();

        let report = cook_to_dir(&project, &starter_project_root(), &dir).expect("cook IO");
        assert!(report.is_ok());
        assert!(!stale.exists(), "stale texture_999.psxt should be purged");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_cook_removes_stale_cooked_manifest() {
        let mut project = ProjectDocument::starter();
        demote_player_spawns(&mut project);

        let dir = std::env::temp_dir().join(format!(
            "psxed-playtest-stale-manifest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let cooked_manifest = dir.join(COOKED_MANIFEST_FILENAME);
        std::fs::write(&cooked_manifest, "stale cooked manifest").unwrap();

        let report = cook_to_dir(&project, &starter_project_root(), &dir).expect("cook IO");
        assert!(!report.is_ok());
        assert!(
            !cooked_manifest.exists(),
            "failed cook should not leave stale cooked manifest"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn texture_shared_across_materials_emits_single_asset() {
        // Two materials in the starter both use the first Delven texture.
        // After cook + package the texture should appear once in
        // ASSETS even though both materials reference it.
        let mut project = ProjectDocument::starter();
        // Find the starter room texture id and an existing material to
        // clone-and-retint as a second material referencing the
        // same texture.
        let room_texture_id = project
            .resources
            .iter()
            .find_map(|r| match &r.data {
                ResourceData::Texture { psxt_path }
                    if psxt_path.ends_with("delven_01_slateflr1a_q2.psxt") =>
                {
                    Some(r.id)
                }
                _ => None,
            })
            .expect("starter has first Delven texture");

        // Reassign every wall material in the room to a new
        // material that *also* points at the same room texture. After
        // cook the world has 2 cooker material slots (floor + the
        // new wall material) but both resolve to the same texture,
        // so playtest should emit 1 texture asset.
        let new_material_id = project.add_resource(
            "DelvenOnWalls",
            ResourceData::Material(crate::MaterialResource::opaque(Some(room_texture_id))),
        );
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .expect("starter has a room");
        if let Some(node) = scene.node_mut(room_id) {
            if let NodeKind::Room { grid } = &mut node.kind {
                // The minimal starter is a single floor tile with
                // no walls. Grow to a 2x1 grid and add a north wall
                // on the new cell so the test has a wall material
                // alongside the floor. The original cell keeps its
                // starter Delven material; the new cell's floor and
                // wall both use new_material_id, giving the cooker
                // two distinct material slots that both share the
                // same Delven texture.
                let sector_size = grid.sector_size;
                let (sx, sz) =
                    grid.extend_to_include(grid.origin[0] + grid.width as i32, grid.origin[1]);
                grid.set_floor(sx, sz, 0, Some(new_material_id));
                grid.add_wall(
                    sx,
                    sz,
                    crate::GridDirection::North,
                    0,
                    sector_size,
                    Some(new_material_id),
                );
            }
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        // 2 distinct material slots both reference the same
        // texture (room material dedup); the model atlas adds
        // one more texture so the total is 2 -- what we're
        // testing here is that walls don't double-count their
        // shared room texture, not the absolute count.
        let room_texture_slots: Vec<_> = package
            .materials
            .iter()
            .filter(|material| {
                let asset = &package.assets[material.texture_asset_index];
                asset.filename == "texture_000.psxt"
            })
            .collect();
        assert!(
            room_texture_slots.len() >= 2,
            "expected at least two cooked material slots to share the first Delven texture"
        );
        let first_room_asset = room_texture_slots[0].texture_asset_index;
        assert!(room_texture_slots
            .iter()
            .all(|material| material.texture_asset_index == first_room_asset));
    }

    #[test]
    fn material_sidedness_reaches_playtest_manifest_flags() {
        let mut project = ProjectDocument::starter();
        let material = project
            .resources
            .iter_mut()
            .find_map(|resource| match &mut resource.data {
                ResourceData::Material(material) => Some(material),
                _ => None,
            })
            .expect("starter has a material");
        material.face_sidedness = crate::MaterialFaceSidedness::Back;
        material.sync_legacy_sidedness();

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert!(package
            .materials
            .iter()
            .any(|m| m.face_sidedness == crate::MaterialFaceSidedness::Back));

        let src = render_manifest_source(&package);
        assert!(
            src.contains("flags: 1"),
            "back-sided material should encode FACE_BACK in flags"
        );
    }

    #[test]
    fn missing_texture_path_fails_with_clear_error() {
        // Point a texture resource at a bogus path; cook should
        // refuse and the error should mention the file.
        let mut project = ProjectDocument::starter();
        let target = project
            .resources
            .iter_mut()
            .find_map(|r| match &mut r.data {
                ResourceData::Texture { psxt_path } => Some(psxt_path),
                _ => None,
            })
            .expect("starter has at least one texture");
        *target = "this/does/not/exist.psxt".to_string();

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("does/not/exist")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn missing_model_mesh_path_fails_with_clear_error() {
        // Bend the starter player's model resource at a bogus mesh
        // path; cook should refuse rather than silently
        // emitting a Model record without bytes.
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.model_path = "no/such/model.psxmdl".to_string();
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("no/such/model.psxmdl")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn animation_clip_override_out_of_range_fails() {
        // Author a per-instance clip override past the model's
        // clip count → cook refuses with an explicit error
        // mentioning the offending node.
        let mut project = ProjectDocument::starter();
        set_first_model_instance_clip(&mut project, 999);
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("clip override 999 out of range")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn model_with_no_atlas_fails_when_placed() {
        // Strip the starter player's texture_path; cook must
        // refuse the placed instance instead of silently
        // dropping it at runtime.
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.texture_path = None;
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("no atlas")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn model_with_no_clips_fails_when_placed() {
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.skeleton = None;
                model.clips.clear();
                model.default_clip = None;
                model.preview_clip = None;
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report
                .errors
                .iter()
                .any(|e| e.contains("no animation clips")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn starter_project_emits_one_light_record() {
        // Starter Stone Room ships with a "Preview Light" node.
        // It should now appear in `package.lights` with a
        // sensible intensity_q8 derived from the editor's
        // authored intensity float.
        let project = ProjectDocument::starter();
        let expected_color = starter_light_color(&project);
        let expected_intensity_q8 = starter_light_intensity_q8(&project);
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("starter cooks");
        assert_eq!(package.lights.len(), 1);
        let light = package.lights[0];
        assert_eq!(light.room, 0);
        assert!(light.radius > 0);
        assert_eq!(light.intensity_q8, expected_intensity_q8);
        assert_eq!(light.color, expected_color);
    }

    #[test]
    fn chunk_boundary_light_is_emitted_for_each_overlapped_chunk() {
        let mut project = project_with_one_room();
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let room_id = {
            let scene = project.active_scene();
            scene
                .nodes()
                .iter()
                .find(|n| matches!(n.kind, NodeKind::Room { .. }))
                .expect("starter has a room")
                .id
        };
        if let Some(room) = project.active_scene_mut().node_mut(room_id) {
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("starter room is a room");
            };
            *grid = crate::WorldGrid::empty(1, 40, crate::DEFAULT_WORLD_SECTOR_SIZE);
            for z in 0..grid.depth {
                grid.set_floor(0, z, 0, Some(floor_material));
            }
        }
        for id in starter_light_ids(&project) {
            let Some(light) = project.active_scene_mut().node_mut(id) else {
                continue;
            };
            light.transform.translation = [0.0, 0.0, -10.5];
            let NodeKind::PointLight { radius, .. } = &mut light.kind else {
                continue;
            };
            *radius = 2.0;
        }
        let player_character = player_character_resource_id(&project);
        demote_player_spawns(&mut project);
        let spawn_id = project.active_scene_mut().add_node(
            room_id,
            "Chunk Test Spawn",
            NodeKind::SpawnPoint {
                player: true,
                character: Some(player_character),
            },
        );
        if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
            spawn.transform.translation = [0.0, 0.0, -19.0];
        }

        let (package, report) = build_package(&project, &starter_project_root());

        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.rooms.len(), 4);
        assert_eq!(package.lights.len(), 2);
        assert!(package.lights.iter().any(|light| light.room == 0));
        assert!(package.lights.iter().any(|light| light.room == 1));
    }

    #[test]
    fn chunk_boundary_floor_transition_wall_stays_with_canonical_chunk() {
        let mut project = project_with_one_room();
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let room_id = project
            .active_scene()
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .expect("starter has a room")
            .id;
        if let Some(room) = project.active_scene_mut().node_mut(room_id) {
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("starter room is a room");
            };
            *grid = crate::WorldGrid::empty(17, 1, crate::DEFAULT_WORLD_SECTOR_SIZE);
            for x in 0..grid.width {
                let height = if x < 16 { 0 } else { 512 };
                grid.set_floor(x, 0, height, Some(floor_material));
            }
        }
        let player = player_spawn_node_id(&project);
        if let Some(node) = project.active_scene_mut().node_mut(player) {
            node.transform.translation = [0.0, 0.0, 0.0];
        }

        let (package, report) = build_package(&project, &starter_project_root());

        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.rooms.len(), 2);
        let mut transition_walls = 0usize;
        for room in &package.rooms {
            let world = psx_asset::World::from_bytes(&package.assets[room.world_asset_index].bytes)
                .expect("chunk psxw parses");
            for x in 0..world.width() {
                for z in 0..world.depth() {
                    let Some(sector) = world.sector(x, z) else {
                        continue;
                    };
                    for local_wall in 0..sector.wall_count() {
                        let wall = world.sector_wall(sector, local_wall).expect("wall exists");
                        if wall.direction() == psxw::direction::EAST
                            && wall.heights() == [0, 0, 512, 512]
                        {
                            transition_walls += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(transition_walls, 1);
    }

    #[test]
    fn starter_project_bakes_static_surface_lights() {
        let project = ProjectDocument::starter();
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("starter cooks");
        let room = &package.rooms[0];
        let asset = &package.assets[room.world_asset_index];
        let world = psx_asset::World::from_bytes(&asset.bytes).expect("room psxw parses");
        assert!(world.static_vertex_lighting());
        assert!((0..world.surface_light_count())
            .filter_map(|index| world.surface_light(index))
            .any(|light| light.vertex_rgb().iter().any(|rgb| *rgb != [0, 0, 0])));
    }

    #[test]
    fn diagonal_walls_bake_static_surface_lights() {
        use crate::world_cook::{
            CookedGridSector, CookedGridVerticalFace, CookedGridWalls, DEFAULT_BAKED_VERTEX_RGB,
        };
        use crate::{MaterialFaceSidedness, PsxBlendMode};

        fn diagonal_wall(heights: [i32; 4]) -> CookedGridVerticalFace {
            CookedGridVerticalFace {
                heights,
                material: 0,
                shape: psxw::wall_shape::QUAD,
                uvs: psxw::WALL_UVS,
                baked_vertex_rgb: DEFAULT_BAKED_VERTEX_RGB,
                solid: true,
            }
        }

        let source = ProjectDocument::starter().resources[0].id;
        let mut room = CookedRoomBakeInput {
            room_index: 0,
            world_asset_index: 0,
            world_origin: [0, 0],
            cooked: CookedWorldGrid {
                width: 1,
                depth: 1,
                sector_size: 1024,
                sectors: vec![Some(CookedGridSector {
                    floor: None,
                    ceiling: None,
                    walls: CookedGridWalls {
                        north_west_south_east: vec![diagonal_wall([0, 16, 1024, 1008])],
                        north_east_south_west: vec![diagonal_wall([32, 48, 960, 944])],
                        ..CookedGridWalls::default()
                    },
                })],
                materials: vec![CookedWorldMaterial {
                    slot: 0,
                    source,
                    texture: None,
                    blend_mode: PsxBlendMode::Opaque,
                    tint: [128, 128, 128],
                    face_sidedness: MaterialFaceSidedness::Both,
                }],
                ambient_color: [32, 24, 16],
                static_vertex_lighting: true,
                fog_enabled: false,
                fog_color: [0, 0, 0],
                fog_near: 0,
                fog_far: 0,
            },
        };

        bake_static_surface_lights(std::slice::from_mut(&mut room), &[]);

        let sector = room.cooked.sectors[0].as_ref().expect("sector");
        let cases = [
            (
                psxw::direction::NORTH_WEST_SOUTH_EAST,
                &sector.walls.north_west_south_east[0],
            ),
            (
                psxw::direction::NORTH_EAST_SOUTH_WEST,
                &sector.walls.north_east_south_west[0],
            ),
        ];
        for (direction, wall) in cases {
            let verts =
                wall_vertices(0, 0, 1024, direction, wall.heights).expect("diagonal wall vertices");
            let expected = bake_surface_vertex_rgb(
                &room.cooked.materials,
                room.cooked.ambient_color,
                verts,
                wall.material,
                &[],
            );
            assert_ne!(expected, DEFAULT_BAKED_VERTEX_RGB);
            assert_eq!(wall.baked_vertex_rgb, expected);
        }
    }

    #[test]
    fn light_with_zero_radius_fails() {
        let mut project = ProjectDocument::starter();
        let ids = starter_light_ids(&project);
        let scene = project.active_scene_mut();
        for id in ids {
            if let Some(node) = scene.node_mut(id) {
                match &mut node.kind {
                    NodeKind::PointLight { radius, .. } => *radius = 0.0,
                    _ => {}
                }
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("radius")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn light_with_negative_intensity_fails() {
        let mut project = ProjectDocument::starter();
        let ids = starter_light_ids(&project);
        let scene = project.active_scene_mut();
        for id in ids {
            if let Some(node) = scene.node_mut(id) {
                match &mut node.kind {
                    NodeKind::PointLight { intensity, .. } => *intensity = -0.5,
                    _ => {}
                }
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("intensity")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn light_radius_converts_sectors_to_world_units() {
        // Author a 4-sector radius; cook stores world units using
        // the room's current sector size.
        let mut project = ProjectDocument::starter();
        let sector_size = project
            .active_scene()
            .nodes()
            .iter()
            .find_map(|node| match &node.kind {
                NodeKind::Room { grid } => Some(grid.sector_size),
                _ => None,
            })
            .expect("starter has a room");
        let ids = starter_light_ids(&project);
        let scene = project.active_scene_mut();
        for id in ids {
            if let Some(node) = scene.node_mut(id) {
                match &mut node.kind {
                    NodeKind::PointLight { radius, .. } => *radius = 4.0,
                    _ => {}
                }
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.lights[0].radius, (sector_size * 4) as u16);
    }

    #[test]
    fn rendered_manifest_emits_lights_block() {
        let project = ProjectDocument::starter();
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("cooks");
        let color = package.lights[0].color;
        let src = render_manifest_source(&package);
        assert!(src.contains("PointLightRecord"));
        assert!(src.contains("pub static LIGHTS"));
        assert!(!src.contains("SurfaceLightRecord"));
        assert!(!src.contains("SURFACE_LIGHTS"));
        assert!(src.contains("intensity_q8"));
        assert!(src.contains(&format!(
            "color: [{}, {}, {}]",
            color[0], color[1], color[2]
        )));
    }

    #[test]
    fn equipment_component_emits_weapon_and_hitbox_records() {
        let starter = ProjectDocument::starter();
        let mut starter_model = starter
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::Model(model) => Some(model.clone()),
                _ => None,
            })
            .expect("starter has a model");
        starter_model.clips.push(crate::ModelAnimationClip {
            name: "neutral idle".to_string(),
            psxanim_path: "assets/animations/standalone_fbx/neutral_idle.psxanim".to_string(),
            calibration: Default::default(),
        });
        let mut project = ProjectDocument::new("equipment-test");
        let texture = project.add_resource(
            "Floor Texture",
            ResourceData::Texture {
                psxt_path: "assets/textures/delven_01_slateflr1a_q2.psxt".to_string(),
            },
        );
        let material = project.add_resource(
            "Floor",
            ResourceData::Material(crate::MaterialResource::opaque(Some(texture))),
        );
        let model = project.add_resource("Wraith Model", ResourceData::Model(starter_model));
        let character = project.add_resource(
            "Wraith Character",
            ResourceData::Character(crate::CharacterResource {
                model: Some(model),
                idle_clip: Some(0),
                walk_clip: Some(0),
                run_clip: Some(0),
                ..crate::CharacterResource::defaults()
            }),
        );
        let weapon = project.add_resource(
            "Practice Sword",
            ResourceData::Weapon(crate::WeaponResource {
                model: Some(model),
                default_character_socket: "right_hand_grip".to_string(),
                grip: crate::WeaponGrip {
                    name: "grip".to_string(),
                    translation: [8, 16, 0],
                    rotation_q12: [0, 1024, 0],
                },
                hitboxes: vec![crate::WeaponHitbox {
                    name: "Blade".to_string(),
                    shape: crate::WeaponHitShape::Capsule {
                        start: [0, 0, 0],
                        end: [0, 640, 0],
                        radius: 32,
                    },
                    active_start_frame: 4,
                    active_end_frame: 9,
                }],
            }),
        );

        let scene = project.active_scene_mut();
        let mut grid = crate::WorldGrid::empty(2, 2, 1024);
        grid.set_floor(0, 0, 0, Some(material));
        grid.set_floor(1, 1, 0, Some(material));
        let room = scene.add_node(scene.root, "Room", NodeKind::Room { grid });
        let entity = scene.add_node(room, "Player", NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = [0.5, 0.0, 0.5];
        }
        scene.add_node(
            entity,
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: Some(model),
                material: None,
                visual_offset: [0; 3],
                visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
            },
        );
        scene.add_node(
            entity,
            "Animator",
            NodeKind::Animator {
                clip: Some(0),
                action_clips: Vec::new(),
                autoplay: true,
            },
        );
        scene.add_node(
            entity,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(character),
                settings: CharacterControllerSettings::default(),
                player: true,
            },
        );
        scene.add_node(
            entity,
            "Equipment",
            NodeKind::Equipment {
                weapon: Some(weapon),
                character_socket: "right_hand_grip".to_string(),
                weapon_grip: "grip".to_string(),
            },
        );

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.weapons.len(), 1);
        assert_eq!(package.equipment.len(), 1);
        assert_eq!(package.weapon_hitboxes.len(), 1);
        assert_eq!(package.model_sockets.len(), 1);
        assert_eq!(package.models[0].socket_first, 0);
        assert_eq!(package.models[0].socket_count, 1);
        assert_eq!(package.model_sockets[0].name, "right_hand_grip");
        assert_eq!(package.model_sockets[0].joint, 0);
        assert_eq!(package.weapons[0].model, Some(0));
        assert_eq!(package.weapons[0].grip_translation, [8, 16, 0]);
        assert_eq!(package.equipment[0].weapon, 0);
        assert_eq!(
            package.equipment[0].flags & psx_level::equipment_flags::PLAYER,
            psx_level::equipment_flags::PLAYER
        );

        let src = render_manifest_source(&package);
        assert!(src.contains("pub static MODEL_SOCKETS"));
        assert!(src.contains("LevelModelSocketRecord"));
        assert!(src.contains("pub static WEAPONS"));
        assert!(src.contains("pub static EQUIPMENT"));
        assert!(src.contains("WeaponHitShapeRecord::Capsule"));
    }

    #[test]
    fn out_of_range_model_default_clip_fails_at_cook() {
        // Bend the starter player's default_clip past its clip
        // count; cook must refuse rather than emit a runtime
        // record that resolves to no animation.
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.default_clip = Some(999);
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("default_clip 999")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn missing_default_clip_resolves_to_clip_zero() {
        // A model with `default_clip: None` plus a populated
        // clip list should cook fine -- runtime gets clip 0 as
        // the resolved default. No bind-pose sentinel.
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.default_clip = None;
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        let model = &package.models[0];
        assert_eq!(model.default_clip, 0);
        // Sanity: never emit the old u16::MAX sentinel.
        assert!(model.default_clip < model.clip_count);
    }

    #[test]
    fn playtest_packages_only_runtime_required_player_clips() {
        let project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        let authored_clip_count = project.resolved_model_animation_clips(player_model).len();
        assert!(
            authored_clip_count > 4,
            "starter should expose library clips for this regression"
        );

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        let (model_index, model) = package
            .models
            .iter()
            .enumerate()
            .find(|(_, model)| model.source_resource == player_model)
            .expect("player model is packaged");

        assert!(
            (model.clip_count as usize) < authored_clip_count,
            "runtime should not package the full editor animation library"
        );

        let character = package
            .characters
            .iter()
            .find(|character| character.model == model_index as u16)
            .expect("player character is packaged");
        for action in CharacterAnimationAction::ALL {
            let clip = character.action_clips[action.to_index()];
            if action.required_for_player() {
                assert!(clip < model.clip_count);
            } else if clip != CHARACTER_CLIP_NONE {
                assert!(clip < model.clip_count);
            }
        }
    }

    #[test]
    fn room_material_must_be_4bpp() {
        // Swap the starter's brick material to point at the
        // model's 8bpp atlas, which lives at the same project.
        // Cook should refuse the room material 8bpp depth.
        let mut project = ProjectDocument::starter();
        // Rewire the actually-used starter room texture to the
        // wraith atlas path so it parses but with the wrong CLUT
        // entry count.
        let used_texture = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::Material(material) => material.texture,
                _ => None,
            })
            .expect("starter has a used room texture");
        for resource in project.resources.iter_mut() {
            if let ResourceData::Texture { psxt_path } = &mut resource.data {
                if resource.id == used_texture {
                    *psxt_path = "assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt"
                        .to_string();
                }
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("must be 4bpp")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn model_atlas_must_be_8bpp() {
        // Swap the player atlas to a 4bpp room texture path so
        // the cook runs the depth check on a known-bad atlas.
        let mut project = ProjectDocument::starter();
        let player_model = player_model_resource_id(&project);
        for resource in project.resources.iter_mut() {
            if resource.id == player_model {
                let ResourceData::Model(model) = &mut resource.data else {
                    continue;
                };
                model.texture_path =
                    Some("assets/textures/delven_01_slateflr1a_q2.psxt".to_string());
                break;
            }
        }
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(package.is_none());
        assert!(
            report.errors.iter().any(|e| e.contains("must be 8bpp")),
            "errors: {:?}",
            report.errors,
        );
    }

    #[test]
    fn model_atlas_preserves_source_texture_flags() {
        let project = ProjectDocument::starter();
        let root = starter_project_root();
        let player_model = player_model_resource_id(&project);
        let source_texture_path = project
            .resources
            .iter()
            .find_map(|resource| {
                if resource.id != player_model {
                    return None;
                }
                let ResourceData::Model(model) = &resource.data else {
                    return None;
                };
                model.texture_path.as_deref()
            })
            .expect("starter player has a texture");
        let source_bytes = std::fs::read(root.join(source_texture_path)).expect("source atlas");
        let source_flags = psx_asset::Texture::from_bytes(&source_bytes)
            .expect("source atlas parses")
            .flags();

        let (package, report) = build_package(&project, &root);
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("starter cooks");
        let cooked_atlas = package
            .assets
            .iter()
            .find(|asset| asset.filename.ends_with("/atlas.psxt"))
            .expect("model atlas asset");
        let cooked_flags = psx_asset::Texture::from_bytes(&cooked_atlas.bytes)
            .expect("cooked atlas parses")
            .flags();

        assert_eq!(cooked_flags, source_flags);
    }

    #[test]
    fn two_instances_of_one_model_dedup_to_one_record() {
        // Add two explicit MeshInstances that reference the same
        // model resource as the starter's player. The cook
        // emits two `model_instances` but only one `models[]`
        // entry.
        let mut project = ProjectDocument::starter();
        let model_id = player_model_resource_id(&project);
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        for name in ["PlayerClone2", "PlayerClone3"] {
            scene.add_node(
                room_id,
                name,
                NodeKind::MeshInstance {
                    mesh: Some(model_id),
                    material: None,
                    animation_clip: None,
                },
            );
        }
        let (package, _) = build_package(&project, &starter_project_root());
        let package = package.expect("cooks");
        assert_eq!(package.models.len(), 1);
        assert_eq!(package.model_instances.len(), 2);
        // Both instances point at the same model index.
        assert_eq!(
            package.model_instances[0].model,
            package.model_instances[1].model
        );
    }

    #[test]
    fn entity_model_instance_preserves_authored_yaw() {
        let mut project = ProjectDocument::starter();
        let model_id = player_model_resource_id(&project);
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        let entity = scene.add_node(room_id, "Rotated Prop", NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.rotation_degrees[1] = 90.0;
        }
        scene.add_node(
            entity,
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: Some(model_id),
                material: None,
                visual_offset: [24, 8, -12],
                visual_scale_q8: crate::MODEL_SCALE_ONE_Q8 + 32,
            },
        );
        let renderer_id = scene
            .node(entity)
            .and_then(|node| node.children.first().copied())
            .expect("renderer child");
        scene
            .node_mut(renderer_id)
            .expect("renderer exists")
            .transform
            .rotation_degrees[1] = 45.0;

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.model_instances.len(), 1);
        assert_eq!(package.model_instances[0].yaw, 1024);
        assert_eq!(package.model_instances[0].visual_yaw, 512);
        assert_eq!(package.model_instances[0].visual_offset, [24, 8, -12]);
        assert_eq!(
            package.model_instances[0].visual_scale_q8,
            crate::MODEL_SCALE_ONE_Q8 + 32
        );
    }

    #[test]
    fn image_prop_preserves_authored_pitch_yaw_roll() {
        let mut project = ProjectDocument::starter();
        let material_id = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a material")
            .id;
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        let prop_id = scene.add_node(
            room_id,
            "Rotated Image Prop",
            NodeKind::ImageProp {
                material: Some(material_id),
                width: 256,
                height: 512,
                cylindrical_billboard: false,
                collision_enabled: false,
                collision_size: [256, 512, 64],
            },
        );
        if let Some(node) = scene.node_mut(prop_id) {
            node.transform.rotation_degrees = [45.0, 90.0, 270.0];
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        let prop = package
            .image_props
            .iter()
            .find(|prop| prop.width == 256 && prop.height == 512)
            .expect("image prop cooks");
        assert_eq!(prop.pitch, 512);
        assert_eq!(prop.yaw, 1024);
        assert_eq!(prop.roll, 3072);
    }

    #[test]
    fn non_player_character_controller_cooks_idle_model_instance_with_yaw() {
        let mut project = ProjectDocument::starter();
        let character_id = player_character_resource_id(&project);
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        let enemy = scene.add_node(room_id, "Facing Enemy", NodeKind::Entity);
        if let Some(node) = scene.node_mut(enemy) {
            node.transform.translation = [0.0, 0.0, 0.0];
            node.transform.rotation_degrees[1] = 180.0;
        }
        scene.add_node(
            enemy,
            "Character Controller",
            NodeKind::CharacterController {
                character: Some(character_id),
                settings: CharacterControllerSettings::default(),
                player: false,
            },
        );

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        assert!(
            !report
                .warnings
                .iter()
                .any(|w| w.contains("Non-player Entity character")),
            "non-player character controller should cook, warnings: {:?}",
            report.warnings
        );
        let package = package.expect("cooks");
        assert_eq!(package.model_instances.len(), 1);
        let instance = package.model_instances[0];
        assert_eq!(instance.yaw, 2048);
        assert_eq!(instance.model, package.characters[0].model);
        assert_eq!(
            instance.clip,
            package.characters[0].action_clips[CharacterAnimationAction::Idle.to_index()]
        );
    }

    #[test]
    fn entity_model_instance_y_snaps_to_floor_under_authored_xz() {
        let mut project = ProjectDocument::starter();
        let model_id = player_model_resource_id(&project);
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Room { .. }))
            .map(|n| n.id)
            .unwrap();
        if let Some(room) = scene.node_mut(room_id) {
            let NodeKind::Room { grid } = &mut room.kind else {
                panic!("starter room is a room");
            };
            let (sx, sz) = grid.editor_cells_to_array([0.0, 0.0]).unwrap();
            grid.set_floor(sx, sz, 512, Some(floor_material));
        }
        let entity = scene.add_node(room_id, "Floor Snapped Prop", NodeKind::Entity);
        if let Some(node) = scene.node_mut(entity) {
            node.transform.translation = [0.0, 9.0, 0.0];
        }
        scene.add_node(
            entity,
            "Model Renderer",
            NodeKind::ModelRenderer {
                model: Some(model_id),
                material: None,
                visual_offset: [0; 3],
                visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
            },
        );

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.expect("cooks");
        assert_eq!(package.model_instances.len(), 1);
        assert_eq!(package.model_instances[0].y, 512);
    }

    #[test]
    fn rendered_manifest_emits_model_records() {
        let project = ProjectDocument::starter();
        let (package, _) = build_package(&project, &starter_project_root());
        let src = render_manifest_source(&package.expect("cooks"));
        assert!(src.contains("LevelModelRecord"));
        assert!(src.contains("collision_radius:"));
        assert!(src.contains("LevelModelInstanceRecord"));
        assert!(src.contains("visual_yaw:"));
        assert!(src.contains("LevelModelClipRecord"));
        assert!(src.contains("LevelModelClipBoundsRecord"));
        assert!(src.contains("LevelModelFrameBoundsRecord"));
        assert!(src.contains("MODEL_INSTANCES"));
        assert!(src.contains("MODELS"));
        assert!(src.contains("MODEL_CLIPS"));
        assert!(src.contains("MODEL_CLIP_BOUNDS"));
        assert!(src.contains("MODEL_FRAME_BOUNDS"));
        assert!(src.contains("AssetKind::ModelMesh"));
        assert!(src.contains("AssetKind::ModelAnimation"));
    }

    /// Helper: starter project with the player spawn moved to
    /// editor coord `(ex, ez)`.
    fn project_with_spawn_at(ex: f32, ez: f32) -> (ProjectDocument, NodeId, NodeId) {
        let mut project = ProjectDocument::starter();
        let (room_id, spawn_id) = {
            let scene = project.active_scene();
            let room = scene
                .nodes()
                .iter()
                .find(|n| matches!(n.kind, crate::NodeKind::Room { .. }))
                .expect("starter has a room");
            (room.id, player_spawn_node_id(&project))
        };
        if let Some(node) = project.active_scene_mut().node_mut(spawn_id) {
            node.transform.translation = [ex, 0.0, ez];
        }
        (project, room_id, spawn_id)
    }

    fn expected_package_room_local_xz(
        project: &ProjectDocument,
        room_id: NodeId,
        package: &PlaytestPackage,
        package_room: u16,
        ex: f32,
        ez: f32,
    ) -> (i32, i32) {
        let scene = project.active_scene();
        let room = scene.node(room_id).expect("room exists");
        let crate::NodeKind::Room { grid } = &room.kind else {
            panic!("expected room");
        };
        let cooked_room = &package.rooms[package_room as usize];
        let world_cells = grid.editor_to_world_cells([ex, ez]);
        let s = cooked_room.sector_size as f32;
        (
            ((world_cells[0] - cooked_room.origin_x as f32) * s) as i32,
            ((world_cells[1] - cooked_room.origin_z as f32) * s) as i32,
        )
    }

    #[test]
    fn spawn_at_room_centre_lands_at_array_centre() {
        let (project, room_id, _) = project_with_spawn_at(0.0, 0.0);
        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.unwrap();
        let spawn = package.spawn.unwrap();
        assert_eq!(
            (spawn.x, spawn.z),
            expected_package_room_local_xz(&project, room_id, &package, spawn.room, 0.0, 0.0)
        );
    }

    #[test]
    fn spawn_after_negative_grow_lands_in_same_physical_cell() {
        let (mut project, room_id, _) = project_with_spawn_at(-1.0, 0.0);
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;

        // The minimal starter is a single floor tile, so editor
        // (-1, 0) is outside the original grid. Pre-grow the grid
        // to contain the spawn so the pre/post comparison is well
        // defined; the test still exercises the -X grow path below.
        if let Some(node) = project.active_scene_mut().node_mut(room_id) {
            if let crate::NodeKind::Room { grid } = &mut node.kind {
                if let Some(initial) = grid.editor_cells_to_array([-1.0, 0.0]) {
                    let _ = initial;
                } else {
                    let world_cells = grid.editor_to_world_cells([-1.0, 0.0]);
                    let (sx, sz) = grid.extend_to_include(
                        world_cells[0].floor() as i32,
                        world_cells[1].floor() as i32,
                    );
                    grid.set_floor(sx, sz, 0, Some(floor_material));
                }
            }
        }

        let (pre, _) = build_package(&project, &starter_project_root());
        let pre = pre.unwrap();
        let pre_spawn = pre.spawn.unwrap();
        assert_eq!(
            (pre_spawn.x, pre_spawn.z),
            expected_package_room_local_xz(&project, room_id, &pre, pre_spawn.room, -1.0, 0.0)
        );

        let scene = project.active_scene_mut();
        if let Some(node) = scene.node_mut(room_id) {
            if let crate::NodeKind::Room { grid } = &mut node.kind {
                let (sx, sz) = grid.extend_to_include(-1, 0);
                grid.set_floor(sx, sz, 0, Some(floor_material));
            }
        }

        let (post, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let post = post.unwrap();
        let post_spawn = post.spawn.unwrap();
        assert_eq!(
            (post_spawn.x, post_spawn.z),
            expected_package_room_local_xz(&project, room_id, &post, post_spawn.room, -1.0, 0.0)
        );
    }

    #[test]
    fn entity_after_negative_grow_uses_same_array_relative_formula() {
        let (mut project, room_id, _) = project_with_spawn_at(0.0, 0.0);
        let floor_material = project
            .resources
            .iter()
            .find(|resource| matches!(resource.data, ResourceData::Material(_)))
            .expect("starter has a room material")
            .id;
        let scene = project.active_scene_mut();
        let entity_id = scene.add_node(
            room_id,
            "Marker",
            crate::NodeKind::MeshInstance {
                mesh: None,
                material: None,
                animation_clip: None,
            },
        );
        if let Some(node) = scene.node_mut(entity_id) {
            node.transform.translation = [0.0, 0.0, 0.0];
        }
        if let Some(node) = scene.node_mut(room_id) {
            if let crate::NodeKind::Room { grid } = &mut node.kind {
                grid.extend_to_include(0, -1);
                if let Some((sx, sz)) = grid.editor_cells_to_array([0.0, 0.0]) {
                    grid.set_floor(sx, sz, 0, Some(floor_material));
                }
            }
        }

        let (package, report) = build_package(&project, &starter_project_root());
        assert!(report.is_ok(), "errors: {:?}", report.errors);
        let package = package.unwrap();
        assert_eq!(package.entities.len(), 1);
        let e = package.entities[0];
        assert_eq!(
            (e.x, e.z),
            expected_package_room_local_xz(&project, room_id, &package, e.room, 0.0, 0.0)
        );
    }

    #[test]
    fn empty_package_renders_a_valid_skeleton() {
        let package = PlaytestPackage::default();
        let src = render_manifest_source(&package);
        assert!(src.contains("pub static ASSETS: &[LevelAssetRecord] = &[\n];"));
        assert!(src.contains("pub static MATERIALS: &[LevelMaterialRecord] = &[\n];"));
        assert!(src.contains("pub static ROOMS: &[LevelRoomRecord] = &[\n];"));
        assert!(src.contains("pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[\n];"));
        assert!(src.contains("pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[\n];"));
        assert!(src.contains("pub static VISIBILITY_PVS_BITS: &[u8] = &[\n];"));
        assert!(
            src.contains("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[\n];")
        );
        assert!(src.contains("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[\n];"));
        assert!(
            src.contains("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[\n];")
        );
        assert!(src
            .contains("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[\n];"));
        assert!(src.contains("pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[\n];"));
        assert!(src.contains("pub static ENTITIES: &[EntityRecord] = &[\n];"));
        assert!(src.contains("pub static PLAYER_SPAWN"));
    }
}
