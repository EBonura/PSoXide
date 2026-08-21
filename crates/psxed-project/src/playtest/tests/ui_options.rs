use super::*;
use crate::SceneWorldLayer;

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
    assert!(manifest.contains("pub const CACHED_ROOM_DEPTH_MODE: u8 = 2;"));
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
    // A Camera parented to the player overrides the World node's settings
    // (see player_camera_component_drives_cooked_camera), so the authored
    // camera the cook should reproduce is that one when it exists.
    let scene = project.active_scene();
    let expected_camera = scene
        .nodes()
        .iter()
        .find_map(|node| match &node.kind {
            NodeKind::Camera { settings } => Some(*settings),
            _ => None,
        })
        .or_else(|| {
            scene.nodes().iter().find_map(|node| match &node.kind {
                NodeKind::World { camera, .. } => Some(*camera),
                _ => None,
            })
        })
        .expect("starter authors camera settings");
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
fn default_health_gauge_cooks_as_one_resident_seven_frame_texture() {
    let project = ProjectDocument::starter();
    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut used_ui_source_paths = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, _scenes, _sfx_samples, _sfx_cues, _flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        &crate::default_project_dir(),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut used_ui_source_paths,
        &mut report,
    );
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let gauge = nodes
        .iter()
        .find(|node| {
            matches!(
                node.kind,
                UiNodeKind::Bar {
                    value: UiValueBinding::PlayerHealth,
                    ..
                }
            )
        })
        .expect("health gauge cooked");
    assert_eq!(gauge.option, 7);
    assert_eq!((gauge.width, gauge.height), (106, 29));
    let asset_index = gauge.texture_asset.expect("gauge has texture asset");
    let asset = &assets[asset_index];
    assert_eq!(asset.streamed_class, StreamedClass::None);
    let texture = psx_asset::Texture::from_bytes(&asset.bytes).expect("parse gauge atlas");
    assert_eq!((texture.width(), texture.height()), (106, 203));
    assert_eq!(texture.depth(), TextureDepth::Bit4);
    assert_eq!(texture.clut_entries(), 16);
    assert!(used_ui_source_paths
        .iter()
        .any(|path| path.ends_with("assets/ui/health_bar_clean_slim.psxt")));
}

#[test]
fn tracked_health_gauge_projects_attach_the_hud_to_gameplay() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let project_paths = [
        manifest.join("../../projects/default/project.ron"),
        manifest.join("../../projects/mantis/project.ron"),
        manifest.join("../../projects/quake-e1m1-geometry/project.ron"),
        manifest.join("../../projects/tech-demo/project.ron"),
        manifest.join("../../samples/cortex_v1/project.ron"),
        manifest.join("../../archive/fixtures/brush-open-courtyard/project.ron"),
    ];

    for project_path in project_paths {
        let project = ProjectDocument::load_from_path(&project_path)
            .unwrap_or_else(|error| panic!("{}: {error}", project_path.display()));
        let hud = project
            .ui_scenes
            .iter()
            .find(|scene| scene.name == "HUD")
            .unwrap_or_else(|| panic!("{} has no HUD scene", project_path.display()));
        let gameplay = project
            .scene_states
            .iter()
            .find(|state| state.world == SceneWorldLayer::Gameplay)
            .unwrap_or_else(|| panic!("{} has no gameplay state", project_path.display()));
        assert_eq!(
            gameplay.ui_scene,
            Some(hud.id),
            "{} does not activate its HUD during gameplay",
            project_path.display()
        );
    }
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
    let prompt = scene.add_node(
        group,
        "Prompt",
        UiNodeKind::Label {
            rect: UiRect::new(8, 6, 48, 12),
            text: "Open".to_string(),
            random_message: false,
            messages: Vec::new(),
            tag: "prompt".to_string(),
            align: crate::UiTextAlign::Left,
            wrap: false,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: -2,
            color: [220, 226, 240],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );
    scene.node_mut(prompt).expect("prompt node").visible_when =
        crate::UiVisibilityCondition::LoadingComplete;

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _paints, scenes, _sfx_samples, _sfx_cues, flow, _cdda_tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut Vec::new(),
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
    assert_ne!(
        nodes[label_index].flags & psx_level::ui_node_flags::LOADING_COMPLETE_ONLY,
        0
    );
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
            transparent: false,
            shape: None,
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
        &mut Vec::new(),
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
fn clipped_transparent_rect_cooks_border_and_compact_shape_style() {
    let mut project = ProjectDocument::new("ui-shape");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Cut Panel",
        UiNodeKind::Rect {
            rect: UiRect::new(12, 18, 100, 30),
            color: [8, 10, 12],
            gradient: None,
            transparent: true,
            shape: Some(crate::UiShapeStyle {
                semi_transparent_fill: false,
                corner_cut: 6,
                cut_top_left: true,
                cut_top_right: false,
                cut_bottom_right: true,
                cut_bottom_left: false,
                border_width: 2,
                border_color: [18, 92, 110],
                border_gradient: Some(crate::UiGradient::new(
                    [70, 120, 126],
                    crate::UiGradientDirection::Horizontal,
                )),
            }),
        },
    );
    scene.add_node(
        scene.root,
        "Play",
        UiNodeKind::Button {
            rect: UiRect::new(24, 80, 72, 18),
            label: "PLAY".to_string(),
            align: UiTextAlign::Center,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
            color: [8, 10, 12],
            background_gradient: None,
            text_color: [90, 120, 126],
            text_gradient: None,
            transparent: false,
            shape: Some(crate::UiShapeStyle {
                semi_transparent_fill: true,
                corner_cut: 6,
                cut_top_left: true,
                cut_top_right: false,
                cut_bottom_right: true,
                cut_bottom_left: false,
                border_width: 2,
                border_color: [18, 92, 110],
                border_gradient: Some(crate::UiGradient::new(
                    [70, 120, 126],
                    crate::UiGradientDirection::Horizontal,
                )),
            }),
            action: UiAction::Back,
            sfx: crate::UiSfxBindings::default(),
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, paints, _scenes, _samples, _cues, _flow, _tracks) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut Vec::new(),
        &mut report,
    );

    assert!(report.is_ok(), "warnings/errors: {:?}", report);
    let panel = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Rect { .. }))
        .expect("styled rect cooked");
    assert!(psx_level::ui_shape::is_encoded(panel.option));
    assert_eq!(
        psx_level::ui_shape::corners(panel.option),
        psx_level::ui_shape::TOP_LEFT | psx_level::ui_shape::BOTTOM_RIGHT
    );
    assert_eq!(psx_level::ui_shape::cut(panel.option), 6);
    assert_eq!(psx_level::ui_shape::border(panel.option), 2);
    assert!(psx_level::ui_shape::transparent(panel.option));
    assert_eq!(panel.background, [18, 92, 110]);
    assert_eq!(panel.background_paint, Some(0));
    assert_eq!(paints[0].from, [18, 92, 110]);
    assert_eq!(paints[0].to, [70, 120, 126]);

    let button = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Button { .. }))
        .expect("styled button cooked");
    assert!(psx_level::ui_shape::is_encoded(button.option));
    assert_eq!(psx_level::ui_shape::corners(button.option), 0b0101);
    assert_eq!(psx_level::ui_shape::cut(button.option), 6);
    assert_eq!(psx_level::ui_shape::border(button.option), 2);
    assert!(!psx_level::ui_shape::transparent(button.option));
    assert!(psx_level::ui_shape::semi_transparent_fill(button.option));
    assert_eq!(button.background, [18, 92, 110]);
    assert_eq!(button.background_paint, Some(0));
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
        &mut Vec::new(),
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
fn random_label_messages_cook_into_compact_runtime_text() {
    let mut project = ProjectDocument::new("ui-random-label");
    let scene = project.active_ui_scene_mut().expect("default ui scene");
    scene.add_node(
        scene.root,
        "Lore",
        UiNodeKind::Label {
            rect: UiRect::new(8, 8, 200, 32),
            text: "fallback".to_string(),
            random_message: true,
            messages: vec!["first".to_string(), "second".to_string()],
            tag: String::new(),
            align: crate::UiTextAlign::Left,
            wrap: true,
            font: crate::UiFontChoice::Basic,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: 0,
            color: [220, 226, 240],
            gradient: None,
            effect: UiImageEffect::None,
        },
    );

    let mut texture_asset_for_resource = HashMap::new();
    let mut assets = Vec::new();
    let mut report = PlaytestValidationReport::default();
    let (nodes, _, _, _, _, _, _) = cook_ui_nodes(
        &project,
        Path::new("."),
        &mut texture_asset_for_resource,
        &mut assets,
        &mut Vec::new(),
        &mut report,
    );

    assert!(report.is_ok(), "warnings/errors: {:?}", report);
    let lore = nodes
        .iter()
        .find(|node| matches!(node.kind, UiNodeKind::Label { .. }))
        .expect("label cooked");
    assert_eq!(lore.text, "first\u{1f}second");
    assert_ne!(
        lore.flags & psx_level::ui_node_flags::TEXT_RANDOM_MESSAGE,
        0
    );
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
        &mut Vec::new(),
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
        &mut Vec::new(),
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
            shape: None,
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
        &mut Vec::new(),
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
            shape: None,
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
        &mut Vec::new(),
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
            shape: None,
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
            shape: None,
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
        &mut Vec::new(),
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
    let package = PlaytestPackage {
        options: vec![PlaytestOption {
            id: 7,
            min: 0,
            max: 5,
            step: 1,
            default: 3,
        }],
        ..Default::default()
    };
    let src = render_manifest_source(&package);
    assert!(src.contains("pub static OPTIONS: &[LevelOptionDef] = &[\n"));
    assert!(src.contains("LevelOptionDef { id: 7, min: 0, max: 5, step: 1, default: 3 },"));
}
