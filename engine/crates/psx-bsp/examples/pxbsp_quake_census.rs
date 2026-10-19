//! Draw a Quake map (cooked to PXBSP by quake-cook) through the shared world
//! renderer at cameras taken from the map's own entities, and print what the
//! frame selected.
//!
//! A Quake map keeps Quake's Z-up space, so the view is built here from a Quake
//! yaw and pitch instead of through `load_pxbsp_view`, whose axis frame is
//! the Y-up one. The renderer takes the frame as a `ViewTransform`, so nothing
//! else changes.
//!
//! ```sh
//! cargo run --release -p psx-bsp --example pxbsp_quake_census -- MAP.pxbsp [--cameras N] [--csv OUT]
//! ```

use std::fmt::Write as _;

use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::render::{
    configure_projection, Camera, PxbspTextureBinding, Renderer, ViewTransform,
    DEFAULT_PACKET_WORDS,
};
use psx_bsp::{SliceReader, Vec3I32};
use psx_gte::math::{Mat3I16, Vec3I32 as GteVec3I32};

/// `PxbspEntity::class_id` of a Quake map entity (`quake_formats::quake_pxbsp`).
const QUAKE_ENTITY_CLASS: u16 = 0x0100;
/// The renderer's view scale, 3.0 in Q12 (`PXBSP_VIEW_SCALE_Q12`).
const VIEW_SCALE: f64 = 3.0;

/// A Quake view (X forward at yaw 0, Y left, Z up) in the GTE's right, down,
/// forward axes at the renderer's view scale.
fn quake_view(origin_units: [i32; 3], yaw_degrees: f64, pitch_degrees: f64) -> ViewTransform {
    let (yaw, pitch) = (yaw_degrees.to_radians(), pitch_degrees.to_radians());
    let forward = [
        pitch.cos() * yaw.cos(),
        pitch.cos() * yaw.sin(),
        pitch.sin(),
    ];
    // right = forward x up, with up = +Z.
    let length = forward[0].hypot(forward[1]);
    let right = [forward[1] / length, -forward[0] / length, 0.0];
    // down = forward x right.
    let down = [
        forward[1] * right[2] - forward[2] * right[1],
        forward[2] * right[0] - forward[0] * right[2],
        forward[0] * right[1] - forward[1] * right[0],
    ];
    let row = |axis: [f64; 3]| axis.map(|value| (value * VIEW_SCALE * 4096.0).round() as i16);
    let m = [row(right), row(down), row(forward)];
    let translation = |row: &[i16; 3]| {
        (0..3)
            .map(|axis| i32::from(row[axis]) * -origin_units[axis])
            .sum::<i32>()
            >> 12
    };
    ViewTransform {
        rotation: Mat3I16 { m },
        translation: GteVec3I32::new(translation(&m[0]), translation(&m[1]), translation(&m[2])),
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("MAP.pxbsp path");
    let mut camera_limit = 16usize;
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

    // Candidate eyes: Quake entities standing in empty leaves, eye height up.
    let entities = map.entities();
    let mut eyes = Vec::new();
    let (mut quake, mut point, mut empty) = (0usize, 0usize, 0usize);
    for index in 0..entities.len() {
        let entity = entities.get(index).expect("entity");
        if entity.class_id != QUAKE_ENTITY_CLASS || entity.model != u16::MAX {
            continue;
        }
        quake += 1;
        let eye = Vec3I32 {
            x: entity.origin.x,
            y: entity.origin.y,
            z: entity.origin.z + (32 << 12),
        };
        let Some(leaf) = map.point_leaf_index(eye) else {
            continue;
        };
        point += 1;
        if std::env::var_os("CENSUS_DEBUG").is_some() {
            eprintln!(
                "origin=({}, {}, {}) leaf={leaf} contents={:?}",
                entity.origin.x >> 12,
                entity.origin.y >> 12,
                entity.origin.z >> 12,
                map.leaves().get(leaf).map(|l| l.contents)
            );
        }
        if map.leaves().get(leaf).map(|leaf| leaf.contents) == Some(-1) {
            empty += 1;
            eyes.push((eye, leaf));
        }
    }
    eprintln!(
        "entities={} quake point-entities={quake} in-world={point} eyes in empty leaves={empty}",
        entities.len()
    );
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
    let visible_leaf_domain = map
        .brush_models()
        .get(0)
        .map(|world| world.visible_leaves as usize)
        .expect("world model");

    let mut csv = String::from(
        "camera,leaf,yaw,candidates,visible,batches,packets,triangles,words,overflow,unresolved\n",
    );
    let (mut candidates_sum, mut visible_sum, mut batches_sum) = (0u64, 0u64, 0u64);
    let (mut packets_sum, mut triangles_sum, mut frames) = (0u64, 0u64, 0u64);
    let (mut overflow, mut unresolved) = (0u64, 0u64);
    for (camera_index, (eye, leaf)) in eyes.iter().enumerate() {
        // Faces the PVS offers this camera: unique faces of the visible leaves.
        let mut visibility = vec![0u8; visible_leaf_domain.div_ceil(8)];
        let domain = map
            .leaf_visibility_into(*leaf, &mut visibility)
            .unwrap_or(0)
            .min(visible_leaf_domain);
        let mut seen = vec![false; map.faces().len()];
        let marks = map.mark_surfaces();
        for visible in 0..domain {
            if visibility[visible >> 3] & (1 << (visible & 7)) == 0 {
                continue;
            }
            let Some(leaf) = map.leaves().get(visible + 1) else {
                continue;
            };
            let start = leaf.first_mark_surface as usize;
            let end = (start + leaf.mark_surface_count as usize).min(marks.len());
            for mark in start..end {
                seen[marks.get(mark).expect("mark") as usize] = true;
            }
        }
        let candidates = seen.iter().filter(|&&face| face).count() as u64;

        for yaw in [0.0, 90.0, 180.0, 270.0] {
            let origin_units = [eye.x >> 12, eye.y >> 12, eye.z >> 12];
            let view = quake_view(origin_units, yaw, 0.0);
            let camera = Camera {
                origin: *eye,
                angles: [0; 3],
            };
            let frame = renderer.draw_pxbsp_world(&map, camera, view, &bindings, 0, &mut packets);
            let stats = frame.stats;
            writeln!(
                csv,
                "{camera_index},{leaf},{yaw},{candidates},{},{},{},{},{},{},{}",
                stats.visible_faces,
                stats.surface_batches,
                stats.packets,
                stats.hardware_triangles,
                frame.packet_words,
                u8::from(stats.packet_overflow_avoided),
                stats.unresolved_material_faces,
            )
            .unwrap();
            candidates_sum += candidates;
            visible_sum += u64::from(stats.visible_faces);
            batches_sum += u64::from(stats.surface_batches);
            packets_sum += u64::from(stats.packets);
            triangles_sum += u64::from(stats.hardware_triangles);
            overflow += u64::from(stats.packet_overflow_avoided);
            unresolved += u64::from(stats.unresolved_material_faces);
            frames += 1;
        }
    }
    if let Some(path) = csv_path {
        std::fs::write(&path, &csv).unwrap_or_else(|error| panic!("write {path}: {error}"));
    }
    let mean = |sum: u64| {
        if frames == 0 {
            0.0
        } else {
            sum as f64 / frames as f64
        }
    };
    println!(
        "{path}: cameras={} frames={frames} mean candidates={:.1} visible={:.1} batches={:.1} packets={:.1} triangles={:.1} overflow_frames={overflow} unresolved_faces={unresolved}",
        eyes.len(),
        mean(candidates_sum),
        mean(visible_sum),
        mean(batches_sum),
        mean(packets_sum),
        mean(triangles_sum),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_view(view: &ViewTransform, point: [i32; 3]) -> [i32; 3] {
        let row = |axis: usize, translation: i32| {
            (0..3)
                .map(|column| i32::from(view.rotation.m[axis][column]) * point[column])
                .sum::<i32>()
                / 4096
                + translation
        };
        [
            row(0, view.translation.x),
            row(1, view.translation.y),
            row(2, view.translation.z),
        ]
    }

    #[test]
    fn a_point_ahead_lands_on_the_view_axis_at_scaled_depth() {
        let origin = [10, 20, 30];
        // Yaw 90 looks along +Y; Quake +Y is left, so +X is to the right.
        let view = quake_view(origin, 90.0, 0.0);
        assert_eq!(to_view(&view, [10, 120, 30]), [0, 0, 300]);
        let right = to_view(&view, [110, 20, 30]);
        assert!(right[0] > 0 && right[1] == 0, "{right:?}");
        let up = to_view(&view, [10, 20, 130]);
        assert!(up[1] < 0 && up[0] == 0, "up is negative GTE y: {up:?}");
    }

    #[test]
    fn the_view_keeps_handedness_at_every_yaw() {
        for yaw in [0.0, 37.0, 90.0, 213.0, 300.0] {
            let view = quake_view([0, 0, 0], yaw, 20.0);
            let m = view.rotation.m.map(|row| row.map(f64::from));
            let determinant = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
                - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
                + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
            assert!(determinant > 0.0, "reflection at yaw {yaw}");
        }
    }
}
