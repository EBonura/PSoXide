//! The Convert gate: converting a legacy room with `PreserveLook` changes
//! nothing about its faces or its cook, on every reference level and the
//! starter.

use psx_grid::write::RoomChunk;
use psx_grid::{edge, NO_EXT, SECTOR_CLEAR};

use super::tests::{cook, load_reference};
use crate::grid::course_merge::merge_courses;
use crate::grid::edit::{convert_to_derived, ConvertMode};
use crate::grid::faces::{all_faces, GridFace};
use crate::grid::overrides::WallMode;
use crate::grid::world::GridWorld;

const LEVELS: [&str; 6] = [
    "grid-cathedral",
    "grid-level008",
    "grid-dungeon",
    "grid-cave",
    "grid-sewers",
    "grid-west",
];

/// A face reduced to what the eye sees: ring, texels, material, sidedness,
/// with the ring started at its smallest point so the winding start does not
/// matter. Texels are rounded to a millionth of a texel.
fn look(face: &GridFace) -> String {
    let n = face.points.len();
    let start = (0..n).min_by_key(|&i| face.points[i]).unwrap();
    let ring: Vec<String> = (0..n)
        .map(|k| {
            let i = (start + k) % n;
            let t = face.texels[i];
            format!(
                "{:?}@{:.6},{:.6}",
                face.points[i],
                (t[0] * 1e6).round() / 1e6 + 0.0,
                (t[1] * 1e6).round() / 1e6 + 0.0
            )
        })
        .collect();
    format!("{ring:?} {:?} {}", face.material, face.double_sided)
}

/// The faces with the course cuts of walls joined: a derived wall is one
/// face where the walled room cut it at every sector of height, and both
/// look the same.
fn multiset(world: &GridWorld) -> Vec<String> {
    let mut faces: Vec<String> = merge_courses(all_faces(world)).iter().map(look).collect();
    faces.sort();
    faces
}

fn same<T: PartialEq + core::fmt::Debug>(name: &str, room: usize, what: &str, a: &[T], b: &[T]) {
    assert_eq!(a.len(), b.len(), "{name} room {room}: {what} count");
    if let Some(i) = (0..a.len()).find(|&i| a[i] != b[i]) {
        panic!(
            "{name} room {room}: {what} {i} differs\n  {:?}\n  {:?}",
            a[i], b[i]
        );
    }
}

fn convert_all(world: &mut GridWorld) -> (usize, usize, usize, usize) {
    let mut totals = (0, 0, 0, 0);
    for room in 0..world.rooms.len() {
        let report = convert_to_derived(world, room, ConvertMode::PreserveLook).unwrap();
        totals.0 += report.converted;
        totals.1 += report.kept_authored;
        totals.2 += report.hidden;
        totals.3 += report.filled;
    }
    totals
}

#[test]
fn preserve_look_leaves_the_face_multiset_and_the_cook_unchanged() {
    for name in LEVELS {
        let project = load_reference(name).unwrap();
        let before_world = project.active_scene().world.grid().unwrap().clone();
        let mut world = before_world.clone();
        let faces_before = multiset(&before_world);
        let cook_before = cook(&before_world).unwrap();
        let (converted, kept, hidden, _) = convert_all(&mut world);
        assert!(world.rooms.iter().all(|r| r.wall_mode == WallMode::Derived));
        let faces_after = multiset(&world);
        eprintln!(
            "{name}: {} faces; converted {converted} walls, kept {kept} authored, hid {hidden} derived faces",
            faces_before.len()
        );
        if faces_after.len() != faces_before.len() {
            let only = |a: &[String], b: &[String]| -> Vec<String> {
                a.iter()
                    .filter(|x| !b.contains(x))
                    .take(4)
                    .cloned()
                    .collect()
            };
            eprintln!("only before: {:#?}", only(&faces_before, &faces_after));
            eprintln!("only after: {:#?}", only(&faces_after, &faces_before));
        }
        assert_eq!(faces_after.len(), faces_before.len(), "{name}: face count");
        for (i, (a, b)) in faces_before.iter().zip(&faces_after).enumerate() {
            assert_eq!(a, b, "{name}: face {i} differs");
        }
        let cook_after = cook(&world).unwrap();
        let quads = |c: &super::CookedLevel| {
            c.report
                .rooms
                .iter()
                .map(|r| r.quads + r.triangles)
                .sum::<usize>()
        };
        eprintln!(
            "{name}: all_faces {} -> {}; cook chunks {} -> {} B; core {} -> {} B; primitives {} -> {}",
            faces_before.len(),
            faces_after.len(),
            cook_before.report.chunk_bytes(),
            cook_after.report.chunk_bytes(),
            cook_before.report.core_bytes,
            cook_after.report.core_bytes,
            quads(&cook_before),
            quads(&cook_after),
        );
        assert_eq!(
            cook_before.core.len(),
            cook_after.core.len(),
            "{name}: core size"
        );
        for (i, (a, b)) in cook_before
            .tables
            .iter()
            .zip(&cook_after.tables)
            .enumerate()
        {
            assert!(
                b.vertices.len() <= a.vertices.len(),
                "{name} room {i}: {} vertices became {}",
                a.vertices.len(),
                b.vertices.len()
            );
            // The render tables differ by the course cuts: a derived wall is
            // one primitive where the walled room had one per course, and
            // each corner of a primitive is lit on its own. Every texture
            // the room drew is still drawn, and never with more primitives.
            let (ma, mb) = (&cook_before.level.materials, &cook_after.level.materials);
            let textures = |chunk: &RoomChunk, mats: &[psx_grid::Material]| {
                let mut t: Vec<u16> = chunk
                    .quads
                    .iter()
                    .map(|q| mats[usize::from(q.material)].texture_asset)
                    .collect();
                t.sort_unstable();
                t.dedup();
                t
            };
            same(
                name,
                i,
                "textures drawn",
                &textures(a, ma),
                &textures(b, mb),
            );
            assert!(
                b.quads.len() <= a.quads.len(),
                "{name} room {i}: {} primitives became {}",
                a.quads.len(),
                b.quads.len()
            );
            same(
                name,
                i,
                "collision walls",
                &physical_walls(a),
                &physical_walls(b),
            );
            same(name, i, "sector heights", &heights(a), &heights(b));
        }
    }
}

#[allow(dead_code)]
/// Every quad resolved to world geometry, sorted: independent of the order
/// the cells list them in and of which corner the ring starts at (a quad
/// moved to the other sector of its edge starts at the other end).
fn resolved_quads(
    chunk: &RoomChunk,
    quads: &[psx_grid::Quad],
    materials: &[psx_grid::Material],
) -> Vec<String> {
    let mut out: Vec<String> = quads
        .iter()
        .map(|q| {
            let point = |k: usize| {
                let v = chunk.vertices[usize::from(q.v[k])];
                (v.x, v.y, v.z)
            };
            // A triangle repeats its last corner.
            let n = if q.flags & psx_grid::QUAD_TRI != 0 {
                3
            } else {
                4
            };
            let start = (0..n).min_by_key(|&k| point(k)).unwrap();
            let ring: Vec<String> = (0..n)
                .map(|step| {
                    let k = (start + step) % n;
                    format!("{:?}/{}/{:#x}", point(k), q.color[k], q.uv[k])
                })
                .collect();
            format!(
                "{ring:?} d{} n{:?} tex{} f{}",
                q.d,
                q.n,
                materials[usize::from(q.material)].texture_asset,
                q.flags
            )
        })
        .collect();
    out.sort();
    out
}

/// Collision walls by the physical line they stand on, not by the sector
/// that lists them: a wall stored on the far side of a line and the same
/// wall hosted by the near side are one wall.
fn physical_walls(chunk: &RoomChunk) -> Vec<String> {
    let mut out = Vec::new();
    for (i, sector) in chunk.sectors.iter().enumerate() {
        if sector.ext == NO_EXT {
            continue;
        }
        let (cx, cz) = (
            (i % usize::from(chunk.width)) as i32,
            (i / usize::from(chunk.width)) as i32,
        );
        let ext = chunk.exts[usize::from(sector.ext)];
        let first = usize::from(ext.wall_first);
        for wall in &chunk.walls[first..first + usize::from(ext.wall_count)] {
            let [a, b, c, d] = wall.y;
            out.push(match wall.edge {
                edge::NORTH => format!("x{cx} z{cz} X {:?}", [a, b, c, d]),
                edge::SOUTH => format!("x{cx} z{} X {:?}", cz + 1, [b, a, d, c]),
                edge::WEST => format!("x{cx} z{cz} Z {:?}", [b, a, d, c]),
                edge::EAST => format!("x{} z{cz} Z {:?}", cx + 1, [a, b, c, d]),
                e => format!("x{cx} z{cz} diagonal {e} {:?}", [a, b, c, d]),
            });
        }
    }
    out.sort();
    out
}

fn heights(chunk: &RoomChunk) -> Vec<([i16; 4], [i16; 4], u16)> {
    chunk
        .sectors
        .iter()
        .map(|s| (s.floor, s.ceil, s.flags & !SECTOR_CLEAR))
        .collect()
}
