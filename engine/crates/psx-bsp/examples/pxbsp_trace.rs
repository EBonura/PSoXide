//! Inspect one cooked PXBSP body trace in engine world units.
//!
//! usage: pxbsp_trace <brush_world.pxbsp> x,y,z dx,dy,dz radius height hull

use psx_bsp::collision::{Trace, TraceScratch};
use psx_bsp::collision_provider::PxbspCollisionProvider;
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};
use psx_engine::{
    CollisionTrace, CollisionTraceProvider, CollisionTraceQuery, CollisionTraceShape, RoomPoint,
};

fn point(spec: &str) -> RoomPoint {
    let mut values = spec.split(',').map(|value| value.parse::<i32>().unwrap());
    RoomPoint::new(
        values.next().unwrap(),
        values.next().unwrap(),
        values.next().unwrap(),
    )
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 6, "expected map start delta radius height hull");
    let bytes = std::fs::read(&args[0]).expect("read pxbsp");
    let mut map = PxbspResidentMap::default();
    map.load(0, &mut SliceReader::new(&bytes))
        .expect("load pxbsp");

    let start = point(&args[1]);
    let delta = point(&args[2]);
    let end = RoomPoint::new(
        start.x.saturating_add(delta.x),
        start.y.saturating_add(delta.y),
        start.z.saturating_add(delta.z),
    );
    let radius = args[3].parse::<i32>().unwrap();
    let height = args[4].parse::<i32>().unwrap();
    let hull_index = args[5].parse::<usize>().unwrap();
    let shape = CollisionTraceShape::Body { radius, height };
    let mut scratch = TraceScratch::new();
    let to_q12 = |point: RoomPoint| Vec3I32 {
        x: point.x.saturating_mul(4096),
        y: point.y.saturating_mul(4096),
        z: point.z.saturating_mul(4096),
    };
    let mut raw = Trace::default();
    assert!(map
        .model_collision_hull(0, hull_index)
        .expect("world hull")
        .trace_into(&to_q12(start), &to_q12(end), &mut scratch, &mut raw));
    println!("raw={raw:?}");
    let mut provider = PxbspCollisionProvider::new(&map, hull_index, &[], shape, &mut scratch)
        .expect("collision provider");
    let mut trace = CollisionTrace::default();
    assert!(provider.trace_into(CollisionTraceQuery { start, end, shape }, &mut trace,));
    let leaf = |point: RoomPoint| {
        map.point_leaf_index(Vec3I32 {
            x: point.x.saturating_mul(4096),
            y: point.y.saturating_mul(4096),
            z: point.z.saturating_mul(4096),
        })
    };
    println!("start={start:?} leaf={:?}", leaf(start));
    println!("end={end:?} leaf={:?}", leaf(end));
    println!("trace={trace:?}");
}
