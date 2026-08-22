//! Orthographic SIDE VIEW of a cooked model+clip frame at the runtime's
//! placement: origin = floor + apply(clip floor lift), vertex = origin +
//! apply(pose * v), blend-weighted like the runtime. Writes a PPM: horizontal
//! axis = model Z (facing) or X, vertical = world Y, floor line drawn at y=0.
//! No camera, no perspective: a vertex on the floor line IS on the floor.
//!
//! usage: side_view <model.psxmdl> <clip.psxanim> <visual_scale_q8> <frame> <axis x|z> <out.ppm>
use psxed_project::playtest::{model_bounds_joint_transform, transform_model_bounds_vertex};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mb = std::fs::read(&a[0]).unwrap();
    let cb = std::fs::read(&a[1]).unwrap();
    let scale_q8: u32 = a[2].parse().unwrap();
    let frame: u16 = a[3].parse().unwrap();
    let axis = if a[4] == "x" { 0 } else { 2 };
    let out = &a[5];
    let model = psx_asset::Model::from_bytes(&mb).unwrap();
    let clip = psx_asset::Animation::from_bytes(&cb).unwrap();
    let composed = ((model.local_to_world_q12() as u32 * scale_q8 + 128) / 256).clamp(1, 65535);
    let apply = |v: i32| ((v as i64 * composed as i64) >> 12) as i32;
    // The clip's own cooked floor (frame 0 lowest raw vertex), like the runtime.
    let mut clip_floor = i32::MAX;
    let posed = |frame: u16, out: &mut Vec<[i32; 3]>| {
        for part_index in 0..model.part_count() {
            let part = model.part(part_index).unwrap();
            let Some(pose) = clip.pose(frame, part.joint_index() as u16) else {
                continue;
            };
            let t = model_bounds_joint_transform(pose, 0x1000);
            for v in part.first_vertex()..part.first_vertex() + part.vertex_count() {
                let vertex = model.vertex(v).unwrap();
                let mut p = transform_model_bounds_vertex(t, vertex);
                if vertex.is_blend() {
                    if let Some(second) = clip.pose(frame, vertex.joint1 as u16) {
                        let t2 = model_bounds_joint_transform(second, 0x1000);
                        let q = transform_model_bounds_vertex(t2, vertex);
                        let w = vertex.blend as i64;
                        for k in 0..3 {
                            p[k] = (((p[k] as i64) * (256 - w) + (q[k] as i64) * w) >> 8) as i32;
                        }
                    }
                }
                out.push(p);
            }
        }
    };
    let mut f0 = Vec::new();
    posed(0, &mut f0);
    for p in &f0 {
        clip_floor = clip_floor.min(p[1]);
    }
    let lift = apply(-clip_floor);
    let mut pts = Vec::new();
    posed(frame, &mut pts);
    // World-space (floor at y = 0): y_world = lift + apply(y_model)
    let world: Vec<(i32, i32)> = pts
        .iter()
        .map(|p| (apply(p[axis]), lift + apply(p[1])))
        .collect();
    let lowest = world.iter().map(|w| w.1).min().unwrap();
    let highest = world.iter().map(|w| w.1).max().unwrap();
    println!(
        "frame {frame}: clip_floor_model={clip_floor} lift={lift} lowest_world_y={lowest} highest={highest} (floor is 0)"
    );

    // Render: 4 px per world unit, y up, floor line, origin cross.
    const PX: i32 = 4;
    let (w, h) = (100 * PX, 100 * PX);
    let mut img = vec![30u8; (w * h * 3) as usize];
    let put = |img: &mut Vec<u8>, x: i32, y: i32, c: [u8; 3]| {
        if x >= 0 && x < w && y >= 0 && y < h {
            let i = ((y * w + x) * 3) as usize;
            img[i..i + 3].copy_from_slice(&c);
        }
    };
    let to_px = |wx: i32, wy: i32| ((50 + wx) * PX, (h - 1) - (10 + wy) * PX);
    // floor line and a 10-unit grid
    for x in 0..w {
        let (_, fy) = to_px(0, 0);
        put(&mut img, x, fy, [220, 60, 60]);
        for g in (10..90).step_by(10) {
            let (_, gy) = to_px(0, g);
            put(&mut img, x, gy, [55, 55, 55]);
        }
    }
    for (wx, wy) in &world {
        let (px, py) = to_px(*wx, *wy);
        for dx in 0..PX {
            for dy in 0..PX {
                put(&mut img, px + dx, py + dy, [200, 220, 255]);
            }
        }
    }
    let (ox, oy) = to_px(0, lift);
    for d in -6..=6 {
        put(&mut img, ox + d, oy, [255, 200, 0]);
        put(&mut img, ox, oy + d, [255, 200, 0]);
    }
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    bytes.extend_from_slice(&img);
    std::fs::write(out, bytes).unwrap();
}
