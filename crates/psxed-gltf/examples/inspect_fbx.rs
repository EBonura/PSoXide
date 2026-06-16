//! Throwaway FBX inspector for validating commissioned model deliveries.
//! Dumps skeleton/joint hierarchy, mesh stats, animation takes, and the
//! cooked RigidModelReport. Run:
//!   cargo run -p psxed-gltf --example inspect_fbx -- /path/to/model.fbx

use std::collections::HashMap;
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).expect("usage: inspect_fbx <file.fbx>");
    let path = Path::new(&path);
    let filename = path.to_string_lossy();

    let scene = ufbx::load_file(
        &filename,
        ufbx::LoadOpts {
            target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
            target_unit_meters: 1.0,
            clean_skin_weights: true,
            generate_missing_normals: true,
            load_external_files: true,
            ignore_missing_external_files: true,
            filename: ufbx::StringOpt::Ref(&filename),
            ..Default::default()
        },
    )
    .expect("failed to load FBX");

    // Node index map for parent name lookup.
    let mut idx: HashMap<usize, usize> = HashMap::new();
    for (i, n) in scene.nodes.iter().enumerate() {
        idx.insert(n.as_ref() as *const ufbx::Node as usize, i);
    }
    let name_of = |i: usize| -> String {
        scene.nodes[i].element.name.to_string()
    };

    println!("=== SCENE ===");
    println!("total nodes: {}", scene.nodes.len());
    println!("meshes: {}", scene.nodes.iter().filter(|n| n.mesh.is_some()).count());
    println!("anim stacks (takes): {}", scene.anim_stacks.len());

    // --- Mesh + skin ---
    let mut deform_bones: Vec<usize> = Vec::new();
    let mut total_tris = 0usize;
    let mut total_verts = 0usize;
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for node in scene.nodes.iter() {
        let Some(mesh) = node.mesh.as_deref() else { continue };
        if mesh.num_vertices == 0 || mesh.num_faces == 0 { continue }
        total_verts += mesh.num_vertices;
        let mut tri = Vec::new();
        for face in mesh.faces.iter().copied() {
            tri.clear();
            ufbx::triangulate_face_vec(&mut tri, mesh, face);
            total_tris += tri.len() / 3;
        }
        // AABB in geometry space
        for i in 0..mesh.vertex_position.values.len() {
            let v = mesh.vertex_position.values[i];
            let p = [v.x as f32, v.y as f32, v.z as f32];
            for a in 0..3 { min[a] = min[a].min(p[a]); max[a] = max[a].max(p[a]); }
        }
        if let Some(skin) = mesh.skin_deformers.as_ref().first() {
            for cluster in &skin.clusters {
                if let Some(bone) = cluster.bone_node.as_deref() {
                    if let Some(&bi) = idx.get(&(bone as *const ufbx::Node as usize)) {
                        deform_bones.push(bi);
                    }
                }
            }
        }
    }

    // --- Weight / influence histogram (single-bind vs smooth skin) ---
    let mut infl_hist = [0usize; 6]; // index = #influences (>0 weight), clamp 5
    let mut max_infl = 0usize;
    for node in scene.nodes.iter() {
        let Some(mesh) = node.mesh.as_deref() else { continue };
        let Some(skin) = mesh.skin_deformers.as_ref().first() else { continue };
        for sv in skin.vertices.iter() {
            let begin = sv.weight_begin as usize;
            let end = begin + sv.num_weights as usize;
            let n = skin.weights.get(begin..end).unwrap_or(&[])
                .iter().filter(|w| w.weight > 0.0).count();
            max_infl = max_infl.max(n);
            infl_hist[n.min(5)] += 1;
        }
    }
    println!("\n=== SKIN WEIGHTS (influences per vertex) ===");
    println!("max influences on any vertex: {max_infl}");
    for n in 0..=5 {
        let label = if n == 5 { "5+".to_string() } else { n.to_string() };
        if infl_hist[n] > 0 { println!("  {label} influences: {} verts", infl_hist[n]); }
    }
    if max_infl <= 1 { println!("  => PURE RIGID BIND (1 bone/vertex) -- ideal for PS1"); }
    else { println!("  => smooth skin (multi-weight); importer approximates to rigid parts"); }

    println!("\n=== MESH ===");
    println!("vertices: {total_verts}");
    println!("triangles: {total_tris}");
    println!("AABB min: [{:.3}, {:.3}, {:.3}]", min[0], min[1], min[2]);
    println!("AABB max: [{:.3}, {:.3}, {:.3}]", max[0], max[1], max[2]);
    println!("size (x,y,z): [{:.3}, {:.3}, {:.3}]", max[0]-min[0], max[1]-min[1], max[2]-min[2]);
    println!("center: [{:.3}, {:.3}, {:.3}]",
        (min[0]+max[0])/2.0, (min[1]+max[1])/2.0, (min[2]+max[2])/2.0);
    println!("(up axis = Y; height = Y size; floor contact wants min.Y ~ 0)");

    // --- Skeleton ---
    println!("\n=== SKELETON (deform joints = skin-cluster bones) ===");
    println!("deform joint count: {}", deform_bones.len());
    println!("(engine target 20-24, hard cap JOINT_CAP=32)");
    let ik_markers = ["ik", "ctrl", "pole", "target", "heel", "roll", "twist", "pv"];
    let mut ik_hits = Vec::new();
    for &bi in deform_bones.iter() {
        let parent = scene.nodes[bi].parent.as_deref()
            .and_then(|p| idx.get(&(p as *const ufbx::Node as usize)).copied())
            .map(|pi| name_of(pi))
            .unwrap_or_else(|| "(root)".into());
        let nm = name_of(bi);
        let low = nm.to_ascii_lowercase();
        if ik_markers.iter().any(|m| low.contains(m)) { ik_hits.push(nm.clone()); }
        println!("  {nm:32} <- {parent}");
    }
    // Also scan ALL nodes for IK/control bones that aren't deform joints.
    println!("\n=== IK / CONTROL BONE SCAN (all nodes) ===");
    let mut any_ik = false;
    for node in scene.nodes.iter() {
        let nm = node.element.name.to_string();
        let low = nm.to_ascii_lowercase();
        if ik_markers.iter().any(|m| low.contains(m)) {
            let deform = idx.get(&(node.as_ref() as *const ufbx::Node as usize))
                .map(|i| deform_bones.contains(i)).unwrap_or(false);
            println!("  {nm:32} deform_joint={deform}");
            any_ik = true;
        }
    }
    if !any_ik { println!("  none found (clean: no IK/control bone name markers)"); }
    if !ik_hits.is_empty() {
        println!("  WARNING: {} deform joints look like IK/control bones", ik_hits.len());
    }

    // --- Takes ---
    println!("\n=== ANIMATION TAKES ===");
    if scene.anim_stacks.is_empty() {
        println!("  (no anim stacks -- importer will emit a 1-frame bind_pose clip)");
    }
    for stack in scene.anim_stacks.iter() {
        let dur = stack.time_end - stack.time_begin;
        println!("  '{}'  [{:.3}s .. {:.3}s]  dur={:.3}s  ~{} frames @15Hz",
            stack.element.name, stack.time_begin, stack.time_end, dur,
            (dur * 15.0).round() as i64 + 1);
    }

    // --- Cook with engine defaults ---
    // Compare cook configs to isolate missing-poly / tearing causes.
    println!("\n=== COOK CONFIG COMPARISON (source faces above) ===");
    println!("{:<28}{:>8}{:>8}{:>8}{:>10}", "config", "verts", "faces", "parts", "blendV");
    for (label, prune, rigid) in [
        ("rigid + prune=4 (current)", 4u16, true),
        ("rigid + prune=0 (no prune)", 0, true),
        ("blend + prune=4", 4, false),
        ("blend + prune=0", 0, false),
    ] {
        let c = psxed_gltf::RigidModelConfig {
            force_single_bind: rigid,
            prune_detached_face_islands: prune,
            ..Default::default()
        };
        match psxed_gltf::convert_fbx_rigid_model_path(path, &c) {
            Ok(p) => {
                let m = &p.model;
                let vc = u16::from_le_bytes([m[16], m[17]]) as usize;
                let jc = u16::from_le_bytes([m[12], m[13]]) as usize;
                let pc = u16::from_le_bytes([m[14], m[15]]) as usize;
                let mc = u16::from_le_bytes([m[20], m[21]]) as usize;
                let voff = 12 + 16 + jc * 4 + mc * 8 + pc * 16;
                let blendv = (0..vc).filter(|i| m[voff + i * 8 + 7] != 0).count();
                println!("{:<28}{:>8}{:>8}{:>8}{:>10}", label, p.report.cooked_vertices, p.report.faces, p.report.parts, blendv);
            }
            Err(e) => println!("{label}: ERR {e}"),
        }
    }

    let force = std::env::args().any(|a| a == "--single-bind");
    println!("\n=== COOK (128x128 8bpp, 15Hz, force_single_bind={force}) ===");
    let cfg = psxed_gltf::RigidModelConfig { force_single_bind: force, ..Default::default() };
    match psxed_gltf::convert_fbx_rigid_model_path(path, &cfg) {
        Ok(pkg) => {
            let r = &pkg.report;
            println!("  joints: {}", r.joints);
            println!("  source verts: {}  cooked verts: {}  faces: {}  parts: {}",
                r.source_vertices, r.cooked_vertices, r.faces, r.parts);
            println!("  local_height: {}  local_to_world_q12: {}", r.local_height, r.local_to_world_q12);
            println!("  model bytes: {}  texture bytes: {}  anim bytes: {}",
                r.model_bytes, r.texture_bytes, r.animation_bytes);
            println!("  clips ({}):", r.clip_frames.len());
            for (name, frames) in &r.clip_frames {
                println!("    {name:40} {frames} frames");
            }
            // Decode cooked flags + count CPU-blend vertices (record[7]!=0).
            let m = &pkg.model;
            let flags = u16::from_le_bytes([m[6], m[7]]);
            let rigid = flags & (1 << 2) != 0;
            let blend = flags & (1 << 3) != 0;
            let jc = u16::from_le_bytes([m[12], m[13]]) as usize;
            let pc = u16::from_le_bytes([m[14], m[15]]) as usize;
            let vc = u16::from_le_bytes([m[16], m[17]]) as usize;
            let mc = u16::from_le_bytes([m[20], m[21]]) as usize;
            let vtx_off = 12 + 16 + jc * 4 + mc * 8 + pc * 16;
            let mut blend_verts = 0usize;
            for i in 0..vc { if m[vtx_off + i * 8 + 7] != 0 { blend_verts += 1; } }
            println!("  flags: RIGID_SKINNED={rigid} BLEND_SKIN={blend}");
            println!("  CPU-blend vertices (2-bone): {blend_verts} / {vc}  ({} pure single-bind)", vc - blend_verts);
            if r.joints > 32 {
                println!("  *** ERROR: {} joints exceeds JOINT_CAP=32; renderer will drop the extras ***", r.joints);
            }
        }
        Err(e) => println!("  COOK FAILED: {e}"),
    }
}
