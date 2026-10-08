use super::*;

#[test]
fn model_storage_fits_all_meshes_and_projection_fits_the_largest() {
    let bytes = std::fs::read(
        crate::default_project_dir()
            .join("assets/models/tank_boss_animated_model/tank_boss_animated_model.psxmdl"),
    )
    .expect("starter tank model exists");
    let mesh = psx_asset::Model::from_bytes(&bytes).unwrap();
    let (vertices, faces, parts) = (mesh.vertex_count(), mesh.face_count(), mesh.part_count());
    let model = PlaytestModel {
        name: "capacity fixture".into(),
        source_resource: ResourceId(1),
        mesh_asset_index: 0,
        texture_asset_index: None,
        clip_first: 0,
        clip_count: 0,
        default_clip: 0,
        socket_first: 0,
        socket_count: 0,
        world_height: 64,
        collision_radius: 8,
    };
    let package = PlaytestPackage {
        assets: vec![PlaytestAsset {
            kind: PlaytestAssetKind::ModelMesh,
            bytes,
            filename: "fixture.psxmdl".into(),
            source_label: "fixture".into(),
            streamed_class: StreamedClass::None,
        }],
        models: vec![model.clone(), model],
        ..PlaytestPackage::default()
    };
    let source = render_manifest_source(&package);
    for (name, count) in [
        ("MODEL_PROJECTED_VERTEX_CAPACITY", usize::from(vertices)),
        ("MODEL_FACE_CAPACITY", usize::from(faces) * 2),
        ("MODEL_PART_CAPACITY", usize::from(parts) * 2),
        ("MODEL_DECODED_VERTEX_CAPACITY", usize::from(vertices) * 2),
    ] {
        assert!(
            source.contains(&format!("pub const {name}: usize = {count};")),
            "{name}"
        );
    }
    let empty = render_manifest_source(&PlaytestPackage::default());
    assert!(empty.contains("pub const MODEL_PROJECTED_VERTEX_CAPACITY: usize = 1;"));
}

#[test]
fn vitality_circles_emit_a_dedicated_manifest_table() {
    let package = PlaytestPackage {
        vitality_circles: vec![PlaytestVitalityCircle {
            room: 3,
            x: -40,
            y: 12,
            z: 90,
            radius: 256,
            axis: 1,
            refill_per_second: 14,
            drain_per_second: 9,
        }],
        ..PlaytestPackage::default()
    };
    let source = render_manifest_source(&package);
    assert!(source.contains("pub static VITALITY_CIRCLES: &[LevelVitalityCircleRecord]"));
    assert!(source.contains(
        "room: RoomIndex(3), x: -40, y: 12, z: 90, radius: 256, axis: 1, refill_per_second: 14, drain_per_second: 9"
    ));
}

#[test]
fn reflective_model_material_packs_probe_controls_without_losing_sidedness() {
    let material = PlaytestModelMaterialOverride {
        texture_asset_index: None,
        blend_mode: crate::PsxBlendMode::Average,
        tint_rgb: [128; 3],
        motion: crate::MaterialUvMotion::default(),
        secondary_layer: None,
        reflection_probe: Some(crate::ReflectionProbeMaterial {
            facet_normals: true,
            enabled: true,
            strength: 173,
            roughness: 191,
        }),
        face_sidedness: crate::MaterialFaceSidedness::Both,
    };

    let flags = model_material_flags(&material);
    let cooked = psx_level::LevelModelMaterialOverride {
        texture_asset: None,
        blend_mode: 0,
        tint_rgb: [128; 3],
        motion: psx_level::LevelMaterialUvMotion::default(),
        secondary_layer: None,
        flags,
    };
    assert_eq!(cooked.sidedness(), psx_level::LevelMaterialSidedness::Both);
    assert!(cooked.uses_room_reflection_probe());
    assert!(cooked.uses_facet_reflection());
    assert_eq!(cooked.reflection_roughness_level(), 2);
    assert_eq!(cooked.reflection_strength(), 173);
}

#[test]
fn room_texture_vram_bytes_match_runtime_compact_tile_upload() {
    let bytes =
        std::fs::read(crate::default_project_dir().join("assets/textures/sanctum_slate.psxt"))
            .expect("starter slate texture exists");
    let asset = PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: "texture_000.psxt".to_string(),
        source_label: "Sanctum slate".to_string(),
        streamed_class: StreamedClass::None,
    };

    assert_eq!(asset_vram_bytes(&asset), 8 * 32 * 2 + 16 * 2);
}

#[test]
fn model_atlas_vram_bytes_match_runtime_atlas_upload() {
    let bytes = std::fs::read(
        crate::default_project_dir()
            .join("assets/models/tank_boss_animated_model/tank_boss_animated_model.psxt"),
    )
    .expect("starter tank atlas exists");
    let asset = PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: "models/model_000_tank_boss/atlas.psxt".to_string(),
        source_label: "Tank Boss atlas".to_string(),
        streamed_class: StreamedClass::None,
    };

    assert_eq!(asset_vram_bytes(&asset), 32 * 128 * 2 + 16 * 2);
}

#[test]
fn write_cdda_tracks_cooks_sector_aligned_payloads_and_lists_paths() {
    let dir = std::env::temp_dir().join(format!(
        "psxed-project-test-{}-{}",
        std::process::id(),
        "cdda-tracks"
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let source = dir.join("menu music.wav");
    std::fs::write(&source, test_wav_mono_44k(&[0, 1200, -1200, 0])).unwrap();
    let cdda_dir = dir.join(CDDA_TRACKS_DIRNAME);
    std::fs::create_dir_all(&cdda_dir).unwrap();

    let mut package = PlaytestPackage::default();
    package.cdda_tracks.push(PlaytestCddaTrack {
        track: 2,
        wav_path: source.to_string_lossy().into_owned(),
        playback_speed_q12: crate::UI_MUSIC_PLAYBACK_SPEED_UNITY_Q12,
    });

    let list = write_cdda_tracks(&package, &cdda_dir).unwrap();
    let target = cdda_dir.join("track02.cdda");
    let cooked_len = std::fs::metadata(&target).unwrap().len();
    assert!(list.contains(&target.canonicalize().unwrap().display().to_string()));
    assert_eq!(cooked_len % psx_iso::SECTOR_BYTES as u64, 0);
    assert!(cooked_len > 0);

    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn empty_package_emits_gameplay_only_flow_and_no_scenes() {
    let package = PlaytestPackage::default();
    let src = render_manifest_source(&package);
    assert!(src.contains(
        "pub static UI_FONTS: &[&psx_font::BitmapFont] = &[\n    &psx_font::fonts::BASIC,\n];"
    ));
    assert!(src.contains("const _: () = assert!(UI_FONTS.len() <= 8);"));
    assert!(src.contains("pub static UI_SCENES: &[LevelUiScene] = &[\n];"));
    assert!(src.contains(
            "LevelSceneState { id: 0, name: \"Gameplay\", world: LevelWorldLayer::Gameplay, ui_scene: 65535, flags: 0, start_state: 65535 },"
        ));
    assert!(src.contains(
            "pub static GAME_FLOW: GameFlow = GameFlow {\n    states: &[\n        FlowState::SceneState { state: 0 },\n    ],\n    scene_states: SCENE_STATES,\n    entry: 0,\n    transition: LevelTransition { kind: LevelTransitionKind::None, frames: 0, color: [0, 0, 0], seed: 0 },\n};"
        ));
}

#[test]
fn ui_scene_table_and_flow_emit_addressable_scenes() {
    let package = PlaytestPackage {
        ui_nodes: vec![PlaytestUiNode {
            parent: None,
            kind: UiNodeKind::Canvas {
                width: 320,
                height: 240,
            },
            x: 0,
            y: 0,
            width: 320,
            height: 240,
            color: [0, 0, 0],
            background: [0, 0, 0],
            accent: [0, 0, 0],
            color_paint: None,
            background_paint: None,
            accent_paint: None,
            value: UiValueBinding::ConstantQ12(0),
            max: UiValueBinding::ConstantQ12(0),
            texture_asset: None,
            image_effect: UiImageEffect::None,
            text: String::new(),
            tag: String::new(),
            action: PlaytestUiAction::default(),
            option: psx_level::UI_OPTION_NONE,
            rotation_degrees: 0,
            flags: 0,
            sfx_first: psx_level::UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
        }],
        ui_scenes: vec![PlaytestUiScene {
            id: 7,
            name: "Pause".to_string(),
            node_first: 0,
            node_count: 1,
            focus_style: crate::ui_types::UiFocusStyle::default(),
        }],
        game_flow: PlaytestGameFlow {
            states: vec![
                PlaytestFlowState::SceneState { state: 1 },
                PlaytestFlowState::SceneState { state: 0 },
            ],
            scene_states: vec![
                PlaytestSceneState::gameplay(),
                PlaytestSceneState {
                    id: 1,
                    name: "Pause".to_string(),
                    world: PlaytestWorldLayer::None,
                    ui_scene: 7,
                    flags: psx_level::scene_state_flags::UI_INPUT,
                    start_state: 0,
                },
            ],
            entry: 0,
            transition: PlaytestTransition::NONE,
        },
        ..Default::default()
    };

    let src = render_manifest_source(&package);
    assert!(src.contains(
        "LevelUiScene { id: 7, name: \"Pause\", node_first: 0, node_count: 1, focus_style: \
         LevelUiFocusStyle { effect: LevelUiFocusEffect::Solid, color_a: (248, 224, 96), \
         color_b: (96, 88, 40), period: 96, thickness: 1, margin: 1, corner_len: 8 } },"
    ));
    assert!(src.contains("LevelSceneState { id: 1, name: \"Pause\", world: LevelWorldLayer::None, ui_scene: 7, flags: 1, start_state: 0 },"));
    assert!(src.contains("FlowState::SceneState { state: 1 },"));
    assert!(src.contains("FlowState::SceneState { state: 0 },"));
    assert!(src.contains("entry: 0,"));
}

#[test]
fn button_and_slider_nodes_render_action_accent_and_option_fields() {
    let package = PlaytestPackage {
        ui_nodes: vec![
            PlaytestUiNode {
                parent: None,
                kind: UiNodeKind::Button {
                    rect: crate::UiRect::new(0, 0, 80, 18),
                    label: "Play".to_string(),
                    tag: "menu.play".to_string(),
                    align: UiTextAlign::Center,
                    font: crate::UiFontChoice::Basic8x16,
                    font_scale: crate::default_ui_font_scale(),
                    letter_spacing: crate::default_ui_letter_spacing(),
                    color: [50, 60, 70],
                    background_gradient: None,
                    text_color: [236, 240, 248],
                    text_gradient: None,
                    transparent: false,
                    focus_chrome: false,
                    shape: None,
                    action: UiAction::Back,
                    sfx: crate::UiSfxBindings::default(),
                },
                x: 0,
                y: 0,
                width: 80,
                height: 18,
                color: [50, 60, 70],
                background: [0, 0, 0],
                accent: [0, 0, 0],
                color_paint: None,
                background_paint: None,
                accent_paint: None,
                value: UiValueBinding::ConstantQ12(0),
                max: UiValueBinding::ConstantQ12(0),
                texture_asset: None,
                image_effect: UiImageEffect::None,
                text: "Play".to_string(),
                tag: "menu.play".to_string(),
                action: PlaytestUiAction::GotoScene { scene: 7 },
                option: psx_level::UI_OPTION_NONE,
                rotation_degrees: 0,
                flags: 0,
                sfx_first: psx_level::UI_SFX_NONE,
                sfx_count: 0,
                font: 1,
                font_scale: crate::UI_FONT_SCALE_ONE_Q8 * 2,
                letter_spacing: 3,
            },
            PlaytestUiNode {
                parent: None,
                kind: UiNodeKind::Slider {
                    rect: crate::UiRect::new(0, 0, 96, 8),
                    option: crate::OptionId(3),
                    track: [11, 12, 13],
                    track_gradient: None,
                    fill: [21, 22, 23],
                    fill_gradient: None,
                    knob: [31, 32, 33],
                    knob_gradient: None,
                    sfx: crate::UiSfxBindings::default(),
                },
                x: 0,
                y: 0,
                width: 96,
                height: 8,
                color: [11, 12, 13],
                background: [21, 22, 23],
                accent: [31, 32, 33],
                color_paint: None,
                background_paint: None,
                accent_paint: None,
                value: UiValueBinding::ConstantQ12(0),
                max: UiValueBinding::ConstantQ12(0),
                texture_asset: None,
                image_effect: UiImageEffect::None,
                text: String::new(),
                tag: String::new(),
                action: PlaytestUiAction::default(),
                option: 3,
                rotation_degrees: 0,
                flags: 0,
                sfx_first: psx_level::UI_SFX_NONE,
                sfx_count: 0,
                font: 0,
                font_scale: crate::default_ui_font_scale(),
                letter_spacing: crate::default_ui_letter_spacing(),
            },
        ],
        ..Default::default()
    };

    let src = render_manifest_source(&package);
    assert!(src.contains("    &psx_font::fonts::BASIC_8X16,\n"));
    assert!(!src.contains("    &psx_font::fonts::BASIC,\n"));
    assert!(src.contains("kind: LevelUiNodeKind::Button"));
    assert!(src.contains("action: LevelUiAction::GotoScene { scene: 7 }"));
    assert!(src.contains("font: 0"));
    assert!(src.contains("font_scale: 512"));
    assert!(src.contains("letter_spacing: 3"));
    assert!(src.contains("tag: \"menu.play\""));
    assert!(src.contains("kind: LevelUiNodeKind::Slider"));
    assert!(src.contains("accent: [31, 32, 33]"));
    assert!(src.contains("option: 3"));
}

#[test]
fn ui_gradient_paints_emit_table_and_node_refs() {
    let package = PlaytestPackage {
        ui_paints: vec![PlaytestUiPaint {
            from: [20, 30, 40],
            to: [80, 90, 100],
            direction: UiGradientDirection::Horizontal,
        }],
        ui_nodes: vec![PlaytestUiNode {
            parent: None,
            kind: UiNodeKind::Rect {
                rect: crate::UiRect::new(0, 0, 80, 18),
                color: [20, 30, 40],
                gradient: Some(crate::UiGradient::new(
                    [80, 90, 100],
                    UiGradientDirection::Horizontal,
                )),
                transparent: false,
                shape: None,
            },
            x: 0,
            y: 0,
            width: 80,
            height: 18,
            color: [20, 30, 40],
            background: [0, 0, 0],
            accent: [0, 0, 0],
            color_paint: Some(0),
            background_paint: None,
            accent_paint: None,
            value: UiValueBinding::ConstantQ12(0),
            max: UiValueBinding::ConstantQ12(0),
            texture_asset: None,
            image_effect: UiImageEffect::None,
            text: String::new(),
            tag: String::new(),
            action: PlaytestUiAction::default(),
            option: psx_level::UI_OPTION_NONE,
            rotation_degrees: 0,
            flags: 0,
            sfx_first: psx_level::UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: crate::default_ui_font_scale(),
            letter_spacing: crate::default_ui_letter_spacing(),
        }],
        ..Default::default()
    };

    let src = render_manifest_source(&package);
    assert!(src.contains("pub static UI_PAINTS: &[LevelUiPaintRecord]"));
    assert!(src.contains(
            "LevelUiPaintRecord { from: [20, 30, 40], to: [80, 90, 100], direction: LevelUiGradientDirection::Horizontal }"
        ));
    assert!(src.contains("color_paint: 0"));
    assert!(src.contains("background_paint: psx_level::UI_PAINT_NONE"));
}

#[test]
fn ui_sfx_bank_streams_from_ui_pack_after_every_asset_chunk() {
    let sample = |bytes: Vec<u8>, name: &str| PlaytestUiSfxSample {
        bytes,
        filename: name.into(),
        source_path: name.into(),
    };
    let package = PlaytestPackage {
        assets: vec![test_texture_asset(0), test_texture_asset(1)],
        ui_sfx_samples: vec![
            sample(vec![1; 40], "a.psau"),
            sample(vec![2; 3000], "b.psau"),
        ],
        ..Default::default()
    };

    // Sample i is chunk assets.len() + i, after every asset id.
    let chunks = ui_pack_chunks(&package);
    assert_eq!(
        chunks,
        vec![(2, &[1u8; 40][..]), (3, &[2u8; 3000][..])],
        "non-streamed assets are not UI.PAK chunks; the two samples follow the asset ids"
    );
    let toc = ui_pack_toc(&package);
    assert_eq!(
        toc.iter().map(|entry| entry.chunk_id).collect::<Vec<_>>(),
        [2, 3]
    );
    assert_eq!(toc[1].byte_size, 3000);

    let src = render_manifest_source(&package);
    assert!(src.contains("pub const UI_SFX_PACK_FIRST_CHUNK: u32 = 2;"));
    assert!(src.contains("pub const UI_SFX_MAX_SAMPLE_BYTES: usize = 3000;"));
    assert!(src.contains(
        "#[cfg(feature = \"cd-stream-bench\")]\npub static UI_SFX_SAMPLE_001_BYTES: &[u8] = &[];"
    ));
    assert!(src.contains(
        "#[cfg(not(feature = \"cd-stream-bench\"))]\npub static UI_SFX_SAMPLE_001_BYTES: &[u8] = {"
    ));
    assert!(src.contains("bytes: *include_bytes!(\"ui_sfx/b.psau\") };"));
}

fn test_texture_asset(index: usize) -> PlaytestAsset {
    let bytes =
        std::fs::read(crate::default_project_dir().join("assets/textures/sanctum_slate.psxt"))
            .expect("starter slate texture exists");
    PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: format!("texture_{index:03}.psxt"),
        source_label: format!("Texture {index}"),
        streamed_class: StreamedClass::None,
    }
}
