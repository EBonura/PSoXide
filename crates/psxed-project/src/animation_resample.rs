//! Cook-time clip resampling under a worst-case rotation error budget.
//!
//! Measured on the shipped clips: an idle holds 14 seconds of very slow motion
//! and changes about 30 units per frame, while a heavy attack changes 360. One
//! rate for both is wrong in one direction or the other, and picking a stride
//! per clip by hand is the same decision made worse. So the cook is given an
//! error budget instead, and each clip keeps the lowest rate it can hold to.
//!
//! Why resample rather than drop every Nth frame. Clips are endpoint-inclusive
//! (the last frame repeats the first so looping playback can blend back), which
//! only survives frame-dropping when the stride divides the interval count. The
//! idle has 169 intervals, whose only useful divisor is 13. Resampling by
//! interpolation lands the last output frame on the last input frame at any
//! rate, so the loop closes no matter what rate is chosen.
//!
//! The error metric mirrors playback exactly: the runtime reconstructs a pose by
//! blending its two neighbouring stored frames (`looped_pose_sample_q12`), so
//! that is how a candidate rate is scored, at the ORIGINAL frame times.

use psx_asset::{Animation, JointPose};

/// Rotation matrix elements are Q12, where 4096 is 1.0.
const Q12_ONE: i32 = 4096;

/// Approximate `sin(degrees) * 4096` without floating point, good to a few
/// tenths of a degree over the small angles a sane budget uses.
///
/// ponytail: a table, because the engine is fixed-point everywhere and a budget
/// above 15 degrees is not a budget. Angles past the table clamp to its end.
fn q12_sine_of_degrees(degrees: u8) -> i32 {
    const SINE_Q12: [i32; 16] = [
        0, 71, 143, 214, 286, 357, 428, 499, 570, 641, 711, 782, 852, 921, 991, 1060,
    ];
    SINE_Q12[(degrees as usize).min(SINE_Q12.len() - 1)]
}

/// One pose blended between two others, the way the runtime blends stored
/// frames. `numerator/denominator` is the position between `a` and `b`.
fn blend(a: &JointPose, b: &JointPose, numerator: i32, denominator: i32) -> JointPose {
    let mix = |x: i32, y: i32| x + (y - x) * numerator / denominator.max(1);
    let mut matrix = [[0i16; 3]; 3];
    for column in 0..3 {
        for row in 0..3 {
            matrix[column][row] =
                mix(a.matrix[column][row] as i32, b.matrix[column][row] as i32) as i16;
        }
    }
    // Start from a real translation so the vector type stays inferred; the
    // one `JointPose` uses is not the same `Vec3I32` this crate imports.
    let mut translation = a.translation;
    translation.x = mix(a.translation.x, b.translation.x);
    translation.y = mix(a.translation.y, b.translation.y);
    translation.z = mix(a.translation.z, b.translation.z);
    JointPose { matrix, translation }
}

/// Sample the source clip at a fractional frame position.
fn sample_at(animation: &Animation<'_>, position: i64, scale: i64, joint: u16) -> Option<JointPose> {
    let last = animation.frame_count().saturating_sub(1);
    let whole = (position / scale) as u16;
    if whole >= last {
        return animation.pose(last, joint);
    }
    let fraction = (position % scale) as i32;
    let a = animation.pose(whole, joint)?;
    if fraction == 0 {
        return Some(a);
    }
    let b = animation.pose(whole + 1, joint)?;
    Some(blend(&a, &b, fraction, scale as i32))
}

/// Output frame count for a target rate, keeping both endpoints.
fn frames_at_rate(source_frames: u16, source_hz: u16, target_hz: u16) -> u16 {
    let intervals = source_frames.saturating_sub(1) as u32;
    let scaled = (intervals * target_hz.max(1) as u32).div_ceil(source_hz.max(1) as u32);
    (scaled.max(1) + 1).min(u16::MAX as u32) as u16
}

/// Worst single rotation-element error, in Q12, that playing the clip back at
/// `target_hz` would introduce at the original frame times.
fn worst_error_q12(animation: &Animation<'_>, target_hz: u16) -> i32 {
    let source_frames = animation.frame_count();
    let source_hz = animation.sample_rate_hz();
    let joints = animation.joint_count();
    let out_frames = frames_at_rate(source_frames, source_hz, target_hz);
    if out_frames < 2 {
        return i32::MAX;
    }
    // Output frame i samples the source at i * (source_frames - 1) / (out_frames - 1).
    let scale = (out_frames - 1) as i64;
    let span = (source_frames - 1) as i64;
    let mut worst = 0i32;
    for frame in 0..source_frames {
        // Where this original frame falls between two OUTPUT frames.
        let position = frame as i64 * scale;
        let lower = position / span;
        let upper = (lower + 1).min(scale);
        let fraction = (position - lower * span) as i32;
        for joint in 0..joints {
            let Some(truth) = animation.pose(frame, joint) else {
                continue;
            };
            let Some(a) = sample_at(animation, lower * span, scale, joint) else {
                continue;
            };
            let Some(b) = sample_at(animation, upper * span, scale, joint) else {
                continue;
            };
            let reconstructed = blend(&a, &b, fraction, span as i32);
            for column in 0..3 {
                for row in 0..3 {
                    let delta = (reconstructed.matrix[column][row] as i32
                        - truth.matrix[column][row] as i32)
                        .abs();
                    worst = worst.max(delta);
                }
            }
        }
    }
    worst
}

/// Lowest rate this clip can be cooked at while staying inside the budget.
///
/// Returns the source rate unchanged when nothing cheaper fits, when the budget
/// is off, or when the clip is too short to have an interior to lose.
pub fn chosen_rate_hz(animation: &Animation<'_>, budget_degrees: u8) -> u16 {
    let source_hz = animation.sample_rate_hz();
    if budget_degrees == 0 || animation.frame_count() < 4 || source_hz < 2 {
        return source_hz;
    }
    let limit = q12_sine_of_degrees(budget_degrees) * Q12_ONE / Q12_ONE;
    let mut chosen = source_hz;
    // Walk down from just under the authored rate and keep the cheapest that
    // holds. Rates are small, so this is a handful of passes.
    for candidate in 1..source_hz {
        if frames_at_rate(animation.frame_count(), source_hz, candidate) >= animation.frame_count() {
            continue;
        }
        if worst_error_q12(animation, candidate) <= limit {
            chosen = candidate;
            break;
        }
    }
    chosen
}

/// Rewrite a clip at `target_hz`. Returns `None` when the target matches the
/// source, so callers can keep the original bytes untouched.
///
/// The output is a flat v2 blob; the caller's normal compaction pass picks the
/// packed v3 encoding and translation shift from it exactly as it would for an
/// unresampled clip.
pub fn resample_animation_bytes(animation: &Animation<'_>, target_hz: u16) -> Option<Vec<u8>> {
    let source_hz = animation.sample_rate_hz();
    let source_frames = animation.frame_count();
    if target_hz == 0 || target_hz >= source_hz || source_frames < 2 {
        return None;
    }
    let joints = animation.joint_count();
    let out_frames = frames_at_rate(source_frames, source_hz, target_hz);
    if out_frames >= source_frames {
        return None;
    }
    let scale = (out_frames - 1) as i64;
    let span = (source_frames - 1) as i64;

    let mut poses = Vec::with_capacity(out_frames as usize * joints as usize);
    let mut max_abs = 0i32;
    for frame in 0..out_frames {
        for joint in 0..joints {
            let pose = sample_at(animation, frame as i64 * span, scale, joint)?;
            for value in [pose.translation.x, pose.translation.y, pose.translation.z] {
                max_abs = max_abs.max(if value == i32::MIN { i32::MAX } else { value.abs() });
            }
            poses.push(pose);
        }
    }

    let mut shift = 0u16;
    let mut scaled = max_abs;
    while scaled > i16::MAX as i32 && shift < 15 {
        scaled = (scaled + 1) >> 1;
        shift += 1;
    }

    Some(write_flat_clip(joints, out_frames, target_hz, shift, &poses))
}

/// Write a flat v2 clip blob. The caller's normal compaction pass picks the
/// packed v3 encoding from it, exactly as it would for an untouched clip.
fn write_flat_clip(
    joints: u16,
    frames: u16,
    sample_rate_hz: u16,
    shift: u16,
    poses: &[JointPose],
) -> Vec<u8> {
    let payload_len = psxed_format::animation::AnimationHeader::SIZE
        + poses.len() * psxed_format::animation::POSE_RECORD_SIZE;
    let mut out = Vec::with_capacity(psxed_format::AssetHeader::SIZE + payload_len);
    out.extend_from_slice(&psxed_format::animation::MAGIC);
    out.extend_from_slice(&psxed_format::animation::VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(payload_len as u32).to_le_bytes());
    out.extend_from_slice(&joints.to_le_bytes());
    out.extend_from_slice(&frames.to_le_bytes());
    out.extend_from_slice(&sample_rate_hz.to_le_bytes());
    out.extend_from_slice(&shift.to_le_bytes());
    for pose in poses {
        for column in pose.matrix {
            for value in column {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        for value in [pose.translation.x, pose.translation.y, pose.translation.z] {
            out.extend_from_slice(
                &crate::playtest::quantize_animation_translation(value, shift as u8).to_le_bytes(),
            );
        }
    }
    out
}

/// Per-frame motion of a clip: how far the whole pose moves from the frame
/// before it, summed over every joint.
fn frame_motion(animation: &Animation<'_>) -> Vec<i64> {
    let joints = animation.joint_count();
    let frames = animation.frame_count();
    let mut motion = Vec::with_capacity(frames.saturating_sub(1) as usize);
    for frame in 1..frames {
        let mut total = 0i64;
        for joint in 0..joints {
            let (Some(a), Some(b)) = (
                animation.pose(frame - 1, joint),
                animation.pose(frame, joint),
            ) else {
                continue;
            };
            for column in 0..3 {
                for row in 0..3 {
                    total += (b.matrix[column][row] as i64 - a.matrix[column][row] as i64).abs();
                }
            }
            total += (b.translation.x as i64 - a.translation.x as i64).abs();
            total += (b.translation.y as i64 - a.translation.y as i64).abs();
            total += (b.translation.z as i64 - a.translation.z as i64).abs();
        }
        motion.push(total);
    }
    motion
}

/// `true` when the clip's last frame repeats its first, which is how the cook
/// writes a loop so playback can blend the end back to the start.
///
/// Trimming a loop is not the same operation: its quiet stretch is part of the
/// cycle, and cutting it would make the animation jump. Only one-shots are
/// safe to trim, and this is how they are told apart without asking anyone to
/// author a flag.
pub fn is_looping(animation: &Animation<'_>) -> bool {
    let last = animation.frame_count().saturating_sub(1);
    if last == 0 {
        return false;
    }
    (0..animation.joint_count()).all(|joint| {
        match (animation.pose(0, joint), animation.pose(last, joint)) {
            (Some(a), Some(b)) => a.matrix == b.matrix && a.translation == b.translation,
            _ => false,
        }
    })
}

/// First and last frame worth keeping: the run of leading and trailing frames
/// whose motion sits under `still_percent` of the clip's own peak is dead time.
///
/// Measured on the shipped attacks, 16 to 31% of each one is this, and the
/// cooked character record plays the full range, so it is also half a second of
/// nothing before the swing starts.
///
/// Returns the whole clip when trimming is off, when the clip loops, or when
/// there is not enough left to be worth it.
pub fn live_frame_range(animation: &Animation<'_>, still_percent: u8) -> (u16, u16) {
    let frames = animation.frame_count();
    let whole = (0, frames.saturating_sub(1));
    if still_percent == 0 || frames < MIN_TRIMMED_FRAMES + 2 || is_looping(animation) {
        return whole;
    }
    let motion = frame_motion(animation);
    let Some(&peak) = motion.iter().max() else {
        return whole;
    };
    if peak == 0 {
        return whole;
    }
    let threshold = peak * still_percent as i64 / 100;
    let live: Vec<usize> = motion
        .iter()
        .enumerate()
        .filter(|(_, &m)| m > threshold)
        .map(|(i, _)| i)
        .collect();
    let (Some(&first_live), Some(&last_live)) = (live.first(), live.last()) else {
        return whole;
    };
    // `motion[i]` is the step INTO frame i + 1, so the first live frame is the
    // one the first live step starts from.
    let first = first_live as u16;
    let last = (last_live as u16 + 1).min(frames - 1);
    if last.saturating_sub(first) + 1 < MIN_TRIMMED_FRAMES {
        return whole;
    }
    (first, last)
}

/// Smallest clip a trim may leave behind. Below this the runtime has nothing to
/// blend and the saving is not worth the risk.
const MIN_TRIMMED_FRAMES: u16 = 4;

/// Rewrite a clip keeping only frames `first..=last`, at the same rate.
pub fn trim_animation_bytes(
    animation: &Animation<'_>,
    first: u16,
    last: u16,
) -> Option<Vec<u8>> {
    if first == 0 && last + 1 >= animation.frame_count() {
        return None;
    }
    let joints = animation.joint_count();
    let kept = last.checked_sub(first)?.checked_add(1)?;
    let mut poses = Vec::with_capacity(kept as usize * joints as usize);
    let mut max_abs = 0i32;
    for frame in first..=last {
        for joint in 0..joints {
            let pose = animation.pose(frame, joint)?;
            for value in [pose.translation.x, pose.translation.y, pose.translation.z] {
                max_abs = max_abs.max(if value == i32::MIN { i32::MAX } else { value.abs() });
            }
            poses.push(pose);
        }
    }
    let mut shift = 0u16;
    let mut scaled = max_abs;
    while scaled > i16::MAX as i32 && shift < 15 {
        scaled = (scaled + 1) >> 1;
        shift += 1;
    }
    Some(write_flat_clip(
        joints,
        kept,
        animation.sample_rate_hz(),
        shift,
        &poses,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a clip from a per-frame rotation value.
    ///
    /// Note a straight ramp is the WRONG fixture for "this clip must keep its
    /// rate": linear interpolation reconstructs a ramp exactly at any rate, so
    /// the resampler correctly throws away almost all of it. Only curvature
    /// costs anything, which is what `oscillating_clip` supplies.
    fn clip_from(frames: u16, joints: u16, hz: u16, value_of: impl Fn(u16) -> i16) -> Vec<u8> {
        clip_from_with(frames, joints, hz, value_of, |frame| frame as i16)
    }

    fn clip_from_with(
        frames: u16,
        joints: u16,
        hz: u16,
        value_of: impl Fn(u16) -> i16,
        translation_of: impl Fn(u16) -> i16,
    ) -> Vec<u8> {
        let payload_len = psxed_format::animation::AnimationHeader::SIZE
            + frames as usize * joints as usize * psxed_format::animation::POSE_RECORD_SIZE;
        let mut out = Vec::new();
        out.extend_from_slice(&psxed_format::animation::MAGIC);
        out.extend_from_slice(&psxed_format::animation::VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(payload_len as u32).to_le_bytes());
        out.extend_from_slice(&joints.to_le_bytes());
        out.extend_from_slice(&frames.to_le_bytes());
        out.extend_from_slice(&hz.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        for frame in 0..frames {
            for _ in 0..joints {
                let value = value_of(frame);
                let matrix: [[i16; 3]; 3] = [[value, 0, 0], [0, value, 0], [0, 0, value]];
                for column in matrix {
                    for v in column {
                        out.extend_from_slice(&v.to_le_bytes());
                    }
                }
                for _ in 0..3 {
                    out.extend_from_slice(&translation_of(frame).to_le_bytes());
                }
            }
        }
        out
    }

    fn linear_clip(frames: u16, joints: u16, hz: u16, step_q12: i16) -> Vec<u8> {
        clip_from(frames, joints, hz, move |frame| {
            4096 - (frame as i32 * step_q12 as i32).min(4096) as i16
        })
    }

    /// Alternates every frame, the shape linear interpolation cannot follow.
    fn oscillating_clip(frames: u16, joints: u16, hz: u16, amplitude: i16) -> Vec<u8> {
        clip_from(frames, joints, hz, move |frame| {
            if frame % 2 == 0 { 4096 } else { 4096 - amplitude }
        })
    }

    #[test]
    fn a_still_clip_gives_up_almost_every_frame() {
        // Nothing moves, so any rate reconstructs it exactly.
        let bytes = linear_clip(60, 4, 12, 0);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        assert_eq!(chosen_rate_hz(&animation, 2), 1);
    }

    #[test]
    fn a_fast_clip_keeps_its_authored_rate() {
        // Alternating every frame: any rate below the authored one blows a
        // 2 degree budget, because the wobble lives between the kept frames.
        let bytes = oscillating_clip(60, 4, 12, 600);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        assert_eq!(chosen_rate_hz(&animation, 2), 12);
    }

    #[test]
    fn the_budget_off_switch_leaves_every_clip_alone() {
        let bytes = linear_clip(60, 4, 12, 0);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        assert_eq!(chosen_rate_hz(&animation, 0), 12);
        assert!(resample_animation_bytes(&animation, 12).is_none());
    }

    #[test]
    fn a_resampled_clip_keeps_its_endpoints_and_duration() {
        // Endpoint-inclusive clips loop by blending the last stored frame back
        // to the first; a resample that misses the endpoint breaks the loop.
        let bytes = linear_clip(51, 3, 12, 40);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        let resampled = resample_animation_bytes(&animation, 4).expect("resampled");
        let out = Animation::from_bytes(&resampled).expect("resampled clip");

        assert_eq!(out.sample_rate_hz(), 4);
        assert!(out.frame_count() < animation.frame_count());
        // duration within one output frame
        let source_ms = 1000 * (animation.frame_count() - 1) as u32 / 12;
        let out_ms = 1000 * (out.frame_count() - 1) as u32 / 4;
        assert!(
            out_ms.abs_diff(source_ms) <= 1000 / 4,
            "{out_ms} vs {source_ms}"
        );

        let first_in = animation.pose(0, 0).expect("pose");
        let first_out = out.pose(0, 0).expect("pose");
        assert_eq!(first_out.matrix, first_in.matrix);
        let last_in = animation.pose(animation.frame_count() - 1, 0).expect("pose");
        let last_out = out.pose(out.frame_count() - 1, 0).expect("pose");
        assert_eq!(last_out.matrix, last_in.matrix);
    }

    /// Still frames, then real motion, then still frames again: the shape the
    /// trim exists for.
    fn padded_clip(head: u16, live: u16, tail: u16, joints: u16, hz: u16) -> Vec<u8> {
        clip_from(head + live + tail, joints, hz, move |frame| {
            if frame < head {
                4096
            } else if frame < head + live {
                4096 - (frame - head) as i16 * 100
            } else {
                4096 - (live.saturating_sub(1)) as i16 * 100
            }
        })
    }

    #[test]
    fn trimming_drops_the_dead_ends_and_keeps_the_motion() {
        let bytes = padded_clip(10, 20, 12, 3, 15);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        let (first, last) = live_frame_range(&animation, 15);
        assert_eq!((first, last), (10, 29), "must keep exactly the moving span");

        let trimmed = trim_animation_bytes(&animation, first, last).expect("trimmed");
        let out = Animation::from_bytes(&trimmed).expect("trimmed clip");
        assert_eq!(out.frame_count(), 20);
        assert_eq!(out.sample_rate_hz(), animation.sample_rate_hz());
        // the kept frames must be the original ones, untouched
        for frame in 0..out.frame_count() {
            assert_eq!(
                out.pose(frame, 0).unwrap().matrix,
                animation.pose(frame + first, 0).unwrap().matrix,
                "frame {frame} changed"
            );
        }
    }

    #[test]
    fn a_looping_clip_is_never_trimmed() {
        // Its quiet stretch is part of the cycle; cutting it makes the loop
        // jump. Detected from the data, not from an authored flag.
        // Constant translation as well, or the last frame would not match the
        // first and this would not be a loop at all.
        let bytes = clip_from_with(
            40,
            3,
            12,
            |frame| if frame == 0 || frame == 39 { 4096 } else { 4096 - 200 },
            |_| 0,
        );
        let animation = Animation::from_bytes(&bytes).expect("clip");
        assert!(is_looping(&animation));
        assert_eq!(live_frame_range(&animation, 15), (0, 39));
    }

    #[test]
    fn trimming_off_and_all_motion_leave_the_clip_alone() {
        let bytes = padded_clip(10, 20, 12, 3, 15);
        let animation = Animation::from_bytes(&bytes).expect("clip");
        assert_eq!(live_frame_range(&animation, 0), (0, 41));

        // A clip that moves throughout has no dead ends to find.
        let busy = linear_clip(30, 3, 15, 60);
        let busy = Animation::from_bytes(&busy).expect("clip");
        let (first, last) = live_frame_range(&busy, 15);
        assert_eq!((first, last), (0, 29));
        assert!(trim_animation_bytes(&busy, first, last).is_none());
    }

    #[test]
    fn the_chosen_rate_holds_the_budget_it_was_given() {
        // The whole contract: whatever rate comes back, replaying the clip at
        // it must stay inside the budget at the original frame times.
        for step in [0, 20, 60, 120, 400] {
            let bytes = linear_clip(48, 3, 12, step);
            let animation = Animation::from_bytes(&bytes).expect("clip");
            for budget in [1u8, 3, 8] {
                let rate = chosen_rate_hz(&animation, budget);
                if rate == animation.sample_rate_hz() {
                    continue;
                }
                let limit = q12_sine_of_degrees(budget);
                assert!(
                    worst_error_q12(&animation, rate) <= limit,
                    "step {step} budget {budget} chose {rate} Hz over budget"
                );
            }
        }
    }
}



