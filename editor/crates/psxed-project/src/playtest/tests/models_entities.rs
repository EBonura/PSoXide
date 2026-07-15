use super::*;

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
        if action.required_for_player() || clip != CHARACTER_CLIP_NONE {
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
    let rewired = project
        .resources
        .iter_mut()
        .find_map(|resource| match &mut resource.data {
            ResourceData::Material(material) => material.psxt_path.as_mut(),
            _ => None,
        })
        .expect("starter has a used room texture");
    *rewired = "assets/models/obsidian_wraith/obsidian_wraith_128x128_8bpp.psxt".to_string();
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
        node.transform.rotation_degrees = [30.0, 90.0, 60.0];
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
    assert_eq!(package.model_instances[0].pitch, 341);
    assert_eq!(package.model_instances[0].roll, 682);
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
    assert!(src.contains("pub const CACHED_ROOM_DEPTH_MODE: u8 = 2;"));
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
