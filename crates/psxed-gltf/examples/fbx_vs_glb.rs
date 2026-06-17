//! Cook the run from the FBX (ufbx read) vs from the SDK-produced glb (our glTF
//! read) and diff the joint rotation matrices. The glb (FBX2glTF, Autodesk SDK)
//! is the known-correct reference. If the FBX cook diverges, the bug is in how
//! ufbx reads the FBX animation.
//!   cargo run -p psxed-gltf --example fbx_vs_glb

use std::path::PathBuf;
use psxed_gltf::{convert_fbx_rigid_model_path, convert_rigid_model_path, CookedClip, RigidModelConfig};

fn run_clip(clips: &[CookedClip]) -> &CookedClip {
    clips.iter().max_by_key(|c| c.frames).unwrap()
}

fn decode(b: &[u8]) -> (usize, usize, Vec<[i16; 9]>) {
    let g = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let (jc, fc) = (g(12), g(14));
    let mut r = Vec::new();
    for i in 0..jc * fc {
        let o = 20 + i * 24;
        let mut m = [0i16; 9];
        for k in 0..9 { m[k] = i16::from_le_bytes([b[o + k * 2], b[o + k * 2 + 1]]); }
        r.push(m);
    }
    (jc, fc, r)
}

fn main() {
    let fbx = PathBuf::from(std::env::var("HOME").unwrap()).join("Downloads/Drunk Run Forward.fbx");
    let glb = PathBuf::from("/tmp/drunkrun.glb");
    let cfg = RigidModelConfig { force_single_bind: true, animation_fps: 30, ignore_embedded_animations: false, ..Default::default() };
    let names = ["Hips","Spine","Spine1","Spine2","Neck","Head","LSh","LArm","LFA","LHand","RSh","RArm","RFA","RHand","LUpLeg","LLeg","LFoot","LToe","RUpLeg","RLeg","RFoot","RToe","RToeEnd"];

    let fpkg = convert_fbx_rigid_model_path(&fbx, &cfg).unwrap();
    let gpkg = convert_rigid_model_path(&glb, &cfg).unwrap();
    let fc = run_clip(&fpkg.clips);
    let gc = run_clip(&gpkg.clips);
    let (fjc, ffc, fr) = decode(&fc.bytes);
    let (gjc, gfc, gr) = decode(&gc.bytes);
    println!("FBX run '{}' joints={fjc} frames={ffc} | GLB run '{}' joints={gjc} frames={gfc}", fc.sanitized_name, gc.sanitized_name);
    if fjc != gjc { println!("joint count differs -> skeletons differ, can't index-align"); }

    let jc = fjc.min(gjc);
    let frames = ffc.min(gfc);
    let mut md = vec![0i32; jc];
    for f in 0..frames {
        for j in 0..jc {
            let a = fr[f * fjc + j]; let b = gr[f * gjc + j];
            for k in 0..9 { md[j] = md[j].max((a[k] as i32 - b[k] as i32).abs()); }
        }
    }
    println!("\nper-joint max rotation diff FBX-vs-GLB (q12; 4096=1.0; ~40=1%):");
    for j in 0..jc {
        let nm = names.get(j).copied().unwrap_or("?");
        let bar = "#".repeat((md[j] / 200).min(40) as usize);
        println!("  {:<8} {:>6}  {}", nm, md[j], bar);
    }
}
