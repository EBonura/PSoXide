//! Per-clip and per-model statistics over a directory written by `anim_dump`.
//! Usage: anim_stats <dump-dir>

use std::collections::HashSet;
use std::path::PathBuf;

use psx_asset::{Animation, Model};

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("dump dir"));
    let mut models: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    models.sort();
    let mut grand = 0usize;
    for mdir in models {
        let mbytes = std::fs::read(mdir.join("model.psxmdl")).unwrap();
        let model = Model::from_bytes(&mbytes).unwrap();
        let (mut lo, mut hi) = ([i32::MAX; 3], [i32::MIN; 3]);
        for v in 0..model.vertex_count() {
            let p = model.vertex(v).unwrap().position;
            for (i, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[i] = lo[i].min(c as i32);
                hi[i] = hi[i].max(c as i32);
            }
        }
        println!(
            "MODEL {} joints={} verts={} faces={} parts={} l2w_q12={} extent=({},{},{})",
            mdir.file_name().unwrap().to_string_lossy(),
            model.joint_count(),
            model.vertex_count(),
            model.face_count(),
            model.part_count(),
            model.local_to_world_q12(),
            hi[0] - lo[0],
            hi[1] - lo[1],
            hi[2] - lo[2]
        );
        let mut clips: Vec<_> = std::fs::read_dir(&mdir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "psxanim"))
            .collect();
        clips.sort();
        let mut total = 0usize;
        println!("  clip                                    ver  hz frames joints  bytes  uniq/poses  static_joints");
        for c in clips {
            let b = std::fs::read(&c).unwrap();
            let a = Animation::from_bytes(&b).unwrap();
            let ver = u16::from_le_bytes([b[4], b[5]]);
            let (j, f) = (a.joint_count(), a.frame_count());
            let mut uniq: HashSet<Vec<i32>> = HashSet::new();
            let mut statics = 0;
            for jj in 0..j {
                let first = a.pose(0, jj).unwrap();
                let mut moving = false;
                for ff in 0..f {
                    let p = a.pose(ff, jj).unwrap();
                    let mut key: Vec<i32> = p.matrix.iter().flatten().map(|&x| x as i32).collect();
                    key.extend([p.translation.x, p.translation.y, p.translation.z]);
                    uniq.insert(key);
                    if p != first {
                        moving = true;
                    }
                }
                if !moving {
                    statics += 1;
                }
            }
            total += b.len().next_multiple_of(4);
            println!(
                "  {:<40} v{} {:>3} {:>5} {:>5} {:>7}  {:>5}/{:<5}  {}",
                c.file_name().unwrap().to_string_lossy(),
                ver,
                a.sample_rate_hz(),
                f,
                j,
                b.len(),
                uniq.len(),
                j as usize * f as usize,
                statics
            );
        }
        println!("  TOTAL clips bytes (4-aligned): {total}");
        grand += total;
    }
    println!("GRAND {grand}");
}
