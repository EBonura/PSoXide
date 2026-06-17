//! Compare two FBX skeletons' BIND poses bone-by-bone, matched by name suffix
//! (after the last ':'). Prints the angular difference of each bone's LOCAL and
//! GLOBAL rest rotation. Large LOCAL + small GLOBAL => same physical pose,
//! different bone-axis convention (retargetable in world space). Large GLOBAL
//! => genuinely different rest pose.
//!   cargo run -p psxed-gltf --example bind_diff -- <a.fbx> <b.fbx>

use std::collections::HashMap;

type Q = [f64; 4];

fn qmul(a: Q, b: Q) -> Q {
    let [ax, ay, az, aw] = a;
    let [bx, by, bz, bw] = b;
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn angle_deg(a: Q, b: Q) -> f64 {
    let dot = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3]).abs().clamp(0.0, 1.0);
    2.0 * dot.acos().to_degrees()
}

fn load(path: &str) -> ufbx::SceneRoot {
    let fname = path.to_string();
    ufbx::load_file(
        &fname,
        ufbx::LoadOpts {
            target_axes: ufbx::CoordinateAxes::right_handed_y_up(),
            target_unit_meters: 1.0,
            filename: ufbx::StringOpt::Ref(&fname),
            ..Default::default()
        },
    )
    .expect("failed to load FBX")
}

fn key(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_ascii_lowercase()
}

/// global rest rotation per node index, accumulated up the parent chain.
fn globals(scene: &ufbx::Scene) -> Vec<Q> {
    let idx: HashMap<usize, usize> = scene
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.as_ref() as *const ufbx::Node as usize, i))
        .collect();
    let mut g = vec![[0.0, 0.0, 0.0, 1.0]; scene.nodes.len()];
    let mut done = vec![false; scene.nodes.len()];
    fn rec(i: usize, scene: &ufbx::Scene, idx: &HashMap<usize, usize>, g: &mut [Q], done: &mut [bool]) -> Q {
        if done[i] {
            return g[i];
        }
        let n = &scene.nodes[i];
        let r = n.local_transform.rotation;
        let local = [r.x, r.y, r.z, r.w];
        let parent = n
            .parent
            .as_ref()
            .and_then(|p| idx.get(&(p.as_ref() as *const ufbx::Node as usize)).copied());
        let gp = match parent {
            Some(p) => rec(p, scene, idx, g, done),
            None => [0.0, 0.0, 0.0, 1.0],
        };
        g[i] = qmul(gp, local);
        done[i] = true;
        g[i]
    }
    for i in 0..scene.nodes.len() {
        rec(i, scene, &idx, &mut g, &mut done);
    }
    g
}

fn main() {
    let mut a = std::env::args().skip(1);
    let fa = a.next().expect("usage: bind_diff <a.fbx> <b.fbx>");
    let fb = a.next().expect("usage: bind_diff <a.fbx> <b.fbx>");
    let sa = load(&fa);
    let sb = load(&fb);
    let ga = globals(&sa);
    let gb = globals(&sb);

    // index file B nodes by key
    let mut bmap: HashMap<String, usize> = HashMap::new();
    for (i, n) in sb.nodes.iter().enumerate() {
        bmap.insert(key(&n.element.name.to_string()), i);
    }

    println!("{:<22} {:>10} {:>10}", "bone", "LOCAL deg", "GLOBAL deg");
    println!("{}", "-".repeat(44));
    let focus = [
        "hips", "spine", "spine1", "spine2", "neck", "head",
        "leftarm", "leftforearm", "lefthand", "rightarm", "rightforearm",
        "leftupleg", "leftleg", "leftfoot", "rightupleg", "rightleg", "rightfoot",
    ];
    for (ia, na) in sa.nodes.iter().enumerate() {
        let k = key(&na.element.name.to_string());
        if !focus.contains(&k.as_str()) {
            continue;
        }
        let Some(&ib) = bmap.get(&k) else { continue };
        let ra = na.local_transform.rotation;
        let rb = sb.nodes[ib].local_transform.rotation;
        let local = angle_deg([ra.x, ra.y, ra.z, ra.w], [rb.x, rb.y, rb.z, rb.w]);
        let global = angle_deg(ga[ia], gb[ib]);
        println!("{:<22} {:>10.1} {:>10.1}", k, local, global);
    }
}
