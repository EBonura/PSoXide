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
fn playtest_folds_pose_corrections_into_packaged_animation_bytes() {
    let mut project = ProjectDocument::starter();
    let project_root = starter_project_root();
    let player_model = player_model_resource_id(&project);
    let resolved = project.resolved_model_animation_clips(player_model);
    let clip_resource = resolved
        .first()
        .and_then(|clip| clip.animation_resource)
        .expect("default resolved clip has a resource");
    let base_path = project
        .resource(clip_resource)
        .and_then(|resource| match &resource.data {
            ResourceData::AnimationClip(clip) => Some(clip.psxanim_path.clone()),
            _ => None,
        })
        .expect("animation clip resource");
    let base_bytes =
        std::fs::read(resolve_path(&base_path, &project_root)).expect("base animation exists");
    let base = psx_asset::Animation::from_bytes(&base_bytes).expect("base animation parses");
    let ResourceData::AnimationClip(clip) = &mut project.resource_mut(clip_resource).unwrap().data
    else {
        panic!("animation clip expected");
    };
    clip.pose_corrections
        .push(crate::AnimationPoseCorrectionKey {
            frame: 0,
            joint: 0,
            rotation_q12: [0, 256, 0],
            translation: [8, 0, 0],
        });

    let (package, report) = build_package(&project, &project_root);
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("project cooks");
    let model = package
        .models
        .iter()
        .find(|model| model.source_resource == player_model)
        .expect("player model packaged");
    let packaged_clip = &package.model_clips[model.clip_first as usize];
    let packaged_bytes = &package.assets[packaged_clip.animation_asset_index].bytes;
    let packaged =
        psx_asset::Animation::from_bytes(packaged_bytes).expect("packaged animation parses");

    assert_eq!(packaged.frame_count(), base.frame_count());
    assert_ne!(
        packaged.pose(0, 0).expect("corrected pose"),
        base.pose(0, 0).expect("base pose")
    );
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
fn model_atlas_accepts_4bpp() {
    // A normal 4bpp PSXT is also a valid model atlas: the runtime selects a
    // 4bpp tpage and allocates the corresponding 16-entry CLUT.
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
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    assert!(package.is_some());
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    let vertices = crate::box_prop_vertices_for_size(512);
    let mut uvs = [crate::GridUvTransform::IDENTITY; crate::BOX_PROP_FACE_COUNT];
    uvs[0].offset = [5, 7];
    uvs[0].span = [11, 13];
    let prop_id = scene.add_node(
        room_id,
        "Cooked Box Prop",
        NodeKind::BoxProp {
            materials: [Some(material_id); crate::BOX_PROP_FACE_COUNT],
            uvs,
            vertices,
            collision_enabled: true,
            break_flags: psx_level::box_prop_flags::BREAK_ON_WALK
                | psx_level::box_prop_flags::BREAK_ON_ATTACK,
            erosion: crate::BoxPropErosion::default(),
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
    assert_eq!(
        prop.uvs[0],
        [(5, 7), (16, 7), (16, 20), (5, 20)],
        "authored per-face UVs survive the cook"
    );
    assert_eq!(prop.flags & psx_level::box_prop_flags::COLLISION_ENABLED, 1);
    assert_ne!(prop.flags & psx_level::box_prop_flags::BREAK_ON_WALK, 0);
    assert_ne!(prop.flags & psx_level::box_prop_flags::BREAK_ON_ATTACK, 0);
    assert_eq!(
        prop.surface_count, 0,
        "plain boxes keep the compact legacy path"
    );
}

#[test]
fn eroded_box_prop_cooks_shared_runtime_surfaces() {
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
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("starter has a room");
    let mut erosion = crate::BoxPropErosion::default();
    erosion.apply_broken_top_template();
    scene.add_node(
        room_id,
        "Broken Wall",
        NodeKind::BoxProp {
            materials: [Some(material_id); crate::BOX_PROP_FACE_COUNT],
            uvs: [crate::GridUvTransform::IDENTITY; crate::BOX_PROP_FACE_COUNT],
            vertices: crate::box_prop_vertices_for_size(512),
            collision_enabled: true,
            break_flags: 0,
            erosion,
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("eroded box cooks");
    let prop = package.box_props.last().expect("box prop record");
    assert!(prop.surface_count > crate::BOX_PROP_FACE_COUNT as u16);
    let first = usize::from(prop.surface_first);
    let end = first + usize::from(prop.surface_count);
    let surfaces = &package.box_prop_surfaces[first..end];
    assert_eq!(usize::from(prop.surface_count), surfaces.len());
    assert!(surfaces
        .iter()
        .all(|surface| usize::from(surface.source_face) < crate::BOX_PROP_FACE_COUNT));
    assert!(surfaces.iter().any(|surface| surface.source_face == 4));
}

#[test]
fn cylinder_prop_cooks_shared_triangle_and_quad_surfaces() {
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
        .find(|node| matches!(node.kind, NodeKind::Section { .. }))
        .map(|node| node.id)
        .expect("starter has a room");
    let mut geometry = crate::CylinderPropGeometry {
        broken_ends: crate::CylinderBrokenEnds::Top,
        ..Default::default()
    };
    geometry.top_bulge.enabled = true;
    scene.add_node(
        room_id,
        "Broken Column",
        NodeKind::CylinderProp {
            materials: [Some(material_id); crate::CYLINDER_PROP_MATERIAL_COUNT],
            uvs: [crate::GridUvTransform::IDENTITY; crate::CYLINDER_PROP_MATERIAL_COUNT],
            geometry,
            collision_enabled: true,
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cylinder cooks");
    let prop = package.cylinder_props.last().expect("cylinder record");
    assert_eq!(
        prop.flags & psx_level::cylinder_prop_flags::COLLISION_ENABLED,
        psx_level::cylinder_prop_flags::COLLISION_ENABLED
    );
    let first = usize::from(prop.surface_first);
    let end = first + usize::from(prop.surface_count);
    let surfaces = &package.cylinder_prop_surfaces[first..end];
    assert!(surfaces.iter().any(|surface| surface.vertex_count == 3));
    assert!(surfaces.iter().any(|surface| surface.vertex_count == 4));
    assert!(surfaces
        .iter()
        .any(|surface| { surface.material_slot == crate::CYLINDER_PROP_MATERIAL_FRACTURE }));
    assert!(prop.bounds_max[1] > prop.bounds_min[1]);
    let source = render_manifest_source(&package);
    assert!(source.contains("pub static CYLINDER_PROPS: &[LevelCylinderPropRecord]"));
    assert!(source.contains("pub static CYLINDER_PROP_SURFACES: &[LevelCylinderPropSurfaceRecord]"));
    assert!(source.contains("vertex_count: 3"));
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    // Raise the floor under (0,0) so a floor-snap would be observable.
    let sector_size = {
        let room = scene.node_mut(room_id).expect("room node");
        let NodeKind::Section { grid } = &mut room.kind else {
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
            uvs: [crate::GridUvTransform::IDENTITY; crate::BOX_PROP_FACE_COUNT],
            vertices,
            collision_enabled: false,
            break_flags: 0,
            erosion: crate::BoxPropErosion::default(),
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
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
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    if let Some(room) = scene.node_mut(room_id) {
        let NodeKind::Section { grid } = &mut room.kind else {
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
    let package = package.expect("cooks");
    for model in &package.models {
        assert_eq!(
            package.assets[model.mesh_asset_index].streamed_class,
            StreamedClass::PersistentGameplay
        );
        assert_eq!(
            package.assets[model.texture_asset_index.expect("atlas")].streamed_class,
            StreamedClass::PersistentGameplay
        );
    }
    for clip in &package.model_clips {
        assert_eq!(
            package.assets[clip.animation_asset_index].streamed_class,
            StreamedClass::PersistentGameplay
        );
    }
    let src = render_manifest_source(&package);
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
    assert!(src.contains("flags: asset_flags::STREAMED_GAMEPLAY_PERSISTENT"));
    assert!(src.contains("pub const PERSISTENT_ASSET_PAGE_COUNT: usize ="));
    assert!(src.contains("#[cfg(feature = \"cd-stream-bench\")]\npub static ASSET_"));
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
            .find(|n| matches!(n.kind, crate::NodeKind::Section { .. }))
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
    let crate::NodeKind::Section { grid } = &room.kind else {
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
        if let crate::NodeKind::Section { grid } = &mut node.kind {
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
        if let crate::NodeKind::Section { grid } = &mut node.kind {
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
        if let crate::NodeKind::Section { grid } = &mut node.kind {
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

#[test]
fn model_renderer_material_override_cooks_onto_instance() {
    let mut project = ProjectDocument::starter();
    let model_id = player_model_resource_id(&project);
    let material_id = project
        .resources
        .iter()
        .find(|resource| {
            matches!(&resource.data, ResourceData::Material(material) if material.psxt_path.is_some())
        })
        .expect("starter has a textured material")
        .id;
    // Author the covering material's blend/tint/sidedness.
    let ResourceData::Material(material) = &mut project
        .resources
        .iter_mut()
        .find(|resource| resource.id == material_id)
        .unwrap()
        .data
    else {
        unreachable!();
    };
    material.blend_mode = crate::PsxBlendMode::Average;
    material.tint = [96, 128, 160];
    material.face_sidedness = crate::MaterialFaceSidedness::Both;
    let mut secondary_layer = crate::ModelSecondaryLayer::default();
    secondary_layer.motion.enabled = true;
    secondary_layer.motion.speed_u_q8 = 3 * 256;
    secondary_layer.motion.speed_v_q8 = -2 * 256;
    secondary_layer.motion.phase_u = 7;
    secondary_layer.motion.phase_v = 11;
    material.secondary_layer = Some(secondary_layer);

    let scene = project.active_scene_mut();
    let room_id = scene
        .nodes()
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Section { .. }))
        .map(|n| n.id)
        .unwrap();
    let entity = scene.add_node(room_id, "Covered Prop", NodeKind::Entity);
    scene.add_node(
        entity,
        "Model Renderer",
        NodeKind::ModelRenderer {
            model: Some(model_id),
            material: Some(material_id),
            visual_offset: [0; 3],
            visual_scale_q8: crate::MODEL_SCALE_ONE_Q8,
        },
    );

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let inst = package
        .model_instances
        .iter()
        .find(|inst| inst.material_override.is_some())
        .expect("covered instance cooks");
    let material_override = inst.material_override.unwrap();
    assert_eq!(material_override.blend_mode, crate::PsxBlendMode::Average);
    assert_eq!(material_override.tint_rgb, [96, 128, 160]);
    assert_eq!(
        material_override.face_sidedness,
        crate::MaterialFaceSidedness::Both
    );
    // The material's psxt became a cooked texture asset requirement.
    let texture_asset_index = material_override
        .texture_asset_index
        .expect("covering material carries its texture asset");
    let asset = &package.assets[texture_asset_index];
    assert_eq!(asset.kind, PlaytestAssetKind::Texture);
    let secondary = material_override
        .secondary_layer
        .expect("generated secondary layer cooks");
    assert_eq!(secondary.blend_mode, crate::PsxBlendMode::AddQuarter);
    assert_eq!(secondary.tint_rgb, [0x70, 0x78, 0x80]);
    assert!(secondary.motion.enabled);
    assert_eq!(secondary.motion.speed_u_q8, 3 * 256);
    assert_eq!(secondary.motion.speed_v_q8, -2 * 256);
    assert_eq!(secondary.motion.phase_u, 7);
    assert_eq!(secondary.motion.phase_v, 11);
    let secondary_asset_index = secondary
        .texture_asset_index
        .expect("generated layer has a texture asset");
    let secondary_asset = &package.assets[secondary_asset_index];
    assert_eq!(secondary_asset.kind, PlaytestAssetKind::Texture);
    let texture = psx_asset::Texture::from_bytes(&secondary_asset.bytes)
        .expect("generated secondary PSXT parses");
    assert_eq!((texture.width(), texture.height()), (128, 128));
    assert_eq!(texture.clut_entries(), 16);
    assert!(!texture.index_zero_transparent());

    // Manifest side: the instance literal carries the override and the
    // owning room's residency lists the covering texture.
    let src = render_manifest_source(&package);
    assert!(
        src.contains(&format!(
            "material_override: Some(LevelModelMaterialOverride {{ texture_asset: Some(AssetId({})), blend_mode: 1, tint_rgb: [96, 128, 160], motion: LevelMaterialUvMotion {{ enabled: false, speed_u_q8: 2048, speed_v_q8: 0, phase_u: 0, phase_v: 0 }}, secondary_layer: Some(LevelModelSecondaryLayer {{ texture_asset: Some(AssetId({})), blend_mode: 4, tint_rgb: [112, 120, 128], motion: LevelMaterialUvMotion {{ enabled: true, speed_u_q8: 768, speed_v_q8: -512, phase_u: 7, phase_v: 11 }}, flags: 0 }}), flags: 2 }})",
            texture_asset_index,
            secondary_asset_index,
        )),
        "manifest instance literal missing override: {src}"
    );

    let ResourceData::Material(material) = &mut project.resource_mut(material_id).unwrap().data
    else {
        unreachable!();
    };
    material.set_secondary_layer_enabled(false);
    let retained_recipe = material
        .secondary_layer
        .as_ref()
        .expect("disabled layer remains authored");
    assert_eq!(retained_recipe.motion.speed_u_q8, 3 * 256);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("disabled overlay still cooks");
    let material_override = package
        .model_instances
        .iter()
        .find_map(|instance| instance.material_override)
        .expect("base material override remains");
    assert!(
        material_override.secondary_layer.is_none(),
        "disabled authored layers must not reach the runtime"
    );
}

#[test]
fn player_character_profile_material_cooks_without_renderer_override() {
    let mut project = project_with_one_room();
    let material_id = project
        .resources
        .iter()
        .find(|resource| {
            matches!(&resource.data, ResourceData::Material(material) if material.psxt_path.is_some())
        })
        .expect("starter has a textured material")
        .id;
    let character_id = project
        .active_scene()
        .nodes()
        .iter()
        .find_map(|node| match node.kind {
            NodeKind::CharacterController {
                character: Some(id),
                player: true,
                ..
            } => Some(id),
            _ => None,
        })
        .expect("starter player has a character profile");
    let ResourceData::Character(character) = &mut project.resource_mut(character_id).unwrap().data
    else {
        panic!("player controller references a Character");
    };
    character.material = Some(material_id);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let material_override = package.characters[0]
        .material_override
        .expect("profile material cooks onto the player");
    assert!(material_override.texture_asset_index.is_some());
}

#[test]
fn player_model_renderer_material_override_cooks_onto_character() {
    let mut project = project_with_one_room();
    let material_id = project
        .resources
        .iter()
        .find(|resource| {
            matches!(&resource.data, ResourceData::Material(material) if material.psxt_path.is_some())
        })
        .expect("starter has a textured material")
        .id;
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
    let NodeKind::ModelRenderer { material, .. } = &mut scene.node_mut(renderer_id).unwrap().kind
    else {
        panic!("expected model renderer");
    };
    *material = Some(material_id);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let character = &package.characters[0];
    let material_override = character
        .material_override
        .expect("player character carries the covering material");
    let asset = &package.assets[material_override
        .texture_asset_index
        .expect("covering material carries its texture asset")];
    assert_eq!(asset.kind, PlaytestAssetKind::Texture);
    // No extra static instance appears for the player's renderer.
    assert!(package.model_instances.is_empty());
}

#[test]
fn player_model_renderer_material_without_texture_keeps_model_atlas() {
    let mut project = project_with_one_room();
    let material_id = project.add_resource(
        "Crystal Atlas",
        ResourceData::Material(crate::MaterialResource::translucent(
            None,
            crate::PsxBlendMode::Average,
        )),
    );
    let spawn_id = player_spawn_node_id(&project);
    let renderer_id = project
        .active_scene()
        .node(spawn_id)
        .and_then(|node| {
            node.children.iter().find_map(|child| {
                project.active_scene().node(*child).and_then(|node| {
                    matches!(node.kind, NodeKind::ModelRenderer { .. }).then_some(node.id)
                })
            })
        })
        .expect("starter player has a model renderer");
    let NodeKind::ModelRenderer { material, .. } = &mut project
        .active_scene_mut()
        .node_mut(renderer_id)
        .unwrap()
        .kind
    else {
        panic!("expected model renderer");
    };
    *material = Some(material_id);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let material_override = package.characters[0]
        .material_override
        .expect("player character carries its optical override");
    assert_eq!(material_override.texture_asset_index, None);
    assert_eq!(material_override.blend_mode, crate::PsxBlendMode::Average);
    assert_eq!(material_override.tint_rgb, [128, 128, 128]);
    let manifest = render_manifest_source(&package);
    assert!(manifest.contains(
        "material_override: Some(LevelModelMaterialOverride { texture_asset: None, blend_mode: 1, tint_rgb: [128, 128, 128], motion: LevelMaterialUvMotion { enabled: false, speed_u_q8: 2048, speed_v_q8: 0, phase_u: 0, phase_v: 0 }, secondary_layer: None, flags: 2 })"
    ));
}

#[test]
fn exclusive_player_average_noise_material_collapses_to_one_atlas_pass() {
    let mut project = project_with_one_room();
    let spawn_id = player_spawn_node_id(&project);
    let renderer_id = project
        .active_scene()
        .node(spawn_id)
        .and_then(|node| {
            node.children.iter().find_map(|child| {
                project.active_scene().node(*child).and_then(|node| {
                    matches!(node.kind, NodeKind::ModelRenderer { .. }).then_some(node.id)
                })
            })
        })
        .expect("starter player has a model renderer");
    let renderer_model = match project.active_scene().node(renderer_id).unwrap().kind {
        NodeKind::ModelRenderer {
            model: Some(model), ..
        } => model,
        _ => panic!("starter renderer has a model"),
    };
    // The starter model fixture uses an 8bpp atlas; point it at the starter's
    // existing 4bpp material texture so this exercises the compatible path
    // used by the Cortex player without introducing another binary fixture.
    let four_bpp_atlas = project
        .resources
        .iter()
        .find_map(|resource| match &resource.data {
            ResourceData::Material(material) => material.psxt_path.clone(),
            _ => None,
        })
        .expect("starter has a 4bpp material texture");
    let model = match &mut project.resource_mut(renderer_model).unwrap().data {
        ResourceData::Model(model) => model,
        _ => panic!("renderer references a model resource"),
    };
    model.texture_path = Some(four_bpp_atlas);
    let mut material = crate::MaterialResource::translucent(None, crate::PsxBlendMode::Average);
    material.tint = [160, 176, 192];
    material.secondary_layer = Some(crate::ModelSecondaryLayer::default());
    let material_id = project.add_resource("Player Crystal", ResourceData::Material(material));
    let NodeKind::ModelRenderer { material, .. } = &mut project
        .active_scene_mut()
        .node_mut(renderer_id)
        .unwrap()
        .kind
    else {
        panic!("expected model renderer");
    };
    *material = Some(material_id);

    let (package, report) = build_package(&project, &starter_project_root());
    assert!(report.is_ok(), "errors: {:?}", report.errors);
    let package = package.expect("cooks");
    let character = &package.characters[0];
    let material = character.material_override.expect("player material cooks");
    assert_eq!(material.blend_mode, crate::PsxBlendMode::Average);
    assert_eq!(material.tint_rgb, [128; 3]);
    assert_eq!(material.secondary_layer, None);
    assert_eq!(material.texture_asset_index, None);

    let model = &package.models[usize::from(character.model)];
    let atlas = &package.assets[model.texture_asset_index.expect("model atlas")];
    assert!(atlas.source_label.contains("fused player material"));
    let texture = psx_asset::Texture::from_bytes(&atlas.bytes).expect("fused atlas parses");
    assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit4);
    assert_eq!(texture.clut_entries(), 16);
    assert!(texture.index_zero_transparent());
}
