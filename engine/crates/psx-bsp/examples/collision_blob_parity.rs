//! Compare the cooked collision blob walker against the reference hull tracer
//! on a cooked map.
use psx_bsp::collision::{CollisionHull, Trace, TraceScratch};
use psx_bsp::{ClipNode, Plane, RecordSlice, Vec3I32};
fn hull(x: &(Vec<u8>, Vec<u8>, i16)) -> CollisionHull<'_> {
    CollisionHull::new(
        RecordSlice::<Plane>::new(&x.0).unwrap(),
        RecordSlice::<ClipNode>::new(&x.1).unwrap(),
        x.2,
    )
    .unwrap()
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let load = |prefix: &str| {
        (
            std::fs::read(format!("{prefix}.planes")).unwrap(),
            std::fs::read(format!("{prefix}.nodes")).unwrap(),
            std::fs::read_to_string(format!("{prefix}.heads"))
                .unwrap()
                .trim_matches(|c| c == '[' || c == ']')
                .split(',')
                .next()
                .unwrap()
                .trim()
                .parse::<i16>()
                .unwrap(),
        )
    };
    let a = load(&args[1]);
    let b = load(&args[2]);
    let aa = hull(&a);
    let bb = hull(&b);
    let mut sa = TraceScratch::new();
    let mut sb = TraceScratch::new();
    let mut rng = 0x5a83abf1u32;
    let mut starts_solid = 0;
    let mut all_solid = 0;
    let mut boundary_count = 0;
    let mut hash = 0xcbf29ce484222325u64;
    for i in 0..20000 {
        let mut point = || {
            let mut p = [0; 3];
            for axis in 0..3 {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                p[axis] = ((rng % 4097) as i32 - 2048) * 4096;
                if i % 3 == 1 {
                    p[axis] += ((rng >> 20) & 4095) as i32;
                }
                if i % 97 == 0 {
                    p[axis] = [i32::MIN, i32::MAX, -1][axis];
                }
            }
            Vec3I32 {
                x: p[0],
                y: p[1],
                z: p[2],
            }
        };
        let start = point();
        let mut end = point();
        if i % 5 == 0 {
            end.x = start.x;
            end.z = start.z;
        }
        if i % 101 == 0 {
            end = start;
        }
        let mut ta = Trace::default();
        let mut tb = Trace::default();
        let oa = aa.trace_into(&start, &end, &mut sa, &mut ta);
        let ob = bb.trace_into(&start, &end, &mut sb, &mut tb);
        assert_eq!((oa, ta), (ob, tb), "ray{i} {start:?} {end:?}");
        assert_eq!(aa.point_contents(start), bb.point_contents(start));
        assert_eq!(
            aa.segment_obstructed(&start, &end, &mut sa),
            bb.segment_obstructed(&start, &end, &mut sb)
        );
        if ta.fraction < 4096 || ta.start_solid.is_set() {
            for axis in 0..3 {
                for delta in [-4096i32, -128, -1, 0, 1, 128, 4096] {
                    let mut p = ta.end;
                    let component = match axis {
                        0 => &mut p.x,
                        1 => &mut p.y,
                        _ => &mut p.z,
                    };
                    *component = component.wrapping_add(delta);
                    let mut q = p;
                    let component = match axis {
                        0 => &mut q.x,
                        1 => &mut q.y,
                        _ => &mut q.z,
                    };
                    *component = component.wrapping_sub(delta.saturating_mul(2));
                    let mut x = Trace::default();
                    let mut y = Trace::default();
                    let ox = aa.trace_into(&p, &q, &mut sa, &mut x);
                    let oy = bb.trace_into(&p, &q, &mut sb, &mut y);
                    assert_eq!(
                        (ox, x),
                        (oy, y),
                        "contact ray{i} axis{axis} delta{delta} {p:?} {q:?}"
                    );
                    assert_eq!(aa.point_contents(p), bb.point_contents(p));
                    assert_eq!(
                        aa.segment_obstructed(&p, &q, &mut sa),
                        bb.segment_obstructed(&p, &q, &mut sb)
                    );
                    boundary_count += 1;
                }
            }
        }
        starts_solid += ta.start_solid.is_set() as usize;
        all_solid += ta.all_solid.is_set() as usize;
        for byte in format!("{oa}:{ta:?}").bytes() {
            hash = (hash ^ byte as u64).wrapping_mul(0x100000001b3);
        }
    }
    let mut split_rays = 0;
    for record in b.0.chunks_exact(14) {
        if a.0.chunks_exact(14).any(|r| r == record) {
            continue;
        }
        let axis = i32::from_le_bytes(record[10..14].try_into().unwrap());
        if !(0..3).contains(&axis) {
            continue;
        }
        let distance = i32::from_le_bytes(record[6..10].try_into().unwrap());
        for u in (-2048..=2048).step_by(128) {
            for v in (-2048..=2048).step_by(128) {
                for delta in [-4096i32, -128, -1, 0, 1, 128, 4096] {
                    let mut p = [0; 3];
                    p[axis as usize] = distance.wrapping_add(delta);
                    p[(axis as usize + 1) % 3] = u * 4096;
                    p[(axis as usize + 2) % 3] = v * 4096;
                    let mut q = p;
                    q[axis as usize] = distance.wrapping_sub(delta);
                    let p = Vec3I32 {
                        x: p[0],
                        y: p[1],
                        z: p[2],
                    };
                    let q = Vec3I32 {
                        x: q[0],
                        y: q[1],
                        z: q[2],
                    };
                    let mut x = Trace::default();
                    let mut y = Trace::default();
                    let ox = aa.trace_into(&p, &q, &mut sa, &mut x);
                    let oy = bb.trace_into(&p, &q, &mut sb, &mut y);
                    assert_eq!(
                        (ox, x),
                        (oy, y),
                        "new split axis{axis} distance{distance} {p:?} {q:?}"
                    );
                    assert_eq!(aa.point_contents(p), bb.point_contents(p));
                    split_rays += 1;
                }
            }
        }
    }
    println!("new_split_adjacent={split_rays}");
    println!("20000fulltraces+points+booleans match starts_solid={starts_solid} all_solid={all_solid} fingerprint={hash:016x} additional_contact_adjacent={boundary_count}");
}
