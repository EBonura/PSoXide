use super::*;

fn assert_quat_close(actual: [f32; 4], expected: [f32; 4]) {
    let direct = actual
        .iter()
        .zip(expected)
        .map(|(a, e)| (a - e).abs())
        .fold(0.0f32, f32::max);
    let flipped = actual
        .iter()
        .zip(expected)
        .map(|(a, e)| (a + e).abs())
        .fold(0.0f32, f32::max);
    assert!(
        direct.min(flipped) < 0.0001,
        "expected {expected:?}, got {actual:?}"
    );
}

fn quat_z_degrees(degrees: f32) -> [f32; 4] {
    let radians = degrees.to_radians() * 0.5;
    [0.0, 0.0, radians.sin(), radians.cos()]
}

fn quat_x_degrees(degrees: f32) -> [f32; 4] {
    let radians = degrees.to_radians() * 0.5;
    [radians.sin(), 0.0, 0.0, radians.cos()]
}

fn quat_y_degrees(degrees: f32) -> [f32; 4] {
    let radians = degrees.to_radians() * 0.5;
    [0.0, radians.sin(), 0.0, radians.cos()]
}

#[test]
fn humanoid_node_match_key_aliases_synty_and_meshy_bones() {
    assert_eq!(node_match_key("LeftUpLeg"), node_match_key("UpperLeg_L"));
    assert_eq!(node_match_key("LeftUpLeg"), node_match_key("thigh_l"));
    assert_eq!(node_match_key("LeftLeg"), node_match_key("LowerLeg_L"));
    assert_eq!(node_match_key("LeftLeg"), node_match_key("calf_l"));
    assert_eq!(node_match_key("LeftFoot"), node_match_key("Ankle_L"));
    assert_eq!(node_match_key("LeftFoot"), node_match_key("foot_l"));
    assert_eq!(node_match_key("LeftToeBase"), node_match_key("ball_l"));
    assert_eq!(node_match_key("LeftShoulder"), node_match_key("clavicle_l"));
    assert_eq!(node_match_key("LeftArm"), node_match_key("Shoulder_L"));
    assert_eq!(node_match_key("LeftArm"), node_match_key("upperarm_l"));
    assert_eq!(node_match_key("LeftForeArm"), node_match_key("Elbow_L"));
    assert_eq!(node_match_key("LeftForeArm"), node_match_key("lowerarm_l"));
    assert_eq!(node_match_key("LeftHand"), node_match_key("Hand_L"));
    assert_eq!(node_match_key("Neck"), node_match_key("neck_01"));
    assert_eq!(node_match_key("Spine"), node_match_key("Spine_01"));
    assert_eq!(node_match_key("Spine01"), node_match_key("Spine_02"));
    assert_eq!(node_match_key("Spine02"), node_match_key("Spine_03"));
    assert_ne!(node_match_key("Armature"), node_match_key("Root"));
}

#[test]
fn humanoid_spine_mapping_follows_hierarchy_when_source_numbers_are_reversed() {
    let target_names = ["Hips", "Spine", "Spine1", "Spine2", "Neck"];
    let source_names = ["Hips", "Spine02", "Spine01", "Spine", "neck"];
    let parents = [None, Some(0), Some(1), Some(2), Some(3)];
    let mut mapping = vec![Some(0), Some(3), Some(2), Some(1), Some(4)];

    align_humanoid_spine_mapping(
        &target_names,
        &parents,
        &source_names,
        &parents,
        &mut mapping,
    );

    assert_eq!(mapping, vec![Some(0), Some(1), Some(2), Some(3), Some(4)]);
}

#[test]
fn humanoid_spine_mapping_preserves_standard_chain_order() {
    let names = ["Hips", "Spine", "Spine1", "Spine2", "Neck"];
    let parents = [None, Some(0), Some(1), Some(2), Some(3)];
    let mut mapping = vec![Some(0), Some(1), Some(2), Some(3), Some(4)];

    align_humanoid_spine_mapping(&names, &parents, &names, &parents, &mut mapping);

    assert_eq!(mapping, vec![Some(0), Some(1), Some(2), Some(3), Some(4)]);
}

#[test]
fn fbx_companion_texture_search_finds_meshy_obj_export_sibling() {
    let root = std::env::temp_dir().join(format!(
        "psxed-gltf-fbx-texture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    let pack = root.join("Sword and Shield Pack");
    let sibling = root.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture_obj");
    std::fs::create_dir_all(&pack).unwrap();
    std::fs::create_dir_all(&sibling).unwrap();
    let source = pack.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture.fbx");
    let texture = sibling.join("Meshy_AI_Crimson_Cross_Knight_0516082504_texture.png");
    std::fs::write(&source, b"fbx").unwrap();
    std::fs::write(&texture, b"png").unwrap();

    assert_eq!(find_companion_fbx_texture(&source), Some(texture));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn retarget_mapped_frame_trs_keeps_target_offsets_and_child_inheritance() {
    let target_parents = vec![None, Some(0)];
    let source_parents = vec![None, Some(0)];
    let target_base = vec![
        Trs {
            translation: [1.0, 2.0, 3.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [4.0, 5.0, 6.0],
            rotation: quat_z_degrees(10.0),
            scale: [1.0, 1.0, 1.0],
        },
    ];
    let source_base = vec![
        Trs {
            translation: [100.0, 200.0, 300.0],
            rotation: identity_quat(),
            scale: [2.0, 2.0, 2.0],
        },
        Trs {
            translation: [400.0, 500.0, 600.0],
            rotation: identity_quat(),
            scale: [3.0, 3.0, 3.0],
        },
    ];
    let source_pose = vec![
        Trs {
            translation: [120.0, 240.0, 360.0],
            rotation: quat_z_degrees(45.0),
            scale: [2.0, 2.0, 2.0],
        },
        Trs {
            translation: [420.0, 540.0, 660.0],
            rotation: quat_z_degrees(20.0),
            scale: [3.0, 3.0, 3.0],
        },
    ];
    let mapping = vec![Some(0), None];

    let retargeted = retarget_mapped_frame_trs(
        &target_parents,
        &target_base,
        &source_parents,
        &source_base,
        &source_pose,
        &mapping,
    );

    assert_eq!(retargeted[0].translation, target_base[0].translation);
    assert_eq!(retargeted[1].translation, target_base[1].translation);
    assert_eq!(retargeted[0].scale, target_base[0].scale);
    assert_eq!(retargeted[1].scale, target_base[1].scale);
    assert_quat_close(retargeted[0].rotation, quat_z_degrees(45.0));
    assert_quat_close(retargeted[1].rotation, target_base[1].rotation);
}

#[test]
fn retarget_mapped_frame_trs_rebases_world_delta_across_different_bone_axes() {
    let target_parents = vec![None];
    let source_parents = vec![None];
    let source_base_rotation = quat_x_degrees(90.0);
    let target_base_rotation = quat_y_degrees(90.0);
    let source_local_delta = quat_z_degrees(35.0);
    let target_base = vec![Trs {
        translation: [0.0, 0.0, 0.0],
        rotation: target_base_rotation,
        scale: [1.0, 1.0, 1.0],
    }];
    let source_base = vec![Trs {
        translation: [0.0, 0.0, 0.0],
        rotation: source_base_rotation,
        scale: [1.0, 1.0, 1.0],
    }];
    let source_pose = vec![Trs {
        translation: [0.0, 0.0, 0.0],
        rotation: quat_mul(source_base_rotation, source_local_delta),
        scale: [1.0, 1.0, 1.0],
    }];
    let mapping = vec![Some(0)];

    let retargeted = retarget_mapped_frame_trs(
        &target_parents,
        &target_base,
        &source_parents,
        &source_base,
        &source_pose,
        &mapping,
    );

    let source_world_delta = quat_mul(source_pose[0].rotation, quat_inverse(source_base_rotation));
    assert_quat_close(
        retargeted[0].rotation,
        quat_mul(source_world_delta, target_base_rotation),
    );
    assert!(
        retargeted[0].rotation != quat_mul(target_base_rotation, source_local_delta),
        "different source and target bone axes must not reuse the source-local delta"
    );
}

#[test]
fn retarget_mapped_bone_directions_preserves_animated_limb_direction() {
    let parents = vec![None, Some(0)];
    let source_base = vec![
        Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [1.0, 0.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
    ];
    let source_pose = vec![
        Trs {
            rotation: quat_z_degrees(90.0),
            ..source_base[0]
        },
        source_base[1],
    ];
    let target_base = vec![
        source_base[0],
        Trs {
            translation: [0.0, 1.0, 0.0],
            ..source_base[1]
        },
    ];
    let mut retargeted = retarget_mapped_frame_trs(
        &parents,
        &target_base,
        &parents,
        &source_base,
        &source_pose,
        &[Some(0), Some(1)],
    );
    retarget_mapped_bone_directions(
        &parents,
        &source_pose,
        &parents,
        &mut retargeted,
        &[(0, 1, 0, 1)],
    );

    let source_globals = compute_global_matrices(
        &parents,
        &source_pose.iter().map(Trs::matrix).collect::<Vec<_>>(),
    );
    let target_globals = compute_global_matrices(
        &parents,
        &retargeted.iter().map(Trs::matrix).collect::<Vec<_>>(),
    );
    let source_direction = normalize3(sub3(
        matrix_translation(source_globals[1]),
        matrix_translation(source_globals[0]),
    ));
    let target_direction = normalize3(sub3(
        matrix_translation(target_globals[1]),
        matrix_translation(target_globals[0]),
    ));
    assert!(vec3_close(source_direction, target_direction, 0.0001));
}

#[test]
fn retarget_mapped_frame_trs_reconstructs_child_from_global_bind_delta() {
    let parents = vec![None, Some(0)];
    let target_base = vec![
        Trs {
            translation: [1.0, 2.0, 3.0],
            rotation: quat_y_degrees(70.0),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [4.0, 5.0, 6.0],
            rotation: quat_x_degrees(25.0),
            scale: [1.0, 1.0, 1.0],
        },
    ];
    let source_base = vec![
        Trs {
            translation: [100.0, 200.0, 300.0],
            rotation: quat_x_degrees(90.0),
            scale: [2.0, 2.0, 2.0],
        },
        Trs {
            translation: [400.0, 500.0, 600.0],
            rotation: quat_z_degrees(15.0),
            scale: [3.0, 3.0, 3.0],
        },
    ];
    let source_pose = vec![
        Trs {
            rotation: quat_mul(source_base[0].rotation, quat_z_degrees(30.0)),
            ..source_base[0]
        },
        Trs {
            rotation: quat_mul(source_base[1].rotation, quat_y_degrees(35.0)),
            ..source_base[1]
        },
    ];

    let retargeted = retarget_mapped_frame_trs(
        &parents,
        &target_base,
        &parents,
        &source_base,
        &source_pose,
        &[Some(0), Some(1)],
    );

    let source_bind_child_global = quat_mul(source_base[0].rotation, source_base[1].rotation);
    let source_pose_child_global = quat_mul(source_pose[0].rotation, source_pose[1].rotation);
    let target_bind_child_global = quat_mul(target_base[0].rotation, target_base[1].rotation);
    let expected_child_global = quat_mul(
        quat_mul(
            source_pose_child_global,
            quat_inverse(source_bind_child_global),
        ),
        target_bind_child_global,
    );
    let actual_child_global = quat_mul(retargeted[0].rotation, retargeted[1].rotation);
    assert_quat_close(actual_child_global, expected_child_global);
    assert_eq!(retargeted[0].translation, target_base[0].translation);
    assert_eq!(retargeted[1].translation, target_base[1].translation);
    assert_eq!(retargeted[0].scale, target_base[0].scale);
    assert_eq!(retargeted[1].scale, target_base[1].scale);
}

#[test]
fn imports_minimal_glb_triangle() {
    let glb = minimal_triangle_glb();
    let psxm = convert_slice(&glb, &Config::default()).unwrap();
    let mesh = psx_asset::Mesh::from_bytes(&psxm).unwrap();
    assert_eq!(mesh.vert_count(), 3);
    assert_eq!(mesh.face_count(), 1);
    assert!(mesh.has_face_colors());
    assert!(mesh.has_normals());
    assert_eq!(mesh.face_color(0), Some((64, 128, 255)));
}

#[test]
fn native_model_imports_static_glb_triangle_with_bind_pose_and_atlas() {
    let glb = minimal_triangle_glb();
    let package = convert_rigid_model_slice(&glb, &RigidModelConfig::default()).unwrap();

    let model = psx_asset::Model::from_bytes(&package.model).unwrap();
    assert_eq!(model.joint_count(), 1);
    assert_eq!(model.part_count(), 1);
    assert_eq!(model.vertex_count(), 3);
    assert_eq!(model.face_count(), 1);

    assert_eq!(package.clips.len(), 1);
    assert_eq!(package.clips[0].sanitized_name, "bind_pose");
    assert_eq!(package.clips[0].frames, 1);
    let animation = psx_asset::Animation::from_bytes(&package.clips[0].bytes).unwrap();
    assert_eq!(animation.joint_count(), 1);
    assert_eq!(animation.frame_count(), 1);

    let texture = psx_asset::Texture::from_bytes(package.texture.as_deref().unwrap()).unwrap();
    assert_eq!(texture.depth(), psxed_format::texture::Depth::Bit8);
    assert_eq!(texture.clut_entries(), 256);
    assert_eq!(
        package.report.clip_frames,
        vec![("bind_pose".to_string(), 1)]
    );
    assert_eq!(package.report.texture_bytes, package.texture.unwrap().len());
}

#[test]
fn triangle_strip_gets_triangulated() {
    let faces = triangulate_indices(&[0, 1, 2, 3], Mode::TriangleStrip).unwrap();
    assert_eq!(faces, vec![[0, 1, 2], [2, 1, 3]]);
}

#[test]
fn model_precision_scale_targets_world_height() {
    let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [2.0, 4.0, 1.0], 30_000.0).unwrap();
    let local_height = bounds.encoded_axis_size(0.0, 4.0);
    assert_eq!(local_height, 60_000);
    assert_eq!(choose_local_to_world_q12(local_height, 1024), 70);
}

#[test]
fn native_model_normals_use_source_winding_after_engine_face_flip() {
    let mut source = SkinnedSourceMesh {
        vertices: vec![
            test_source_vertex([0.0, 0.0, 0.0]),
            test_source_vertex([1.0, 0.0, 0.0]),
            test_source_vertex([0.0, 1.0, 0.0]),
        ],
        faces: vec![SourceFace {
            indices: [0, 2, 1],
            joint: 0,
        }],
    };

    rebuild_source_normals(&mut source, &[[0, 1, 2]]);

    assert_eq!(source.faces[0].indices, [0, 2, 1]);
    for vertex in &source.vertices {
        assert!((vertex.normal[0] - 0.0).abs() < 0.0001);
        assert!((vertex.normal[1] - 0.0).abs() < 0.0001);
        assert!((vertex.normal[2] - 1.0).abs() < 0.0001);
    }
}

#[test]
fn native_model_compacts_duplicate_part_vertices() {
    let source = SkinnedSourceMesh {
        vertices: vec![
            test_source_vertex([0.0, 0.0, 0.0]),
            test_source_vertex([1.0, 0.0, 0.0]),
            test_source_vertex([0.0, 1.0, 0.0]),
        ],
        faces: vec![
            SourceFace {
                indices: [0, 1, 2],
                joint: 0,
            },
            SourceFace {
                indices: [0, 2, 1],
                joint: 1,
            },
        ],
    };
    let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], 30_000.0).unwrap();

    let (bytes, vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        &[None, None],
        &[0, 1],
        [255, 255, 255, 255],
        128,
        128,
        psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12,
        false,
    )
    .unwrap();

    let model = psx_asset::Model::from_bytes(&bytes).unwrap();
    assert_eq!(vertices, 3);
    assert_eq!(parts, 2);
    assert_eq!(model.part(0).unwrap().vertex_count(), 3);
    assert_eq!(model.part(1).unwrap().vertex_count(), 0);
    assert_eq!(model.face(0).unwrap().corners[0].vertex_index, 0);
    assert_eq!(model.face(1).unwrap().corners[0].vertex_index, 0);
    assert_eq!(model.face(1).unwrap().corners[1].vertex_index, 2);
    assert_eq!(model.face(1).unwrap().corners[2].vertex_index, 1);
}

#[test]
fn native_model_prunes_small_detached_cooked_position_islands() {
    let mut source = SkinnedSourceMesh {
        vertices: vec![
            test_source_vertex([0.0, 0.0, 0.0]),
            test_source_vertex([1.0, 0.0, 0.0]),
            test_source_vertex([1.0, 1.0, 0.0]),
            test_source_vertex([0.0, 1.0, 0.0]),
            test_source_vertex([4.0, 0.0, 0.0]),
            test_source_vertex([4.5, 0.0, 0.0]),
            test_source_vertex([4.0, 0.5, 0.0]),
        ],
        faces: vec![
            SourceFace {
                indices: [0, 1, 2],
                joint: 0,
            },
            SourceFace {
                indices: [0, 2, 3],
                joint: 0,
            },
            SourceFace {
                indices: [4, 5, 6],
                joint: 0,
            },
        ],
    };
    let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [4.5, 1.0, 0.0], 30_000.0).unwrap();

    let removed = prune_detached_face_islands(&mut source, &bounds, 1);

    assert_eq!(removed, 1);
    assert_eq!(
        source
            .faces
            .iter()
            .map(|face| face.indices)
            .collect::<Vec<_>>(),
        vec![[0, 1, 2], [0, 2, 3]]
    );
}

#[test]
fn native_model_reassigns_vertex_only_joints_to_face_part() {
    let mut foreign_joint_vertex = test_source_vertex([0.0, 0.0, 0.0]);
    foreign_joint_vertex.joints = [1, 0, 0, 0];
    foreign_joint_vertex.dominant_joint = 1;
    let source = SkinnedSourceMesh {
        vertices: vec![
            foreign_joint_vertex,
            SourceVertex {
                position: [1.0, 0.0, 0.0],
                ..foreign_joint_vertex
            },
            SourceVertex {
                position: [0.0, 1.0, 0.0],
                ..foreign_joint_vertex
            },
        ],
        faces: vec![SourceFace {
            indices: [0, 1, 2],
            joint: 0,
        }],
    };
    let bounds = ModelBounds::from_min_max([0.0, 0.0, 0.0], [1.0, 1.0, 0.0], 30_000.0).unwrap();

    let (bytes, vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        &[None, None],
        &[0, 1],
        [255, 255, 255, 255],
        128,
        128,
        psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12,
        false,
    )
    .unwrap();

    let model = psx_asset::Model::from_bytes(&bytes).unwrap();
    assert_eq!(vertices, 3);
    assert_eq!(parts, 1);
    assert_eq!(model.part_count(), 1);
    assert_eq!(model.part(0).unwrap().joint_index(), 0);
    assert_eq!(model.part(0).unwrap().vertex_count(), 3);
    assert_eq!(model.part(0).unwrap().face_count(), 1);
}

#[test]
fn root_translation_normalization_restores_bind_pose_translation() {
    let base = vec![
        Trs {
            translation: [0.25, 1.0, -0.5],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [2.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0, 1.0, 1.0],
        },
    ];
    let mut frame = base.clone();
    frame[0].translation = [-4.0, 3.0, 9.0];
    frame[1].translation = [8.0, 1.0, 2.0];

    restore_root_translations(&mut frame, &base, &[0]);

    assert_eq!(frame[0].translation, base[0].translation);
    assert_eq!(frame[1].translation, [8.0, 1.0, 2.0]);
}

#[test]
fn cooked_animation_pose_scale_is_stripped_when_enabled() {
    let channels = vec![AnimationChannel {
        node_index: 0,
        interpolation: Interpolation::Linear,
        times: vec![0.0, 1.0],
        values: ChannelValues::Scale(vec![[2.0, 2.0, 2.0], [2.0, 2.0, 2.0]]),
    }];
    let parents = [None];
    let base_trs = [Trs {
        translation: [0.0, 0.0, 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        scale: [1.0, 1.0, 1.0],
    }];
    let joints = [0usize];
    let inverse_bind_matrices = [identity_matrix()];
    let bounds = ModelBounds::from_min_max([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0], 30_000.0).unwrap();

    let stripped = cook_animation_bytes(
        &channels,
        &parents,
        &base_trs,
        &joints,
        &joints,
        &inverse_bind_matrices,
        &bounds,
        0.0,
        1.0,
        1,
        false,
        true,
    )
    .unwrap()
    .unwrap();
    let kept = cook_animation_bytes(
        &channels,
        &parents,
        &base_trs,
        &joints,
        &joints,
        &inverse_bind_matrices,
        &bounds,
        0.0,
        1.0,
        1,
        false,
        false,
    )
    .unwrap()
    .unwrap();

    assert_eq!(first_pose_matrix_component(&stripped, 0), 4096);
    assert_eq!(first_pose_matrix_component(&stripped, 4), 4096);
    assert_eq!(first_pose_matrix_component(&stripped, 8), 4096);
    assert_eq!(first_pose_matrix_component(&kept, 0), 8192);
}

#[test]
fn mapped_gltf_same_bind_preserves_local_translation_keys() {
    let target_parents = [None, Some(0), Some(1), Some(2), Some(3), Some(4)];
    let source_parents = [None, Some(0), Some(1), Some(2), Some(3), Some(4)];
    let base = [
        Trs {
            translation: [0.0, 0.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [0.0, 10.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [0.0, 20.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [0.0, 30.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [0.0, 40.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
        Trs {
            translation: [0.0, 50.0, 0.0],
            rotation: identity_quat(),
            scale: [1.0, 1.0, 1.0],
        },
    ];
    let mapping = [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)];
    let channels = [AnimationChannel {
        node_index: 1,
        interpolation: Interpolation::Linear,
        times: vec![0.0, 1.0],
        values: ChannelValues::Translation(vec![[0.0, 10.0, 0.0], [0.0, 24.0, 0.0]]),
    }];

    assert!(mapped_local_binds_match(&base, &base, &mapping));
    let copied = evaluate_mapped_gltf_frame_trs(
        &channels,
        1.0,
        &target_parents,
        &base,
        &source_parents,
        &base,
        &mapping,
        true,
    );
    assert_eq!(copied[1].translation, [0.0, 24.0, 0.0]);

    let retargeted = evaluate_mapped_gltf_frame_trs(
        &channels,
        1.0,
        &target_parents,
        &base,
        &source_parents,
        &base,
        &mapping,
        false,
    );
    assert_eq!(retargeted[1].translation, [0.0, 10.0, 0.0]);
}

#[test]
fn pose_record_round_trips_encoded_model_space() {
    let bounds = ModelBounds::from_min_max([-2.0, -3.0, -4.0], [4.0, 5.0, 6.0], 30_000.0).unwrap();
    let skin = compose_trs([3.0, -2.0, 1.0], quat_z_degrees(90.0), [1.0, 1.0, 1.0]);
    let bytes = finish_animation_bytes(1, 1, 15, &[pose_record(&skin, &bounds)]).unwrap();

    let animation = psx_asset::Animation::from_bytes(&bytes).unwrap();
    let pose = animation.pose(0, 0).unwrap();
    let source = [1.0, 2.0, 3.0];
    let encoded = bounds.normalize_point(source).map(q12_i32);
    let actual = [
        (((pose.matrix[0][0] as i32) * encoded[0]
            + (pose.matrix[1][0] as i32) * encoded[1]
            + (pose.matrix[2][0] as i32) * encoded[2])
            >> 12)
            + pose.translation.x,
        (((pose.matrix[0][1] as i32) * encoded[0]
            + (pose.matrix[1][1] as i32) * encoded[1]
            + (pose.matrix[2][1] as i32) * encoded[2])
            >> 12)
            + pose.translation.y,
        (((pose.matrix[0][2] as i32) * encoded[0]
            + (pose.matrix[1][2] as i32) * encoded[1]
            + (pose.matrix[2][2] as i32) * encoded[2])
            >> 12)
            + pose.translation.z,
    ];
    let expected = bounds
        .normalize_point(transform_point(&skin, source))
        .map(q12_i32);
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() <= 2,
            "axis {axis}: actual {} expected {}",
            actual[axis],
            expected[axis]
        );
    }
}

#[test]
fn root_joint_nodes_skips_children_of_other_skin_joints() {
    let parents = vec![None, Some(0), Some(1), Some(0)];
    assert_eq!(root_joint_nodes(&[1, 2, 3], &parents), vec![1, 3]);
}

#[test]
fn bone_collapse_reweights_subtree_and_rebuilds_joint_table() {
    let parents = vec![None, Some(0), Some(1), Some(0)];
    let node_names = vec![
        "mixamorig:LeftHand".to_string(),
        "mixamorig:LeftHandIndex1".to_string(),
        "FingerTip".to_string(),
        "ArmDecoration".to_string(),
    ];
    let mut joints = vec![0, 1, 2, 3];
    let mut inverse_bind_matrices = vec![identity_matrix(); 4];
    inverse_bind_matrices[3][3][0] = 3.0;
    let mut source = SkinnedSourceMesh {
        vertices: vec![SourceVertex {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            joints: [2, 3, 1, 0],
            weights: [0.5, 0.3, 0.2, 0.0],
            dominant_joint: 2,
        }],
        faces: Vec::new(),
    };

    let removed = collapse_bone_subtrees(
        &mut source,
        &mut joints,
        &mut inverse_bind_matrices,
        &parents,
        &node_names,
        &["HANDINDEX".to_string()],
    )
    .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(joints, vec![0, 3]);
    assert_eq!(inverse_bind_matrices.len(), 2);
    assert_eq!(inverse_bind_matrices[1][3][0], 3.0);
    assert_eq!(source.vertices[0].joints, [0, 1, 0, 0]);
    assert!((source.vertices[0].weights[0] - 0.7).abs() < 0.0001);
    assert!((source.vertices[0].weights[1] - 0.3).abs() < 0.0001);
    assert_eq!(source.vertices[0].dominant_joint, 0);
}

#[test]
fn bone_collapse_root_selection_is_independent_of_joint_order() {
    let parents = vec![None, Some(0), Some(1)];
    let node_names = vec![
        "HandIndexRoot".to_string(),
        "HandIndexChild".to_string(),
        "HandIndexTip".to_string(),
    ];
    let mut joints = vec![2, 0, 1];
    let mut inverse_bind_matrices = vec![identity_matrix(); 3];
    let mut source = SkinnedSourceMesh {
        vertices: vec![SourceVertex {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            joints: [0, 2, 0, 0],
            weights: [0.5, 0.5, 0.0, 0.0],
            dominant_joint: 0,
        }],
        faces: Vec::new(),
    };

    let removed = collapse_bone_subtrees(
        &mut source,
        &mut joints,
        &mut inverse_bind_matrices,
        &parents,
        &node_names,
        &["handindex".to_string()],
    )
    .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(joints, vec![0]);
    assert_eq!(source.vertices[0].joints, [0, 0, 0, 0]);
    assert_eq!(source.vertices[0].weights, [1.0, 0.0, 0.0, 0.0]);
}

#[test]
fn default_bone_collapse_removes_mixamo_terminal_bones() {
    let parents = vec![None, Some(0), Some(1), Some(0), Some(3)];
    let node_names = vec![
        "mixamorig:Hips".to_string(),
        "mixamorig:RightToeBase".to_string(),
        "mixamorig:RightToe_End".to_string(),
        "mixamorig:Head".to_string(),
        "mixamorig:HeadTop_End".to_string(),
    ];
    let mut joints = vec![0, 1, 2, 3, 4];
    let mut inverse_bind_matrices = vec![identity_matrix(); 5];
    let mut source = SkinnedSourceMesh {
        vertices: vec![SourceVertex {
            position: [0.0; 3],
            normal: [0.0, 1.0, 0.0],
            uv: [0.0; 2],
            joints: [2, 4, 0, 0],
            weights: [0.6, 0.4, 0.0, 0.0],
            dominant_joint: 2,
        }],
        faces: Vec::new(),
    };

    let removed = collapse_bone_subtrees(
        &mut source,
        &mut joints,
        &mut inverse_bind_matrices,
        &parents,
        &node_names,
        &default_collapse_bone_patterns(),
    )
    .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(joints, vec![0, 1, 3]);
    assert_eq!(source.vertices[0].joints, [1, 2, 0, 0]);
    assert_eq!(source.vertices[0].weights, [0.6, 0.4, 0.0, 0.0]);
}

fn minimal_triangle_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    for f in [
        0.0f32, 0.0, 0.0, //
        1.0, 0.0, 0.0, //
        0.0, 1.0, 0.0,
    ] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }

    let json = format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"mesh": 0}}],
  "buffers": [{{"byteLength": {}}}],
  "bufferViews": [
{{"buffer": 0, "byteOffset": 0, "byteLength": 36, "target": 34962}},
{{"buffer": 0, "byteOffset": 36, "byteLength": 6, "target": 34963}}
  ],
  "accessors": [
{{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
 "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 0.0]}},
{{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}}
  ],
  "materials": [
{{"pbrMetallicRoughness": {{"baseColorFactor": [0.25, 0.5, 1.0, 1.0]}}}}
  ],
  "meshes": [
{{"primitives": [{{"attributes": {{"POSITION": 0}}, "indices": 1, "material": 0, "mode": 4}}]}}
  ]
}}"#,
        bin.len()
    );
    let json = padded(json.into_bytes(), b' ');
    let bin = padded(bin, 0);

    let total_len = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total_len);
    out.extend_from_slice(&0x4654_6C67u32.to_le_bytes()); // "glTF"
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total_len as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // JSON
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // BIN
    out.extend_from_slice(&bin);
    out
}

fn padded(mut bytes: Vec<u8>, pad: u8) -> Vec<u8> {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(pad);
    }
    bytes
}

fn first_pose_matrix_component(bytes: &[u8], component: usize) -> i16 {
    // The writer picks v3 (Q11-packed) for rigid clips and falls back
    // to v2 when animated scale pushes elements past Q12 one; decode
    // whichever this blob is so the assertions stay in Q3.12 terms.
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let offset = psxed_format::AssetHeader::SIZE + psxed_format::animation::AnimationHeader::SIZE;
    if version == psxed_format::animation::VERSION_V3 {
        let block: [u8; psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3] = bytes
            [offset..offset + psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3]
            .try_into()
            .unwrap();
        return psxed_format::animation::decode_rotation_q11(&block)[component];
    }
    let at = offset + component * 2;
    i16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn test_source_vertex(position: [f32; 3]) -> SourceVertex {
    SourceVertex {
        position,
        normal: [0.0, 1.0, 0.0],
        uv: [0.0, 0.0],
        joints: [0; 4],
        weights: [1.0, 0.0, 0.0, 0.0],
        dominant_joint: 0,
    }
}

// Temporary diagnostic for the flipped-normal report on imported
// static GLB props: prints source node determinants (a mirroring
// transform inverts winding) and the cooked faces' outward-facing
// ratio. Run with: cargo test -p psxed-gltf diagnose_static_glb -- --ignored --nocapture
#[test]
#[ignore]
fn diagnose_static_glb_winding() {
    let path = std::env::var("DIAG_GLB")
        .unwrap_or_else(|_| "/Users/ebonura/Downloads/ps1_clean_power_barricade.glb".to_string());
    let (document, _buffers, _images) = gltf::import(&path).expect("glb loads");
    fn det3(m: &[[f32; 4]; 4]) -> f32 {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }
    fn walk(node: gltf::Node<'_>, parent: [[f32; 4]; 4], depth: usize) {
        let world = mul_matrix(&parent, &node.transform().matrix());
        let has_mesh = node.mesh().is_some();
        println!(
            "{:indent$}node '{}' det={:.4} mesh={}",
            "",
            node.name().unwrap_or("?"),
            det3(&world),
            has_mesh,
            indent = depth * 2
        );
        for child in node.children() {
            walk(child, world, depth + 1);
        }
    }
    for scene in document.scenes() {
        for node in scene.nodes() {
            walk(node, identity_matrix(), 0);
        }
    }

    // Source-truth check: does the file's own winding agree with its
    // authored normals? PS1-style hobby assets often ship reversed
    // winding and rely on doubleSided materials.
    let (document2, buffers2, _) = gltf::import(&path).expect("glb reloads");
    for mesh in document2.meshes() {
        for primitive in mesh.primitives() {
            let double_sided = primitive.material().double_sided();
            let reader = primitive.reader(|buffer| Some(buffers2[buffer.index()].0.as_slice()));
            let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|iter| iter.collect())
                .unwrap_or_default();
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|iter| iter.into_u32().collect())
                .unwrap_or_else(|| (0..positions.len() as u32).collect());
            let mut agree = 0usize;
            let mut oppose = 0usize;
            for tri in indices.chunks_exact(3) {
                let (a, b, c) = (
                    positions[tri[0] as usize],
                    positions[tri[1] as usize],
                    positions[tri[2] as usize],
                );
                let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let geometric = [
                    ab[1] * ac[2] - ab[2] * ac[1],
                    ab[2] * ac[0] - ab[0] * ac[2],
                    ab[0] * ac[1] - ab[1] * ac[0],
                ];
                if normals.is_empty() {
                    continue;
                }
                let stored = tri
                    .iter()
                    .map(|i| normals[*i as usize])
                    .fold([0f32; 3], |acc, n| {
                        [acc[0] + n[0], acc[1] + n[1], acc[2] + n[2]]
                    });
                let dot =
                    geometric[0] * stored[0] + geometric[1] * stored[1] + geometric[2] * stored[2];
                if dot >= 0.0 {
                    agree += 1;
                } else {
                    oppose += 1;
                }
            }
            println!(
                "primitive: double_sided={double_sided} winding-vs-normals agree={agree} oppose={oppose}"
            );
        }
    }

    let package =
        convert_rigid_model_path(&path, &RigidModelConfig::default()).expect("rigid cook");
    let model = psx_asset::Model::from_bytes(&package.model).expect("model decodes");
    let mut centroid = [0f64; 3];
    let count = model.vertex_count() as usize;
    for index in 0..count {
        let v = model.vertex(index as u16).unwrap().position;
        centroid[0] += v.x as f64;
        centroid[1] += v.y as f64;
        centroid[2] += v.z as f64;
    }
    for c in &mut centroid {
        *c /= count.max(1) as f64;
    }
    let mut outward = 0usize;
    let mut inward = 0usize;
    for face_index in 0..model.face_count() {
        let face = model.face(face_index).unwrap();
        let p = |corner: usize| {
            let v = model
                .vertex(face.corners[corner].vertex_index)
                .unwrap()
                .position;
            [v.x as f64, v.y as f64, v.z as f64]
        };
        let (a, b, c) = (p(0), p(1), p(2));
        let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let normal = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let center = [
            (a[0] + b[0] + c[0]) / 3.0 - centroid[0],
            (a[1] + b[1] + c[1]) / 3.0 - centroid[1],
            (a[2] + b[2] + c[2]) / 3.0 - centroid[2],
        ];
        let dot = normal[0] * center[0] + normal[1] * center[1] + normal[2] * center[2];
        if dot >= 0.0 {
            outward += 1;
        } else {
            inward += 1;
        }
    }
    println!("cooked faces: outward={outward} inward={inward}");
    std::fs::write("/tmp/diag_prop.psxmdl", &package.model).unwrap();
    std::fs::write("/tmp/diag_prop.psxanim", &package.clips[0].bytes).unwrap();
    println!("wrote /tmp/diag_prop.psxmdl + .psxanim");
}

#[test]
fn mixed_winding_faces_orient_to_stored_normals() {
    // Triangle in the XZ plane wound so its geometric normal points
    // -Y, while the authored normals say +Y: the winding opposes the
    // normals and must re-orient at import. The flipped order agrees.
    let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
    let up = [[0.0, 1.0, 0.0]; 3];
    assert!(winding_opposes_stored_normals(positions, up));
    assert!(!winding_opposes_stored_normals(
        [positions[0], positions[2], positions[1]],
        up
    ));

    // A reversed source face cooks to the same engine winding as the
    // equivalent correctly-wound face, and its shading-normal face
    // entry is corrected too.
    let mut source = SkinnedSourceMesh::default();
    source.vertices.extend([
        test_source_vertex([0.0, 0.0, 0.0]),
        test_source_vertex([1.0, 0.0, 0.0]),
        test_source_vertex([0.0, 0.0, 1.0]),
    ]);
    let mut normal_faces = Vec::new();
    push_imported_face(&mut source, &mut normal_faces, [0, 1, 2], false);
    push_imported_face(&mut source, &mut normal_faces, [0, 1, 2], true);
    assert_eq!(source.faces[0].indices, [0, 2, 1]);
    assert_eq!(source.faces[1].indices, [0, 1, 2]);
    assert_eq!(normal_faces[0], [0, 1, 2]);
    assert_eq!(normal_faces[1], [0, 2, 1]);

    // Mirrored node transforms invert winding on their own.
    let identity = identity_matrix();
    assert!(!transform_reverses_winding(&identity));
    let mut mirrored = identity_matrix();
    mirrored[0][0] = -1.0;
    assert!(transform_reverses_winding(&mirrored));
}
