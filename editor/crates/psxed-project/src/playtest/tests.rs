use super::*;
use crate::{NodeKind, ProjectDocument, UiRect};

fn starter_project_root() -> PathBuf {
    crate::default_project_dir()
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "psxed-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ))
}

fn test_wav_mono_44k() -> Vec<u8> {
    let sample_rate = 44_100u32;
    let samples = [0i16, 2048, -2048, 4096, -4096, 0, 1024, -1024];
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_bytes as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[test]
#[ignore = "diagnostic: run with --ignored --nocapture to inspect demo11 cook"]
fn diag_demo11_cook() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/demo11");
    let mut project =
        ProjectDocument::load_from_path(root.join("project.ron")).expect("load demo11");
    project.normalize_loaded();

    // Authored side: room node, its floors, the player entity Y.
    let scene = project.active_scene();
    for node in scene.nodes() {
        if let NodeKind::Room { grid } = &node.kind {
            println!(
                "ROOM node id={} name={:?} sector_size={} base_elev={} floors={}",
                node.id.raw(),
                node.name,
                grid.sector_size,
                grid.elevation,
                grid.floor_count()
            );
            for i in 0..grid.floor_count() {
                if let Some(f) = grid.floor(i) {
                    println!(
                        "   floor {i}: elevation={} populated_cells={} dims={}x{} origin={:?}",
                        f.elevation,
                        f.populated_sector_count(),
                        f.width,
                        f.depth,
                        f.origin,
                    );
                }
            }
        }
    }
    for node in scene.nodes() {
        let is_player = matches!(&node.kind, NodeKind::SpawnPoint { player: true, .. })
            || scene.nodes().iter().any(|c| {
                c.parent == Some(node.id)
                    && matches!(&c.kind, NodeKind::CharacterController { player: true, .. })
            });
        if is_player {
            println!(
                "PLAYER node id={} name={:?} translation={:?}",
                node.id.raw(),
                node.name,
                node.transform.translation
            );
            // Physics check: where is the player relative to each
            // floor's surface at its cell?
            if let Some(room) = enclosing_room(scene, node) {
                if let NodeKind::Room { grid } = &room.kind {
                    let s = grid.sector_size;
                    let node_y_eng = (node.transform.translation[1] * s as f32) as i32;
                    let local = grid.editor_to_room_local([
                        node.transform.translation[0],
                        node.transform.translation[2],
                    ]);
                    println!(
                        "  player_y_engine={} (room-local cell from xz={:?})",
                        node_y_eng, local
                    );
                    for i in 0..grid.floor_count() {
                        if let Some(f) = grid.floor(i) {
                            let surf =
                                f.floor_height_at_room_local(local[0] as i32, local[2] as i32);
                            println!(
                                    "   floor {i}: base_elev={} surface_at_cell={:?} -> player is {} above base",
                                    f.elevation,
                                    surf,
                                    node_y_eng - f.elevation
                                );
                        }
                    }
                }
            }
        }
    }

    let (package, report) = build_package(&project, &root);
    println!("report.ok={} errors={:?}", report.is_ok(), report);
    let package = package.expect("demo11 cooked");

    println!("=== COOKED rooms ({}) ===", package.rooms.len());
    for (i, r) in package.rooms.iter().enumerate() {
        println!(
                "  room[{i}] name={:?} origin=({},{}) origin_y={} size={}x{} portal_first={} portal_count={}",
                r.name, r.origin_x, r.origin_z, r.origin_y, r.material_first, r.material_count,
                r.portal_first, r.portal_count
            );
    }
    println!("=== COOKED chunks ({}) ===", package.chunks.len());
    for (i, c) in package.chunks.iter().enumerate() {
        println!(
            "  chunk[{i}] room={} authored_room={} origin=({},{}) {}x{} neighbours={:?}",
            c.room, c.authored_room, c.origin_x, c.origin_z, c.width, c.depth, c.neighbours
        );
    }
    println!("=== room_portals ({}) ===", package.room_portals.len());
    for (i, p) in package.room_portals.iter().enumerate() {
        println!(
            "  portal[{i}] src={} dst={} kind={} normal={:?} verts={:?}",
            p.source_room, p.destination_room, p.kind, p.normal, p.vertices
        );
    }
    println!(
        "=== room_floor_links ({}) ===",
        package.room_floor_links.len()
    );
    for (i, l) in package.room_floor_links.iter().enumerate() {
        println!(
            "  link[{i}] room={} cell=({},{}) above={:?} below={:?}",
            l.room, l.x, l.z, l.above_room, l.below_room
        );
    }
    if let Some(spawn) = package.spawn {
        println!(
            "=== SPAWN room={} pos=({},{},{}) -> room origin_y={} ===",
            spawn.room,
            spawn.x,
            spawn.y,
            spawn.z,
            package
                .rooms
                .get(spawn.room as usize)
                .map(|r| r.origin_y)
                .unwrap_or(-1)
        );
    }
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
    let mut project = ProjectDocument::starter();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let (world_kind, room_grid, player_name, player_kind, player_children, light_template) = {
        let scene = project.active_scene();
        let world = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::World { .. }))
            .expect("starter must contain a World");
        let room = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .expect("starter must contain a Room");
        let NodeKind::Room { grid } = &room.kind else {
            unreachable!();
        };
        let player = scene
            .nodes()
            .iter()
            .find(|node| is_player_spawn_node(scene, node))
            .expect("starter must contain a player spawn entity");
        let children: Vec<_> = player
            .children
            .iter()
            .filter_map(|id| {
                let child = scene.node(*id)?;
                Some((child.name.clone(), child.kind.clone(), child.transform))
            })
            .collect();
        let light = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::PointLight { .. }))
            .map(|node| (node.name.clone(), node.kind.clone()));
        (
            world.kind.clone(),
            grid.clone(),
            player.name.clone(),
            player.kind.clone(),
            children,
            light,
        )
    };

    let mut grid = WorldGrid::stone_room(1, 1, room_grid.sector_size, room_material, room_material);
    grid.ambient_color = room_grid.ambient_color;
    grid.fog_enabled = room_grid.fog_enabled;
    grid.fog_color = room_grid.fog_color;
    grid.fog_near = room_grid.fog_near;
    grid.fog_far = room_grid.fog_far;
    grid.atmosphere_enabled = room_grid.atmosphere_enabled;
    grid.atmosphere_color = room_grid.atmosphere_color;
    grid.atmosphere_density = room_grid.atmosphere_density;
    grid.atmosphere_fall_speed_q4 = room_grid.atmosphere_fall_speed_q4;
    grid.atmosphere_wind_speed_q4 = room_grid.atmosphere_wind_speed_q4;

    let mut scene = crate::Scene::new("Main");
    let world_id = scene.root;
    if let Some(world) = scene.node_mut(world_id) {
        world.name = "World".to_string();
        world.kind = world_kind;
    }
    let room_id = scene.add_node(world_id, "Room", NodeKind::Room { grid });
    if let Some((name, kind)) = light_template {
        let light_id = scene.add_node(room_id, name, kind);
        if let Some(light) = scene.node_mut(light_id) {
            light.transform.translation = [0.0, 1.5, 0.0];
        }
    }
    let player_id = scene.add_node(room_id, player_name, player_kind);
    if let Some(player) = scene.node_mut(player_id) {
        player.transform.translation = [0.0, 0.0, 0.0];
        player.transform.rotation_degrees = [0.0, 0.0, 0.0];
    }
    for (name, kind, transform) in player_children {
        let child_id = scene.add_node(player_id, name, kind);
        if let Some(child) = scene.node_mut(child_id) {
            child.transform = transform;
        }
    }
    project.scenes[0] = scene;

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
        .filter(|node| {
            matches!(
                node.kind,
                NodeKind::ModelRenderer { model: Some(_), .. } | NodeKind::Animator { .. }
            )
        })
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
    assert!(manifest.contains("pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[];"));
    assert!(manifest.contains("pub const CACHED_ROOM_DEPTH_MODE: u8 = 0;"));
    assert!(manifest.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MODE: u8 = 0;"));
    assert!(manifest.contains("pub const CACHED_ROOM_DRAW_ORDER_MODE: u8 = 0;"));
    assert!(manifest.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;"));
    assert!(manifest.contains("pub static ROOM_NEAR_ROOMS: &[RoomIndex] = &[];"));
    assert!(manifest.contains("pub static ROOM_OVERLAPPED_ROOMS: &[RoomIndex] = &[];"));
    assert!(manifest.contains("pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[];"));
    assert!(manifest.contains("pub static VISIBILITY_PVS_BITS: &[u8] = &[];"));
    assert!(
        manifest.contains("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];")
    );
    assert!(manifest.contains("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];"));
    assert!(
        manifest.contains("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];")
    );
    assert!(
        manifest.contains("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];")
    );
    assert!(manifest.contains("pub static MODEL_SOCKETS: &[LevelModelSocketRecord] = &[];"));
    assert!(manifest.contains("pub static UI_NODES: &[LevelUiNodeRecord] = &[];"));
    assert!(manifest.contains("pub static WEAPONS: &[LevelWeaponRecord] = &[];"));
    assert!(manifest.contains("pub static EQUIPMENT: &[EquipmentRecord] = &[];"));
    assert!(manifest.contains("pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[];"));
}

#[test]
fn starter_project_validates_and_cooks() {
    let project = project_with_one_room();
    let expected_camera = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::World { camera, .. } => Some(*camera),
            _ => None,
        })
        .expect("starter world has camera settings");
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
    assert_eq!(package.rooms[0].camera.distance, expected_camera.distance);
    assert_eq!(package.rooms[0].camera.height, expected_camera.height);
    assert_eq!(
        package.rooms[0].camera.target_height,
        expected_camera.target_height
    );
    assert_eq!(
        package.rooms[0].camera.min_floor_clearance,
        expected_camera.min_floor_clearance
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
fn ui_nodes_cook_in_hierarchy_order_with_local_offsets() {
    let mut project = ProjectDocument::new("ui");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    let group = scene.add_node(
        scene.root,
        "Panel",
        UiNodeKind::Group {
            rect: UiRect::new(40, 30, 100, 50),
        },
    );
    scene.add_node(
        group,
        "Prompt",
        UiNodeKind::Label {
            rect: UiRect::new(8, 6, 48, 12),
            text: "Open".to_string(),
            tag: "prompt".to_string(),
            align: crate::UiTextAlign::Left,
            wrap: false,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: -2,
            color: [220, 226, 240],
            gradient: None,
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, scenes, _sfx_samples, _sfx_cues, flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );
    assert_eq!(scenes.len(), 1);
    assert_eq!(scenes[0].node_first, 0);
    assert_eq!(scenes[0].node_count as usize, nodes.len());
    let first_ui_state = flow
        .scene_states
        .iter()
        .find(|state| state.ui_scene == scenes[0].id)
        .expect("ui scene state cooked");
    assert_eq!(
        flow.states.first(),
        Some(&PlaytestFlowState::SceneState {
            state: first_ui_state.id
        })
    );
    let gameplay_state = flow
        .scene_states
        .iter()
        .find(|state| state.world == PlaytestWorldLayer::Gameplay)
        .expect("gameplay scene state cooked");
    assert_eq!(
        flow.states.last(),
        Some(&PlaytestFlowState::SceneState {
            state: gameplay_state.id
        })
    );
    // With the default boot target (Gameplay), entry points at the gameplay
    // state (the last one), not the first UI scene. Authoring a UI boot
    // target via `project.boot` is what moves entry onto a menu scene.
    assert_eq!(flow.entry as usize, flow.states.len() - 1);
    assert_eq!(
        flow.states.get(flow.entry as usize),
        Some(&PlaytestFlowState::SceneState {
            state: gameplay_state.id
        })
    );
    let group_index = nodes
        .iter()
        .position(|node| {
            matches!(
                &node.kind,
                UiNodeKind::Group { rect } if *rect == UiRect::new(40, 30, 100, 50)
            )
        })
        .expect("group cooked");
    let label_index = nodes
        .iter()
        .position(|node| node.text == "Open")
        .expect("label cooked");

    assert!(group_index < label_index);
    assert_eq!(nodes[label_index].parent, Some(group_index as u16));
    assert_eq!((nodes[label_index].x, nodes[label_index].y), (8, 6));
    assert_eq!(nodes[label_index].tag, "prompt");
    assert_eq!(nodes[label_index].letter_spacing, -2);
}

#[test]
fn ui_gradient_roles_cook_to_paint_table_refs() {
    let mut project = ProjectDocument::new("ui-gradient");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Panel",
        UiNodeKind::Rect {
            rect: UiRect::new(4, 5, 80, 20),
            color: [20, 30, 40],
            gradient: Some(crate::UiGradient::new(
                [80, 90, 100],
                crate::UiGradientDirection::Horizontal,
            )),
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, paints, _scenes, _sfx_samples, _sfx_cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );

    assert!(report.is_ok(), "warnings/errors: {:?}", report);
    assert_eq!(paints.len(), 1);
    assert_eq!(paints[0].from, [20, 30, 40]);
    assert_eq!(paints[0].to, [80, 90, 100]);
    assert_eq!(paints[0].direction, crate::UiGradientDirection::Horizontal);
    let rect = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Rect { .. }))
        .expect("rect cooked");
    assert_eq!(rect.color_paint, Some(0));
    assert_eq!(rect.background_paint, None);
    assert_eq!(rect.accent_paint, None);
}

#[test]
fn ui_image_effect_cooks_to_image_node() {
    let mut project = ProjectDocument::new("ui-image-effect");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Glow",
        UiNodeKind::Image {
            rect: UiRect::new(4, 5, 80, 20),
            texture: None,
            tint: [128, 128, 128],
            effect: UiImageEffect::DiagonalSweep,
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, _scenes, _sfx_samples, _sfx_cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );

    assert!(report.is_ok(), "warnings/errors: {:?}", report);
    let image = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Image { .. }))
        .expect("image cooked");
    assert_eq!(image.image_effect, UiImageEffect::DiagonalSweep);
}

#[test]
fn boot_target_sets_game_flow_entry_to_the_chosen_scene() {
    // Two UI scenes; choose the SECOND as the boot target and confirm the
    // cooked flow enters that scene's state (not index 0, not gameplay).
    let mut project = ProjectDocument::new("boot");
    project.ui_scenes.push(crate::UiScene::empty_canvas(
        "Menu",
        crate::UiSceneId::UNASSIGNED,
    ));
    project.normalize_loaded(); // hands out stable scene ids
    let menu_id = project.ui_scenes[1].id;
    project.boot = crate::BootTarget::UiScene(menu_id);

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (_nodes, _paints, scenes, _sfx_samples, _sfx_cues, flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );

    let menu_pos = scenes
        .iter()
        .position(|scene| u64::from(scene.id) == menu_id.raw())
        .expect("menu scene cooked");
    let menu_state = flow
        .scene_states
        .iter()
        .find(|state| state.ui_scene == scenes[menu_pos].id)
        .expect("menu scene state cooked");
    assert_eq!(
        flow.states.get(flow.entry as usize),
        Some(&PlaytestFlowState::SceneState {
            state: menu_state.id
        })
    );
}

#[test]
fn boot_target_falls_back_to_gameplay_when_scene_missing() {
    // A boot target pointing at a non-existent scene must not wedge boot:
    // entry falls back to the gameplay state.
    let mut project = ProjectDocument::new("boot-missing");
    project.normalize_loaded();
    project.boot = crate::BootTarget::UiScene(crate::UiSceneId(9999));

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (_nodes, _paints, _scenes, _sfx_samples, _sfx_cues, flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );
    let gameplay_state = flow
        .scene_states
        .iter()
        .find(|state| state.world == PlaytestWorldLayer::Gameplay)
        .expect("gameplay scene state cooked");
    assert_eq!(
        flow.states.get(flow.entry as usize),
        Some(&PlaytestFlowState::SceneState {
            state: gameplay_state.id
        })
    );
}

#[test]
fn button_and_slider_cook_action_colours_and_option_binding() {
    let mut project = ProjectDocument::new("ui");
    // A second scene so GotoScene has a non-trivial target id to
    // resolve and we can assert the low-16-bit lowering.
    let target_scene = project.add_ui_scene("Pause");
    let option = project.add_option("Volume");

    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Play",
        UiNodeKind::Button {
            rect: UiRect::new(10, 12, 80, 18),
            label: "Play".to_string(),
            align: UiTextAlign::Center,
            font: crate::UiFontChoice::Basic8x16,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: 4,
            color: [50, 60, 70],
            background_gradient: None,
            text_color: [236, 240, 248],
            text_gradient: None,
            transparent: false,
            action: UiAction::GotoScene(target_scene),
            sfx: crate::UiSfxBindings::default(),
        },
    );
    scene.add_node(
        scene.root,
        "Volume",
        UiNodeKind::Slider {
            rect: UiRect::new(10, 40, 96, 8),
            option,
            track: [11, 12, 13],
            track_gradient: None,
            fill: [21, 22, 23],
            fill_gradient: None,
            knob: [31, 32, 33],
            knob_gradient: None,
            sfx: crate::UiSfxBindings::default(),
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, _scenes, _sfx_samples, _sfx_cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );

    let button = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Button { .. }))
        .expect("button cooked");
    assert_eq!(button.text, "Play");
    assert_eq!(button.font, 1);
    assert_eq!(button.letter_spacing, 4);
    assert_eq!(button.color, [50, 60, 70]);
    assert_eq!(button.option, psx_level::UI_OPTION_NONE);
    assert_eq!(
        button.action,
        PlaytestUiAction::GotoScene {
            scene: (target_scene.raw() & u16::MAX as u64) as u16,
        }
    );

    let slider = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Slider { .. }))
        .expect("slider cooked");
    // Track -> color, fill -> background, knob -> accent.
    assert_eq!(slider.color, [11, 12, 13]);
    assert_eq!(slider.background, [21, 22, 23]);
    assert_eq!(slider.accent, [31, 32, 33]);
    assert_eq!(slider.option, (option.raw() & u16::MAX as u32) as u16);
    assert_eq!(slider.action, PlaytestUiAction::default());
}

#[test]
fn button_sfx_cooks_wav_to_sample_and_cue_range() {
    let root = unique_temp_dir("ui-sfx-cook");
    let sfx_dir = root.join("assets/sfx");
    std::fs::create_dir_all(&sfx_dir).expect("sfx dir");
    std::fs::write(sfx_dir.join("click.wav"), test_wav_mono_44k()).expect("test wav");

    let mut project = ProjectDocument::new("ui-sfx");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Play",
        UiNodeKind::Button {
            rect: UiRect::new(10, 12, 80, 18),
            label: "Play".to_string(),
            align: UiTextAlign::Center,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
            color: [50, 60, 70],
            background_gradient: None,
            text_color: [236, 240, 248],
            text_gradient: None,
            transparent: false,
            action: UiAction::Back,
            sfx: crate::UiSfxBindings {
                activate: vec![UiSfxCue {
                    wav_path: "assets/sfx/click.wav".to_string(),
                    volume: 73,
                    pitch_q12: 5120,
                }],
                ..crate::UiSfxBindings::default()
            },
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, _scenes, samples, cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        &root,
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );
    let _ = std::fs::remove_dir_all(&root);

    assert!(report.is_ok(), "warnings/errors: {:?}", report);
    assert_eq!(samples.len(), 1);
    assert_eq!(&samples[0].bytes[0..4], b"PSAU");
    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].sample, 0);
    assert_eq!(cues[0].event, psx_level::LevelUiSfxEvent::Activate);
    assert_eq!(cues[0].volume_percent, 73);
    assert_eq!(cues[0].pitch_q12, 5120);
    let button = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Button { .. }))
        .expect("button cooked");
    assert_eq!(button.sfx_first, 0);
    assert_eq!(button.sfx_count, 1);
}

#[test]
fn button_set_option_and_back_actions_lower_to_runtime_ids() {
    let mut project = ProjectDocument::new("ui");
    let option = project.add_option("Difficulty");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Harder",
        UiNodeKind::Button {
            rect: UiRect::new(0, 0, 40, 16),
            label: "+".to_string(),
            align: UiTextAlign::Center,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
            color: [40, 40, 40],
            background_gradient: None,
            text_color: [236, 240, 248],
            text_gradient: None,
            transparent: false,
            action: UiAction::SetOption { option, delta: 2 },
            sfx: crate::UiSfxBindings::default(),
        },
    );
    scene.add_node(
        scene.root,
        "Back",
        UiNodeKind::Button {
            rect: UiRect::new(0, 20, 40, 16),
            label: "Back".to_string(),
            align: UiTextAlign::Center,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
            color: [40, 40, 40],
            background_gradient: None,
            text_color: [236, 240, 248],
            text_gradient: None,
            transparent: false,
            action: UiAction::Back,
            sfx: crate::UiSfxBindings::default(),
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, _scenes, _sfx_samples, _sfx_cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut report,
    );

    let set_option = nodes
        .iter()
        .find(|node| node.text == "+")
        .expect("set-option button cooked");
    assert_eq!(
        set_option.action,
        PlaytestUiAction::SetOption {
            option: (option.raw() & u16::MAX as u32) as u16,
            delta: 2,
        }
    );

    let back = nodes
        .iter()
        .find(|node| node.text == "Back")
        .expect("back button cooked");
    assert_eq!(back.action, PlaytestUiAction::Back);
}

#[test]
fn cook_options_flattens_every_kind() {
    // One option of each kind; the cook collapses them to bounded
    // integer triples keyed by the low-16-bit option id.
    let mut project = ProjectDocument::new("ui");
    let int_id = project.add_option("Volume");
    if let Some(opt) = project.options.iter_mut().find(|o| o.id == int_id) {
        opt.kind = crate::OptionKind::IntRange {
            min: 2,
            max: 9,
            step: 3,
            default: 5,
        };
    }
    let enum_id = project.add_option("Quality");
    if let Some(opt) = project.options.iter_mut().find(|o| o.id == enum_id) {
        opt.kind = crate::OptionKind::Enum {
            variants: vec!["Low".into(), "Medium".into(), "High".into()],
            default: 2,
        };
    }
    let bool_id = project.add_option("Subtitles");
    if let Some(opt) = project.options.iter_mut().find(|o| o.id == bool_id) {
        opt.kind = crate::OptionKind::Bool { default: true };
    }

    let options = cook_options(&project);
    assert_eq!(options.len(), 3);

    let int_opt = options
        .iter()
        .find(|o| o.id == (int_id.raw() & u16::MAX as u32) as u16)
        .expect("int option cooked");
    assert_eq!(
        (int_opt.min, int_opt.max, int_opt.step, int_opt.default),
        (2, 9, 3, 5)
    );

    // Enum -> [0, variants - 1] step 1, default = variant index.
    let enum_opt = options
        .iter()
        .find(|o| o.id == (enum_id.raw() & u16::MAX as u32) as u16)
        .expect("enum option cooked");
    assert_eq!(
        (enum_opt.min, enum_opt.max, enum_opt.step, enum_opt.default),
        (0, 2, 1, 2)
    );

    // Bool -> [0, 1] step 1, default = 1 for true.
    let bool_opt = options
        .iter()
        .find(|o| o.id == (bool_id.raw() & u16::MAX as u32) as u16)
        .expect("bool option cooked");
    assert_eq!(
        (bool_opt.min, bool_opt.max, bool_opt.step, bool_opt.default),
        (0, 1, 1, 1)
    );
}

#[test]
fn manifest_emits_cooked_options_table() {
    let mut package = PlaytestPackage::default();
    package.options = vec![PlaytestOption {
        id: 7,
        min: 0,
        max: 5,
        step: 1,
        default: 3,
    }];
    let src = render_manifest_source(&package);
    assert!(src.contains("pub static OPTIONS: &[LevelOptionDef] = &[\n"));
    assert!(src.contains("LevelOptionDef { id: 7, min: 0, max: 5, step: 1, default: 3 },"));
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
fn package_resolves_vertical_floor_links_to_runtime_rooms() {
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let scene = project.active_scene_mut();
    let world_id = scene.root;
    let source_id = scene
        .nodes()
        .iter()
        .find(|node| node.name == "Room" && matches!(node.kind, NodeKind::Room { .. }))
        .map(|node| node.id)
        .expect("source room");
    let target_grid = WorldGrid::stone_room(
        1,
        1,
        crate::DEFAULT_WORLD_SECTOR_SIZE,
        room_material,
        room_material,
    );
    let target_id = scene.add_node(world_id, "Below", NodeKind::Room { grid: target_grid });
    let source = scene.node_mut(source_id).expect("source node");
    let NodeKind::Room { grid } = &mut source.kind else {
        panic!("source should be room");
    };
    grid.set_floor_below(0, 0, Some(crate::GridFloorLink::room(target_id)));

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    assert_eq!(package.room_floor_links.len(), 1);
    assert_eq!(package.room_floor_links[0].room, 0);
    assert_eq!(package.room_floor_links[0].x, 0);
    assert_eq!(package.room_floor_links[0].z, 0);
    assert_eq!(package.room_floor_links[0].above_room, None);
    assert_eq!(package.room_floor_links[0].below_room, Some(1));
}

#[test]
fn floors_cook_to_stacked_rooms_with_auto_links() {
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);
    let baseline = build_package(&project, &starter_project_root())
        .0
        .expect("baseline package")
        .rooms
        .len();

    // Add a populated floor above the base, kept at its auto-stacked
    // elevation, fully overlapping the base footprint.
    {
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
            .expect("room node");
        let node = scene.node_mut(room_id).expect("room node");
        let NodeKind::Room { grid } = &mut node.kind else {
            panic!("expected a room");
        };
        let (w, d, s, origin) = (grid.width, grid.depth, grid.sector_size, grid.origin);
        // Punch a hole at a cell shared by both floors so the
        // hole-gated portal generator emits a vertical portal there:
        // floor 0 must have no ceiling and floor 1 no floor at (0,0).
        if let Some(sector) = grid.sector_mut(0, 0) {
            sector.ceiling = None;
        }
        grid.push_floor();
        let floor1 = grid.floor_mut(1).expect("floor 1");
        let elevation = floor1.elevation;
        *floor1 = WorldGrid::stone_room(w, d, s, room_material, room_material);
        floor1.origin = origin;
        floor1.elevation = elevation;
        // Open the floor-1 floor at the hole cell.
        if let Some(sector) = floor1.sector_mut(0, 0) {
            sector.floor = None;
        }
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    // Floor 1 cooks as its own room chunk(s) on top of the base.
    assert!(
        package.rooms.len() > baseline,
        "the upper floor should add cooked rooms ({} vs {baseline})",
        package.rooms.len()
    );
    // At least one cooked room sits above Y=0 (the stacked floor).
    assert!(
        package.rooms.iter().any(|room| room.origin_y > 0),
        "an upper floor should cook at a stacked origin_y"
    );
    // The floors are auto-wired with vertical room links.
    assert!(
        package
            .room_floor_links
            .iter()
            .any(|link| link.above_room.is_some() || link.below_room.is_some()),
        "consecutive floors should be auto-linked"
    );
    // ...and with vertical portal quads (kind=1) so the portal
    // clipper / portal view have geometry between the floors. Portals
    // are emitted only at actual holes (floor-1 floor open AND floor-0
    // ceiling open); we punched one at (0,0), so expect a reciprocal
    // up/down pair there with ±Y normals.
    let vertical: Vec<_> = package
        .room_portals
        .iter()
        .filter(|p| p.kind == 1)
        .collect();
    assert!(
        !vertical.is_empty(),
        "stacked floors should emit vertical portal quads"
    );
    assert!(
        vertical.iter().any(|p| p.normal == [0, 1, 0])
            && vertical.iter().any(|p| p.normal == [0, -1, 0]),
        "vertical portals should be reciprocal (both +Y and -Y normals): {vertical:?}"
    );
    // Each vertical portal is planar in Y (a horizontal quad at the
    // boundary elevation).
    for p in &vertical {
        let y = p.vertices[0][1];
        assert!(
            p.vertices.iter().all(|v| v[1] == y),
            "a vertical portal quad must be planar in Y: {:?}",
            p.vertices
        );
    }
}

#[test]
fn entities_bind_to_their_explicit_floor() {
    // Two-floor room; a spawn marker on each floor, distinguished by
    // the explicit `SceneNode::floor` field (NOT by Y -- the authored
    // standing height is a placement default and can't select a
    // floor). The cook must bind each marker to the runtime room for
    // its own floor (distinct origin_y).
    let mut project = project_with_one_room();
    let room_material = project
        .resources
        .iter()
        .find(|resource| matches!(resource.data, ResourceData::Material(_)))
        .map(|resource| resource.id);

    let room_id = {
        let scene = project.active_scene_mut();
        let room_id = scene
            .nodes()
            .iter()
            .find(|node| matches!(node.kind, NodeKind::Room { .. }))
            .map(|node| node.id)
            .expect("room node");
        let node = scene.node_mut(room_id).expect("room node");
        let NodeKind::Room { grid } = &mut node.kind else {
            panic!("expected a room");
        };
        let (w, d, s, origin) = (grid.width, grid.depth, grid.sector_size, grid.origin);
        grid.push_floor();
        let floor1 = grid.floor_mut(1).expect("floor 1");
        let elevation = floor1.elevation;
        *floor1 = WorldGrid::stone_room(w, d, s, room_material, room_material);
        floor1.origin = origin;
        floor1.elevation = elevation;
        room_id
    };

    // Two markers at the SAME transform; only the explicit floor
    // differs. This proves binding is by `floor`, not Y.
    let scene = project.active_scene_mut();
    let ground = scene.add_node(
        room_id,
        "Ground Marker",
        NodeKind::SpawnPoint {
            player: false,
            character: None,
        },
    );
    let ground_node = scene.node_mut(ground).unwrap();
    ground_node.transform.translation = [0.0, 0.0, 0.0];
    ground_node.floor = 0;
    let upper = scene.add_node(
        room_id,
        "Upper Marker",
        NodeKind::SpawnPoint {
            player: false,
            character: None,
        },
    );
    let upper_node = scene.node_mut(upper).unwrap();
    upper_node.transform.translation = [0.0, 0.0, 0.0];
    upper_node.floor = 1;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");

    // Both markers cooked; each binds to a room whose origin_y matches
    // its floor. The base marker -> origin_y 0; the upper -> origin_y > 0.
    let origin_y_of = |room_index: u16| package.rooms[room_index as usize].origin_y;
    let marker_origin_ys: Vec<i32> = package
        .entities
        .iter()
        .filter(|e| matches!(e.kind, PlaytestEntityKind::Marker))
        .map(|e| origin_y_of(e.room))
        .collect();
    assert!(
        marker_origin_ys.contains(&0),
        "the ground marker should bind to floor 0 (origin_y 0): {marker_origin_ys:?}"
    );
    assert!(
        marker_origin_ys.iter().any(|y| *y > 0),
        "the upper marker should bind to the stacked floor (origin_y > 0): {marker_origin_ys:?}"
    );
}

#[test]
fn room_vertical_placement_flows_from_transform_into_origin_y() {
    // Ground placement: a room left at the default transform Y
    // cooks to origin_y == 0, preserving today's behaviour.
    let project = project_with_one_room();
    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let package = package.expect("package");
    assert_eq!(package.rooms[0].origin_y, 0);

    // Raised placement: authoring translation[1] = 2 sectors must
    // cook to engine units (2 * sector_size). The cook reads the
    // Room node transform, so the authored Y reaches the record
    // even though the per-chunk grid does not carry elevation yet.
    let mut project = project_with_one_room();
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::Room { .. }))
        .map(|node| node.id)
        .expect("room node");
    scene.node_mut(room_id).expect("room").transform.translation[1] = 2.0;
    let (raised, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "{report:?}");
    let raised = raised.expect("package");
    // 2 authored sectors converted to engine units. Derived from
    // the room's own sector_size so the test holds whatever the
    // starter project uses.
    assert_eq!(raised.rooms[0].origin_y, 2 * raised.rooms[0].sector_size);
    assert!(raised.rooms[0].sector_size > 0);
}

#[test]
fn visibility_pvs_adds_one_cell_boundary_shell() {
    let width = 1;
    let radius = DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS;
    let depth = radius + 6;
    let mut cells: Vec<PlaytestVisibilityCell> =
        (0..depth).map(|z| visibility_test_cell(0, z, 0)).collect();
    let index_by_coord = visibility_index_by_coord(width, depth, &cells);
    assign_visibility_portals(width, depth, &index_by_coord, &mut cells);

    let visible = visibility_indices_for_anchor(0, width, depth, &cells, &index_by_coord, radius);

    assert_eq!(visible.len(), radius as usize + 2);
    assert!(visible.contains(&0));
    assert!(visible.contains(&(radius as usize)));
    assert!(visible.contains(&(radius as usize + 1)));
    assert!(!visible.contains(&(radius as usize + 2)));
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

    let visible = visibility_indices_for_anchor(
        0,
        width,
        depth,
        &cells,
        &index_by_coord,
        DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS,
    );

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

    append_visibility_pvs(
        width,
        depth,
        &cells,
        &index_by_coord,
        DEFAULT_PLAYTEST_VISIBILITY_CELL_RADIUS,
        &mut pvs,
        &mut bits,
    );

    assert_eq!(pvs.len(), 2);
    assert_eq!(bits.len(), 1);
    assert_eq!(pvs[0].byte_first, pvs[1].byte_first);
    assert_eq!(pvs[0].byte_count, 1);
    assert_eq!(bits[0], 0b0000_0011);
}

#[test]
fn oversized_authored_room_fails_without_manual_split() {
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
    assert!(package.is_none());
    assert!(!report.is_ok());
    assert!(
        report
            .errors
            .iter()
            .any(|error| error.contains("runtime cap")),
        "errors: {:?}",
        report.errors
    );
}

#[test]
fn portal_room_cook_emits_directed_room_portals() {
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
        *grid = crate::WorldGrid::stone_room(
            2,
            1,
            crate::DEFAULT_WORLD_SECTOR_SIZE,
            Some(floor_material),
            Some(floor_material),
        );
    }
    let portal_id = project.active_scene_mut().add_node(
        room_id,
        "Portal",
        NodeKind::Portal {
            target_room: None,
            target_entry: String::new(),
            entry_name: String::new(),
            geometry: None,
        },
    );
    if let Some(portal) = project.active_scene_mut().node_mut(portal_id) {
        portal.transform.translation = [0.0, 0.0, 0.0];
    }
    let spawn_id = player_spawn_node_id(&project);
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [-0.25, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert_eq!(package.rooms.len(), 2);
    assert_eq!(package.room_portals.len(), 2);
    assert_eq!(package.rooms[0].portal_first, 0);
    assert_eq!(package.rooms[0].portal_count, 1);
    assert_eq!(package.rooms[1].portal_first, 1);
    assert_eq!(package.rooms[1].portal_count, 1);
    assert_eq!(package.room_portals[0].source_room, 0);
    assert_eq!(package.room_portals[0].destination_room, 1);
    assert_eq!(package.room_portals[0].normal, [-1, 0, 0]);
    let src = render_manifest_source(&package);
    assert!(src.contains(
        "pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[\n    LevelRoomPortalRecord"
    ));
}

#[test]
fn manual_portal_rooms_emit_warm_residency_hints() {
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
        *grid = crate::WorldGrid::stone_room(
            2,
            1,
            crate::DEFAULT_WORLD_SECTOR_SIZE,
            Some(floor_material),
            Some(floor_material),
        );
    }
    let portal_id = project.active_scene_mut().add_node(
        room_id,
        "Portal",
        NodeKind::Portal {
            target_room: None,
            target_entry: String::new(),
            entry_name: String::new(),
            geometry: None,
        },
    );
    if let Some(portal) = project.active_scene_mut().node_mut(portal_id) {
        portal.transform.translation = [0.0, 0.0, 0.0];
    }
    let spawn_id = player_spawn_node_id(&project);
    if let Some(spawn) = project.active_scene_mut().node_mut(spawn_id) {
        spawn.transform.translation = [-0.25, 0.0, 0.0];
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
    for action in [
        CharacterAnimationAction::Idle,
        CharacterAnimationAction::Walk,
        CharacterAnimationAction::Run,
        CharacterAnimationAction::Roll,
        CharacterAnimationAction::LightAttack,
        CharacterAnimationAction::HeavyAttack,
    ] {
        assert_ne!(
            character.action_clips[action.to_index()],
            CHARACTER_CLIP_NONE,
            "{action:?} should be mapped for the starter player"
        );
    }
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
fn world_physics_and_physics_body_drive_cooked_gravity() {
    let mut project = project_with_one_room();
    let world_id = project
        .active_scene()
        .nodes()
        .iter()
        .find(|node| matches!(node.kind, NodeKind::World { .. }))
        .expect("starter has world")
        .id;
    if let Some(world) = project.active_scene_mut().node_mut(world_id) {
        let NodeKind::World { physics, .. } = &mut world.kind else {
            panic!("expected world");
        };
        physics.gravity_per_tick = 123;
    }

    let player = player_spawn_node_id(&project);
    project.active_scene_mut().add_node(
        player,
        "Physics Body",
        NodeKind::PhysicsBody {
            settings: crate::PhysicsBodySettings { weight_q8: 384 },
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("package returned on ok report");
    assert!(package
        .rooms
        .iter()
        .all(|room| room.gravity_per_tick == 123));
    assert_eq!(package.characters[0].weight_q8, 384);
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
    let character_id = player_character_resource_id(&project);
    // Bump idle clip past the model's clip count so cook
    // validation must reject.
    if let Some(resource) = project.resource_mut(character_id) {
        if let crate::ResourceData::Character(c) = &mut resource.data {
            c.animation_set = None;
            c.action_clips.clear();
            c.idle_clip = Some(99);
        }
    }
    let animator_ids: Vec<NodeId> = project
        .active_scene()
        .nodes()
        .iter()
        .filter(|node| matches!(node.kind, NodeKind::Animator { .. }))
        .map(|node| node.id)
        .collect();
    for id in animator_ids {
        project.active_scene_mut().remove_node(id);
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
    assert!(src.contains("pub static ROOM_PORTALS"));
    assert!(src.contains("pub static ROOM_NEAR_ROOMS"));
    assert!(src.contains("pub static ROOM_OVERLAPPED_ROOMS"));
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

    let blob =
        std::fs::read(dir.join(ROOMS_DIRNAME).join("room_000.psxw")).expect("room blob written");
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
    // Two materials in the starter both use the same room texture.
    // After cook + package the texture should appear once in
    // ASSETS even though both materials reference it.
    let mut project = project_with_one_room();
    // Find the starter room texture id and an existing material to
    // clone-and-retint as a second material referencing the
    // same texture.
    let room_texture_id = project
        .resources
        .iter()
        .find_map(|r| match &r.data {
            ResourceData::Texture { psxt_path } if psxt_path.ends_with("bigdoor_1a.psxt") => {
                Some(r.id)
            }
            _ => None,
        })
        .expect("starter has room texture");

    // Reassign every wall material in the room to a new
    // material that *also* points at the same room texture. After
    // cook the world has 2 cooker material slots (floor + the
    // new wall material) but both resolve to the same texture,
    // so playtest should emit 1 texture asset.
    let new_material_id = project.add_resource(
        "BigdoorOnWalls",
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
            // starter room material; the new cell's floor and
            // wall both use new_material_id, giving the cooker
            // two distinct material slots that both share the
            // same room texture.
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
        "expected at least two cooked material slots to share the starter room texture"
    );
    let first_room_asset = room_texture_slots[0].texture_asset_index;
    assert!(room_texture_slots
        .iter()
        .all(|material| material.texture_asset_index == first_room_asset));
}

#[test]
fn material_sidedness_reaches_playtest_manifest_flags() {
    let mut project = project_with_one_room();
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
    let mut project = project_with_one_room();
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
    let project = project_with_one_room();
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
fn room_light_is_emitted_once_without_generated_splits() {
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
        *grid = crate::WorldGrid::empty(1, 16, crate::DEFAULT_WORLD_SECTOR_SIZE);
        for z in 0..grid.depth {
            grid.set_floor(0, z, 0, Some(floor_material));
        }
    }
    for id in starter_light_ids(&project) {
        let Some(light) = project.active_scene_mut().node_mut(id) else {
            continue;
        };
        light.transform.translation = [0.0, 0.0, 0.0];
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
        spawn.transform.translation = [0.0, 0.0, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());

    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.rooms.len(), 1);
    assert_eq!(package.lights.len(), 1);
    assert_eq!(package.lights[0].room, 0);
}

#[test]
fn floor_transition_wall_stays_in_single_manual_room() {
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
    assert_eq!(package.rooms.len(), 1);
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
        origin_y: 0,
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
fn billboard_image_props_bake_vertical_static_lighting() {
    let mut props = vec![PlaytestImageProp {
        room: 0,
        texture_asset_index: 0,
        x: 0,
        y: 0,
        z: 0,
        pitch: 0,
        yaw: 0,
        roll: 0,
        width: 128,
        height: 512,
        tint_rgb: [128, 128, 128],
        baked_vertex_rgb: [(128, 128, 128); 4],
        flags: image_prop_flags::CYLINDRICAL_BILLBOARD,
    }];
    let room = CookedRoomBakeInput {
        room_index: 0,
        world_asset_index: 0,
        world_origin: [0, 0],
        origin_y: 0,
        cooked: CookedWorldGrid {
            width: 0,
            depth: 0,
            sector_size: 1024,
            sectors: Vec::new(),
            materials: Vec::new(),
            ambient_color: [32, 32, 32],
            static_vertex_lighting: true,
            fog_enabled: false,
            fog_color: [0, 0, 0],
            fog_near: 0,
            fog_far: 0,
        },
    };
    let lights = [PlaytestLight {
        room: 0,
        x: 0,
        y: 512,
        z: 0,
        radius: 1024,
        intensity_q8: 256,
        color: [128, 128, 128],
    }];

    bake_static_image_prop_lights(&mut props, &[room], &lights);

    assert_eq!(props[0].baked_vertex_rgb[0], (160, 160, 160));
    assert_eq!(props[0].baked_vertex_rgb[1], (160, 160, 160));
    assert_eq!(props[0].baked_vertex_rgb[2], (96, 96, 96));
    assert_eq!(props[0].baked_vertex_rgb[3], (96, 96, 96));
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
fn interactable_component_emits_prompt_and_message_records() {
    let mut project = ProjectDocument::starter();
    let scene = project.active_scene_mut();
    let room = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Room { .. }))
        .map(|n| n.id)
        .expect("starter has room");
    let entity = scene.add_node(room, "Echo Body", NodeKind::Entity);
    scene.add_node(
        entity,
        "Interactable",
        NodeKind::Interactable {
            kind: crate::InteractableKind::Message {
                title: "ECHO REMNANT".to_string(),
                body: "The signal breaks here.".to_string(),
            },
            prompt: "READ ECHO".to_string(),
            radius: 128,
            enabled: true,
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    assert_eq!(package.interactables.len(), 1);
    assert_eq!(package.interactable_messages.len(), 1);
    assert_eq!(package.interactables[0].prompt, "READ ECHO");
    assert_eq!(package.interactables[0].radius, 128);
    assert_eq!(package.interactable_messages[0].title, "ECHO REMNANT");

    let src = render_manifest_source(&package);
    assert!(src.contains("pub static INTERACTABLE_MESSAGES"));
    assert!(src.contains("pub static INTERACTABLES"));
    assert!(src.contains("InteractableKind::Message"));
    assert!(src.contains("READ ECHO"));
    assert!(src.contains("The signal breaks here."));
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
            pose_frame: 0,
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
    let mut project = project_with_one_room();
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
                *psxt_path =
                    "assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt".to_string();
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
            model.texture_path = Some("assets/textures/delven_01_slateflr1a_q2.psxt".to_string());
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
fn box_prop_cooks_faces_vertices_and_collision() {
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
    let vertices = crate::box_prop_vertices_for_size(512);
    let prop_id = scene.add_node(
        room_id,
        "Cooked Box Prop",
        NodeKind::BoxProp {
            materials: [Some(material_id); crate::BOX_PROP_FACE_COUNT],
            vertices,
            collision_enabled: true,
            break_flags: psx_level::box_prop_flags::BREAK_ON_WALK
                | psx_level::box_prop_flags::BREAK_ON_ATTACK,
        },
    );
    if let Some(node) = scene.node_mut(prop_id) {
        node.transform.rotation_degrees = [45.0, 90.0, 270.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let prop = package
        .box_props
        .iter()
        .find(|prop| prop.vertices == vertices)
        .expect("box prop cooks");
    assert!(prop.texture_asset_indices.iter().all(Option::is_some));
    assert_eq!(prop.pitch, 512);
    assert_eq!(prop.yaw, 1024);
    assert_eq!(prop.roll, 3072);
    assert_eq!(prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED, 1);
    assert_ne!(prop.flags & psx_level::box_prop_flags::BREAK_ON_WALK, 0);
    assert_ne!(prop.flags & psx_level::box_prop_flags::BREAK_ON_ATTACK, 0);
}

#[test]
fn box_prop_cooks_authored_y_instead_of_snapping_to_floor() {
    // A box prop authored above the floor (e.g. stacked on top of
    // another box) must cook to its authored Y, matching the editor
    // preview (which uses the raw, un-anchored origin). Floor-anchoring
    // would collapse it onto the room floor underneath, which samples
    // the floor grid and ignores any box stacked there.
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
    // Raise the floor under (0,0) so a floor-snap would be observable.
    let sector_size = {
        let room = scene.node_mut(room_id).expect("room node");
        let NodeKind::Room { grid } = &mut room.kind else {
            panic!("starter room is a room");
        };
        let (sx, sz) = grid.editor_cells_to_array([0.0, 0.0]).unwrap();
        grid.set_floor(sx, sz, 512, Some(material_id));
        grid.sector_size as f32
    };
    let elevation = 9.0f32; // well above the raised floor
    let vertices = crate::box_prop_vertices_for_size(512);
    let prop_id = scene.add_node(
        room_id,
        "Elevated Box Prop",
        NodeKind::BoxProp {
            materials: [Some(material_id); crate::BOX_PROP_FACE_COUNT],
            vertices,
            collision_enabled: false,
            break_flags: 0,
        },
    );
    if let Some(node) = scene.node_mut(prop_id) {
        node.transform.translation = [0.0, elevation, 0.0];
    }

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let prop = package
        .box_props
        .iter()
        .find(|prop| prop.vertices == vertices)
        .expect("box prop cooks");

    let expected_y = (elevation * sector_size) as i32;
    assert_ne!(
        expected_y, 512,
        "test setup: authored elevation must differ from the floor height"
    );
    assert_eq!(
        prop.y, expected_y,
        "box prop must cook its authored Y (preview-consistent), not snap to the floor"
    );
    // The floor under the box is baked as ground_y so fragments and an
    // unsupported fall settle on the ground, not the elevated bottom.
    assert_eq!(
        prop.ground_y, 512,
        "box prop must bake the room-floor height beneath it as ground_y"
    );
}

#[test]
fn non_player_character_controller_cooks_idle_model_instance_with_yaw() {
    let mut project = project_with_one_room();
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
    assert!(instance.clip < package.models[instance.model as usize].clip_count);
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
    let mut project = project_with_one_room();
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
    assert!(src.contains("pub const CACHED_ROOM_DEPTH_MODE: u8 = 0;"));
    assert!(src.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MODE: u8 = 0;"));
    assert!(src.contains("pub const CACHED_ROOM_DRAW_ORDER_MODE: u8 = 0;"));
    assert!(src.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = 0;"));
    assert!(src.contains("pub static ROOMS: &[LevelRoomRecord] = &[\n];"));
    assert!(src.contains("pub static ROOM_CHUNKS: &[LevelChunkRecord] = &[\n];"));
    assert!(src.contains("pub static ROOM_PORTALS: &[LevelRoomPortalRecord] = &[\n];"));
    assert!(src.contains("pub static ROOM_NEAR_ROOMS: &[RoomIndex] = &[\n];"));
    assert!(src.contains("pub static ROOM_OVERLAPPED_ROOMS: &[RoomIndex] = &[\n];"));
    assert!(src.contains("pub static VISIBILITY_PVS: &[LevelVisibilityPvsRecord] = &[\n];"));
    assert!(src.contains("pub static VISIBILITY_PVS_BITS: &[u8] = &[\n];"));
    assert!(src.contains("pub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[\n];"));
    assert!(src.contains("pub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[\n];"));
    assert!(src.contains("pub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[\n];"));
    assert!(
        src.contains("pub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[\n];")
    );
    assert!(src.contains("pub static ROOM_RESIDENCY: &[RoomResidencyRecord] = &[\n];"));
    assert!(src.contains("pub static UI_NODES: &[LevelUiNodeRecord] = &[\n];"));
    assert!(src.contains("pub static OPTIONS: &[LevelOptionDef] = &[\n];"));
    assert!(src.contains("pub static ENTITIES: &[EntityRecord] = &[\n];"));
    assert!(src.contains("pub static PLAYER_SPAWN"));
}
