//! Dump the bind-pose position bbox of vertices dominated by each skin bone,
//! to see whether a given joint's geometry is flat in some axis at source.
//!   cargo run -p psxed-gltf --example head_check -- <model.fbx>

use std::collections::HashMap;

fn main() {
    let path = std::env::args().nth(1).expect("usage: head_check <file.fbx>");
    let fname = path.clone();
    let scene = ufbx::load_file(
        &fname,
        ufbx::LoadOpts {
            target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
            target_unit_meters: 1.0,
            filename: ufbx::StringOpt::Ref(&fname),
            ..Default::default()
        },
    )
    .expect("load");

    for node in scene.nodes.iter() {
        let Some(mesh) = node.mesh.as_deref() else { continue };
        if mesh.num_vertices == 0 {
            continue;
        }
        let Some(skin) = mesh.skin_deformers.get(0) else {
            println!("mesh has no skin");
            continue;
        };
        // dominant bone per control-point vertex
        let nv = mesh.num_vertices;
        let mut dom: Vec<(i32, f64)> = vec![(-1, 0.0); nv];
        for vi in 0..nv {
            let w = skin.vertices[vi];
            let first = w.weight_begin as usize;
            let cnt = w.num_weights as usize;
            for k in 0..cnt {
                let sw = skin.weights[first + k];
                if sw.weight > dom[vi].1 {
                    let cl = &skin.clusters[sw.cluster_index as usize];
                    dom[vi] = (
                        cl.bone_node
                            .as_ref()
                            .map(|n| n.element.element_id as i32)
                            .unwrap_or(-1),
                        sw.weight,
                    );
                }
            }
        }
        // bone id -> name
        let mut bone_name: HashMap<i32, String> = HashMap::new();
        for cl in skin.clusters.iter() {
            if let Some(bn) = cl.bone_node.as_ref() {
                bone_name.insert(bn.element.element_id as i32, bn.element.name.to_string());
            }
        }
        // bbox per dominant bone, iterating face-corners (robust position read)
        let mut bb: HashMap<i32, ([f64; 3], [f64; 3], usize)> = HashMap::new();
        for ci in 0..mesh.num_indices {
            let cp = mesh.vertex_indices[ci] as usize;
            let b = dom[cp].0;
            let p = mesh.vertex_position[ci];
            let pos = [p.x, p.y, p.z];
            let e = bb.entry(b).or_insert(([f64::MAX; 3], [f64::MIN; 3], 0));
            for a in 0..3 {
                e.0[a] = e.0[a].min(pos[a]);
                e.1[a] = e.1[a].max(pos[a]);
            }
            e.2 += 1;
        }
        let mut keys: Vec<_> = bb.keys().copied().collect();
        keys.sort();
        println!("{:<22}{:>5} {:>10}{:>10}{:>10}", "bone", "n", "Xcenter", "Ycenter", "Zcenter");
        for k in keys {
            let (mn, mx, n) = bb[&k];
            let nm = bone_name.get(&k).cloned().unwrap_or_else(|| format!("id{k}"));
            let nm = nm.rsplit(':').next().unwrap_or(&nm).to_string();
            println!(
                "{:<22}{:>5} {:>10.4}{:>10.4}{:>10.4}",
                nm,
                n,
                (mn[0] + mx[0]) * 0.5,
                (mn[1] + mx[1]) * 0.5,
                (mn[2] + mx[2]) * 0.5
            );
        }
        break;
    }
}
