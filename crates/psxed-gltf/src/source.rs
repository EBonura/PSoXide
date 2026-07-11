//! glTF skinned-mesh source extraction.
//!
//! Reads vertices, joints, the node hierarchy, and global bind matrices out
//! of a glTF document into the shared cook types, plus the precision-bounds
//! accumulation used to size the fixed-point model space.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_precision_bounds(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<PrecisionBounds, Error> {
    let mut bounds = BoundsAccumulator::new();
    include_pose_bounds(
        &mut bounds,
        base_trs,
        source,
        parents,
        joints,
        inverse_bind_matrices,
        strip_animation_scale,
    );

    for animation in document.animations() {
        let channels = read_animation_channels(&animation, buffers)?;
        if channels.is_empty() {
            continue;
        }

        let Some((min_time, max_time)) = channel_time_range(&channels) else {
            continue;
        };

        let duration = max_time - min_time;
        let frame_count = (duration * fps as f32).round() as usize + 1;
        ensure_u16("animation frames", frame_count)?;
        for frame in 0..frame_count {
            let time = (min_time + frame as f32 / fps as f32).min(max_time);
            let mut frame_trs = base_trs.to_vec();
            for channel in &channels {
                channel.apply(time, &mut frame_trs);
            }
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

    for extra in extra_fbx_animation_scenes {
        let mapping = gltf_fbx_animation_node_mapping(document, extra.scene);
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

    for extra in extra_gltf_animation_scenes {
        let mapping = gltf_gltf_animation_node_mapping(document, extra.document);
        validate_gltf_animation_mapping(extra.document, &mapping)?;
        let source_parents = build_parent_indices(extra.document);
        let source_base_trs = collect_base_trs(extra.document);
        let copy_full_local_trs = mapped_local_binds_match(base_trs, &source_base_trs, &mapping);
        for animation in extra.document.animations() {
            let channels = read_animation_channels(&animation, extra.buffers)?;
            if channels.is_empty() {
                continue;
            }
            let Some((min_time, max_time)) = channel_time_range(&channels) else {
                continue;
            };
            let duration = max_time - min_time;
            let frame_count = (duration * fps as f32).round() as usize + 1;
            ensure_u16("animation frames", frame_count)?;
            for frame in 0..frame_count {
                let time = (min_time + frame as f32 / fps as f32).min(max_time);
                let mut frame_trs = evaluate_mapped_gltf_frame_trs(
                    &channels,
                    time,
                    parents,
                    base_trs,
                    &source_parents,
                    &source_base_trs,
                    &mapping,
                    copy_full_local_trs,
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

pub(crate) fn channel_time_range(channels: &[AnimationChannel]) -> Option<(f32, f32)> {
    let mut min_time = f32::INFINITY;
    let mut max_time = f32::NEG_INFINITY;
    for channel in channels {
        for time in &channel.times {
            min_time = min_time.min(*time);
            max_time = max_time.max(*time);
        }
    }
    if min_time.is_finite() && max_time.is_finite() && max_time >= min_time {
        Some((min_time, max_time))
    } else {
        None
    }
}

pub(crate) fn include_pose_bounds(
    bounds: &mut BoundsAccumulator,
    trs: &[Trs],
    source: &SkinnedSourceMesh,
    parents: &[Option<usize>],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    strip_animation_scale: bool,
) {
    let locals: Vec<[[f32; 4]; 4]> = trs.iter().map(|trs| trs.matrix()).collect();
    let globals = compute_global_matrices(parents, &locals);
    let skins: Vec<[[f32; 4]; 4]> = joints
        .iter()
        .copied()
        .enumerate()
        .map(|(joint_index, node_index)| {
            let skin = mul_matrix(&globals[node_index], &inverse_bind_matrices[joint_index]);
            if strip_animation_scale {
                strip_pose_scale(skin)
            } else {
                skin
            }
        })
        .collect();

    for face in &source.faces {
        let skin = skins[face.joint as usize];
        for vertex_index in face.indices {
            bounds.include(transform_point(
                &skin,
                source.vertices[vertex_index].position,
            ));
        }
    }
}

pub(crate) fn choose_local_to_world_q12(local_height: i32, world_height: u16) -> u16 {
    if local_height <= 0 || world_height == 0 {
        return psxed_format::model::DEFAULT_LOCAL_TO_WORLD_Q12;
    }
    let local_height = local_height as u32;
    let numerator = world_height as u32 * 4096;
    ((numerator + local_height / 2) / local_height).clamp(1, u16::MAX as u32) as u16
}

pub(crate) fn read_skinned_mesh(
    mesh: &gltf::Mesh<'_>,
    buffers: &[gltf::buffer::Data],
    joint_count: usize,
) -> Result<SkinnedSourceMesh, Error> {
    let mut source = SkinnedSourceMesh::default();
    let mut normal_faces = Vec::new();
    for primitive in mesh.primitives() {
        let primitive_index = primitive.index();
        if primitive.mode() != Mode::Triangles
            && primitive.mode() != Mode::TriangleStrip
            && primitive.mode() != Mode::TriangleFan
        {
            return Err(Error::UnsupportedMode {
                mode: primitive.mode(),
            });
        }
        let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "POSITION",
            })?
            .collect();
        let normals: Option<Vec<[f32; 3]>> = reader
            .read_normals()
            .map(|iter| iter.map(normalize3).collect());
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "TEXCOORD_0",
            })?
            .into_f32()
            .collect();
        let joints: Vec<[u16; 4]> = reader
            .read_joints(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "JOINTS_0",
            })?
            .into_u16()
            .collect();
        let weights: Vec<[f32; 4]> = reader
            .read_weights(0)
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "WEIGHTS_0",
            })?
            .into_f32()
            .collect();

        let vertex_count = positions.len();
        if normals
            .as_ref()
            .is_some_and(|normals| normals.len() != vertex_count)
            || uvs.len() != vertex_count
            || joints.len() != vertex_count
            || weights.len() != vertex_count
        {
            return Err(Error::BadSkin("primitive attribute counts differ"));
        }
        for (joint_indices, vertex_weights) in joints.iter().zip(&weights) {
            if joint_indices
                .iter()
                .zip(vertex_weights)
                .any(|(joint, weight)| *weight > 0.0 && *joint as usize >= joint_count)
            {
                return Err(Error::BadSkin(
                    "vertex joint index outside skin joint table",
                ));
            }
        }

        let base = source.vertices.len();
        source.vertices.extend((0..vertex_count).map(|index| {
            let cleaned_joints = joint_indices_or_zero(joints[index], weights[index]);
            SourceVertex {
                position: positions[index],
                normal: normals
                    .as_ref()
                    .map(|normals| normals[index])
                    .unwrap_or([0.0, 1.0, 0.0]),
                uv: uvs[index],
                joints: cleaned_joints,
                weights: weights[index],
                dominant_joint: dominant_vertex_joint(cleaned_joints, weights[index]),
            }
        }));

        let local_indices: Vec<u32> = if let Some(indices) = reader.read_indices() {
            indices.into_u32().collect()
        } else {
            (0..vertex_count as u32).collect()
        };
        for face in triangulate_indices(&local_indices, primitive.mode())? {
            let la = checked_index(face[0], vertex_count)?;
            let lb = checked_index(face[1], vertex_count)?;
            let lc = checked_index(face[2], vertex_count)?;
            // glTF fronts are CCW; the cooked order is the flip for
            // the engine's NCLIP convention (see push_imported_face).
            let reversed = normals.as_ref().is_some_and(|normals| {
                winding_opposes_stored_normals(
                    [positions[la], positions[lb], positions[lc]],
                    [normals[la], normals[lb], normals[lc]],
                )
            });
            push_imported_face(
                &mut source,
                &mut normal_faces,
                [la + base, lb + base, lc + base],
                reversed,
            );
        }
    }
    // Normals stay in the source surface convention: only the cooked
    // packet index order is converted for GTE/NCLIP culling.
    rebuild_source_normals(&mut source, &normal_faces);
    Ok(source)
}

pub(crate) fn read_static_gltf_mesh(
    mesh: &gltf::Mesh<'_>,
    transform: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    source: &mut SkinnedSourceMesh,
    normal_faces: &mut Vec<[usize; 3]>,
) -> Result<(), Error> {
    for primitive in mesh.primitives() {
        let primitive_index = primitive.index();
        if primitive.mode() != Mode::Triangles
            && primitive.mode() != Mode::TriangleStrip
            && primitive.mode() != Mode::TriangleFan
        {
            return Err(Error::UnsupportedMode {
                mode: primitive.mode(),
            });
        }
        let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let positions_raw: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or(Error::MissingAttribute {
                primitive_index,
                attribute: "POSITION",
            })?
            .collect();
        let positions: Vec<[f32; 3]> = positions_raw
            .iter()
            .map(|position| transform_point(transform, *position))
            .collect();
        // Authored normals (raw mesh space, matching `positions_raw`)
        // drive per-face winding orientation below.
        let normals: Option<Vec<[f32; 3]>> = reader
            .read_normals()
            .map(|iter| iter.map(normalize3).collect());
        let mirrored = transform_reverses_winding(transform);
        let vertex_count = positions.len();
        if normals
            .as_ref()
            .is_some_and(|normals| normals.len() != vertex_count)
        {
            return Err(Error::BadSkin("primitive attribute counts differ"));
        }
        let uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .map(|iter| iter.into_f32().collect())
            .unwrap_or_else(|| vec![[0.0, 0.0]; vertex_count]);
        if uvs.len() != vertex_count {
            return Err(Error::BadSkin("primitive attribute counts differ"));
        }

        let base = source.vertices.len();
        source
            .vertices
            .extend((0..vertex_count).map(|index| SourceVertex {
                position: positions[index],
                normal: [0.0, 1.0, 0.0],
                uv: uvs[index],
                joints: [0, 0, 0, 0],
                weights: [1.0, 0.0, 0.0, 0.0],
                dominant_joint: 0,
            }));

        let local_indices: Vec<u32> = if let Some(indices) = reader.read_indices() {
            indices.into_u32().collect()
        } else {
            (0..vertex_count as u32).collect()
        };
        for face in triangulate_indices(&local_indices, primitive.mode())? {
            let la = checked_index(face[0], vertex_count)?;
            let lb = checked_index(face[1], vertex_count)?;
            let lc = checked_index(face[2], vertex_count)?;
            // Test in raw mesh space (normals are raw); a mirroring
            // node transform inverts the winding once more.
            let opposes = normals.as_ref().is_some_and(|normals| {
                winding_opposes_stored_normals(
                    [positions_raw[la], positions_raw[lb], positions_raw[lc]],
                    [normals[la], normals[lb], normals[lc]],
                )
            });
            push_imported_face(
                source,
                normal_faces,
                [la + base, lb + base, lc + base],
                opposes != mirrored,
            );
        }
    }
    Ok(())
}

/// Push one imported triangle, choosing the cooked winding for the
/// engine's single-sided render path.
///
/// `indices` is the source-order (CCW front) triangle; the cooked
/// order is its flip, matching the GTE/NCLIP convention. `reversed`
/// marks triangles whose source winding is itself backwards (judged
/// against authored normals and/or a mirroring node transform):
/// double-sided source materials frequently mix windings and rely on
/// the renderer drawing both sides, which the engine does not, so
/// such faces re-orient here instead of vanishing in the scene.
/// `normal_faces` always receives the outward (front) order so
/// rebuilt shading normals stay consistent with the corrected faces.
pub(crate) fn push_imported_face(
    source: &mut SkinnedSourceMesh,
    normal_faces: &mut Vec<[usize; 3]>,
    indices: [usize; 3],
    reversed: bool,
) {
    let front = if reversed {
        [indices[0], indices[2], indices[1]]
    } else {
        indices
    };
    normal_faces.push(front);
    source.faces.push(SourceFace {
        indices: [front[0], front[2], front[1]],
        joint: 0,
    });
}

/// `true` when a triangle's geometric winding normal opposes its
/// authored vertex normals. Both inputs must be in the same
/// coordinate space.
pub(crate) fn winding_opposes_stored_normals(
    positions: [[f32; 3]; 3],
    normals: [[f32; 3]; 3],
) -> bool {
    let geometric = cross3(
        sub3(positions[1], positions[0]),
        sub3(positions[2], positions[0]),
    );
    if length_sq3(geometric) <= 0.000001 {
        return false;
    }
    let stored = [
        normals[0][0] + normals[1][0] + normals[2][0],
        normals[0][1] + normals[1][1] + normals[2][1],
        normals[0][2] + normals[1][2] + normals[2][2],
    ];
    geometric[0] * stored[0] + geometric[1] * stored[1] + geometric[2] * stored[2] < 0.0
}

/// `true` when a node transform mirrors geometry (negative
/// determinant), which inverts triangle winding.
pub(crate) fn transform_reverses_winding(transform: &[[f32; 4]; 4]) -> bool {
    let m = transform;
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    det < 0.0
}

pub(crate) fn rebuild_source_normals(source: &mut SkinnedSourceMesh, normal_faces: &[[usize; 3]]) {
    let mut normals = vec![[0.0f32; 3]; source.vertices.len()];
    for face in normal_faces {
        let a = source.vertices[face[0]].position;
        let b = source.vertices[face[1]].position;
        let c = source.vertices[face[2]].position;
        let normal = cross3(sub3(b, a), sub3(c, a));
        if length_sq3(normal) <= 0.000001 {
            continue;
        }
        for index in *face {
            normals[index][0] += normal[0];
            normals[index][1] += normal[1];
            normals[index][2] += normal[2];
        }
    }

    for (vertex, normal) in source.vertices.iter_mut().zip(normals) {
        if length_sq3(normal) > 0.000001 {
            vertex.normal = normalize3(normal);
        } else {
            vertex.normal = normalize3(vertex.normal);
        }
    }
}

pub(crate) fn collect_static_precision_bounds(
    source: &SkinnedSourceMesh,
) -> Result<PrecisionBounds, Error> {
    let mut bounds = BoundsAccumulator::new();
    for face in &source.faces {
        for vertex_index in face.indices {
            bounds.include(source.vertices[vertex_index].position);
        }
    }
    bounds.finish()
}

pub(crate) fn prune_detached_face_islands(
    source: &mut SkinnedSourceMesh,
    bounds: &ModelBounds,
    max_faces: usize,
) -> usize {
    if max_faces == 0 || source.faces.len() <= 1 {
        return 0;
    }

    let mut faces_by_position: BTreeMap<[i16; 3], Vec<usize>> = BTreeMap::new();
    for (face_index, face) in source.faces.iter().enumerate() {
        for vertex_index in face.indices {
            let key = model_vertex_position_key_for_source(source.vertices[vertex_index], bounds);
            let faces = faces_by_position.entry(key).or_default();
            if faces.last().copied() != Some(face_index) {
                faces.push(face_index);
            }
        }
    }

    let mut adjacency = vec![Vec::new(); source.faces.len()];
    for faces in faces_by_position.values() {
        for (offset, face_a) in faces.iter().copied().enumerate() {
            for face_b in faces.iter().copied().skip(offset + 1) {
                adjacency[face_a].push(face_b);
                adjacency[face_b].push(face_a);
            }
        }
    }

    let mut components: Vec<Vec<usize>> = Vec::new();
    let mut seen = vec![false; source.faces.len()];
    for start in 0..source.faces.len() {
        if seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        seen[start] = true;
        while let Some(face_index) = stack.pop() {
            component.push(face_index);
            for next in adjacency[face_index].iter().copied() {
                if !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        components.push(component);
    }

    if components.len() <= 1 {
        return 0;
    }
    let Some((largest_index, largest)) = components
        .iter()
        .enumerate()
        .max_by_key(|(_, component)| component.len())
    else {
        return 0;
    };
    if largest.len() <= max_faces {
        return 0;
    }

    let mut keep = vec![true; source.faces.len()];
    let mut removed = 0usize;
    for (component_index, component) in components.iter().enumerate() {
        if component_index == largest_index || component.len() > max_faces {
            continue;
        }
        for face_index in component {
            keep[*face_index] = false;
            removed += 1;
        }
    }
    if removed == 0 {
        return 0;
    }

    source.faces = source
        .faces
        .iter()
        .copied()
        .enumerate()
        .filter_map(|(index, face)| keep[index].then_some(face))
        .collect();
    removed
}

/// Collapse every vertex to a pure single-bone bind: keep only the
/// dominant joint at full weight and drop all secondary influences.
///
/// The cooked PSMD format already stores one dominant joint per vertex
/// plus an optional secondary blend bone (the CPU-blend path). Forcing
/// single bind here removes the secondary entirely, so the cooked model
/// stays on the GTE single-bone fast path with no `BLEND_SKIN` verts --
/// the ideal rigid representation for PS1 segmented characters.
pub(crate) fn collapse_to_single_bind(source: &mut SkinnedSourceMesh) {
    for vertex in &mut source.vertices {
        let dominant = vertex.dominant_joint;
        vertex.joints = [dominant, 0, 0, 0];
        vertex.weights = [1.0, 0.0, 0.0, 0.0];
    }
}

pub(crate) fn joint_indices_or_zero(joints: [u16; 4], weights: [f32; 4]) -> [u16; 4] {
    let mut out = joints;
    for i in 0..4 {
        if weights[i] <= 0.0 {
            out[i] = 0;
        }
    }
    out
}

pub(crate) fn dominant_vertex_joint(joints: [u16; 4], weights: [f32; 4]) -> u16 {
    let mut best = 0u16;
    let mut best_weight = f32::NEG_INFINITY;
    for influence in 0..4 {
        let weight = weights[influence];
        if weight > best_weight {
            best_weight = weight;
            best = joints[influence];
        }
    }
    best
}

/// Group faces by per-vertex bone choice. Each vertex has already
/// picked its dominant bone in `read_skinned_mesh`. A face is bound
/// to whichever bone owns the **majority** of its three corners.
///
/// On a 2-1 split the majority vertex pair wins, leaving the third
/// corner as the only "pulled" vertex bound to a foreign bone -- much
/// less visible than the previous face-level binding, which could
/// pull all three corners together. On a 3-way disagreement we fall
/// back to summed-weight scoring so the choice still reflects the
/// face's overall bias.
pub(crate) fn assign_face_joints(source: &mut SkinnedSourceMesh, joint_count: usize) {
    let mut scores = vec![0.0f32; joint_count];
    for face in &mut source.faces {
        let bones = [
            source.vertices[face.indices[0]].dominant_joint,
            source.vertices[face.indices[1]].dominant_joint,
            source.vertices[face.indices[2]].dominant_joint,
        ];

        if bones[0] == bones[1] || bones[0] == bones[2] {
            face.joint = bones[0];
            continue;
        }
        if bones[1] == bones[2] {
            face.joint = bones[1];
            continue;
        }

        scores.fill(0.0);
        for vertex_index in face.indices {
            let vertex = source.vertices[vertex_index];
            for influence in 0..4 {
                let weight = vertex.weights[influence].max(0.0);
                if weight > 0.0 {
                    let joint = vertex.joints[influence] as usize;
                    if joint < scores.len() {
                        scores[joint] += weight;
                    }
                }
            }
        }
        let mut best_joint = 0usize;
        let mut best_score = f32::NEG_INFINITY;
        for (joint, score) in scores.iter().copied().enumerate() {
            if score > best_score {
                best_joint = joint;
                best_score = score;
            }
        }
        face.joint = best_joint as u16;
    }
}

pub(crate) fn read_inverse_bind_matrices(
    skin: &gltf::Skin<'_>,
    buffers: &[gltf::buffer::Data],
    joint_count: usize,
) -> Vec<[[f32; 4]; 4]> {
    let reader = skin.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
    reader
        .read_inverse_bind_matrices()
        .map(|iter| iter.collect())
        .unwrap_or_else(|| vec![identity_matrix(); joint_count])
}

pub(crate) fn build_parent_indices(document: &gltf::Document) -> Vec<Option<usize>> {
    let mut parents = vec![None; document.nodes().count()];
    for node in document.nodes() {
        for child in node.children() {
            parents[child.index()] = Some(node.index());
        }
    }
    parents
}

pub(crate) fn root_joint_nodes(joints: &[usize], parents: &[Option<usize>]) -> Vec<usize> {
    let mut is_joint = vec![false; parents.len()];
    for node in joints.iter().copied() {
        if let Some(slot) = is_joint.get_mut(node) {
            *slot = true;
        }
    }
    joints
        .iter()
        .copied()
        .filter(|node| {
            parents
                .get(*node)
                .copied()
                .flatten()
                .and_then(|parent| is_joint.get(parent).copied())
                != Some(true)
        })
        .collect()
}

pub(crate) fn collect_base_trs(document: &gltf::Document) -> Vec<Trs> {
    document
        .nodes()
        .map(|node| {
            let (translation, rotation, scale) = node.transform().decomposed();
            Trs {
                translation,
                rotation,
                scale,
            }
        })
        .collect()
}

pub(crate) fn compute_global_matrices(
    parents: &[Option<usize>],
    locals: &[[[f32; 4]; 4]],
) -> Vec<[[f32; 4]; 4]> {
    let mut globals = vec![identity_matrix(); locals.len()];
    let mut done = vec![false; locals.len()];
    for index in 0..locals.len() {
        compute_global_matrix(index, parents, locals, &mut globals, &mut done);
    }
    globals
}

pub(crate) fn compute_global_matrix(
    index: usize,
    parents: &[Option<usize>],
    locals: &[[[f32; 4]; 4]],
    globals: &mut [[[f32; 4]; 4]],
    done: &mut [bool],
) -> [[f32; 4]; 4] {
    if done[index] {
        return globals[index];
    }
    let global = if let Some(parent) = parents[index] {
        let parent_global = compute_global_matrix(parent, parents, locals, globals, done);
        mul_matrix(&parent_global, &locals[index])
    } else {
        locals[index]
    };
    globals[index] = global;
    done[index] = true;
    global
}
