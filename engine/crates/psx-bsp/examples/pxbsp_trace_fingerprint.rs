//! Frozen before/after oracle fingerprint for complete mixed-Q12 point/body traces.
use psx_bsp::collision::{Trace, TraceScratch};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).unwrap();
    let mut map = PxbspResidentMap::default();
    map.load(0, &mut SliceReader::new(&bytes)).unwrap();
    let world = map.brush_models().get(0).unwrap();
    let lo = [world.mins.x, world.mins.y, world.mins.z].map(i32::from);
    let hi = [world.maxs.x, world.maxs.y, world.maxs.z].map(i32::from);
    for hull_index in [0, 1, 2] {
        let Some(hull) = map.model_collision_hull(0, hull_index) else {
            continue;
        };
        let mut rng = 0xdefa_4311u32;
        let mut hash = 0xcbf29ce484222325u64;
        let mut scratch = TraceScratch::new();
        let mut successes = 0;
        for index in 0..30000 {
            let mut point = || {
                let mut v = [0; 3];
                for axis in 0..3 {
                    rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    v[axis] = (lo[axis] + (rng % (hi[axis] - lo[axis] + 1) as u32) as i32) * 4096;
                    if index % 3 == 1 {
                        v[axis] = v[axis].wrapping_add((rng >> 20) as i32);
                    }
                    if index % 97 == 0 {
                        v[axis] = match axis {
                            0 => i32::MIN,
                            1 => i32::MAX & !4095,
                            _ => -4096,
                        };
                    }
                }
                Vec3I32 {
                    x: v[0],
                    y: v[1],
                    z: v[2],
                }
            };
            let a = point();
            let b = point();
            let mut trace = Trace::default();
            let ok = hull.trace_into(&a, &b, &mut scratch, &mut trace);
            successes += usize::from(ok);
            let boolean = hull.segment_obstructed(&a, &b, &mut scratch);
            let witness = hull.segment_obstruction_witness(&a, &b, &mut scratch);
            for byte in format!("{ok:?}:{trace:?}:{boolean:?}:{witness:?}").bytes() {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            }
        }
        println!("hull={hull_index} rays=30000 successful={successes} fingerprint={hash:016x}");
    }
}
