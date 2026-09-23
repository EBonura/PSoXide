//! The host reference decoder and the SDK runtime decoder must agree bit for
//! bit, and a cooked clip must stay inside its error bound at the keys.

#![allow(clippy::needless_range_loop)]

use psx_anim_cook::{
    apply, encode, hma_dec, needed_bones, production_opts, world_of, ClipSource, Mat34, Skeleton,
};

fn rot_z(a: f64) -> [[f64; 3]; 3] {
    let (s, c) = a.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}
fn rot_x(a: f64) -> [[f64; 3]; 3] {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

/// A three-bone arm: a swinging shoulder, a bending elbow, a still hand.
struct Swing;
impl ClipSource for Swing {
    fn key(&self) -> usize {
        1
    }
    fn n_int(&self) -> usize {
        30
    }
    fn looping(&self) -> bool {
        true
    }
    fn local_at(&self, pos: f64) -> Vec<Mat34> {
        let ph = pos / 30.0 * std::f64::consts::TAU;
        vec![
            (rot_z(0.6 * ph.sin()), [0.0, 160.0, 0.0]),
            (rot_x(0.9 + 0.7 * (2.0 * ph).sin()), [0.0, -60.0, 0.0]),
            (rot_z(0.0), [0.0, -55.0, 0.0]),
        ]
    }
}

fn skeleton() -> Skeleton {
    let arm: Vec<[f64; 3]> = (0..6).map(|i| [4.0, -10.0 * i as f64, 3.0]).collect();
    Skeleton {
        parents: vec![-1, 0, 1],
        bind_t: vec![[0.0, 160.0, 0.0], [0.0, -60.0, 0.0], [0.0, -55.0, 0.0]],
        verts: vec![arm.clone(), arm.clone(), vec![[2.0, -8.0, 0.0]]],
        scale: 4.0,
    }
}

#[test]
fn runtime_decoder_matches_reference() {
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let clip = Swing;
    let e = encode(&sk, &[&clip], &bones, production_opts(0.5));
    let mut blob = e.bytes.clone();
    blob.extend_from_slice(&[0; 4]);
    let leaked: &'static [u8] = Box::leak(blob.into_boxed_slice());
    let runtime = psx_asset::hma1::Model::new(leaked);
    let reference = hma_dec::ModelView::new(&e.bytes);
    for pos_q8 in (0..30 * 256).step_by(37) {
        let mut a = [psx_asset::hma1::Aff::ZERO; 3];
        runtime.decode(0, pos_q8, &mut a);
        let mut b = [hma_dec::Affine::default(); 3];
        reference.decode(&reference.clip(0), pos_q8, true, &mut b);
        for k in 0..3 {
            assert_eq!(a[k].r, b[k].r, "rotation, bone {k}, pos {pos_q8}");
            assert_eq!(a[k].t, b[k].t, "translation, bone {k}, pos {pos_q8}");
        }
    }
}

#[test]
fn keys_stay_inside_the_vertex_bound() {
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let clip = Swing;
    let tol = 0.5;
    let e = encode(&sk, &[&clip], &bones, production_opts(tol));
    let v = hma_dec::ModelView::new(&e.bytes);
    let mut worst = 0.0f64;
    for f in 0..30 {
        let exact = world_of(&sk.parents, &clip.local_at(f as f64));
        let mut out = [hma_dec::Affine::default(); 3];
        v.decode(&v.clip(0), (f * 256) as u32, true, &mut out);
        for b in 0..3 {
            for p in &sk.verts[b] {
                let x = apply(&exact[b], *p);
                let d: Mat34 = (
                    core::array::from_fn(|i| {
                        core::array::from_fn(|j| out[b].r[i][j] as f64 / 4096.0)
                    }),
                    core::array::from_fn(|i| out[b].t[i] as f64 / 4.0),
                );
                let y = apply(&d, *p);
                let dist = ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2))
                    .sqrt()
                    / sk.scale;
                worst = worst.max(dist);
            }
        }
    }
    // tolerance per bone, plus fixed-point rounding
    assert!(worst < tol + 0.25, "worst vertex error {worst}");
    assert!(
        e.bytes.len() < 3 * 31 * 20,
        "no smaller than one HMD8 palette per frame: {}",
        e.bytes.len()
    );
}

fn jaw_q(open_deg: f64) -> [i16; 4] {
    psx_anim_cook::quat_q12(&rot_z(open_deg.to_radians()))
}

#[test]
fn jaw_controller_matches_reference_both_ways() {
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let e = encode(&sk, &[&Swing], &bones, production_opts(0.5));
    let mut blob = e.bytes.clone();
    blob.extend_from_slice(&[0; 4]);
    let leaked: &'static [u8] = Box::leak(blob.into_boxed_slice());
    let runtime = psx_asset::hma1::Model::new(leaked);
    let reference = hma_dec::ModelView::new(&e.bytes);
    for post in [false, true] {
        for amount in [0u8, 17, 40, 64] {
            let jaw = psx_asset::hma1::Jaw::open(1, post, jaw_q(30.0), amount);
            for pos_q8 in (0..30 * 256).step_by(211) {
                let mut a = [psx_asset::hma1::Aff::ZERO; 3];
                runtime.decode_with(0, pos_q8, &jaw, &mut a);
                let mut b = [hma_dec::Affine::default(); 3];
                let rj = (jaw.bone != usize::MAX).then_some((jaw.bone, jaw.post, jaw.q));
                reference.decode_with(&reference.clip(0), pos_q8, true, rj, &mut b);
                for k in 0..3 {
                    assert_eq!(
                        a[k].r, b[k].r,
                        "post {post} amount {amount} bone {k} pos {pos_q8}"
                    );
                    assert_eq!(
                        a[k].t, b[k].t,
                        "post {post} amount {amount} bone {k} pos {pos_q8}"
                    );
                }
            }
        }
    }
}

#[test]
fn open_jaw_turns_the_bone_and_carries_its_children() {
    // Fully open at a key: the elbow's local rotation gains rot_z(30 deg)
    // before (pre) its own; the hand follows through the hierarchy.
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let e = encode(&sk, &[&Swing], &bones, production_opts(0.25));
    let v = hma_dec::ModelView::new(&e.bytes);
    let q = jaw_q(30.0).map(|x| x as i32);
    let f = 10usize;
    let mut out = [hma_dec::Affine::default(); 3];
    v.decode_with(
        &v.clip(0),
        (f * 256) as u32,
        true,
        Some((1, false, q)),
        &mut out,
    );
    let mut local = Swing.local_at(f as f64);
    let r = local[1].0;
    let d = rot_z(30f64.to_radians());
    local[1].0 =
        core::array::from_fn(|i| core::array::from_fn(|j| (0..3).map(|k| d[i][k] * r[k][j]).sum()));
    let exact = world_of(&sk.parents, &local);
    for b in 0..3 {
        let got: Mat34 = (
            core::array::from_fn(|i| core::array::from_fn(|j| out[b].r[i][j] as f64 / 4096.0)),
            core::array::from_fn(|i| out[b].t[i] as f64 / 4.0),
        );
        for p in &sk.verts[b] {
            let (x, y) = (apply(&exact[b], *p), apply(&got, *p));
            let dist = ((x[0] - y[0]).powi(2) + (x[1] - y[1]).powi(2) + (x[2] - y[2]).powi(2))
                .sqrt()
                / sk.scale;
            assert!(dist < 1.0, "bone {b}: {dist}");
        }
    }
}

/// A minimal HMD8 (one range per bone, one bind palette, no triangles)
/// carrying the tracks section: the runtime entry points games call.
fn hmd8_with_tracks(
    blob: &[u8],
    jaw: Option<psx_anim_cook::JawRecord>,
    hold_quanta: u8,
) -> &'static [u8] {
    let n_bones = 3usize;
    let n_clips = 1usize;
    let mut o = Vec::new();
    o.extend_from_slice(b"HMD8");
    o.extend_from_slice(&(n_bones as u32).to_le_bytes()); // one vertex per bone
    o.extend_from_slice(&0u32.to_le_bytes()); // triangles
    o.extend_from_slice(&0u32.to_le_bytes()); // textures / hitboxes
    o.extend_from_slice(&1u32.to_le_bytes()); // frames
    o.extend_from_slice(&(n_clips as u32).to_le_bytes());
    let md_len_at = o.len();
    o.extend_from_slice(&0u32.to_le_bytes()); // model data length, patched below
    o.extend_from_slice(&0u16.to_le_bytes()); // local to world: identity
    o.extend_from_slice(&(1u16 << 8).to_le_bytes()); // flags: HMA1
    o.extend_from_slice(&(n_bones as u16).to_le_bytes());
    o.extend_from_slice(&(n_bones as u16).to_le_bytes()); // ranges
    o.extend_from_slice(&0u16.to_le_bytes()); // clip: first frame 0
    o.extend_from_slice(&(1u16 | (hold_quanta as u16) << 8).to_le_bytes());
    let md_start = o.len();
    for b in 0..n_bones as u16 {
        for v in [b, 1, b, 0] {
            o.extend_from_slice(&v.to_le_bytes());
        }
    }
    for _ in 0..n_bones {
        o.extend_from_slice(&[0; 6]);
    }
    for _ in 0..n_bones {
        o.extend_from_slice(&[0; 20]);
    }
    let map = [0u8, 1, 2];
    let section = psx_anim_cook::hmd8_section(o.len(), &map, jaw, blob);
    o.extend_from_slice(&section);
    let md_len = (o.len() - md_start) as u32;
    o[md_len_at..md_len_at + 4].copy_from_slice(&md_len.to_le_bytes());
    Box::leak(o.into_boxed_slice())
}

#[test]
fn hmd8_tracks_drive_phase_and_pose() {
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let e = encode(&sk, &[&Swing], &bones, production_opts(0.5));
    let jaw = psx_anim_cook::JawRecord {
        bone: 1,
        post: false,
        open: jaw_q(30.0),
    };
    // 15 quanta = 30 ticks at the 20 Hz game clock
    let md = psx_asset::hmd8::Model::load(hmd8_with_tracks(&e.bytes, Some(jaw), 15));
    assert!(
        md.has_tracks() && md.n_ranges == 3,
        "tracks section rejected"
    );
    assert_eq!(md.clip_hold_ticks(0), 30);
    // 30 source intervals over 30 ticks: one source frame per tick
    assert_eq!(md.looped_clip_phase(0, 30, 7), (0, 0, 7 * 256));
    assert_eq!(md.looped_clip_phase(0, 30, 37), (0, 0, 7 * 256));
    assert_eq!(md.one_shot_clip_phase(0, 30, 99), (0, 0, 30 * 256));
    assert_eq!(md.clip_end_pose(0), (0, 0, 30 * 256));
    let reference = hma_dec::ModelView::new(&e.bytes);
    for (pos, mouth) in [
        (0u32, 0u8),
        (7 * 256 + 100, 0),
        (12 * 256, 64),
        (29 * 256 + 3, 21),
    ] {
        let mut scratch = [psx_asset::hma1::Aff::ZERO; 8];
        let pose = md.pose(0, 0, pos, mouth, &mut scratch);
        let j = psx_asset::hma1::Jaw::open(1, false, jaw.open, mouth);
        let rj = (j.bone != usize::MAX).then_some((j.bone, j.post, j.q));
        let mut b = [hma_dec::Affine::default(); 3];
        reference.decode_with(&reference.clip(0), pos, true, rj, &mut b);
        for k in 0..3 {
            let t = pose.bone(k, false, mouth);
            assert_eq!(t.rotation.m, b[k].r, "bone {k} pos {pos}");
            let q = b[k].t.map(|v| ((v + 2) >> 2) as i16);
            assert_eq!(
                [t.translation.x, t.translation.y, t.translation.z],
                q,
                "bone {k} pos {pos}"
            );
        }
    }
    // too little scratch: the bind palette, never a partial decode
    let mut small = [psx_asset::hma1::Aff::ZERO; 2];
    assert!(matches!(
        md.pose(0, 0, 0, 0, &mut small),
        psx_asset::hmd8::Pose::Palette(_)
    ));
}

#[test]
fn hmd8_rejects_a_jaw_outside_the_tracks() {
    let sk = skeleton();
    let bones = needed_bones(&sk.parents, &[true, true, true]);
    let e = encode(&sk, &[&Swing], &bones, production_opts(0.5));
    let jaw = psx_anim_cook::JawRecord {
        bone: 9,
        post: false,
        open: jaw_q(30.0),
    };
    let md = psx_asset::hmd8::Model::load(hmd8_with_tracks(&e.bytes, Some(jaw), 15));
    assert_eq!(md.n_ranges, 0, "a bad section must fail the whole model");
}
