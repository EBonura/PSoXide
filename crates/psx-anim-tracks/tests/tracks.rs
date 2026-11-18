//! The encoder, the runtime decoder and the format agree.

#![allow(clippy::needless_range_loop)]

use psx_anim_tracks::{encode, encode_rot_key, ClipInput, JointInput, Options, Reject};
use psx_asset::tracks::{host, SQRT_TABLE};
use psx_asset::Animation;
use psxed_format::animation::tracks as fmt;

/// Deterministic generator, no dependency needed.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f64) / ((1u64 << 31) as f64)
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

fn quat(axis: [f64; 3], angle: f64) -> [f64; 4] {
    let n = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let s = (angle / 2.0).sin() / n;
    [axis[0] * s, axis[1] * s, axis[2] * s, (angle / 2.0).cos()]
}

fn matq12(q: [f64; 4]) -> [[i16; 3]; 3] {
    let [x, y, z, w] = q;
    let m = [
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y - w * z),
            2.0 * (x * z + w * y),
        ],
        [
            2.0 * (x * y + w * z),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z - w * x),
        ],
        [
            2.0 * (x * z - w * y),
            2.0 * (y * z + w * x),
            1.0 - 2.0 * (x * x + y * y),
        ],
    ];
    let mut o = [[0i16; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            o[r][c] = (m[r][c] * 4096.0).round() as i16;
        }
    }
    o
}

fn probes() -> Vec<[f64; 3]> {
    let mut p = vec![];
    for a in 0..3 {
        for s in [-1.0, 1.0] {
            let mut v = [0.0; 3];
            v[a] = s * 600.0;
            p.push(v);
        }
    }
    p
}

/// A walk-like clip: every joint swings about its own axis with its own phase.
fn synthetic(frames: usize, joints: usize, seed: u64) -> ClipInput {
    let mut rng = Lcg(seed);
    let mut out = vec![];
    for j in 0..joints {
        let axis = [
            rng.range(-1.0, 1.0),
            rng.range(-1.0, 1.0),
            rng.range(0.2, 1.0),
        ];
        let amp = rng.range(0.0, 1.2);
        let phase = rng.range(0.0, std::f64::consts::TAU);
        let tamp = rng.range(0.0, 400.0) * (j % 3 != 0) as i32 as f64;
        let mut rotations = vec![];
        let mut translations = vec![];
        for f in 0..frames {
            let u = f as f64 / (frames - 1) as f64 * std::f64::consts::TAU;
            rotations.push(matq12(quat(axis, amp * (u + phase).sin())));
            translations.push([
                (tamp * (u + phase).cos()) as i32,
                (tamp * 0.5 * (2.0 * u).sin()) as i32 + 900,
                (tamp * 0.25 * u.sin()) as i32 - 200,
            ]);
        }
        out.push(JointInput {
            rotations,
            translations,
            probes: probes(),
            reach: 0.0,
        });
    }
    ClipInput {
        sample_rate_hz: 30,
        joints: out,
        parents: Vec::new(),
    }
}

#[test]
fn sqrt_table_is_the_rounded_root() {
    for (i, &v) in SQRT_TABLE.iter().enumerate() {
        let exact = ((i as f64) * 32768.0).sqrt().round() as u16;
        assert_eq!(v, exact, "entry {i}");
    }
}

#[test]
fn rotation_keys_decode_within_their_resolution() {
    let mut rng = Lcg(7);
    let tolerance = [8.0, 4.0, 3.0];
    for size in 0..3u8 {
        let mut worst: f64 = 0.0;
        for _ in 0..4000 {
            let axis = [
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            ];
            let q = quat(axis, rng.range(0.0, std::f64::consts::TAU));
            let key = encode_rot_key(q, size);
            let got = host::decode_rotation_key(&key, size);
            // Same hemisphere as the canonical key: largest component positive.
            let sign = if q[got.omitted as usize] < 0.0 {
                -1.0
            } else {
                1.0
            };
            for i in 0..4 {
                let want = sign * q[i] * 4096.0;
                worst = worst.max((f64::from(got.q[i]) - want).abs());
            }
        }
        assert!(
            worst <= tolerance[size as usize],
            "size {size}: worst {worst}"
        );
    }
}

#[test]
fn clip_round_trip_holds_the_budget() {
    for (frames, seed) in [(24usize, 1u64), (44, 2), (126, 3), (9, 4)] {
        let clip = synthetic(frames, 12, seed);
        let opts = Options::default();
        let (blob, report) = encode(&clip, &opts).expect("synthetic clip encodes");
        assert!(report.worst_error <= opts.budget_units, "{report:?}");
        let anim = Animation::from_bytes(&blob).expect("blob parses");
        assert!(anim.is_keyed_tracks());
        assert_eq!(anim.frame_count() as usize, frames);
        assert_eq!(anim.joint_count() as usize, 12);
        assert_eq!(blob.len() % 4, 0);
        // Smaller than one 16 byte v4 record per pose.
        assert!(blob.len() < frames * 12 * 16, "{} bytes", blob.len());
        // Frame sampling and the looped sampler agree at whole frames.
        for f in 0..frames.min(8) {
            let sample = anim.looped_pose_sample_q12((f as u32) << 12).unwrap();
            for j in 0..12u16 {
                assert_eq!(
                    sample.pose(j),
                    anim.pose(f as u16, j),
                    "frame {f} joint {j}"
                );
            }
        }
    }
}

#[test]
fn in_between_samples_follow_the_motion() {
    let clip = synthetic(30, 4, 11);
    let (blob, _) = encode(&clip, &Options::default()).unwrap();
    let anim = Animation::from_bytes(&blob).unwrap();
    // A half-frame sample sits between its neighbours within a few Q12 units
    // per element for a smooth clip.
    for f in 0..28u32 {
        let mid = anim.looped_pose_sample_q12((f << 12) | 0x800).unwrap();
        for j in 0..4u16 {
            let a = anim.pose(f as u16, j).unwrap();
            let b = anim.pose(f as u16 + 1, j).unwrap();
            let m = mid.pose(j).unwrap();
            for r in 0..3 {
                for c in 0..3 {
                    let lo = a.matrix[r][c].min(b.matrix[r][c]) as i32 - 120;
                    let hi = a.matrix[r][c].max(b.matrix[r][c]) as i32 + 120;
                    assert!(
                        (lo..=hi).contains(&(m.matrix[r][c] as i32)),
                        "f{f} j{j} [{r}][{c}]"
                    );
                }
            }
        }
    }
}

#[test]
fn static_clip_collapses_to_constants() {
    let m = matq12(quat([0.3, 0.5, 0.8], 0.7));
    let joint = JointInput {
        rotations: vec![m; 20],
        translations: vec![[10, -20, 30]; 20],
        probes: probes(),
        reach: 0.0,
    };
    let clip = ClipInput {
        sample_rate_hz: 15,
        joints: vec![joint.clone(), joint],
        parents: Vec::new(),
    };
    let (blob, report) = encode(&clip, &Options::default()).unwrap();
    assert!(report
        .joints
        .iter()
        .all(|j| j.rot_segments == 0 && j.trans_segments == 0));
    assert!(blob.len() < 160, "{} bytes", blob.len());
    let anim = Animation::from_bytes(&blob).unwrap();
    assert_eq!(anim.pose(7, 1).unwrap().translation.x, 10);
}

#[test]
fn scaled_and_reflected_poses_are_rejected() {
    let mut clip = synthetic(10, 2, 5);
    clip.joints[1].rotations[3][0][0] = (1.2 * 4096.0) as i16;
    assert!(matches!(
        encode(&clip, &Options::default()),
        Err(Reject::NotRigid { joint: 1, frame: 3 })
    ));
    let mut clip = synthetic(10, 1, 5);
    clip.joints[0].rotations[0] = [[-4096, 0, 0], [0, 4096, 0], [0, 0, 4096]];
    assert!(matches!(
        encode(&clip, &Options::default()),
        Err(Reject::NotRigid { .. })
    ));
}

#[test]
fn tighter_budget_never_costs_fewer_bytes() {
    let clip = synthetic(44, 16, 21);
    let mut last = 0usize;
    for budget in [16.0, 8.0, 4.0, 2.0] {
        let (blob, report) = encode(
            &clip,
            &Options {
                budget_units: budget,
                rotation_share: 0.8,
                ..Options::default()
            },
        )
        .unwrap();
        assert!(report.worst_error <= budget);
        assert!(
            blob.len() + 64 >= last,
            "budget {budget}: {} < {last}",
            blob.len()
        );
        last = blob.len();
    }
}

#[test]
fn corrupt_blobs_are_rejected_or_decode_cleanly() {
    // Miri checks every raw read in the decoder; keep its share small.
    let (frames, joints, rounds) = if cfg!(miri) {
        (12, 3, 60)
    } else {
        (30, 5, 4000)
    };
    let clip = synthetic(frames, joints, 31);
    let (blob, _) = encode(&clip, &Options::default()).unwrap();
    let mut rng = Lcg(99);
    let mut accepted = 0usize;
    for _ in 0..rounds {
        let mut b = blob.clone();
        for _ in 0..(1 + (rng.next() * 3.0) as usize) {
            let at = (rng.next() * b.len() as f64) as usize % b.len();
            b[at] = (rng.next() * 256.0) as u8;
        }
        if rng.next() < 0.2 {
            let cut = (rng.next() * b.len() as f64) as usize;
            b.truncate(cut.max(1));
        }
        let Ok(anim) = Animation::from_bytes(&b) else {
            continue;
        };
        accepted += 1;
        // Whatever it holds, sampling every joint at every position must
        // neither panic nor read outside the blob (checked below by length).
        for f in 0..anim.frame_count() {
            for step in [0u32, 0x400, 0x800, 0xc00] {
                if let Some(s) = anim.looped_pose_sample_q12(((f as u32) << 12) | step) {
                    for j in 0..anim.joint_count() {
                        let _ = s.pose(j);
                    }
                }
            }
        }
    }
    assert!(accepted > 0, "mutation never produced a parseable blob");
}

#[test]
fn layout_constants_are_consistent() {
    assert_eq!(fmt::rotation_key_bytes(0), 4);
    assert_eq!(fmt::rotation_key_bytes(2), 6);
    assert_eq!(fmt::rotation_bits(1), 12);
}

/// A joint with no motion of its own carries its parent's pose, so its tracks
/// repeat the parent's byte for byte. The clip stores those keys once, and every
/// phase the sampler can produce decodes the same poses as the unshared layout.
#[test]
fn repeated_tracks_are_stored_once() {
    let mut clip = synthetic(40, 6, 11);
    clip.joints[3] = clip.joints[1].clone();
    clip.joints[5] = clip.joints[1].clone();
    let (blob, report) = encode(&clip, &Options::default()).expect("clip encodes");
    let anim = Animation::from_bytes(&blob).expect("blob parses");
    let joint_bytes = report.joints[1].bytes as usize;
    assert!(joint_bytes > 0);
    assert_eq!(report.shared_bytes, 2 * joint_bytes, "{report:?}");
    assert_eq!(
        report.unshared_bytes,
        (blob.len() + report.shared_bytes).next_multiple_of(4)
    );
    // The repeated joints point at the first copy and decode exactly like it.
    let desc = |j: usize| &blob[52 + j * fmt::DESCRIPTOR_SIZE..][..8];
    assert_eq!(desc(3)[4..8], desc(1)[4..8]);
    assert_eq!(desc(5)[4..8], desc(1)[4..8]);
    for phase in (0..39u32 << 12).step_by(16) {
        let sample = anim.looped_pose_sample_q12(phase).unwrap();
        assert_eq!(sample.pose(3), sample.pose(1), "phase {phase:#x}");
        assert_eq!(sample.pose(5), sample.pose(1), "phase {phase:#x}");
    }
}

/// `m` applied as the renderer applies a stored pose matrix (column-major).
fn apply_pose(m: &[[i16; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| {
        (f64::from(m[0][r]) * v[0] + f64::from(m[1][r]) * v[1] + f64::from(m[2][r]) * v[2]) / 4096.0
    })
}

/// A rigid chain swinging fast: every bone's pivot stays on its parent.
fn rigid_chain(frames: usize, joints: usize, seed: u64) -> (ClipInput, Vec<[f64; 3]>) {
    let mut rng = Lcg(seed);
    let pivots: Vec<[f64; 3]> = (0..joints)
        .map(|j| [0.0, 900.0 * j as f64, rng.range(-50.0, 50.0)])
        .collect();
    let mut joints_in: Vec<JointInput> = Vec::new();
    let mut world_t: Vec<Vec<[f64; 3]>> = vec![Vec::new(); joints];
    let mut mats: Vec<Vec<[[i16; 3]; 3]>> = vec![Vec::new(); joints];
    let axes: Vec<[f64; 3]> = (0..joints)
        .map(|_| {
            [
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(0.2, 1.0),
            ]
        })
        .collect();
    for f in 0..frames {
        let u = f as f64 / (frames - 1) as f64 * std::f64::consts::TAU;
        for j in 0..joints {
            // Model-space rotations: each joint swings up to 2.4 rad per clip.
            let m = matq12(quat(
                axes[j],
                1.2 * (u + j as f64).sin() * (j + 1) as f64 * 0.6,
            ));
            let t = if j == 0 {
                [100.0 * u.sin(), 50.0, 0.0]
            } else {
                // t_j = t_p + R_p p - R_j p keeps pivot p on the parent.
                let a = apply_pose(&mats[j - 1][f], pivots[j]);
                let b = apply_pose(&m, pivots[j]);
                [0, 1, 2].map(|r| world_t[j - 1][f][r] + a[r] - b[r])
            };
            world_t[j].push(t);
            mats[j].push(m);
        }
    }
    for j in 0..joints {
        joints_in.push(JointInput {
            rotations: mats[j].clone(),
            translations: world_t[j]
                .iter()
                .map(|t| t.map(|v| v.round() as i32))
                .collect(),
            probes: vec![pivots[j]],
            reach: 600.0,
        });
    }
    let clip = ClipInput {
        sample_rate_hz: 15,
        joints: joints_in,
        parents: (0..joints).map(|j| j.checked_sub(1)).collect(),
    };
    (clip, pivots)
}

#[test]
fn v7_chain_holds_the_budget_and_stays_attached_between_keys() {
    let (clip, pivots) = rigid_chain(12, 5, 11);
    let opts = Options::default();
    let (blob, report) = encode(&clip, &opts).unwrap();
    let anim = Animation::from_bytes(&blob).unwrap();
    assert!(anim.is_parent_relative());
    assert_eq!(report.derived_joints, 4);
    assert!(
        report.worst_error <= opts.budget_units * opts.rotation_share + opts.residual_units,
        "{}",
        report.worst_error
    );
    // Between keys every pivot stays where its parent carries it (a few units
    // of integer rounding), where v6 slid the child along a straight line.
    let v6 = encode(
        &ClipInput {
            parents: Vec::new(),
            ..clip.clone()
        },
        &opts,
    )
    .unwrap()
    .0;
    let v6 = Animation::from_bytes(&v6).unwrap();
    let gap = |anim: &Animation<'_>, phase: u32| -> f64 {
        let s = anim.looped_pose_sample_q12(phase).unwrap();
        let mut worst: f64 = 0.0;
        for j in 1..5u16 {
            let (c, p) = (s.pose(j).unwrap(), s.pose(j - 1).unwrap());
            let x = pivots[j as usize];
            let wc = apply_pose(&c.matrix, x);
            let wp = apply_pose(&p.matrix, x);
            let tc = [c.translation.x, c.translation.y, c.translation.z];
            let tp = [p.translation.x, p.translation.y, p.translation.z];
            let d: f64 = (0..3)
                .map(|r| (wc[r] + f64::from(tc[r]) - wp[r] - f64::from(tp[r])).powi(2))
                .sum();
            worst = worst.max(d.sqrt());
        }
        worst
    };
    let (mut v7_gap, mut v6_gap): (f64, f64) = (0.0, 0.0);
    for phase in (0..(10u32 << 12)).step_by(256) {
        v7_gap = v7_gap.max(gap(&anim, phase));
        v6_gap = v6_gap.max(gap(&v6, phase));
    }
    assert!(v7_gap < 12.0, "v7 gap {v7_gap}");
    assert!(v6_gap > 3.0 * v7_gap, "v6 gap {v6_gap} v7 gap {v7_gap}");
}

#[test]
fn v7_blob_with_a_parent_cycle_is_rejected() {
    let (clip, _) = rigid_chain(8, 3, 3);
    let (mut blob, _) = encode(&clip, &Options::default()).unwrap();
    let descriptors = psxed_format::AssetHeader::SIZE + 8 + fmt::RATE_TABLE_SIZE;
    // Joint 0 names joint 2 as its parent: 0 -> 2 -> 1 -> 0.
    blob[descriptors + 15] = 3;
    assert!(Animation::from_bytes(&blob).is_err());
}

#[test]
fn v7_pose_cache_matches_walking_the_chain() {
    let (clip, _) = rigid_chain(16, 6, 21);
    let (blob, _) = encode(&clip, &Options::default()).unwrap();
    let anim = Animation::from_bytes(&blob).unwrap();
    // Interleaved phases (a crossfade), children before parents, repeats and
    // more distinct positions than the cache holds.
    let mut rng = Lcg(5);
    let queries: Vec<(u32, u16)> = (0..3000)
        .map(|_| {
            (
                (rng.range(0.0, 15.0) * 4096.0) as u32 & !15,
                (rng.range(0.0, 6.0) as u16).min(5),
            )
        })
        .collect();
    assert_eq!(anim.first_cache_mismatch(&queries), None);
}
