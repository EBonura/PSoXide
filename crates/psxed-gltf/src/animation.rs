//! glTF animation sampling and PSXM animation-clip cooking.

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_all_animations(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    extra_fbx_animation_scenes: &[FbxExtraAnimationScene<'_>],
    extra_gltf_animation_scenes: &[GltfExtraAnimationScene<'_>],
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<Vec<CookedClip>, Error> {
    let mut clips = Vec::new();
    for (index, animation) in document.animations().enumerate() {
        let channels = read_animation_channels(&animation, buffers)?;
        if channels.is_empty() {
            continue;
        }
        let Some((min_time, max_time)) = channel_time_range(&channels) else {
            continue;
        };
        let Some(bytes) = cook_animation_bytes(
            &channels,
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
        )?
        else {
            continue;
        };
        let frames = animation_frame_count_from_bytes(&bytes);
        let raw_name = animation.name().map(|s| s.to_string());
        clips.push(CookedClip {
            source_name: raw_name.clone(),
            sanitized_name: sanitize_clip_name(raw_name.as_deref(), index),
            bytes,
            frames,
        });
    }
    let mut clip_index = clips.len();
    for extra in extra_fbx_animation_scenes {
        let mapping = gltf_fbx_animation_node_mapping(document, extra.scene);
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
            let Some(bytes) = cook_mapped_gltf_animation_bytes(
                &channels,
                parents,
                base_trs,
                &source_parents,
                &source_base_trs,
                root_joint_nodes,
                joints,
                inverse_bind_matrices,
                bounds,
                min_time,
                max_time,
                fps,
                normalize_root_translation,
                strip_animation_scale,
                &mapping,
                copy_full_local_trs,
            )?
            else {
                continue;
            };
            let frames = animation_frame_count_from_bytes(&bytes);
            let raw_name = gltf_animation_source_name(&animation, extra.fallback_name);
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

pub(crate) fn bind_pose_clip(
    joint_count: usize,
    bounds: &ModelBounds,
    fps: u16,
) -> Result<CookedClip, Error> {
    let joint_count_u16 = ensure_u16("joints", joint_count)?;
    let records = vec![pose_record(&identity_matrix(), bounds); joint_count];
    Ok(CookedClip {
        source_name: Some("Bind Pose".to_string()),
        sanitized_name: "bind_pose".to_string(),
        bytes: finish_animation_bytes(joint_count_u16, 1, fps, &records)?,
        frames: 1,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_animation_bytes(
    channels: &[AnimationChannel],
    parents: &[Option<usize>],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f32,
    max_time: f32,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f32).round() as usize + 1;
    let frame_count_u16 = ensure_u16("animation frames", frame_count)?;
    let joint_count_u16 = ensure_u16("joints", joints.len())?;
    let mut records = Vec::with_capacity(frame_count * joints.len());

    for frame in 0..frame_count {
        let time = (min_time + frame as f32 / fps as f32).min(max_time);
        let mut frame_trs = base_trs.to_vec();
        for channel in channels {
            channel.apply(time, &mut frame_trs);
        }
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn cook_mapped_gltf_animation_bytes(
    channels: &[AnimationChannel],
    target_parents: &[Option<usize>],
    target_base_trs: &[Trs],
    source_parents: &[Option<usize>],
    source_base_trs: &[Trs],
    root_joint_nodes: &[usize],
    joints: &[usize],
    inverse_bind_matrices: &[[[f32; 4]; 4]],
    bounds: &ModelBounds,
    min_time: f32,
    max_time: f32,
    fps: u16,
    normalize_root_translation: bool,
    strip_animation_scale: bool,
    mapping: &[Option<usize>],
    copy_full_local_trs: bool,
) -> Result<Option<Vec<u8>>, Error> {
    let duration = max_time - min_time;
    let frame_count = (duration * fps as f32).round() as usize + 1;
    let frame_count_u16 = ensure_u16("animation frames", frame_count)?;
    let joint_count_u16 = ensure_u16("joints", joints.len())?;
    let mut records = Vec::with_capacity(frame_count * joints.len());

    for frame in 0..frame_count {
        let time = (min_time + frame as f32 / fps as f32).min(max_time);
        let mut frame_trs = evaluate_mapped_gltf_frame_trs(
            channels,
            time,
            target_parents,
            target_base_trs,
            source_parents,
            source_base_trs,
            mapping,
            copy_full_local_trs,
        );
        if normalize_root_translation {
            restore_root_translations(&mut frame_trs, target_base_trs, root_joint_nodes);
        }
        let locals: Vec<[[f32; 4]; 4]> = frame_trs.iter().map(|trs| trs.matrix()).collect();
        let globals = compute_global_matrices(target_parents, &locals);
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

pub(crate) fn gltf_animation_source_name(
    animation: &gltf::Animation<'_>,
    fallback_name: Option<&str>,
) -> Option<String> {
    animation
        .name()
        .filter(|name| !name.trim().is_empty())
        .map(str::to_string)
        .or_else(|| fallback_name.map(str::to_string))
}

pub(crate) fn sanitize_clip_name(source: Option<&str>, fallback_index: usize) -> String {
    let raw = source.unwrap_or("");
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        format!("clip{fallback_index}")
    } else {
        trimmed
    }
}

#[derive(Clone, Debug)]
pub(crate) struct AnimationChannel {
    pub(crate) node_index: usize,
    pub(crate) interpolation: Interpolation,
    pub(crate) times: Vec<f32>,
    pub(crate) values: ChannelValues,
}

#[derive(Clone, Debug)]
pub(crate) enum ChannelValues {
    Translation(Vec<[f32; 3]>),
    Rotation(Vec<[f32; 4]>),
    Scale(Vec<[f32; 3]>),
}

impl AnimationChannel {
    pub(crate) fn apply(&self, time: f32, trs: &mut [Trs]) {
        let Some(target) = trs.get_mut(self.node_index) else {
            return;
        };
        match &self.values {
            ChannelValues::Translation(values) => {
                target.translation = sample_vec3(&self.times, values, time, self.interpolation);
            }
            ChannelValues::Rotation(values) => {
                target.rotation = sample_quat(&self.times, values, time, self.interpolation);
            }
            ChannelValues::Scale(values) => {
                target.scale = sample_vec3(&self.times, values, time, self.interpolation);
            }
        }
    }
}

pub(crate) fn restore_root_translations(
    frame_trs: &mut [Trs],
    base_trs: &[Trs],
    root_joint_nodes: &[usize],
) {
    for node_index in root_joint_nodes.iter().copied() {
        let Some(frame) = frame_trs.get_mut(node_index) else {
            continue;
        };
        let Some(base) = base_trs.get(node_index) else {
            continue;
        };
        frame.translation = base.translation;
    }
}

pub(crate) fn read_animation_channels(
    animation: &gltf::Animation<'_>,
    buffers: &[gltf::buffer::Data],
) -> Result<Vec<AnimationChannel>, Error> {
    let mut channels = Vec::new();
    for channel in animation.channels() {
        let channel_index = channel.index();
        let interpolation = channel.sampler().interpolation();
        if interpolation == Interpolation::CubicSpline {
            return Err(Error::UnsupportedAnimationInterpolation {
                channel_index,
                interpolation,
            });
        }
        let reader = channel.reader(|buffer| Some(buffers[buffer.index()].0.as_slice()));
        let times: Vec<f32> = reader
            .read_inputs()
            .ok_or(Error::MissingAnimationInputs { channel_index })?
            .collect();
        let outputs = reader
            .read_outputs()
            .ok_or(Error::MissingAnimationOutputs { channel_index })?;
        let values = match (channel.target().property(), outputs) {
            (Property::Translation, gltf::animation::util::ReadOutputs::Translations(values)) => {
                ChannelValues::Translation(values.collect())
            }
            (Property::Rotation, gltf::animation::util::ReadOutputs::Rotations(values)) => {
                ChannelValues::Rotation(values.into_f32().collect())
            }
            (Property::Scale, gltf::animation::util::ReadOutputs::Scales(values)) => {
                ChannelValues::Scale(values.collect())
            }
            (Property::MorphTargetWeights, _) => continue,
            _ => return Err(Error::AnimationTypeMismatch { channel_index }),
        };
        channels.push(AnimationChannel {
            node_index: channel.target().node().index(),
            interpolation,
            times,
            values,
        });
    }
    Ok(channels)
}

pub(crate) fn sample_vec3(
    times: &[f32],
    values: &[[f32; 3]],
    time: f32,
    interpolation: Interpolation,
) -> [f32; 3] {
    let (a, b, t) = sample_segment(times, time);
    if interpolation == Interpolation::Step || a == b {
        return values[a];
    }
    lerp3(values[a], values[b], t)
}

pub(crate) fn sample_quat(
    times: &[f32],
    values: &[[f32; 4]],
    time: f32,
    interpolation: Interpolation,
) -> [f32; 4] {
    let (a, b, t) = sample_segment(times, time);
    if interpolation == Interpolation::Step || a == b {
        return normalize4(values[a]);
    }
    nlerp_quat(values[a], values[b], t)
}

pub(crate) fn sample_segment(times: &[f32], time: f32) -> (usize, usize, f32) {
    if times.len() <= 1 || time <= times[0] {
        return (0, 0, 0.0);
    }
    let last = times.len() - 1;
    if time >= times[last] {
        return (last, last, 0.0);
    }
    for index in 0..last {
        let t0 = times[index];
        let t1 = times[index + 1];
        if time >= t0 && time <= t1 {
            let span = (t1 - t0).max(0.000001);
            return (index, index + 1, ((time - t0) / span).clamp(0.0, 1.0));
        }
    }
    (last, last, 0.0)
}

pub(crate) fn compose_trs(
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
) -> [[f32; 4]; 4] {
    let [x, y, z, w] = normalize4(rotation);
    let xx = x * x;
    let yy = y * y;
    let zz = z * z;
    let xy = x * y;
    let xz = x * z;
    let yz = y * z;
    let wx = w * x;
    let wy = w * y;
    let wz = w * z;

    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r02 = 2.0 * (xz + wy);
    let r10 = 2.0 * (xy + wz);
    let r11 = 1.0 - 2.0 * (xx + zz);
    let r12 = 2.0 * (yz - wx);
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);

    [
        [r00 * scale[0], r10 * scale[0], r20 * scale[0], 0.0],
        [r01 * scale[1], r11 * scale[1], r21 * scale[1], 0.0],
        [r02 * scale[2], r12 * scale[2], r22 * scale[2], 0.0],
        [translation[0], translation[1], translation[2], 1.0],
    ]
}

pub(crate) fn strip_pose_scale(mut matrix: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    for column in matrix.iter_mut().take(3) {
        let len_sq = column[0] * column[0] + column[1] * column[1] + column[2] * column[2];
        if len_sq > 0.000001 {
            let inv_len = len_sq.sqrt().recip();
            column[0] *= inv_len;
            column[1] *= inv_len;
            column[2] *= inv_len;
        }
    }
    matrix
}

#[derive(Clone, Debug)]
pub(crate) struct PoseRecord {
    matrix: [i16; 9],
    translation: [i32; 3],
}

pub(crate) fn finish_animation_bytes(
    joint_count: u16,
    frame_count: u16,
    fps: u16,
    records: &[PoseRecord],
) -> Result<Vec<u8>, Error> {
    // Q11 rotation codes cover [-4096, 4096] (Q12 [-1, 1]): a rigid
    // pose always fits, but a clip cooked with animated scale kept can
    // exceed it. Such clips fall back to the 24-byte v2 record rather
    // than clamping their scale away.
    // Q12 one (4096, every identity diagonal) is exactly representable
    // via the reserved code; anything beyond it is animated scale.
    let fits_v3 = records
        .iter()
        .all(|record| record.matrix.iter().all(|&v| v.abs() <= 4096));
    let fits_v4 = fits_v3
        && records.iter().all(|record| {
            let mut block = [0u8; psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V4];
            psxed_format::animation::encode_rotation_q11_cross(&record.matrix, &mut block)
        });
    let (version, record_size) = if fits_v4 {
        (
            psxed_format::animation::VERSION_V4,
            psxed_format::animation::POSE_RECORD_SIZE_V4,
        )
    } else if fits_v3 {
        (
            psxed_format::animation::VERSION_V3,
            psxed_format::animation::POSE_RECORD_SIZE_V3,
        )
    } else {
        (
            psxed_format::animation::VERSION,
            psxed_format::animation::POSE_RECORD_SIZE,
        )
    };
    let payload_len = psxed_format::animation::AnimationHeader::SIZE + records.len() * record_size;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    let translation_shift = animation_translation_shift(records);
    append_asset_header(
        &mut out,
        psxed_format::animation::MAGIC,
        version,
        0,
        payload_len,
    )?;
    append_u16(&mut out, joint_count);
    append_u16(&mut out, frame_count);
    append_u16(&mut out, fps);
    append_u16(&mut out, translation_shift);

    for record in records {
        if fits_v4 {
            append_pose_record_v4(&mut out, record, translation_shift as u8);
        } else if fits_v3 {
            append_pose_record(&mut out, record, translation_shift as u8);
        } else {
            append_pose_record_v2(&mut out, record, translation_shift as u8);
        }
    }

    Ok(out)
}

pub(crate) fn append_pose_record_v4(out: &mut Vec<u8>, record: &PoseRecord, translation_shift: u8) {
    let mut rotation = [0u8; psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V4];
    let encoded = psxed_format::animation::encode_rotation_q11_cross(&record.matrix, &mut rotation);
    debug_assert!(encoded, "v4 preflight accepted every record");
    out.extend_from_slice(&rotation);
    for value in record.translation {
        append_i16(out, quantize_translation_i16(value, translation_shift));
    }
}

pub(crate) fn pose_record(skin_matrix: &[[f32; 4]; 4], bounds: &ModelBounds) -> PoseRecord {
    let mut matrix = [0i16; 9];
    let mut index = 0;
    for column in skin_matrix.iter().take(3) {
        for value in column.iter().take(3) {
            matrix[index] = q12_i16(*value);
            index += 1;
        }
    }
    let center_in_pose = transform_point(skin_matrix, bounds.center);
    PoseRecord {
        matrix,
        translation: [
            q12_i32((center_in_pose[0] - bounds.center[0]) / bounds.extent),
            q12_i32((center_in_pose[1] - bounds.center[1]) / bounds.extent),
            q12_i32((center_in_pose[2] - bounds.center[2]) / bounds.extent),
        ],
    }
}

pub(crate) fn append_pose_record_v2(out: &mut Vec<u8>, record: &PoseRecord, translation_shift: u8) {
    for value in record.matrix {
        append_i16(out, value);
    }
    for value in record.translation {
        append_i16(out, quantize_translation_i16(value, translation_shift));
    }
}

pub(crate) fn append_pose_record(out: &mut Vec<u8>, record: &PoseRecord, translation_shift: u8) {
    // v3: nine Q3.12 rotation elements as packed 12-bit Q11 codes
    // (hl-psx's on-silicon encoding), then the shifted translations.
    let mut rotation = [0u8; psxed_format::animation::POSE_ROTATION_BLOCK_SIZE_V3];
    psxed_format::animation::encode_rotation_q11(&record.matrix, &mut rotation);
    out.extend_from_slice(&rotation);
    for value in record.translation {
        append_i16(out, quantize_translation_i16(value, translation_shift));
    }
}

pub(crate) fn animation_translation_shift(records: &[PoseRecord]) -> u16 {
    let mut max_abs = 0i32;
    for record in records {
        for value in record.translation {
            max_abs = max_abs.max(abs_i32_saturating(value));
        }
    }

    let mut shift = 0u16;
    while max_abs > i16::MAX as i32 && shift < 15 {
        max_abs = (max_abs + 1) >> 1;
        shift += 1;
    }
    shift
}

pub(crate) fn quantize_translation_i16(value: i32, shift: u8) -> i16 {
    let shifted = round_shift_i32(value, shift);
    shifted.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

pub(crate) fn round_shift_i32(value: i32, shift: u8) -> i32 {
    if shift == 0 {
        return value;
    }
    let value = value as i64;
    let half = 1i64 << (shift - 1);
    if value >= 0 {
        ((value + half) >> shift) as i32
    } else {
        -(((-value + half) >> shift) as i32)
    }
}

pub(crate) fn abs_i32_saturating(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else if value < 0 {
        -value
    } else {
        value
    }
}

pub(crate) fn animation_frame_count_from_bytes(bytes: &[u8]) -> usize {
    if bytes.len()
        < psxed_format::AssetHeader::SIZE + psxed_format::animation::AnimationHeader::SIZE
    {
        return 0;
    }
    u16::from_le_bytes([bytes[14], bytes[15]]) as usize
}
