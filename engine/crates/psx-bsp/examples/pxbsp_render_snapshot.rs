//! Render one resident PXBSP camera on the host and report selected work.
//!
//! ```sh
//! cargo run -p psx-bsp --example pxbsp_render_snapshot -- MAP X Y Z YAW
//! ```

use psx_bsp::collision::{Trace, TraceScratch};
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::{
    configure_projection, load_pxbsp_view, Camera, PxbspTextureBinding, Renderer,
    DEFAULT_PACKET_WORDS,
};
use psx_bsp::Vec3I32;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("MAP path");
    let coordinate = |value: Option<String>, label: &str| -> i32 {
        value
            .unwrap_or_else(|| panic!("missing {label}"))
            .parse::<i32>()
            .unwrap_or_else(|_| panic!("invalid {label}"))
    };
    let x = coordinate(args.next(), "X");
    let y = coordinate(args.next(), "Y");
    let z = coordinate(args.next(), "Z");
    let yaw = coordinate(args.next(), "YAW") as i16;
    assert!(args.next().is_none(), "unexpected extra argument");

    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let map = PxbspResidentMap::from_static(1, bytes)
        .unwrap_or_else(|error| panic!("load {path}: {error:?}"));
    let camera = Camera {
        origin: Vec3I32 {
            x: x << 12,
            y: y << 12,
            z: z << 12,
        },
        angles: [0, yaw, 0],
    };
    let point_hull = map.model_collision_hull(0, 0).expect("world point hull");
    let floor_end = Vec3I32 {
        x: camera.origin.x,
        y: camera.origin.y.saturating_sub(1024 << 12),
        z: camera.origin.z,
    };
    let mut floor = Trace::default();
    assert!(point_hull.trace_into(
        &camera.origin,
        &floor_end,
        &mut TraceScratch::new(),
        &mut floor,
    ));
    println!(
        "leaf={:?} contents={:?} floor_y={} floor_fraction={}",
        map.point_leaf_index(camera.origin),
        point_hull.point_contents(camera.origin),
        floor.end.y >> 12,
        floor.fraction,
    );
    configure_projection();
    let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
    let bindings = vec![Some(PxbspTextureBinding::default()); map.materials().len()];
    let mut packets = vec![0u32; DEFAULT_PACKET_WORDS];
    let frame = renderer.draw_pxbsp_world(
        &map,
        camera,
        load_pxbsp_view(camera),
        &bindings,
        0,
        &mut packets,
    );
    println!(
        "faces={} batches={} packets={} triangles={} words={} overflow={}",
        frame.stats.visible_faces,
        frame.stats.surface_batches,
        frame.stats.packets,
        frame.stats.hardware_triangles,
        frame.packet_words,
        frame.stats.packet_overflow_avoided,
    );
}
