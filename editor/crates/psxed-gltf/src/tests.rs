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
fn retarget_mapped_frame_trs_applies_delta_in_source_rest_basis() {
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

    assert_quat_close(
        retargeted[0].rotation,
        quat_mul(target_base_rotation, source_local_delta),
    );
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
    let offset = psxed_format::AssetHeader::SIZE
        + psxed_format::animation::AnimationHeader::SIZE
        + component * 2;
    i16::from_le_bytes([bytes[offset], bytes[offset + 1]])
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
