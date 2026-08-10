use super::*;

#[test]
fn starter_project_emits_expected_texture_assets() {
    // Starter cooks the BIGDOOR_1A room texture, one sky panorama, the
    // Aletha Crystal material, and the player model atlas.
    let project = project_with_one_room();
    let (package, _) = build_package(&project, &starter_project_root());
    let package = package.expect("starter cooks");
    assert_eq!(package.texture_asset_count(), 4);
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
fn room_material_blend_mode_survives_package_cook() {
    let mut project = project_with_one_room();
    let room_material = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| {
            let NodeKind::Section { grid } = &node.kind else {
                return None;
            };
            grid.sectors
                .iter()
                .flatten()
                .find_map(|sector| sector.floor.as_ref()?.material)
        })
        .expect("starter room has a floor material");
    let ResourceData::Material(material) = &mut project
        .resource_mut(room_material)
        .expect("room material remains addressable")
        .data
    else {
        panic!("room surface points to a Material");
    };
    material.blend_mode = crate::PsxBlendMode::Average;

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("translucent room cooks");
    let cooked = package.materials[package.rooms[0].material_first as usize];
    assert_eq!(cooked.blend_mode, crate::PsxBlendMode::Average);
}

#[test]
fn transition_material_survives_the_complete_room_package_cook() {
    let mut project = project_with_one_room();
    let room_material = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| {
            let NodeKind::Section { grid } = &node.kind else {
                return None;
            };
            grid.sectors
                .iter()
                .flatten()
                .find_map(|sector| sector.floor.as_ref()?.material)
        })
        .expect("starter room has a floor material");
    let generated = |color| {
        ResourceData::Material(crate::MaterialResource {
            texture_mode: crate::MaterialTextureMode::Generated,
            generated: crate::GeneratedMaterialTexture {
                size: 32,
                base_color: color,
                noise_enabled: false,
                ..crate::GeneratedMaterialTexture::default()
            },
            ..crate::MaterialResource::opaque(None)
        })
    };
    let stone = project.add_resource("Cook Stone", generated([72, 80, 96]));
    let sand = project.add_resource("Cook Sand", generated([184, 144, 88]));
    let ResourceData::Material(material) = &mut project
        .resource_mut(room_material)
        .expect("room material remains addressable")
        .data
    else {
        panic!("room surface points to a Material");
    };
    material.texture_mode = crate::MaterialTextureMode::Transition;
    material.transition = crate::TransitionMaterialTexture {
        source_a: Some(stone),
        source_b: Some(sand),
        size: 64,
        coverage: 128,
        ..crate::TransitionMaterialTexture::default()
    };

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("transition room cooks");
    let material = package.materials[package.rooms[0].material_first as usize];
    let texture = &package.assets[material.texture_asset_index];
    assert_eq!(texture.kind, PlaytestAssetKind::Texture);
    let decoded = psx_asset::Texture::from_bytes(&texture.bytes).expect("cooked PSXT parses");
    assert_eq!(decoded.depth(), psxed_format::texture::Depth::Bit4);
    assert_eq!((decoded.width(), decoded.height()), (64, 64));
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .join("model_000_aletha_uthana")
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
fn repeated_brush_cook_replaces_generated_output_for_the_selected_mode() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../projects/brush-first-playable");
    let mut project = ProjectDocument::load_from_path(root.join("project.ron"))
        .expect("brush first-playable project");
    let dir = unique_temp_dir("psxed-playtest-repeat-brush");

    project.bsp_cook_mode = crate::brush_world::BrushWorldCookMode::Draft;
    let draft_report = cook_to_dir(&project, &root, &dir).expect("Draft cook IO");
    assert!(draft_report.is_ok(), "{draft_report:?}");
    let draft =
        std::fs::read(dir.join(crate::brush_playtest::BRUSH_WORLD_FILENAME)).expect("Draft PXBSP");

    project.bsp_cook_mode = crate::brush_world::BrushWorldCookMode::Release;
    let release_report = cook_to_dir(&project, &root, &dir).expect("Release cook IO");
    assert!(release_report.is_ok(), "{release_report:?}");
    let release = std::fs::read(dir.join(crate::brush_playtest::BRUSH_WORLD_FILENAME))
        .expect("Release PXBSP");
    let manifest =
        std::fs::read_to_string(dir.join(COOKED_MANIFEST_FILENAME)).expect("refreshed manifest");

    assert_ne!(draft, release, "re-Play must not reuse the Draft PXBSP");
    assert!(manifest.contains("pub const BSP_COOK_IS_RELEASE: bool = true;"));
    let _ = std::fs::remove_dir_all(dir);
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
    // Find the starter room texture path and build a second material
    // pointing at the same image.
    let room_texture_path = project
        .resources
        .iter()
        .find_map(|r| match &r.data {
            ResourceData::Material(material) => material
                .psxt_path
                .as_ref()
                .filter(|path| path.ends_with("bigdoor_1a.psxt"))
                .cloned(),
            _ => None,
        })
        .expect("starter has room texture");

    // Reassign every wall material in the room to a new
    // material that *also* points at the same room texture. After
    // cook the world has 2 cooker material slots (floor + the
    // new wall material) but both resolve to the same texture
    // image, so playtest should emit 1 texture asset.
    let new_material_id = project.add_resource(
        "BigdoorOnWalls",
        ResourceData::Material(crate::MaterialResource::opaque(Some(room_texture_path))),
    );
    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .expect("starter has a room");
    if let Some(node) = scene.node_mut(room_id) {
        if let NodeKind::Section { grid } = &mut node.kind {
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
    // Point a material's texture at a bogus path; cook should
    // refuse and the error should mention the file.
    let mut project = project_with_one_room();
    let target = project
        .resources
        .iter_mut()
        .find_map(|r| match &mut r.data {
            ResourceData::Material(material) => material.psxt_path.as_mut(),
            _ => None,
        })
        .expect("starter has at least one textured material");
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
            // No skeleton -> no resolvable skeleton clips.
            model.skeleton = None;
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
