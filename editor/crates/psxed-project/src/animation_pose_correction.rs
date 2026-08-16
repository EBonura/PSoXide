use psx_asset::{Animation, JointPose, Model};
use psx_engine::{Angle, Mat3I16};

use crate::AnimationPoseCorrectionKey;

/// Fold sparse authoring corrections into a normal cooked animation blob.
///
/// This is deliberately host-side. The returned bytes use the existing PSXA
/// format and need no runtime correction tables or per-joint searches.
pub fn bake_animation_pose_corrections(
    model: &Model<'_>,
    animation: &Animation<'_>,
    keys: &[AnimationPoseCorrectionKey],
) -> Vec<u8> {
    let pivots = model_joint_centroids(model);
    let joint_count = animation.joint_count();
    let frame_count = animation.frame_count();
    let mut poses = Vec::with_capacity(frame_count as usize * joint_count as usize);
    let mut max_translation = 0i32;

    for frame in 0..frame_count {
        for joint in 0..joint_count {
            let pose = animation
                .pose(frame, joint)
                .expect("validated animation frame and joint indices");
            let correction = sample_pose_correction(keys, joint, frame);
            let corrected = apply_pose_correction(
                pose,
                pivots.get(joint as usize).copied().unwrap_or([0; 3]),
                correction,
            );
            max_translation = max_translation
                .max(abs_i32_saturating(corrected.translation.x))
                .max(abs_i32_saturating(corrected.translation.y))
                .max(abs_i32_saturating(corrected.translation.z));
            poses.push(corrected);
        }
    }

    encode_animation(animation, &poses, translation_shift(max_translation))
}

/// Interpolate one joint's sparse correction keys at a sampled frame.
/// Endpoint values hold before the first and after the last key.
pub fn sample_pose_correction(
    keys: &[AnimationPoseCorrectionKey],
    joint: u16,
    frame: u16,
) -> AnimationPoseCorrectionKey {
    let mut previous = None;
    let mut next = None;
    for key in keys.iter().copied().filter(|key| key.joint == joint) {
        if key.frame <= frame
            && previous.is_none_or(|current: AnimationPoseCorrectionKey| key.frame > current.frame)
        {
            previous = Some(key);
        }
        if key.frame >= frame
            && next.is_none_or(|current: AnimationPoseCorrectionKey| key.frame < current.frame)
        {
            next = Some(key);
        }
    }

    match (previous, next) {
        (None, None) => AnimationPoseCorrectionKey {
            frame,
            joint,
            ..Default::default()
        },
        (Some(key), None) | (None, Some(key)) => AnimationPoseCorrectionKey { frame, ..key },
        (Some(a), Some(b)) if a.frame == b.frame => AnimationPoseCorrectionKey { frame, ..a },
        (Some(a), Some(b)) => {
            let numerator = i32::from(frame.saturating_sub(a.frame));
            let denominator = i32::from(b.frame - a.frame).max(1);
            let mut out = AnimationPoseCorrectionKey {
                frame,
                joint,
                ..Default::default()
            };
            for axis in 0..3 {
                out.rotation_q12[axis] = lerp_i32(
                    i32::from(a.rotation_q12[axis]),
                    i32::from(b.rotation_q12[axis]),
                    numerator,
                    denominator,
                )
                .clamp(i16::MIN as i32, i16::MAX as i32)
                    as i16;
                out.translation[axis] = lerp_i32(
                    a.translation[axis],
                    b.translation[axis],
                    numerator,
                    denominator,
                );
            }
            out
        }
    }
}

fn apply_pose_correction(
    pose: JointPose,
    pivot: [i32; 3],
    correction: AnimationPoseCorrectionKey,
) -> JointPose {
    if correction.is_identity() {
        return pose;
    }

    let pose_matrix = Mat3I16 {
        m: transpose_pose_matrix(pose.matrix),
    };
    let delta = euler_q12_rotation(correction.rotation_q12);
    let corrected_matrix = delta.mul(&pose_matrix);
    let mapped_pivot = transform_q12(&pose_matrix, pivot);
    let rotated_pivot = transform_q12(&delta, mapped_pivot);

    let mut translation = pose.translation;
    translation.x = translation
        .x
        .saturating_add(mapped_pivot[0].saturating_sub(rotated_pivot[0]))
        .saturating_add(correction.translation[0]);
    translation.y = translation
        .y
        .saturating_add(mapped_pivot[1].saturating_sub(rotated_pivot[1]))
        .saturating_add(correction.translation[1]);
    translation.z = translation
        .z
        .saturating_add(mapped_pivot[2].saturating_sub(rotated_pivot[2]))
        .saturating_add(correction.translation[2]);
    JointPose {
        matrix: transpose_pose_matrix(corrected_matrix.m),
        translation,
    }
}

fn model_joint_centroids(model: &Model<'_>) -> Vec<[i32; 3]> {
    let mut sums = vec![[0i64; 3]; model.joint_count() as usize];
    let mut counts = vec![0i64; model.joint_count() as usize];
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else {
            continue;
        };
        let joint = part.joint_index() as usize;
        for offset in 0..part.vertex_count() {
            let Some(vertex) = model.vertex(part.first_vertex().saturating_add(offset)) else {
                continue;
            };
            sums[joint][0] += i64::from(vertex.position.x);
            sums[joint][1] += i64::from(vertex.position.y);
            sums[joint][2] += i64::from(vertex.position.z);
            counts[joint] += 1;
        }
    }
    sums.into_iter()
        .zip(counts)
        .map(|(sum, count)| {
            if count == 0 {
                [0; 3]
            } else {
                [
                    (sum[0] / count) as i32,
                    (sum[1] / count) as i32,
                    (sum[2] / count) as i32,
                ]
            }
        })
        .collect()
}

fn encode_animation(animation: &Animation<'_>, poses: &[JointPose], shift: u16) -> Vec<u8> {
    let pose_count = animation.frame_count() as usize * animation.joint_count() as usize;
    debug_assert_eq!(poses.len(), pose_count);
    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + pose_count * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    out.extend_from_slice(&psxed_format::animation::MAGIC);
    out.extend_from_slice(&psxed_format::animation::VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.extend_from_slice(&animation.joint_count().to_le_bytes());
    out.extend_from_slice(&animation.frame_count().to_le_bytes());
    out.extend_from_slice(&animation.sample_rate_hz().to_le_bytes());
    out.extend_from_slice(&shift.to_le_bytes());
    for pose in poses {
        for column in pose.matrix {
            for value in column {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        for value in [pose.translation.x, pose.translation.y, pose.translation.z] {
            out.extend_from_slice(&quantize_translation(value, shift as u8).to_le_bytes());
        }
    }
    out
}

fn euler_q12_rotation(rotation_q12: [i16; 3]) -> Mat3I16 {
    let rx = Mat3I16::rotate_x(Angle::from_q12(rotation_q12[0] as u16).rotate_y_arg());
    let ry = Mat3I16::rotate_y(Angle::from_q12(rotation_q12[1] as u16).rotate_y_arg());
    let rz = Mat3I16::rotate_z(Angle::from_q12(rotation_q12[2] as u16).rotate_y_arg());
    rz.mul(&ry).mul(&rx)
}

fn transpose_pose_matrix(matrix: [[i16; 3]; 3]) -> [[i16; 3]; 3] {
    [
        [matrix[0][0], matrix[1][0], matrix[2][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2]],
    ]
}

fn transform_q12(matrix: &Mat3I16, vector: [i32; 3]) -> [i32; 3] {
    let mut out = [0; 3];
    for (row, value) in out.iter_mut().enumerate() {
        let sum = i64::from(matrix.m[row][0]) * i64::from(vector[0])
            + i64::from(matrix.m[row][1]) * i64::from(vector[1])
            + i64::from(matrix.m[row][2]) * i64::from(vector[2]);
        *value = (sum >> 12).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    }
    out
}

fn lerp_i32(a: i32, b: i32, numerator: i32, denominator: i32) -> i32 {
    let delta = i64::from(b) - i64::from(a);
    (i64::from(a) + delta * i64::from(numerator) / i64::from(denominator))
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn translation_shift(mut max_abs: i32) -> u16 {
    let mut shift = 0;
    while max_abs > i16::MAX as i32 && shift < 15 {
        max_abs = (max_abs + 1) >> 1;
        shift += 1;
    }
    shift
}

fn quantize_translation(value: i32, shift: u8) -> i16 {
    if shift == 0 {
        return value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    let value = i64::from(value);
    let bias = 1i64 << (shift - 1);
    let rounded = if value >= 0 {
        (value + bias) >> shift
    } else {
        -((-value + bias) >> shift)
    };
    rounded.clamp(i64::from(i16::MIN), i64::from(i16::MAX)) as i16
}

fn abs_i32_saturating(value: i32) -> i32 {
    if value == i32::MIN {
        i32::MAX
    } else {
        value.abs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AnimationClipCalibration, AnimationClipResource, AnimationRole};

    #[test]
    fn sparse_keys_hold_endpoints_and_interpolate_between_frames() {
        let keys = [
            AnimationPoseCorrectionKey {
                frame: 4,
                joint: 2,
                rotation_q12: [0, 0, 0],
                translation: [0, 0, 0],
            },
            AnimationPoseCorrectionKey {
                frame: 12,
                joint: 2,
                rotation_q12: [800, -400, 200],
                translation: [80, -40, 20],
            },
        ];

        assert_eq!(sample_pose_correction(&keys, 2, 0).frame, 0);
        assert_eq!(sample_pose_correction(&keys, 2, 0).rotation_q12, [0; 3]);
        assert_eq!(
            sample_pose_correction(&keys, 2, 8).rotation_q12,
            [400, -200, 100]
        );
        assert_eq!(
            sample_pose_correction(&keys, 2, 20).translation,
            [80, -40, 20]
        );
        assert!(sample_pose_correction(&keys, 1, 8).is_identity());
    }

    #[test]
    fn correction_keys_round_trip_with_animation_clip_resources() {
        let clip = AnimationClipResource {
            psxanim_path: "assets/aletha_idle.psxanim".to_string(),
            skeleton: None,
            target_model: None,
            source: None,
            bake: Default::default(),
            role: AnimationRole::Idle,
            looping: true,
            tags: Vec::new(),
            calibration: AnimationClipCalibration::default(),
            pose_corrections: vec![AnimationPoseCorrectionKey {
                frame: 8,
                joint: 3,
                rotation_q12: [128, -64, 32],
                translation: [12, -4, 2],
            }],
        };

        let encoded = ron::to_string(&clip).expect("clip serializes");
        let decoded: AnimationClipResource = ron::from_str(&encoded).expect("clip parses");
        assert_eq!(decoded, clip);
    }

    #[test]
    fn tracked_aletha_correction_reencodes_standard_psxanim_without_runtime_metadata() {
        use crate::{model_import::resolve_path, ProjectDocument, ResourceData};

        let project_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../projects/default/project.ron");
        let project =
            ProjectDocument::load_from_path(&project_path).expect("default project parses");
        let project_root = project_path.parent().expect("project directory");
        let (skeleton, model_path) = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::Model(model) if resource.name == "Aletha Delivered" => {
                    Some((model.skeleton, model.model_path.as_str()))
                }
                _ => None,
            })
            .expect("Aletha Delivered model exists");
        let clip_path = project
            .resources
            .iter()
            .find_map(|resource| match &resource.data {
                ResourceData::AnimationClip(clip)
                    if clip.skeleton == skeleton && resource.name == "aletha_idle" =>
                {
                    Some(clip.psxanim_path.as_str())
                }
                _ => None,
            })
            .expect("Aletha Delivered idle clip exists");
        let model_bytes =
            std::fs::read(resolve_path(model_path, Some(project_root))).expect("model bytes exist");
        let animation_bytes = std::fs::read(resolve_path(clip_path, Some(project_root)))
            .expect("animation bytes exist");
        let model = Model::from_bytes(&model_bytes).expect("model parses");
        let animation = Animation::from_bytes(&animation_bytes).expect("animation parses");
        let original = animation.pose(0, 0).expect("root pose");
        let corrected_bytes = bake_animation_pose_corrections(
            &model,
            &animation,
            &[AnimationPoseCorrectionKey {
                frame: 0,
                joint: 0,
                rotation_q12: [0, 256, 0],
                translation: [8, 0, 0],
            }],
        );
        let corrected = Animation::from_bytes(&corrected_bytes).expect("corrected PSXA parses");

        assert_eq!(corrected.frame_count(), animation.frame_count());
        assert_eq!(corrected.joint_count(), animation.joint_count());
        assert_eq!(corrected.sample_rate_hz(), animation.sample_rate_hz());
        assert_ne!(corrected.pose(0, 0).expect("corrected root"), original);
    }
}
