//! Diagnostic: dump the cooked model's joint table -- index, parent, owned
//! vertex count, and the vertex-averaged local center (the same estimate the
//! bone/hitbox overlay uses). Used to map joint indices to body parts so the
//! hurt-capsule template can target the right bones.
//!   cargo run -p psxed-ui --example joint_dump -- <model.psxmdl>

use psx_asset::Model;

fn main() {
    let path = std::env::args().nth(1).expect("usage: joint_dump <model.psxmdl>");
    let bytes = std::fs::read(&path).expect("read model");
    let model = Model::from_bytes(&bytes).expect("parse model");

    let joint_count = model.joint_count() as usize;
    let mut sums = vec![[0i64; 3]; joint_count];
    let mut counts = vec![0i64; joint_count];
    for part_index in 0..model.part_count() {
        let Some(part) = model.part(part_index) else { continue };
        let joint = part.joint_index() as usize;
        if joint >= joint_count {
            continue;
        }
        let start = part.first_vertex();
        let end = start.saturating_add(part.vertex_count());
        for v in start..end {
            let Some(vertex) = model.vertex(v) else { continue };
            sums[joint][0] += vertex.position.x as i64;
            sums[joint][1] += vertex.position.y as i64;
            sums[joint][2] += vertex.position.z as i64;
            counts[joint] += 1;
        }
    }

    println!("joint_count = {joint_count}");
    println!("{:>3}  {:>6}  {:>6}   {:>8} {:>8} {:>8}", "idx", "parent", "verts", "cx", "cy", "cz");
    for j in 0..joint_count {
        let parent = model
            .joint(j as u16)
            .and_then(|joint| joint.parent())
            .map(|p| p as i32)
            .unwrap_or(-1);
        let (cx, cy, cz) = if counts[j] > 0 {
            (
                (sums[j][0] / counts[j]) as i32,
                (sums[j][1] / counts[j]) as i32,
                (sums[j][2] / counts[j]) as i32,
            )
        } else {
            (0, 0, 0)
        };
        println!("{j:>3}  {parent:>6}  {:>6}   {cx:>8} {cy:>8} {cz:>8}", counts[j]);
    }
}
