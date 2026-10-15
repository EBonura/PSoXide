//! Compare boolean obstruction against full contact traces on a cooked map.
//! Usage: pxbsp_occlusion_parity <brush_world.pxbsp> [ray-count]
use psx_bsp::collision::{Trace, TraceScratch};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&args[1]).expect("map");
    let count = args
        .get(2)
        .map(|s| s.parse::<usize>().unwrap())
        .unwrap_or(30000);
    let mut map = PxbspResidentMap::default();
    map.load(0, &mut SliceReader::new(&bytes))
        .expect("resident map");
    let general: Vec<_> = map.planes().iter().filter(|p| p.kind >= 3).collect();
    println!(
        "planes={} general={} general_y_zero={} general_any_zero={}",
        map.planes().len(),
        general.len(),
        general.iter().filter(|p| p.normal.y == 0).count(),
        general
            .iter()
            .filter(|p| p.normal.x == 0 || p.normal.y == 0 || p.normal.z == 0)
            .count()
    );
    let hull = map.model_collision_hull(0, 0).expect("world point hull");
    let world = map.brush_models().get(0).expect("world model");
    let mins = [world.mins.x, world.mins.y, world.mins.z].map(i32::from);
    let maxs = [world.maxs.x, world.maxs.y, world.maxs.z].map(i32::from);
    let mut parents = vec![0u16; map.nodes().len()];
    assert!(hull.build_solid_cell_parents(&mut parents));
    let mut previous_witness = u16::MAX;
    let mut certified_reuses = 0;
    let mut scratch = TraceScratch::new();
    let mut rng = 0x13c0_7711u32;
    let mut fingerprint = 0xcbf29ce484222325u64;
    let mut clear = 0;
    let mut open_pairs = 0;
    for index in 0..count {
        let mut point = || {
            let mut coordinate = |axis: usize| {
                rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                (mins[axis] + (rng % (maxs[axis] - mins[axis] + 1) as u32) as i32) * 4096
            };
            Vec3I32 {
                x: coordinate(0),
                y: coordinate(1),
                z: coordinate(2),
            }
        };
        let start = point();
        let end = point();
        let mut trace = Trace::default();
        assert!(
            hull.trace_into(&start, &end, &mut scratch, &mut trace),
            "full trace {index}"
        );
        for byte in format!("{trace:?}").bytes() {
            fingerprint = (fingerprint ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
        let expected = trace.start_solid.is_set() || trace.fraction < 4096;
        assert_eq!(
            hull.segment_obstructed(&start, &end, &mut scratch),
            Some(expected),
            "ray {index}: {start:?} -> {end:?}"
        );
        let (blocked, witness) = hull
            .segment_obstruction_witness(&start, &end, &mut scratch)
            .unwrap();
        assert_eq!(blocked, expected, "witness ray {index}");
        if blocked {
            assert!(
                hull.certifies_solid_cell(&parents, witness, &start, &end),
                "own certificate {index}"
            );
        }
        let direct_certificate =
            hull.certifies_solid_cell(&parents, previous_witness, &start, &end);
        let mut constraints = [0u16; 72];
        if let Some(len) =
            hull.solid_cell_constraints_into(&parents, previous_witness, &mut constraints)
        {
            assert_eq!(
                hull.segment_intersects_halfspaces(&constraints[..len], &start, &end),
                direct_certificate,
                "copied full certificate {index}"
            );
        }
        if direct_certificate {
            assert!(expected, "cached certificate {index}");
            certified_reuses += 1;
        }
        previous_witness = witness;
        clear += usize::from(!expected);
        open_pairs += usize::from(
            map.point_leaf_index(start).is_some_and(|i| i != 0)
                && map.point_leaf_index(end).is_some_and(|i| i != 0),
        );
    }
    assert!(
        clear > 100 && open_pairs > 100,
        "sampling must exercise traversable space"
    );
    println!("matched={count} clear={clear} non_solid_endpoint_pairs={open_pairs} trace_fingerprint={fingerprint:016x} certified_reuses={certified_reuses}");
}
