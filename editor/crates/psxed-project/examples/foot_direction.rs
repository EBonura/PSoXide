//! Which way does a clip walk, in the model's own frame?
//! usage: foot_direction <model.psxmdl> <clip.psxanim> <foot_joint> <shin_joint>
//! Prints the model's forward (shin-bottom -> foot-centroid, bind pose) and
//! the posed foot centroid per frame; the planted foot should travel AGAINST forward.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mb = std::fs::read(&a[0]).unwrap();
    let ab = std::fs::read(&a[1]).unwrap();
    let model = psx_asset::Model::from_bytes(&mb).unwrap();
    let anim = psx_asset::Animation::from_bytes(&ab).unwrap();
    let foot: u16 = a[2].parse().unwrap();
    let shin: u16 = a[3].parse().unwrap();
    let verts_of = |joint: u16| -> Vec<[f64; 3]> {
        let mut out = Vec::new();
        for p in 0..u16::MAX {
            let Some(part) = model.part(p) else { break };
            if part.joint_index() != joint {
                continue;
            }
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let pos = model.vertex(v).unwrap().position;
                out.push([pos.x as f64, pos.y as f64, pos.z as f64]);
            }
        }
        out
    };
    let fv = verts_of(foot);
    let sv = verts_of(shin);
    let cen = |vs: &Vec<[f64; 3]>| {
        let n = vs.len() as f64;
        [
            vs.iter().map(|v| v[0]).sum::<f64>() / n,
            vs.iter().map(|v| v[1]).sum::<f64>() / n,
            vs.iter().map(|v| v[2]).sum::<f64>() / n,
        ]
    };
    let fc = cen(&fv);
    if std::env::var("DUMP_FOOT").is_ok() {
        let mut sorted = fv.clone();
        sorted.sort_by(|a, b| a[2].partial_cmp(&b[2]).unwrap());
        for v in &sorted {
            println!("  foot vert x={:6.0} y={:7.0} z={:6.0}", v[0], v[1], v[2]);
        }
        let mut ss = sv.clone();
        ss.sort_by(|a, b| a[1].partial_cmp(&b[1]).unwrap());
        for v in &ss {
            println!("  shin vert x={:6.0} y={:7.0} z={:6.0}", v[0], v[1], v[2]);
        }
    }
    // Which way is up? Head-ish parts vs feet: use the y sign of foot vs shin centroids.
    let sc = cen(&sv);
    let up_is_neg_y = fc[1] > sc[1]; // foot below shin
    println!("foot verts {} shin verts {}  foot centroid ({:.0},{:.0},{:.0}) shin centroid ({:.0},{:.0},{:.0})  Y-down={}",
        fv.len(), sv.len(), fc[0], fc[1], fc[2], sc[0], sc[1], sc[2], up_is_neg_y);
    // ankle ~ lowest shin vertex; forward ~ foot centroid - ankle, horizontal
    let ankle = sv.iter().copied().fold(sv[0], |acc, v| {
        if (up_is_neg_y && v[1] > acc[1]) || (!up_is_neg_y && v[1] < acc[1]) {
            v
        } else {
            acc
        }
    });
    let fwd = [fc[0] - ankle[0], fc[2] - ankle[2]];
    let l = (fwd[0] * fwd[0] + fwd[1] * fwd[1]).sqrt();
    println!(
        "ankle ({:.0},{:.0},{:.0})  forward (bind, xz) = ({:+.2}, {:+.2})",
        ankle[0],
        ankle[1],
        ankle[2],
        fwd[0] / l,
        fwd[1] / l
    );
    println!(
        "frames={} rate={:?}",
        anim.frame_count(),
        anim.sample_rate_hz()
    );
    let mut prev: Option<[f64; 3]> = None;
    for f in 0..anim.frame_count() {
        let p = anim.pose(f, foot).unwrap();
        let m = p.matrix; // column-major Q3.12
        let x = (m[0][0] as f64 * fc[0] + m[1][0] as f64 * fc[1] + m[2][0] as f64 * fc[2]) / 4096.0
            + p.translation.x as f64;
        let y = (m[0][1] as f64 * fc[0] + m[1][1] as f64 * fc[1] + m[2][1] as f64 * fc[2]) / 4096.0
            + p.translation.y as f64;
        let z = (m[0][2] as f64 * fc[0] + m[1][2] as f64 * fc[1] + m[2][2] as f64 * fc[2]) / 4096.0
            + p.translation.z as f64;
        let mut line = format!("{f:3}  foot ({x:8.0},{y:8.0},{z:8.0})");
        if let Some(q) = prev {
            let dx = x - q[0];
            let dz = z - q[2];
            let along = dx * fwd[0] / l + dz * fwd[1] / l;
            line += &format!("  d.along_forward={along:+7.0}");
        }
        prev = Some([x, y, z]);
        println!("{line}");
    }
}
