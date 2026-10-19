//! Hash the packets the shared world renderer emits for a cooked PXBSP map at a
//! deterministic set of cameras. Two builds of the renderer that print the same
//! hash drew every one of those frames identically, word for word, which is the
//! evidence that a change to the renderer is a no-op for that map.
//!
//! ```sh
//! cargo run --release -p psx-bsp --example pxbsp_packet_hash -- MAP.pxbsp [--cameras N] [--csv OUT]
//! ```
//!
//! Cameras sit on a lattice over the world model's bounds, keep the points that
//! land in an empty leaf, and look along eight yaws at two pitches. The map is
//! Y-up (a Cortex map), so the stock `load_pxbsp_view` frame is used.

use std::fmt::Write as _;

use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::{
    configure_projection, load_pxbsp_view, Camera, PxbspTextureBinding, Renderer,
    DEFAULT_PACKET_WORDS,
};
use psx_bsp::{SliceReader, Vec3I32};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn mix(hash: &mut u64, word: u32) {
    for byte in word.to_le_bytes() {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("MAP.pxbsp path");
    let mut camera_limit = 64usize;
    let mut csv_path = None;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--cameras" => camera_limit = args.next().expect("count").parse().expect("count"),
            "--csv" => csv_path = Some(args.next().expect("path")),
            other => panic!("unknown argument {other}"),
        }
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {path}: {error}"));
    let mut map = PxbspResidentMap::with_capacity(4_000_000);
    map.load(1, &mut SliceReader::new(&bytes))
        .unwrap_or_else(|error| panic!("load {path}: {error}"));

    let world = map.brush_models().get(0).expect("world model");
    let (mins, maxs) = (world.mins, world.maxs);
    let steps = 14i32;
    let mut eyes = Vec::new();
    for ix in 0..steps {
        for iy in 0..steps {
            for iz in 0..steps {
                let along = |low: i16, high: i16, i: i32| {
                    i32::from(low) + (i32::from(high) - i32::from(low)) * (2 * i + 1) / (2 * steps)
                };
                let eye = Vec3I32 {
                    x: along(mins.x, maxs.x, ix) << 12,
                    y: along(mins.y, maxs.y, iy) << 12,
                    z: along(mins.z, maxs.z, iz) << 12,
                };
                if let Some(leaf) = map.point_leaf_index(eye) {
                    if map.leaves().get(leaf).map(|leaf| leaf.contents) == Some(-1) {
                        eyes.push(eye);
                    }
                }
            }
        }
    }
    let stride = (eyes.len() / camera_limit.max(1)).max(1);
    let eyes: Vec<_> = eyes
        .into_iter()
        .step_by(stride)
        .take(camera_limit)
        .collect();

    configure_projection();
    let mut renderer = Renderer::new_pxbsp_with_nodes(map.faces().len(), map.nodes().len());
    let bindings = vec![Some(PxbspTextureBinding::default()); map.materials().len()];
    let mut packets = vec![0u32; DEFAULT_PACKET_WORDS];
    let mut hash = FNV_OFFSET;
    let mut csv =
        String::from("camera,yaw,pitch,visible,batches,packets,triangles,words,frame_hash\n");
    let (mut frames, mut visible, mut words) = (0u64, 0u64, 0u64);
    for (index, eye) in eyes.iter().enumerate() {
        for pitch in [0i16, -341] {
            for step in 0..8i16 {
                let yaw = step * 512;
                let camera = Camera {
                    origin: *eye,
                    angles: [pitch, yaw, 0],
                };
                let frame = renderer.draw_pxbsp_world(
                    &map,
                    camera,
                    load_pxbsp_view(camera),
                    &bindings,
                    7,
                    &mut packets,
                );
                let mut frame_hash = FNV_OFFSET;
                for &word in &packets[..frame.packet_words] {
                    mix(&mut frame_hash, word);
                }
                let stats = frame.stats;
                for value in [
                    u32::from(stats.visible_faces),
                    u32::from(stats.visible_sky_apertures),
                    u32::from(stats.surface_batches),
                    stats.packets,
                    stats.hardware_triangles,
                    u32::from(stats.unresolved_material_faces),
                    u32::from(stats.packet_overflow_avoided),
                    frame.packet_words as u32,
                ] {
                    mix(&mut frame_hash, value);
                }
                mix(&mut hash, frame_hash as u32);
                mix(&mut hash, (frame_hash >> 32) as u32);
                writeln!(
                    csv,
                    "{index},{yaw},{pitch},{},{},{},{},{},{frame_hash:016x}",
                    stats.visible_faces,
                    stats.surface_batches,
                    stats.packets,
                    stats.hardware_triangles,
                    frame.packet_words,
                )
                .unwrap();
                frames += 1;
                visible += u64::from(stats.visible_faces);
                words += frame.packet_words as u64;
            }
        }
    }
    if let Some(path) = csv_path {
        std::fs::write(&path, &csv).unwrap_or_else(|error| panic!("write {path}: {error}"));
    }
    println!(
        "cameras={} frames={frames} visible_faces={visible} packet_words={words} hash={hash:016x}",
        eyes.len()
    );
}
