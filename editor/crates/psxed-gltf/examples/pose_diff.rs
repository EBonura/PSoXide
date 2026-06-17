//! Cook the run clip two ways -- NATIVE (the clip file as its own model) vs
//! RETARGET (clip as a pack on the Enemy_01 model) -- and diff the per-joint
//! rotation matrices frame by frame. If they match, the retarget is faithful
//! and the "wrong animation" is elsewhere; if they diverge, the retarget is
//! altering the pose despite the matching bind.
//!   cargo run -p psxed-gltf --example pose_diff

use std::path::PathBuf;
use psxed_gltf::{convert_fbx_rigid_model_path, convert_fbx_rigid_model_path_with_animation_paths, CookedClip, RigidModelConfig};

fn home(p: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join("Downloads").join(p)
}

// pick the clip with the most frames (the run, ~56), skipping the 17-frame junk.
fn run_clip(clips: &[CookedClip]) -> &CookedClip {
    clips.iter().max_by_key(|c| c.frames).unwrap()
}

// decode pose records: returns (joint_count, frame_count, Vec of per-record 9 matrix i16)
fn decode(bytes: &[u8]) -> (usize, usize, Vec<[i16; 9]>) {
    let g = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]) as usize;
    let jc = g(12);
    let fc = g(14);
    let base = 20;
    let rec = 24;
    let mut recs = Vec::new();
    for i in 0..jc * fc {
        let o = base + i * rec;
        let mut m = [0i16; 9];
        for k in 0..9 {
            m[k] = i16::from_le_bytes([bytes[o + k * 2], bytes[o + k * 2 + 1]]);
        }
        recs.push(m);
    }
    (jc, fc, recs)
}

fn main() {
    let model = home("Enemy_01_AnimTest01 (1).fbx");
    let run = home("Drunk Run Forward.fbx");
    let names = ["Hips","Spine","Spine1","Spine2","Neck","Head","LSh","LArm","LFA","LHand","RSh","RArm","RFA","RHand","LUpLeg","LLeg","LFoot","LToe","RUpLeg","RLeg","RFoot","RToe","RToeEnd"];

    let native_cfg = RigidModelConfig { force_single_bind: true, animation_fps: 30, ignore_embedded_animations: false, strip_animation_scale: true, ..Default::default() };
    let retarget_cfg = RigidModelConfig { force_single_bind: true, animation_fps: 30, ignore_embedded_animations: true, ..Default::default() };
    let noscale_cfg = RigidModelConfig { strip_animation_scale: false, ..native_cfg.clone() };

    let native = convert_fbx_rigid_model_path(&run, &native_cfg).unwrap();
    let retarget = convert_fbx_rigid_model_path_with_animation_paths(&model, &[run.clone()], &retarget_cfg).unwrap();
    let noscale = convert_fbx_rigid_model_path(&run, &noscale_cfg).unwrap();

    // does keeping scale change the pose? (i.e. did Mixamo bake joint scale we strip?)
    {
        let a = run_clip(&native.clips);
        let b = run_clip(&noscale.clips);
        let (jc, fc, ra) = decode(&a.bytes);
        let (_, _, rb) = decode(&b.bytes);
        let mut md = 0i32;
        for i in 0..jc * fc { for k in 0..9 { md = md.max((ra[i][k] as i32 - rb[i][k] as i32).abs()); } }
        println!("strip-scale vs keep-scale max matrix diff = {md} (q12; >40 means scale is present & stripped)\n");
    }

    let nc = run_clip(&native.clips);
    let rc = run_clip(&retarget.clips);
    println!("native run clip '{}' {} frames | retarget run clip '{}' {} frames", nc.sanitized_name, nc.frames, rc.sanitized_name, rc.frames);

    let (njc, nfc, nrec) = decode(&nc.bytes);
    let (rjc, rfc, rrec) = decode(&rc.bytes);
    println!("native joints={njc} frames={nfc} | retarget joints={rjc} frames={rfc}");
    if njc != rjc { println!("JOINT COUNT MISMATCH"); return; }
    let frames = nfc.min(rfc);

    // per-joint max abs matrix-element difference across frames (q12 units, 4096 = 1.0)
    let mut maxdiff = vec![0i32; njc];
    for f in 0..frames {
        for j in 0..njc {
            let n = nrec[f * njc + j];
            let r = rrec[f * njc + j];
            for k in 0..9 {
                maxdiff[j] = maxdiff[j].max((n[k] as i32 - r[k] as i32).abs());
            }
        }
    }
    println!("\nper-joint max rotation-matrix-element diff (q12; 4096=1.0; ~40 = 1%):");
    for j in 0..njc {
        let nm = names.get(j).copied().unwrap_or("?");
        let bar = "#".repeat((maxdiff[j] / 100).min(60) as usize);
        println!("  {:<8} {:>6}  {}", nm, maxdiff[j], bar);
    }
}
