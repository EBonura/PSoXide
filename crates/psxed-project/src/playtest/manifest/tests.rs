use super::*;

#[test]
fn cached_room_lighting_policy_emits_no_fog_passthrough() {
    let mut source = String::new();
    write_cached_room_lighting_policy(&mut source, false, false);

    assert!(source.contains("macro_rules! draw_project_cached_room"));
    assert!(source.contains("$draw($($before,)* $lighting, false, $($after,)*)"));
    assert!(!source.contains("ProjectCachedRoomLighting"));
}

#[test]
fn cached_room_lighting_policy_emits_fog_specialization() {
    let mut source = String::new();
    write_cached_room_lighting_policy(&mut source, true, false);

    assert!(source.contains("pub struct ProjectCachedRoomLighting"));
    assert!(source.contains("#[inline(always)]"));
    assert!(source.contains("apply_vertex_fog_weight"));
    assert!(source.contains("&cached_lighting, true"));
    assert!(!source.contains("apply_black_room_fog_weight"));
}

#[test]
fn cached_room_lighting_policy_specializes_black_fog() {
    let mut source = String::new();
    write_cached_room_lighting_policy(&mut source, true, true);

    assert!(source.contains("pub struct ProjectCachedRoomLighting"));
    assert!(source.contains("psx_game_runtime::room_lighting::apply_black_room_fog_weight"));
    assert!(!source.contains("self.lighting.apply_vertex_fog_weight"));
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
    assert_eq!(cooked.reflection_roughness_level(), 2);
    assert_eq!(cooked.reflection_strength(), 173);
}

#[test]
fn room_texture_vram_bytes_match_runtime_compact_tile_upload() {
    let bytes = std::fs::read(
        crate::legacy_grid_starter_dir().join("assets/textures/delven_01_slateflr1a_q2.psxt"),
    )
    .expect("starter Delven texture exists");
    let asset = PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: "texture_000.psxt".to_string(),
        source_label: "Delven slateflr1a q2".to_string(),
        streamed_class: StreamedClass::None,
    };

    assert_eq!(asset_vram_bytes(&asset), 8 * 32 * 2 + 16 * 2);
}

#[test]
fn model_atlas_vram_bytes_match_runtime_atlas_upload() {
    let bytes = std::fs::read(
        crate::legacy_grid_starter_dir()
            .join("assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt"),
    )
    .expect("starter wraith atlas exists");
    let asset = PlaytestAsset {
        kind: PlaytestAssetKind::Texture,
        bytes,
        filename: "models/model_000_obsidian_wraith/atlas.psxt".to_string(),
        source_label: "Obsidian Wraith atlas".to_string(),
        streamed_class: StreamedClass::None,
    };

    assert_eq!(asset_vram_bytes(&asset), 64 * 128 * 2 + 256 * 2);
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
fn world_pack_order_starts_at_spawn_and_walks_chunk_neighbours() {
    let chunks = [
        test_chunk(0, 0, 0, 0, [None, Some(1), Some(2), None]),
        test_chunk(1, 0, 1, 0, [None, None, Some(3), Some(0)]),
        test_chunk(2, 0, 0, 1, [Some(0), Some(3), None, None]),
        test_chunk(3, 0, 1, 1, [Some(1), None, None, Some(2)]),
    ];

    assert_eq!(
        world_pack_order_from_chunks(4, Some(2), &chunks),
        vec![2, 0, 3, 1]
    );
}

#[test]
fn world_pack_order_appends_disconnected_chunks_by_proximity() {
    let chunks = [
        test_chunk(0, 10, 0, 0, [None; 4]),
        test_chunk(1, 11, 50, 0, [None; 4]),
        test_chunk(2, 12, 5, 0, [None; 4]),
    ];

    assert_eq!(
        world_pack_order_from_chunks(3, Some(0), &chunks),
        vec![0, 2, 1]
    );
}

#[test]
fn world_pack_toc_uses_same_layout_as_pack_builder() {
    let package = PlaytestPackage {
        assets: vec![
            test_room_asset(static_lit_test_room_bytes(), 0),
            test_room_asset(static_lit_test_room_bytes(), 1),
            test_room_asset(static_lit_test_room_bytes(), 2),
        ],
        rooms: vec![test_room(0), test_room(1), test_room(2)],
        chunks: vec![
            test_chunk(0, 0, 0, 0, [None, Some(1), Some(2), None]),
            test_chunk(1, 0, 1, 0, [None, None, None, Some(0)]),
            test_chunk(2, 0, 0, 1, [Some(0), None, None, None]),
        ],
        spawn: Some(PlaytestSpawn {
            room: 2,
            x: 0,
            y: 0,
            z: 0,
            yaw: 0,
            flags: 1,
        }),
        ..Default::default()
    };

    let order = world_pack_order(&package);
    assert_eq!(order, vec![2, 0, 1]);
    let refs = order
        .iter()
        .map(|room| {
            (
                *room as u32,
                streamed_room_chunk_payload(&package, *room).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let refs = refs
        .iter()
        .map(|(room, bytes)| (*room, bytes.as_slice()))
        .collect::<Vec<_>>();
    assert_eq!(
        world_pack_toc(&package),
        psx_iso::build_world_pack_layout(&refs).entries
    );

    let manifest = render_manifest_source(&package);
    assert!(manifest.contains("pub const WORLD_RESIDENT_CHUNK_LIMIT: usize = 10;"));
    assert!(manifest.contains("pub const WORLD_STREAM_SLOT_COUNT: usize = 3;"));
    assert!(manifest.contains("pub const WORLD_RESIDENT_PAGE_COUNT: usize = 3;"));
    assert!(manifest.contains("pub const WORLD_PACK_START_LBA: u32 = 1024;"));
    assert!(manifest.contains("pub static WORLD_PACK_TOC: &[LevelWorldPackEntryRecord]"));
    assert!(manifest.contains("LevelWorldPackEntryRecord { room: RoomIndex(2), sector_offset: 1, sector_count: 1, byte_size: 148"));
}

#[test]
fn empty_package_emits_gameplay_only_flow_and_no_scenes() {
    let package = PlaytestPackage::default();
    let src = render_manifest_source(&package);
    assert!(src.contains("pub const WORLD_STREAM_SLOT_COUNT: usize = 1;"));
    assert!(src.contains("pub const WORLD_RESIDENT_PAGE_COUNT: usize = 1;"));
    assert!(src.contains(
        "pub static UI_FONTS: &[&psx_font::BitmapFont] = &[\n    &psx_font::fonts::BASIC,\n];"
    ));
    assert!(src.contains("const _: () = assert!(UI_FONTS.len() <= 8);"));
    assert!(src.contains("pub static UI_SCENES: &[LevelUiScene] = &[\n];"));
    assert!(src.contains(
            "LevelSceneState { id: 0, name: \"Gameplay\", world: LevelWorldLayer::Gameplay, ui_scene: 65535, flags: 0, start_state: 65535 },"
        ));
    assert!(src.contains(
            "pub static GAME_FLOW: GameFlow = GameFlow {\n    states: &[\n        FlowState::SceneState { state: 0 },\n    ],\n    scene_states: SCENE_STATES,\n    entry: 0,\n};"
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
fn cd_stream_manifest_does_not_embed_room_bytes_or_global_cache_tables() {
    let mut package = PlaytestPackage {
        runtime_depth_sort_mode: crate::RuntimeDepthSortMode::PerTriangle,
        runtime_texture_split_mode: crate::RuntimeTextureSplitMode::DepthSorted,
        runtime_room_draw_order_mode: crate::RuntimeRoomDrawOrderMode::Portal,
        runtime_texture_split_max_edge: 96,
        assets: vec![test_room_asset(static_lit_test_room_bytes(), 0)],
        rooms: vec![test_room(0)],
        ..Default::default()
    };
    package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
        room: 0,
        cell_first: 0,
        cell_count: 0,
        cell_vertex_first: 0,
        cell_vertex_count: 0,
        vertex_first: 0,
        vertex_count: 0,
        surface_first: 0,
        surface_count: 0,
    }];

    let src = render_manifest_source(&package);
    assert!(src.contains("pub const CACHED_ROOM_DEPTH_MODE: u8 = 3;"));
    assert!(src.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MODE: u8 = 1;"));
    assert!(src.contains("pub const CACHED_ROOM_DRAW_ORDER_MODE: u8 = 1;"));
    assert!(src.contains("pub const CACHED_ROOM_TEXTURE_SPLIT_MAX_EDGE: u16 = 96;"));
    assert!(src.contains(
        "#[cfg(feature = \"cd-stream-bench\")]\npub static ASSET_000_ROOM_000_BYTES: &[u8] = &[];"
    ));
    assert!(src.contains("#[cfg(not(feature = \"cd-stream-bench\"))]\npub static ASSET_000_ROOM_000_BYTES: &[u8] = {"));
    assert!(src.contains("bytes: *include_bytes!(\"rooms/room_000.psxw\") };"));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read room-surface cache slices from `.psxc` chunks.\npub static ROOM_SURFACE_CACHES: &[LevelRoomSurfaceCacheRecord] = &[];"));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room cells from `.psxc` chunks.\npub static ROOM_CACHE_CELLS: &[LevelCachedRoomCellRecord] = &[];"));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached cell vertex indices from `.psxc` chunks.\npub static ROOM_CACHE_CELL_VERTICES: &[u16] = &[];"));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room vertices from `.psxc` chunks.\npub static ROOM_CACHE_VERTICES: &[LevelCachedRoomVertexRecord] = &[];"));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\n/// Stream builds read cached room surfaces from `.psxc` chunks.\npub static ROOM_CACHE_SURFACES: &[LevelCachedRoomSurfaceRecord] = &[];"));
}

#[test]
fn streamed_room_chunk_payload_splits_collision_and_render_cache_records() {
    let mut package = PlaytestPackage {
        assets: vec![test_room_asset(static_lit_test_room_bytes(), 0)],
        rooms: vec![test_room(0)],
        ..Default::default()
    };
    package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
        room: 0,
        cell_first: 0,
        cell_count: 1,
        cell_vertex_first: 0,
        cell_vertex_count: 4,
        vertex_first: 0,
        vertex_count: 1,
        surface_first: 0,
        surface_count: 1,
    }];
    package.room_cache_cells = vec![PlaytestCachedRoomCell {
        x: 2,
        z: 3,
        min_y: -4,
        max_y: 5,
        visibility_center: [6, 7, 8],
        visibility_radius: 9,
        surface_first: 10,
        surface_count: 11,
        vertex_first: 0,
        vertex_count: 4,
    }];
    package.room_cache_cell_vertices = vec![0, 1, 2, 3];
    package.room_cache_vertices = vec![PlaytestCachedRoomVertex {
        x: 12,
        y: 13,
        z: 14,
    }];
    package.room_cache_surfaces = vec![PlaytestCachedRoomSurface {
        material_slot: 15,
        vertex_indices: [0, 1, 2, 3],
        sample_sx: 16,
        sample_sz: 17,
        sample_ordinal: 18,
        uv_words: [0x1413, 0x1615, 0x1817, 0x1a19],
        baked_vertex_rgb: [(27, 28, 29), (30, 31, 32), (33, 34, 35), (36, 37, 38)],
        kind_flags: 39,
        wall_direction: 40,
        split: 41,
        triangle_index: 42,
    }];

    let payload = streamed_room_chunk_payload(&package, 0).unwrap();
    assert_eq!(
        &payload[..8],
        psx_level::STREAMED_ROOM_CHUNK_MAGIC.as_slice()
    );
    assert_eq!(u32_at(&payload, 8), psx_level::STREAMED_ROOM_CHUNK_VERSION);
    assert_eq!(u32_at(&payload, 12), 0);
    assert_eq!(u32_at(&payload, 16), payload.len() as u32);
    assert_eq!(u32_at(&payload, 20), 64);
    assert_eq!(u32_at(&payload, 24), 84);
    assert_eq!(u32_at(&payload, 28), 148);
    assert_eq!(u32_at(&payload, 32), 1);
    assert_eq!(u32_at(&payload, 36), 192);
    assert_eq!(u32_at(&payload, 40), 1);
    assert_eq!(u32_at(&payload, 44), 204);
    assert_eq!(u32_at(&payload, 48), 1);
    assert_eq!(u32_at(&payload, 52), 184);
    assert_eq!(u32_at(&payload, 56), 4);
    assert_eq!(
        u32_at(&payload, 60),
        psx_level::STREAMED_ROOM_CHUNK_FLAG_COLLISION_COMPACT
    );
    assert_eq!(
        &payload[64..72],
        psx_level::COMPACT_COLLISION_MAGIC.as_slice()
    );
    assert_eq!(u16_at(&payload, 148), 2);
    assert_eq!(i32_at(&payload, 152), -4);
    assert_eq!(u16_at(&payload, 180), 0);
    assert_eq!(u16_at(&payload, 184), 0);
    assert_eq!(u16_at(&payload, 190), 3);
    assert_eq!(i32_at(&payload, 192), 12);
    assert_eq!(u16_at(&payload, 204), 15);
    assert_eq!(payload[243], 42);
}

#[test]
fn streamed_room_chunk_memory_report_accounts_for_collision_render_and_padding() {
    let mut package = PlaytestPackage {
        assets: vec![test_room_asset(static_lit_test_room_bytes(), 0)],
        rooms: vec![test_room(0)],
        ..Default::default()
    };
    package.room_surface_caches = vec![PlaytestRoomSurfaceCache {
        room: 0,
        cell_first: 0,
        cell_count: 1,
        cell_vertex_first: 0,
        cell_vertex_count: 4,
        vertex_first: 0,
        vertex_count: 1,
        surface_first: 0,
        surface_count: 1,
    }];
    package.room_cache_cells = vec![PlaytestCachedRoomCell {
        x: 2,
        z: 3,
        min_y: -4,
        max_y: 5,
        visibility_center: [6, 7, 8],
        visibility_radius: 9,
        surface_first: 10,
        surface_count: 11,
        vertex_first: 0,
        vertex_count: 4,
    }];
    package.room_cache_cell_vertices = vec![0, 1, 2, 3];
    package.room_cache_vertices = vec![PlaytestCachedRoomVertex {
        x: 12,
        y: 13,
        z: 14,
    }];
    package.room_cache_surfaces = vec![PlaytestCachedRoomSurface {
        material_slot: 15,
        vertex_indices: [0, 1, 2, 3],
        sample_sx: 16,
        sample_sz: 17,
        sample_ordinal: 18,
        uv_words: [0x1413, 0x1615, 0x1817, 0x1a19],
        baked_vertex_rgb: [(27, 28, 29), (30, 31, 32), (33, 34, 35), (36, 37, 38)],
        kind_flags: 39,
        wall_direction: 40,
        split: 41,
        triangle_index: 42,
    }];

    let report = streamed_room_chunk_memory_report(&package).unwrap();
    assert_eq!(report.chunks.len(), 1);
    let chunk = report.chunks[0];
    assert_eq!(chunk.room, 0);
    assert_eq!(
        chunk.payload_bytes,
        streamed_room_chunk_payload(&package, 0).unwrap().len()
    );
    assert_eq!(chunk.collision_bytes, 84);
    assert_eq!(chunk.render_cell_bytes, 36);
    assert_eq!(chunk.render_cell_vertex_bytes, 8);
    assert_eq!(chunk.render_vertex_bytes, 12);
    assert_eq!(chunk.render_surface_bytes, 40);
    assert_eq!(chunk.render_cache_bytes, 96);
    assert_eq!(chunk.alignment_padding_bytes, 0);
    assert_eq!(chunk.sector_count, 1);
    assert_eq!(chunk.stream_bytes, psx_iso::SECTOR_USER_DATA_BYTES);
    assert_eq!(
        chunk.sector_padding_bytes,
        psx_iso::SECTOR_USER_DATA_BYTES - chunk.payload_bytes
    );
    assert_eq!(
        report.totals,
        PlaytestStreamMemoryTotals {
            sector_count: chunk.sector_count,
            payload_bytes: chunk.payload_bytes,
            stream_bytes: chunk.stream_bytes,
            header_bytes: chunk.header_bytes,
            collision_bytes: chunk.collision_bytes,
            render_cell_bytes: chunk.render_cell_bytes,
            render_cell_vertex_bytes: chunk.render_cell_vertex_bytes,
            render_vertex_bytes: chunk.render_vertex_bytes,
            render_surface_bytes: chunk.render_surface_bytes,
            render_cache_bytes: chunk.render_cache_bytes,
            alignment_padding_bytes: chunk.alignment_padding_bytes,
            sector_padding_bytes: chunk.sector_padding_bytes,
        }
    );
}

#[test]
fn compact_collision_payload_matches_runtime_room_collision() {
    let bytes = static_lit_test_room_bytes();
    let payload = compact_collision_payload(&bytes, 0, &[]).unwrap();
    assert_eq!(
        payload.len(),
        psx_level::COMPACT_COLLISION_HEADER_BYTES + psx_level::COMPACT_COLLISION_SECTOR_BYTES
    );
    assert_eq!(&payload[..8], psx_level::COMPACT_COLLISION_MAGIC.as_slice());
    assert_eq!(
        u32_at(&payload, psx_level::compact_collision_header::VERSION),
        psx_level::COMPACT_COLLISION_VERSION
    );
    assert_eq!(
        u16_at(&payload, psx_level::compact_collision_header::WIDTH),
        1
    );
    assert_eq!(
        u16_at(&payload, psx_level::compact_collision_header::DEPTH),
        1
    );
    assert_eq!(
        i32_at(&payload, psx_level::compact_collision_header::SECTOR_SIZE),
        1024
    );
    assert_eq!(
        u16_at(&payload, psx_level::compact_collision_header::SECTOR_COUNT),
        1
    );
    assert_eq!(
        &payload[psx_level::compact_collision_header::AMBIENT_RGB
            ..psx_level::compact_collision_header::AMBIENT_RGB + 3],
        &[7, 8, 9]
    );
    let room = psx_engine::CompactCollisionRoom::from_bytes(&payload).unwrap();
    assert_eq!(room.width(), 1);
    assert_eq!(room.depth(), 1);
    assert_eq!(room.ambient_color(), [7, 8, 9]);
    let sector = room.collision().sector(0, 0).unwrap();
    assert!(sector.has_floor());
    assert!(sector.floor_walkable());
    assert_eq!(sector.floor_heights(), [0; 4]);
}

#[test]
fn compact_collision_payload_preserves_floor_links() {
    let bytes = static_lit_test_room_bytes();
    let floor_links = [PlaytestRoomFloorLink {
        room: 0,
        x: 0,
        z: 0,
        above_room: Some(2),
        below_room: Some(3),
    }];
    let payload = compact_collision_payload(&bytes, 0, &floor_links).unwrap();
    let room = psx_engine::CompactCollisionRoom::from_bytes(&payload).unwrap();
    let sector = room.collision().sector(0, 0).unwrap();
    assert_eq!(
        sector.floor_above_room(),
        Some(psx_level::RoomIndex::new(2))
    );
    assert_eq!(
        sector.floor_below_room(),
        Some(psx_level::RoomIndex::new(3))
    );
}

fn test_room_asset(bytes: Vec<u8>, index: usize) -> PlaytestAsset {
    PlaytestAsset {
        kind: PlaytestAssetKind::RoomWorld,
        bytes,
        filename: format!("room_{index:03}.psxw"),
        source_label: format!("Room {index}"),
        streamed_class: StreamedClass::None,
    }
}

fn test_room(world_asset_index: usize) -> PlaytestRoom {
    PlaytestRoom {
        reflection_probe_asset_index: None,
        name: format!("Room {world_asset_index}"),
        world_asset_index: Some(world_asset_index),
        origin_x: 0,
        origin_z: 0,
        origin_y: 0,
        sector_size: 1024,
        draw_distance: 25_000,
        chunk_activation_radius_sectors: 64,
        visibility_radius: 32,
        resident_chunk_limit: 10,
        visible_chunk_limit: 10,
        gravity_per_tick: 96,
        material_first: 0,
        material_count: 0,
        portal_first: 0,
        portal_count: 0,
        near_room_first: 0,
        near_room_count: 0,
        overlapped_room_first: 0,
        overlapped_room_count: 0,
        fog_rgb: [0, 0, 0],
        fog_near: 0,
        fog_far: 0,
        atmosphere_rgb: [0, 0, 0],
        atmosphere_density: 0,
        atmosphere_fall_speed_q4: 0,
        atmosphere_wind_speed_q4: 0,
        sky: PlaytestSky {
            top_rgb: [0, 0, 0],
            horizon_rgb: [0, 0, 0],
            bottom_rgb: [0, 0, 0],
            horizon_percent: 50,
            horizon_thickness_percent: 8,
            skybox_columns: 16,
            skybox_rows: 10,
            flags: 0,
            cyclorama_quads: Vec::new(),
            cloud_layer: PlaytestCloudLayer {
                texture_asset_index: None,
                color_rgb: [0, 0, 0],
                density: 0,
                altitude: 0,
                extent: 0,
                tile_count: 0,
                scroll_speed: [0, 0],
                noise_seed: 0,
                flags: 0,
            },
        },
        far_vista: PlaytestFarVista {
            texture_asset_indices: Vec::new(),
            radius: 0,
            height: 0,
            vertical_offset: 0,
            segments: 0,
            rotation_degrees: 0,
            tint_rgb: [0, 0, 0],
            flags: 0,
        },
        camera: PlaytestCamera {
            distance: 0,
            height: 0,
            target_height: 0,
            lock_rise_percent: 0,
            min_floor_clearance: 0,
            orbit_speed_level: 0,
            position_lag_shift: 0,
            focus_lag_shift: 0,
            distance_lag_shift: 0,
        },
        flags: 0,
    }
}

fn static_lit_test_room_bytes() -> Vec<u8> {
    let asset_header = psxed_format::AssetHeader::SIZE;
    let world_header = psxed_format::world::WorldHeader::SIZE;
    let sector_bytes = psxed_format::world::SectorRecord::SIZE;
    let light_bytes = 2 * psxed_format::world::SurfaceLightRecord::SIZE;
    let payload_len = world_header + sector_bytes + light_bytes;
    let mut out = vec![0u8; asset_header + payload_len];
    out[0..4].copy_from_slice(&psxed_format::world::MAGIC);
    out[4..6].copy_from_slice(&psxed_format::world::VERSION.to_le_bytes());
    out[8..12].copy_from_slice(&(payload_len as u32).to_le_bytes());

    let wh = asset_header;
    out[wh..wh + 2].copy_from_slice(&1u16.to_le_bytes());
    out[wh + 2..wh + 4].copy_from_slice(&1u16.to_le_bytes());
    out[wh + 4..wh + 8].copy_from_slice(&1024i32.to_le_bytes());
    out[wh + 8..wh + 10].copy_from_slice(&1u16.to_le_bytes());
    out[wh + 14..wh + 17].copy_from_slice(&[7, 8, 9]);
    out[wh + 17] = psxed_format::world::world_flags::STATIC_VERTEX_LIGHTING;
    out[wh + 18..wh + 20].copy_from_slice(&2u16.to_le_bytes());

    let sector = wh + world_header;
    out[sector] = psxed_format::world::sector_flags::HAS_FLOOR
        | psxed_format::world::sector_flags::FLOOR_WALKABLE;
    out
}

fn test_chunk(
    room: u16,
    authored_room: u32,
    origin_x: i32,
    origin_z: i32,
    neighbours: [Option<u16>; 4],
) -> PlaytestChunk {
    PlaytestChunk {
        room,
        authored_room,
        chunk_index: room,
        origin_x,
        origin_z,
        width: 1,
        depth: 1,
        neighbours,
        triangles: 0,
        psxw_bytes: 0,
        static_lit_bytes: 0,
        populated_cells: 0,
        flags: 0,
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
