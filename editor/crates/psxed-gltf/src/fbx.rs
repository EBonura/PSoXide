//! FBX (`ufbx`) import backend.
//!
//! Cooks FBX scenes (and extra FBX/glTF animation scenes) into the same
//! rigid-model + animation outputs as the glTF path: skin-weight extraction,
//! node retargeting between source and target skeletons, and base-colour
//! texture baking.

use super::*;

pub(crate) fn convert_fbx_rigid_model_scene(
    scene: &ufbx::Scene,
    source_path: Option<&Path>,
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    convert_fbx_rigid_model_scene_with_extra_animations(scene, source_path, &[], cfg)
}

pub(crate) fn convert_fbx_rigid_model_scene_with_extra_animations(
    scene: &ufbx::Scene,
    source_path: Option<&Path>,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    if cfg.animation_fps == 0 {
        return Err(Error::BadSkin("animation sample rate must be non-zero"));
    }

    let (mesh_node, mesh, skin) = match fbx_skinned_mesh(scene) {
        Ok(mesh) => mesh,
        Err(Error::MissingFbxSkinnedMesh) => {
            if !extra_animation_scenes.is_empty() {
                return Err(Error::BadSkin(
                    "extra animation sources require a skinned source model",
                ));
            }
            return convert_static_fbx_rigid_model_scene(scene, source_path, cfg);
        }
        Err(error) => return Err(error),
    };
    let node_indices = fbx_node_indices(scene);
    let parents = fbx_parent_indices(scene, &node_indices);
    let base_trs = fbx_base_trs(scene);

    let mut joints = Vec::new();
    let mut inverse_bind_matrices = Vec::new();
    for cluster in &skin.clusters {
        let Some(bone_node) = cluster.bone_node.as_deref() else {
            return Err(Error::BadSkin("FBX skin cluster has no bone node"));
        };
        let joint_index = fbx_node_index(&node_indices, bone_node)?;
        joints.push(joint_index);
        inverse_bind_matrices.push(fbx_matrix_to_mat4(cluster.geometry_to_bone));
    }
    if joints.is_empty() {
        return Err(Error::BadSkin("FBX skin has no joints"));
    }
    ensure_u16("joints", joints.len())?;

    let root_joint_nodes = root_joint_nodes(&joints, &parents);
    let mut source = read_fbx_skinned_mesh(mesh, skin, joints.len())?;
    if source.faces.is_empty() {
        return Err(Error::Empty);
    }
    if cfg.force_single_bind {
        collapse_to_single_bind(&mut source);
    }
    assign_face_joints(&mut source, joints.len());
    let bounds_extra_animation_scenes = if cfg.extra_animations_affect_bounds {
        extra_animation_scenes
    } else {
        &[]
    };

    let precision_bounds = collect_fbx_precision_bounds(
        scene,
        bounds_extra_animation_scenes,
        &source,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
        !cfg.ignore_embedded_animations,
    )?;
    let bounds = ModelBounds::from_min_max(
        precision_bounds.min,
        precision_bounds.max,
        MODEL_LOCAL_COORD_LIMIT,
    )?;
    if prune_detached_face_islands(
        &mut source,
        &bounds,
        cfg.prune_detached_face_islands as usize,
    ) > 0
    {
        if source.faces.is_empty() {
            return Err(Error::Empty);
        }
        let precision_bounds = collect_fbx_precision_bounds(
            scene,
            bounds_extra_animation_scenes,
            &source,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            cfg.animation_fps,
            cfg.normalize_root_translation,
            cfg.strip_animation_scale,
            !cfg.ignore_embedded_animations,
        )?;
        let bounds = ModelBounds::from_min_max(
            precision_bounds.min,
            precision_bounds.max,
            MODEL_LOCAL_COORD_LIMIT,
        )?;
        return finish_fbx_rigid_model_scene(
            scene,
            cfg,
            source_path,
            source,
            bounds,
            precision_bounds,
            &parents,
            &base_trs,
            &root_joint_nodes,
            &joints,
            &inverse_bind_matrices,
            mesh_node,
            mesh,
            extra_animation_scenes,
        );
    }

    finish_fbx_rigid_model_scene(
        scene,
        cfg,
        source_path,
        source,
        bounds,
        precision_bounds,
        &parents,
        &base_trs,
        &root_joint_nodes,
        &joints,
        &inverse_bind_matrices,
        mesh_node,
        mesh,
        extra_animation_scenes,
    )
}

pub(crate) fn convert_static_fbx_rigid_model_scene(
    scene: &ufbx::Scene,
    source_path: Option<&Path>,
    cfg: &RigidModelConfig,
) -> Result<RigidModelPackage, Error> {
    let (mesh_node, mesh) = fbx_static_mesh(scene)?;
    let mut source = read_fbx_static_mesh(mesh_node, mesh)?;
    if source.faces.is_empty() {
        return Err(Error::Empty);
    }

    let precision_bounds = collect_static_precision_bounds(&source)?;
    let bounds = ModelBounds::from_min_max(
        precision_bounds.min,
        precision_bounds.max,
        MODEL_LOCAL_COORD_LIMIT,
    )?;
    if prune_detached_face_islands(
        &mut source,
        &bounds,
        cfg.prune_detached_face_islands as usize,
    ) > 0
    {
        if source.faces.is_empty() {
            return Err(Error::Empty);
        }
        let precision_bounds = collect_static_precision_bounds(&source)?;
        let bounds = ModelBounds::from_min_max(
            precision_bounds.min,
            precision_bounds.max,
            MODEL_LOCAL_COORD_LIMIT,
        )?;
        return finish_static_fbx_rigid_model_scene(
            cfg,
            source_path,
            source,
            bounds,
            precision_bounds,
            mesh,
        );
    }

    finish_static_fbx_rigid_model_scene(cfg, source_path, source, bounds, precision_bounds, mesh)
}

pub(crate) fn finish_static_fbx_rigid_model_scene(
    cfg: &RigidModelConfig,
    source_path: Option<&Path>,
    source: SkinnedSourceMesh,
    bounds: ModelBounds,
    precision_bounds: PrecisionBounds,
    mesh: &ufbx::Mesh,
) -> Result<RigidModelPackage, Error> {
    let local_height = bounds.encoded_axis_size(precision_bounds.min[1], precision_bounds.max[1]);
    let local_to_world_q12 = choose_local_to_world_q12(local_height, cfg.world_height);
    let material_color = first_fbx_material_base_color(mesh);
    let texture = cook_fbx_base_color_texture(mesh, source_path, material_color, cfg)?;
    let parents = [None];
    let joints = [0usize];
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        &parents,
        &joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
        cfg.double_sided,
    )?;
    let clips = vec![bind_pose_clip(joints.len(), &bounds, cfg.animation_fps)?];
    let animation_bytes = clips.iter().map(|c| c.bytes.len()).sum();
    let clip_frames = clips
        .iter()
        .map(|c| (c.sanitized_name.clone(), c.frames))
        .collect();
    let report = RigidModelReport {
        source_vertices: source.vertices.len(),
        cooked_vertices,
        faces: source.faces.len(),
        parts,
        joints: joints.len(),
        clip_frames,
        local_height: local_height.max(0) as usize,
        local_to_world_q12,
        model_bytes: model.len(),
        animation_bytes,
        texture_bytes: texture.as_ref().map_or(0, Vec::len),
    };

    Ok(RigidModelPackage {
        model,
        clips,
        texture,
        report,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finish_fbx_rigid_model_scene(
    scene: &ufbx::Scene,
    cfg: &RigidModelConfig,
    source_path: Option<&Path>,
    source: SkinnedSourceMesh,
    bounds: ModelBounds,
    precision_bounds: PrecisionBounds,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    _mesh_node: &ufbx::Node,
    mesh: &ufbx::Mesh,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
) -> Result<RigidModelPackage, Error> {
    let local_height = bounds.encoded_axis_size(precision_bounds.min[1], precision_bounds.max[1]);
    let local_to_world_q12 = choose_local_to_world_q12(local_height, cfg.world_height);
    let material_color = first_fbx_material_base_color(mesh);
    let texture = cook_fbx_base_color_texture(mesh, source_path, material_color, cfg)?;
    let (model, cooked_vertices, parts) = cook_model_blob(
        &source,
        &bounds,
        parents,
        joints,
        material_color,
        cfg.texture_width,
        cfg.texture_height,
        local_to_world_q12,
        cfg.double_sided,
    )?;
    let mut clips = cook_all_fbx_animations(
        scene,
        extra_animation_scenes,
        parents,
        base_trs,
        root_joint_nodes,
        joints,
        inverse_bind_matrices,
        &bounds,
        cfg.animation_fps,
        cfg.normalize_root_translation,
        cfg.strip_animation_scale,
        !cfg.ignore_embedded_animations,
    )?;
    if clips.is_empty() {
        clips.push(bind_pose_clip(joints.len(), &bounds, cfg.animation_fps)?);
    }

    let animation_bytes = clips.iter().map(|c| c.bytes.len()).sum();
    let clip_frames = clips
        .iter()
        .map(|c| (c.sanitized_name.clone(), c.frames))
        .collect();
    let report = RigidModelReport {
        source_vertices: source.vertices.len(),
        cooked_vertices,
        faces: source.faces.len(),
        parts,
        joints: joints.len(),
        clip_frames,
        local_height: local_height.max(0) as usize,
        local_to_world_q12,
        model_bytes: model.len(),
        animation_bytes,
        texture_bytes: texture.as_ref().map_or(0, Vec::len),
    };

    Ok(RigidModelPackage {
        model,
        clips,
        texture,
        report,
    })
}

pub(crate) fn fbx_skinned_mesh(
    scene: &ufbx::Scene,
) -> Result<(&ufbx::Node, &ufbx::Mesh, &ufbx::SkinDeformer), Error> {
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.as_deref() else {
            continue;
        };
        let Some(skin) = mesh.skin_deformers.as_ref().first().map(AsRef::as_ref) else {
            continue;
        };
        if mesh.num_vertices > 0 && mesh.num_faces > 0 {
            return Ok((node, mesh, skin));
        }
    }
    Err(Error::MissingFbxSkinnedMesh)
}

pub(crate) fn fbx_static_mesh(scene: &ufbx::Scene) -> Result<(&ufbx::Node, &ufbx::Mesh), Error> {
    for node in &scene.nodes {
        let Some(mesh) = node.mesh.as_deref() else {
            continue;
        };
        if mesh.num_vertices > 0 && mesh.num_faces > 0 {
            return Ok((node, mesh));
        }
    }
    Err(Error::Empty)
}

pub(crate) fn fbx_node_indices(scene: &ufbx::Scene) -> HashMap<usize, usize> {
    let mut out = HashMap::new();
    for (index, node) in scene.nodes.as_ref().iter().enumerate() {
        out.insert(node.as_ref() as *const ufbx::Node as usize, index);
    }
    out
}

pub(crate) fn fbx_node_index(
    node_indices: &HashMap<usize, usize>,
    node: &ufbx::Node,
) -> Result<usize, Error> {
    node_indices
        .get(&(node as *const ufbx::Node as usize))
        .copied()
        .ok_or(Error::BadSkin("FBX node is not in the scene node table"))
}

pub(crate) fn fbx_parent_indices(
    scene: &ufbx::Scene,
    node_indices: &HashMap<usize, usize>,
) -> Vec<Option<usize>> {
    scene
        .nodes
        .iter()
        .map(|node| {
            node.parent
                .as_deref()
                .and_then(|parent| fbx_node_index(node_indices, parent).ok())
        })
        .collect()
}

pub(crate) fn fbx_base_trs(scene: &ufbx::Scene) -> Vec<Trs> {
    scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.local_transform))
        .collect()
}

pub(crate) fn fbx_transform_to_trs(transform: ufbx::Transform) -> Trs {
    Trs {
        translation: fbx_vec3(transform.translation),
        rotation: fbx_quat(transform.rotation),
        scale: fbx_vec3(transform.scale),
    }
}

pub(crate) fn fbx_vec3(v: ufbx::Vec3) -> [f32; 3] {
    [v.x as f32, v.y as f32, v.z as f32]
}

pub(crate) fn fbx_vec2(v: ufbx::Vec2) -> [f32; 2] {
    [v.x as f32, v.y as f32]
}

pub(crate) fn fbx_quat(q: ufbx::Quat) -> [f32; 4] {
    [q.x as f32, q.y as f32, q.z as f32, q.w as f32]
}

pub(crate) fn fbx_matrix_to_mat4(m: ufbx::Matrix) -> [[f32; 4]; 4] {
    [
        [m.m00 as f32, m.m10 as f32, m.m20 as f32, 0.0],
        [m.m01 as f32, m.m11 as f32, m.m21 as f32, 0.0],
        [m.m02 as f32, m.m12 as f32, m.m22 as f32, 0.0],
        [m.m03 as f32, m.m13 as f32, m.m23 as f32, 1.0],
    ]
}

pub(crate) fn read_fbx_skinned_mesh(
    mesh: &ufbx::Mesh,
    skin: &ufbx::SkinDeformer,
    joint_count: usize,
) -> Result<SkinnedSourceMesh, Error> {
    if !mesh.vertex_position.exists {
        return Err(Error::MissingAttribute {
            primitive_index: 0,
            attribute: "POSITION",
        });
    }

    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    let mut triangulated = Vec::new();
    for face in mesh.faces.iter().copied() {
        triangulated.clear();
        ufbx::triangulate_face_vec(&mut triangulated, mesh, face);
        for tri in triangulated.chunks_exact(3) {
            let mut indices = [0usize; 3];
            for (corner, fbx_index) in tri.iter().copied().enumerate() {
                let fbx_index = fbx_index as usize;
                let Some(source_vertex_index) = mesh
                    .vertex_indices
                    .get(fbx_index)
                    .copied()
                    .map(|index| index as usize)
                else {
                    return Err(Error::BadSkin("FBX face index is outside vertex table"));
                };
                let position = fbx_vec3(mesh.vertex_position[fbx_index]);
                let normal = if mesh.vertex_normal.exists {
                    normalize3(fbx_vec3(mesh.vertex_normal[fbx_index]))
                } else {
                    [0.0, 1.0, 0.0]
                };
                let uv = if mesh.vertex_uv.exists {
                    // FBX UVs use a bottom-origin V; the cooked atlas is stored
                    // top-origin, so flip V to match.
                    let uv = fbx_vec2(mesh.vertex_uv[fbx_index]);
                    [uv[0], 1.0 - uv[1]]
                } else {
                    [0.0, 0.0]
                };
                let (joints, weights) = fbx_skin_weights(skin, source_vertex_index, joint_count);
                let cleaned_joints = joint_indices_or_zero(joints, weights);
                indices[corner] = source.vertices.len();
                source.vertices.push(SourceVertex {
                    position,
                    normal,
                    uv,
                    joints: cleaned_joints,
                    weights,
                    dominant_joint: dominant_vertex_joint(cleaned_joints, weights),
                });
            }
            let reversed = mesh.vertex_normal.exists
                && winding_opposes_stored_normals(
                    indices.map(|index| source.vertices[index].position),
                    indices.map(|index| source.vertices[index].normal),
                );
            push_imported_face(&mut source, &mut normal_faces, indices, reversed);
        }
    }
    rebuild_source_normals(&mut source, &normal_faces);
    Ok(source)
}

pub(crate) fn read_fbx_static_mesh(
    mesh_node: &ufbx::Node,
    mesh: &ufbx::Mesh,
) -> Result<SkinnedSourceMesh, Error> {
    if !mesh.vertex_position.exists {
        return Err(Error::MissingAttribute {
            primitive_index: 0,
            attribute: "POSITION",
        });
    }

    let transform = fbx_matrix_to_mat4(mesh_node.geometry_to_world);
    let mirrored = transform_reverses_winding(&transform);
    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    let mut triangulated = Vec::new();
    for face in mesh.faces.iter().copied() {
        triangulated.clear();
        ufbx::triangulate_face_vec(&mut triangulated, mesh, face);
        for tri in triangulated.chunks_exact(3) {
            let mut indices = [0usize; 3];
            // Raw geometry-space corners for the winding test below;
            // stored vertex positions are world-transformed.
            let mut raw_positions = [[0.0f32; 3]; 3];
            let mut raw_normals = [[0.0f32; 3]; 3];
            for (corner, fbx_index) in tri.iter().copied().enumerate() {
                let fbx_index = fbx_index as usize;
                if mesh.vertex_indices.get(fbx_index).is_none() {
                    return Err(Error::BadSkin("FBX face index is outside vertex table"));
                }
                let raw = fbx_vec3(mesh.vertex_position[fbx_index]);
                raw_positions[corner] = raw;
                if mesh.vertex_normal.exists {
                    raw_normals[corner] = normalize3(fbx_vec3(mesh.vertex_normal[fbx_index]));
                }
                let position = transform_point(&transform, raw);
                let uv = if mesh.vertex_uv.exists {
                    // FBX UVs use a bottom-origin V; the cooked atlas is stored
                    // top-origin, so flip V to match.
                    let uv = fbx_vec2(mesh.vertex_uv[fbx_index]);
                    [uv[0], 1.0 - uv[1]]
                } else {
                    [0.0, 0.0]
                };
                indices[corner] = source.vertices.len();
                source.vertices.push(SourceVertex {
                    position,
                    normal: [0.0, 1.0, 0.0],
                    uv,
                    joints: [0, 0, 0, 0],
                    weights: [1.0, 0.0, 0.0, 0.0],
                    dominant_joint: 0,
                });
            }
            let opposes = mesh.vertex_normal.exists
                && winding_opposes_stored_normals(raw_positions, raw_normals);
            push_imported_face(&mut source, &mut normal_faces, indices, opposes != mirrored);
        }
    }
    rebuild_source_normals(&mut source, &normal_faces);
    Ok(source)
}

pub(crate) fn fbx_skin_weights(
    skin: &ufbx::SkinDeformer,
    vertex_index: usize,
    joint_count: usize,
) -> ([u16; 4], [f32; 4]) {
    let mut influences: Vec<(u16, f32)> = Vec::new();
    if let Some(vertex) = skin.vertices.get(vertex_index) {
        let begin = vertex.weight_begin as usize;
        let end = begin.saturating_add(vertex.num_weights as usize);
        for weight in skin.weights.get(begin..end).unwrap_or(&[]) {
            let joint = weight.cluster_index as usize;
            if joint < joint_count && weight.weight > 0.0 {
                influences.push((joint as u16, weight.weight as f32));
            }
        }
    }
    influences.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    let mut total = 0.0f32;
    for (slot, (joint, weight)) in influences.into_iter().take(4).enumerate() {
        joints[slot] = joint;
        weights[slot] = weight;
        total += weight;
    }
    if total > 0.0 {
        for weight in &mut weights {
            *weight /= total;
        }
    } else {
        weights[0] = 1.0;
    }
    (joints, weights)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_fbx_precision_bounds(
    scene: &ufbx::Scene,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    include_embedded: bool,
) -> Result<PrecisionBounds, Error> {
    let mut bounds = BoundsAccumulator::new();
    // The cooked vertex records store RAW mesh-local positions, so the
    // quantization bounds must contain them. include_pose_bounds only adds
    // the SKINNED bind positions; when a model's rest pose diverges from its
    // skin bind pose those skinned positions don't cover the raw mesh, and
    // the divergent parts (e.g. the head) overflow i16 and clamp flat. Seed
    // the bounds with the raw positions first so nothing clamps.
    for vertex in &source.vertices {
        bounds.include(vertex.position);
    }
    include_pose_bounds(
        &mut bounds,
        base_trs,
        source,
        parents,
        joints,
        inverse_bind_matrices,
        strip_animation_scale,
    );

    if include_embedded {
        for stack in &scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let frame_count = ((max_time - min_time) * fps as f64).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f64 / fps as f64).min(max_time);
                let mut frame_trs = evaluate_fbx_frame_trs(scene, &stack.anim, time);
                if normalize_root_translation {
                    restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
                }
                include_pose_bounds(
                    &mut bounds,
                    &frame_trs,
                    source,
                    parents,
                    joints,
                    inverse_bind_matrices,
                    strip_animation_scale,
                );
            }
        }
    }

    for extra in extra_animation_scenes {
        let mapping = fbx_animation_node_mapping(scene, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let frame_count = ((max_time - min_time) * fps as f64).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f64 / fps as f64).min(max_time);
                let mut frame_trs = evaluate_mapped_fbx_frame_trs(
                    extra.scene,
                    &stack.anim,
                    time,
                    parents,
                    base_trs,
                    &mapping,
                );
                if normalize_root_translation {
                    restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
                }
                include_pose_bounds(
                    &mut bounds,
                    &frame_trs,
                    source,
                    parents,
                    joints,
                    inverse_bind_matrices,
                    strip_animation_scale,
                );
            }
        }
    }

    bounds.finish()
}

pub(crate) fn fbx_stack_time_range(stack: &ufbx::AnimStack) -> Option<(f64, f64)> {
    let min_time = stack.time_begin;
    let max_time = stack.time_end;
    if min_time.is_finite() && max_time.is_finite() && max_time >= min_time {
        Some((min_time, max_time))
    } else {
        None
    }
}

pub(crate) fn evaluate_fbx_frame_trs(
    scene: &ufbx::Scene,
    anim: &ufbx::Anim,
    time: f64,
) -> Vec<Trs> {
    scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.evaluate_transform(anim, time)))
        .collect()
}

pub(crate) fn evaluate_mapped_fbx_frame_trs(
    animation_scene: &ufbx::Scene,
    anim: &ufbx::Anim,
    time: f64,
    target_parents: &[Option<usize>],
    base_trs: &[Trs],
    mapping: &[Option<usize>],
) -> Vec<Trs> {
    let evaluated: Vec<Trs> = animation_scene
        .nodes
        .iter()
        .map(|node| fbx_transform_to_trs(node.evaluate_transform(anim, time)))
        .collect();
    let animation_base = fbx_base_trs(animation_scene);
    let node_indices = fbx_node_indices(animation_scene);
    let source_parents = fbx_parent_indices(animation_scene, &node_indices);
    retarget_mapped_frame_trs(
        target_parents,
        base_trs,
        &source_parents,
        &animation_base,
        &evaluated,
        mapping,
    )
}

pub(crate) fn evaluate_mapped_gltf_frame_trs(
    channels: &[AnimationChannel],
    time: f32,
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    mapping: &[Option<usize>],
    copy_full_local_trs: bool,
) -> Vec<Trs> {
    let mut source_pose_trs = source_base_trs.to_vec();
    for channel in channels {
        channel.apply(time, &mut source_pose_trs);
    }

    if !copy_full_local_trs {
        return retarget_mapped_frame_trs(
            target_parents,
            target_base_trs,
            source_parents,
            source_base_trs,
            &source_pose_trs,
            mapping,
        );
    }

    let mut out = target_base_trs.to_vec();
    for (target_index, source_index) in mapping.iter().copied().enumerate() {
        let Some(source_index) = source_index else {
            continue;
        };
        let (Some(target), Some(source)) =
            (out.get_mut(target_index), source_pose_trs.get(source_index))
        else {
            continue;
        };
        *target = *source;
    }
    out
}

pub(crate) fn retarget_mapped_frame_trs(
    _target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    _source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    source_pose_trs: &[Trs],
    mapping: &[Option<usize>],
) -> Vec<Trs> {
    // Parent-relative (local) joint-angle retarget: each mapped target bone
    // takes the source bone's LOCAL rotation delta from its own bind, re-based
    // onto the target bone's bind. The bone keeps the TARGET's translation and
    // scale (its own proportions); only the joint angle is borrowed.
    //
    // Working in local (parent-relative) space is what makes this correct
    // through a hierarchy: any two same-axis rigs (e.g. two Mixamo skeletons)
    // reproduce the motion exactly, and a matching bind collapses to a direct
    // local copy. The previous implementation took the delta in GLOBAL bind
    // frames, which distorted every child bone whenever the source and target
    // rest poses differed (arms arched backward, mangled feet). The single-root
    // retarget tests passed because for a root node local == global; the bug
    // only surfaced on real multi-bone skeletons.
    let mut out = target_base_trs.to_vec();
    for (target_index, source_index) in mapping.iter().copied().enumerate() {
        let Some(source_index) = source_index else {
            continue;
        };
        let (Some(source_bind), Some(source_pose), Some(target)) = (
            source_base_trs.get(source_index),
            source_pose_trs.get(source_index),
            out.get_mut(target_index),
        ) else {
            continue;
        };
        let joint_delta = quat_mul(quat_inverse(source_bind.rotation), source_pose.rotation);
        target.rotation = quat_mul(target.rotation, joint_delta);
    }
    out
}

pub(crate) fn fbx_animation_node_mapping(
    model_scene: &ufbx::Scene,
    animation_scene: &ufbx::Scene,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for (index, node) in animation_scene.nodes.iter().enumerate() {
        let key = node_match_key(&node.element.name);
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(index);
        }
    }
    model_scene
        .nodes
        .iter()
        .map(|node| {
            let key = node_match_key(&node.element.name);
            animation_nodes.get(&key).copied()
        })
        .collect()
}

pub(crate) fn gltf_fbx_animation_node_mapping(
    document: &gltf::Document,
    animation_scene: &ufbx::Scene,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for (index, node) in animation_scene.nodes.iter().enumerate() {
        let key = node_match_key(&node.element.name);
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(index);
        }
    }
    document
        .nodes()
        .map(|node| {
            let key = node.name().map(node_match_key).unwrap_or_default();
            animation_nodes.get(&key).copied()
        })
        .collect()
}

pub(crate) fn gltf_gltf_animation_node_mapping(
    model_document: &gltf::Document,
    animation_document: &gltf::Document,
) -> Vec<Option<usize>> {
    let mut animation_nodes = HashMap::new();
    for node in animation_document.nodes() {
        let key = node.name().map(node_match_key).unwrap_or_default();
        if !key.is_empty() {
            animation_nodes.entry(key).or_insert(node.index());
        }
    }
    model_document
        .nodes()
        .map(|node| {
            let key = node.name().map(node_match_key).unwrap_or_default();
            animation_nodes.get(&key).copied()
        })
        .collect()
}

pub(crate) fn validate_fbx_animation_mapping(
    animation_scene: &ufbx::Scene,
    mapping: &[Option<usize>],
) -> Result<(), Error> {
    if animation_scene.anim_stacks.is_empty() {
        Ok(())
    } else {
        let mapped = mapping.iter().filter(|entry| entry.is_some()).count();
        if mapped >= 6 {
            Ok(())
        } else if mapped == 0 {
            Err(Error::BadSkin(
                "FBX animation shares no named skeleton nodes with the model",
            ))
        } else {
            Err(Error::BadSkin(
                "FBX animation maps too few humanoid skeleton nodes to retarget safely",
            ))
        }
    }
}

pub(crate) fn validate_gltf_animation_mapping(
    animation_document: &gltf::Document,
    mapping: &[Option<usize>],
) -> Result<(), Error> {
    if animation_document.animations().next().is_none() {
        Ok(())
    } else {
        let mapped = mapping.iter().filter(|entry| entry.is_some()).count();
        if mapped >= 6 {
            Ok(())
        } else if mapped == 0 {
            Err(Error::BadSkin(
                "glTF animation shares no named skeleton nodes with the model",
            ))
        } else {
            Err(Error::BadSkin(
                "glTF animation maps too few humanoid skeleton nodes to retarget safely",
            ))
        }
    }
}

pub(crate) fn mapped_local_binds_match(
    target_base_trs: &[Trs],
    source_base_trs: &[Trs],
    mapping: &[Option<usize>],
) -> bool {
    let mut compared = 0usize;
    for (target_index, source_index) in mapping.iter().copied().enumerate() {
        let Some(source_index) = source_index else {
            continue;
        };
        let (Some(target), Some(source)) = (
            target_base_trs.get(target_index),
            source_base_trs.get(source_index),
        ) else {
            return false;
        };
        compared += 1;
        if !vec3_close(target.translation, source.translation, 0.01)
            || !vec3_close(target.scale, source.scale, 0.001)
            || !quat_close_same_orientation(target.rotation, source.rotation, 0.999)
        {
            return false;
        }
    }
    compared >= 6
}

pub(crate) fn node_match_key(name: &str) -> String {
    let raw = name
        .rsplit([':', '|'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_lowercase();
    let compact: String = raw
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    match raw.as_str() {
        "spine_01" | "spine 01" => return "spine1".to_string(),
        "spine_02" | "spine 02" => return "spine2".to_string(),
        "spine_03" | "spine 03" => return "spine3".to_string(),
        _ => {}
    }
    match compact.as_str() {
        "rootnode" | "root" => "root".to_string(),
        "armature" => "armature".to_string(),
        "hips" | "pelvis" => "hips".to_string(),
        "spine" => "spine1".to_string(),
        "spine1" | "spine01" => "spine2".to_string(),
        "spine2" | "spine02" | "spine3" | "spine03" => "spine3".to_string(),
        "neck" | "neck01" => "neck".to_string(),
        "head" => "head".to_string(),
        "leftshoulder" | "claviclel" | "leftclavicle" | "clavicleleft" => {
            "leftshoulder".to_string()
        }
        "rightshoulder" | "clavicler" | "rightclavicle" | "clavicleright" => {
            "rightshoulder".to_string()
        }
        "leftarm" | "shoulderl" | "leftupperarm" | "upperarml" | "upperarmleft" => {
            "leftarm".to_string()
        }
        "rightarm" | "shoulderr" | "rightupperarm" | "upperarmr" | "upperarmright" => {
            "rightarm".to_string()
        }
        "leftforearm" | "elbowl" | "leftlowerarm" | "lowerarml" | "lowerarmleft" => {
            "leftforearm".to_string()
        }
        "rightforearm" | "elbowr" | "rightlowerarm" | "lowerarmr" | "lowerarmright" => {
            "rightforearm".to_string()
        }
        "lefthand" | "handl" => "lefthand".to_string(),
        "righthand" | "handr" => "righthand".to_string(),
        "leftupleg" | "leftupperleg" | "upperlegl" | "thighl" | "thighleft" => {
            "leftupleg".to_string()
        }
        "rightupleg" | "rightupperleg" | "upperlegr" | "thighr" | "thighright" => {
            "rightupleg".to_string()
        }
        "leftleg" | "leftlowerleg" | "lowerlegl" | "calfl" | "calfleft" => "leftleg".to_string(),
        "rightleg" | "rightlowerleg" | "lowerlegr" | "calfr" | "calfright" => {
            "rightleg".to_string()
        }
        "leftfoot" | "anklel" | "footl" | "footleft" => "leftfoot".to_string(),
        "rightfoot" | "ankler" | "footr" | "footright" => "rightfoot".to_string(),
        "lefttoebase" | "lefttoe" | "toesl" | "balll" | "ballleft" => "lefttoebase".to_string(),
        "righttoebase" | "righttoe" | "toesr" | "ballr" | "ballright" => "righttoebase".to_string(),
        _ => compact,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_all_fbx_animations(
    scene: &ufbx::Scene,
    extra_animation_scenes: &[FbxExtraAnimationScene<'_>],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    include_embedded: bool,
) -> Result<Vec<CookedClip>, Error> {
    let mut clips = Vec::new();
    if include_embedded {
        for (index, stack) in scene.anim_stacks.iter().enumerate() {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let Some(bytes) = cook_fbx_animation_bytes(
                scene,
                &stack.anim,
                parents,
                base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                None,
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = fbx_stack_source_name(stack, None);
            clips.push(CookedClip {
                source_name: raw_name.clone(),
                sanitized_name: sanitize_clip_name(raw_name.as_deref(), index),
                bytes,
                frames,
            });
        }
    }
    let mut clip_index = clips.len();
    for extra in extra_animation_scenes {
        let mapping = fbx_animation_node_mapping(scene, extra.scene);
        validate_fbx_animation_mapping(extra.scene, &mapping)?;
        for stack in &extra.scene.anim_stacks {
            let Some((min_time, max_time)) = fbx_stack_time_range(stack) else {
                continue;
            };
            let Some(bytes) = cook_fbx_animation_bytes(
                extra.scene,
                &stack.anim,
                parents,
                base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                Some(&mapping),
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = fbx_stack_source_name(stack, extra.fallback_name);
            clips.push(CookedClip {
                source_name: raw_name.clone(),
                sanitized_name: sanitize_clip_name(raw_name.as_deref(), clip_index),
                bytes,
                frames,
            });
            clip_index += 1;
        }
    }
    Ok(clips)
}

pub(crate) fn fbx_stack_source_name(
    stack: &ufbx::AnimStack,
    fallback_name: Option<&str>,
) -> Option<String> {
    let stack_name = stack.element.name.trim();
    if !stack_name.is_empty() && stack_name != "Take 001" && stack_name != "mixamo.com" {
        Some(stack_name.to_string())
    } else {
        fallback_name.map(str::to_string)
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_fbx_animation_bytes(
    scene: &ufbx::Scene,
    anim: &ufbx::Anim,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f64,
    max_time: f64,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    node_mapping: Option<&[Option<usize>]>,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f64).round() as usize + 1;
    let frame_count_u16 = ensure_u16("animation frames", frame_count)?;
    let joint_count_u16 = ensure_u16("joints", joints.len())?;
    let mut records = Vec::with_capacity(frame_count * joints.len());

    for frame in 0..frame_count {
        let time = (min_time + frame as f64 / fps as f64).min(max_time);
        let mut frame_trs = if let Some(mapping) = node_mapping {
            evaluate_mapped_fbx_frame_trs(scene, anim, time, parents, base_trs, mapping)
        } else {
            evaluate_fbx_frame_trs(scene, anim, time)
        };
        if normalize_root_translation {
            restore_root_translations(&mut frame_trs, base_trs, root_joint_nodes);
        }
        let locals: Vec<[[f32; 4]; 4]> = frame_trs.iter().map(|trs| trs.matrix()).collect();
        let globals = compute_global_matrices(parents, &locals);
        for (joint_index, node_index) in joints.iter().copied().enumerate() {
            let mut skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                skin = strip_pose_scale(skin);
            }
            records.push(pose_record(&skin, bounds));
        }
    }

    Ok(Some(finish_animation_bytes(
        joint_count_u16,
        frame_count_u16,
        fps,
        &records,
    )?))
}

pub(crate) fn first_fbx_material_base_color(mesh: &ufbx::Mesh) -> [u8; 4] {
    let Some(material) = mesh.materials.as_ref().first().map(AsRef::as_ref) else {
        return [255, 255, 255, 255];
    };
    let map = if material.pbr.base_color.has_value {
        &material.pbr.base_color
    } else {
        &material.fbx.diffuse_color
    };
    if map.has_value {
        [
            linear_to_u8(map.value_vec4.x as f32),
            linear_to_u8(map.value_vec4.y as f32),
            linear_to_u8(map.value_vec4.z as f32),
            linear_to_u8(map.value_vec4.w as f32),
        ]
    } else {
        [255, 255, 255, 255]
    }
}

pub(crate) fn cook_fbx_base_color_texture(
    mesh: &ufbx::Mesh,
    source_path: Option<&Path>,
    fallback_color: [u8; 4],
    cfg: &RigidModelConfig,
) -> Result<Option<Vec<u8>>, Error> {
    let tex_cfg = psxed_tex::Config {
        width: cfg.texture_width,
        height: cfg.texture_height,
        depth: cfg.texture_depth,
        crop: psxed_tex::CropMode::None,
        resampler: psxed_tex::Resampler::Lanczos3,
        transparent_index_zero: true,
    };
    for material in &mesh.materials {
        let texture = material.pbr.base_color.texture.as_deref().or(material
            .fbx
            .diffuse_color
            .texture
            .as_deref());
        let Some(texture) = texture else {
            continue;
        };
        if !texture.content.is_empty() {
            return psxed_tex::convert(&texture.content, &tex_cfg)
                .map(Some)
                .map_err(Error::TextureCook);
        }
        for filename in [
            &texture.absolute_filename,
            &texture.filename,
            &texture.relative_filename,
        ] {
            if filename.is_empty() {
                continue;
            }
            if let Some(path) = resolve_fbx_texture_path(Path::new(filename.as_ref()), source_path)
            {
                if let Ok(bytes) = std::fs::read(path) {
                    return psxed_tex::convert(&bytes, &tex_cfg)
                        .map(Some)
                        .map_err(Error::TextureCook);
                }
            }
        }
    }

    if let Some(path) = source_path.and_then(find_companion_fbx_texture) {
        if let Ok(bytes) = std::fs::read(path) {
            return psxed_tex::convert(&bytes, &tex_cfg)
                .map(Some)
                .map_err(Error::TextureCook);
        }
    }

    // No texture image: a neutral grey checker reads far better than a
    // flat fill, since the model's UVs give it surface detail.
    let _ = fallback_color;
    let bmp = checker_skin_bmp(
        cfg.texture_width.max(1) as u32,
        cfg.texture_height.max(1) as u32,
    );
    psxed_tex::convert(&bmp, &tex_cfg)
        .map(Some)
        .map_err(Error::TextureCook)
}

pub(crate) fn resolve_fbx_texture_path(
    texture_path: &Path,
    source_path: Option<&Path>,
) -> Option<PathBuf> {
    if texture_path.exists() {
        return Some(texture_path.to_path_buf());
    }
    if texture_path.is_absolute() {
        return None;
    }
    let source_dir = source_path.and_then(Path::parent)?;
    let candidate = source_dir.join(texture_path);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

pub(crate) fn find_companion_fbx_texture(source_path: &Path) -> Option<PathBuf> {
    let stem = source_path.file_stem()?.to_str()?;
    let source_dir = source_path.parent()?;
    let mut search_dirs = vec![source_dir.to_path_buf()];
    if let Some(parent) = source_dir.parent() {
        search_dirs.push(parent.join(format!("{stem}_obj")));
        search_dirs.push(parent.join(format!("{stem}_fbx")));
    }
    for dir in search_dirs {
        for ext in ["png", "jpg", "jpeg", "bmp", "tga", "webp"] {
            let candidate = dir.join(format!("{stem}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}
