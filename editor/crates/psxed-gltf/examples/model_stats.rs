//! Throwaway: report what the engine's own parsers see in a cooked bundle.
fn main() {
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).expect("read");
        let name = std::path::Path::new(&path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        if path.ends_with(".psxmdl") {
            match psx_asset::Model::from_bytes(&bytes) {
                Ok(m) => {
                    let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
                    for v in 0..m.vertex_count() {
                        let p = m.vertex(v).unwrap().position;
                        for (a, value) in [p.x, p.y, p.z].iter().enumerate() {
                            lo[a] = lo[a].min(i32::from(*value));
                            hi[a] = hi[a].max(i32::from(*value));
                        }
                    }
                    println!(
                        "{name}: OK verts={} faces={} parts={} joints={} span x={} y={} z={}",
                        m.vertex_count(), m.face_count(), m.part_count(), m.joint_count(),
                        hi[0]-lo[0], hi[1]-lo[1], hi[2]-lo[2]
                    );
                }
                Err(e) => println!("{name}: FAILED {e:?}"),
            }
        } else if path.ends_with(".psxanim") {
            match psx_asset::Animation::from_bytes(&bytes) {
                Ok(a) => {
                    let mut worst = 0i32;
                    for f in 0..a.frame_count() {
                        for j in 0..a.joint_count() {
                            if let Some(p) = a.pose(f, j) {
                                for v in [p.translation.x, p.translation.y, p.translation.z] {
                                    worst = worst.max(v.abs());
                                }
                            }
                        }
                    }
                    println!(
                        "  {name}: OK frames={} joints={} max|translation|={worst}",
                        a.frame_count(), a.joint_count()
                    );
                }
                Err(e) => println!("  {name}: FAILED {e:?}"),
            }
        }
    }
}
