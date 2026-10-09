//! Cook-time packing of animation clips into per-joint keyed tracks
//! (`.psxanim` version 6).
//!
//! The tracks are fitted by `psx-anim-tracks` against a worst-case vertex
//! displacement in model units, measured with the runtime decoder, so the bound
//! holds for what ships. A clip that is not made of rotations (animated scale),
//! cannot meet the bound, or would not get smaller keeps its pose table.

use psx_anim_tracks::{encode, ClipInput, JointInput, Options, Reject};
use psx_asset::{Animation, Model};

/// Every joint is bounded at this lever arm even when it drives few vertices:
/// hurtboxes, scarf anchors and sockets read joint poses far from the joint's
/// own geometry.
const LEVER_ARM_UNITS: f64 = 600.0;

/// A joint that carries an attachment socket also moves whatever hangs off it
/// (a sword is 3750 units long), so rotation error is bounded at a longer reach.
/// The bound cannot go much past 1000 units: a Q12 matrix element is rounded to
/// half a unit in 4096, which is already about 1.3 units of displacement out
/// there, so a tighter bound would reject every clip. The tip of a 3750 unit
/// weapon can therefore move up to 3.7 times the budget (about 9 units, 0.14
/// degrees), against the 2 degrees the resampling budget already allows.
const SOCKET_REACH_UNITS: f64 = 1024.0;

fn axis_points(radius: f64) -> impl Iterator<Item = [f64; 3]> {
    (0..3).flat_map(move |axis| {
        [-radius, radius].into_iter().map(move |r| {
            let mut p = [0.0; 3];
            p[axis] = r;
            p
        })
    })
}

/// Bind-space points each joint drives: its own vertices, vertices that blend
/// towards it, plus lever-arm points.
fn joint_probes(model: &Model<'_>, socket_joints: &[u16]) -> Vec<Vec<[f64; 3]>> {
    let mut out: Vec<Vec<[f64; 3]>> = vec![Vec::new(); model.joint_count() as usize];
    for p in 0..model.part_count() {
        let Some(part) = model.part(p) else { continue };
        for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
            let Some(vertex) = model.vertex(v) else {
                continue;
            };
            let pos = [
                f64::from(vertex.position.x),
                f64::from(vertex.position.y),
                f64::from(vertex.position.z),
            ];
            if let Some(list) = out.get_mut(part.joint_index() as usize) {
                list.push(pos);
            }
            if vertex.is_blend() {
                if let Some(list) = out.get_mut(vertex.joint1 as usize) {
                    list.push(pos);
                }
            }
        }
    }
    for (joint, list) in out.iter_mut().enumerate() {
        let reach = if socket_joints.contains(&(joint as u16)) {
            SOCKET_REACH_UNITS
        } else {
            LEVER_ARM_UNITS
        };
        list.extend(axis_points(reach));
    }
    out
}

/// Pack `bytes` into keyed tracks when that is smaller and inside
/// `budget_units`. Returns the input untouched otherwise. Reports the outcome.
pub(crate) fn pack_tracks(
    bytes: Vec<u8>,
    model: &Model<'_>,
    socket_joints: &[u16],
    budget_units: u16,
    label: &str,
) -> Vec<u8> {
    if budget_units == 0 {
        return bytes;
    }
    let Ok(animation) = Animation::from_bytes(&bytes) else {
        return bytes;
    };
    if animation.is_keyed_tracks() || animation.joint_count() != model.joint_count() {
        return bytes;
    }
    let probes = joint_probes(model, socket_joints);
    let frames = animation.frame_count();
    let mut joints = Vec::with_capacity(animation.joint_count() as usize);
    for j in 0..animation.joint_count() {
        let mut rotations = Vec::with_capacity(frames as usize);
        let mut translations = Vec::with_capacity(frames as usize);
        for f in 0..frames {
            let Some(pose) = animation.pose(f, j) else {
                return bytes;
            };
            rotations.push(pose.matrix);
            translations.push([pose.translation.x, pose.translation.y, pose.translation.z]);
        }
        joints.push(JointInput {
            rotations,
            translations,
            probes: probes[j as usize].clone(),
        });
    }
    let clip = ClipInput {
        sample_rate_hz: animation.sample_rate_hz(),
        joints,
    };
    let options = Options {
        budget_units: f64::from(budget_units),
        ..Options::default()
    };
    match encode(&clip, &options) {
        Ok((packed, report)) if packed.len() < bytes.len() => {
            crate::playtest::emit_cook_output(format_args!(
                "[cook] tracks {label}: {} -> {} B, worst {:.2} units ({:.2} vs stored poses), \
                 {} segment counts {:?}",
                bytes.len(),
                packed.len(),
                report.worst_error,
                report.worst_vs_input,
                report.rates.len(),
                report.rates,
            ));
            packed
        }
        Ok((packed, _)) => {
            crate::playtest::emit_cook_output(format_args!(
                "[cook] tracks {label}: kept pose table, keyed tracks would be {} B against {} B",
                packed.len(),
                bytes.len(),
            ));
            bytes
        }
        Err(reason) => {
            let why = match reason {
                Reject::NotRigid { joint, frame } => {
                    format!("joint {joint} frame {frame} is not a rotation")
                }
                Reject::Unreachable {
                    joint,
                    channel,
                    finest,
                    limit,
                } => format!(
                    "joint {joint} {channel} cannot reach {limit:.2} units, finest track {finest:.2}"
                ),
                other => format!("{other:?}"),
            };
            crate::playtest::emit_cook_output(format_args!(
                "[cook] tracks {label}: kept pose table ({why})"
            ));
            bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use psx_asset::{Animation, Model};

    use crate::playtest::{build_package, PlaytestPackage};
    use crate::ProjectDocument;

    fn cook(budget: u16) -> PlaytestPackage {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../projects/graybox-reach");
        let mut project = ProjectDocument::load_from_path(root.join("project.ron")).unwrap();
        project.animation_track_budget_units = budget;
        let (package, report) = build_package(&project, &root);
        package.unwrap_or_else(|| panic!("graybox reach cooks: {:?}", report.errors))
    }

    /// The contract end to end, on the shipped content: every clip that went
    /// to keyed tracks decodes, frame for frame, within the budget of the pose
    /// table it replaced (plus the shrink the stored matrices carry, which the
    /// encoder measures separately), and clips that cannot be packed are
    /// byte-identical to the untouched cook.
    #[test]
    fn graybox_reach_packs_inside_the_budget() {
        let off = cook(0);
        let on = cook(4);
        assert_eq!(off.model_clips.len(), on.model_clips.len());
        let (mut keyed, mut kept) = (0, 0);
        let (mut off_bytes, mut on_bytes) = (0usize, 0usize);
        for (mi, model) in on.models.iter().enumerate() {
            let mesh = &on.assets[model.mesh_asset_index].bytes;
            let parsed = Model::from_bytes(mesh).unwrap();
            let probes = super::joint_probes(&parsed, &[]);
            for ci in 0..model.clip_count as usize {
                let clip_on = &on.model_clips[model.clip_first as usize + ci];
                let clip_off = &off.model_clips[off.models[mi].clip_first as usize + ci];
                let a_bytes = &on.assets[clip_on.animation_asset_index].bytes;
                let b_bytes = &off.assets[clip_off.animation_asset_index].bytes;
                off_bytes += b_bytes.len();
                on_bytes += a_bytes.len();
                let a = Animation::from_bytes(a_bytes).unwrap();
                if !a.is_keyed_tracks() {
                    kept += 1;
                    assert_eq!(a_bytes, b_bytes, "{} kept its pose table", clip_on.name);
                    continue;
                }
                keyed += 1;
                let b = Animation::from_bytes(b_bytes).unwrap();
                assert_eq!(a.frame_count(), b.frame_count(), "{}", clip_on.name);
                let mut worst: f64 = 0.0;
                for f in 0..a.frame_count() {
                    for j in 0..a.joint_count() {
                        let (pa, pb) = (a.pose(f, j).unwrap(), b.pose(f, j).unwrap());
                        for p in &probes[j as usize] {
                            let apply = |pose: &psx_asset::JointPose| {
                                let m = pose.matrix;
                                [0, 1, 2].map(|r| {
                                    (f64::from(m[r][0]) * p[0]
                                        + f64::from(m[r][1]) * p[1]
                                        + f64::from(m[r][2]) * p[2])
                                        / 4096.0
                                })
                            };
                            let (x, y) = (apply(&pa), apply(&pb));
                            let t = |pose: &psx_asset::JointPose| {
                                [
                                    f64::from(pose.translation.x),
                                    f64::from(pose.translation.y),
                                    f64::from(pose.translation.z),
                                ]
                            };
                            let (tx, ty) = (t(&pa), t(&pb));
                            let d = (0..3)
                                .map(|k| (x[k] + tx[k] - y[k] - ty[k]).powi(2))
                                .sum::<f64>()
                                .sqrt();
                            worst = worst.max(d);
                        }
                    }
                }
                // 4 units of budget plus the distance of the stored matrices from
                // a rotation: the hand-authored walk wind-ups and wind-downs
                // carry up to 9 units of it, which the keyed clip removes.
                assert!(worst <= 4.0 + 6.0, "{}: worst {worst}", clip_on.name);
            }
        }
        assert!(keyed >= 30, "only {keyed} clips went to keyed tracks");
        assert!(kept >= 2, "scaled clips must keep their pose table");
        assert!(
            on_bytes * 10 < off_bytes * 7,
            "keyed tracks saved too little: {on_bytes} of {off_bytes}"
        );
    }
}
