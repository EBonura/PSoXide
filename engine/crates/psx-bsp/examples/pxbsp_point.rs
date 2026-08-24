//! Leaf/PVS lookup for world points in a cooked .pxbsp: prints the leaf index,
//! contents, visible-leaf count, mark references, and unique visible faces for
//! each `x,y,z` (engine world units) given on the command line.
//! usage: pxbsp_point <brush_world.pxbsp> x,y,z [x,y,z ...]
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).expect("read pxbsp");
    let mut map = PxbspResidentMap::default();
    let mut reader = SliceReader::new(&bytes);
    map.load(0, &mut reader).expect("load pxbsp");
    let leaves = map.leaves();
    let marks = map.mark_surfaces();
    let face_count = map.faces().len();
    for spec in &args[1..] {
        let v: Vec<i32> = spec.split(',').map(|s| s.trim().parse().unwrap()).collect();
        let point = Vec3I32 {
            x: v[0] << 12,
            y: v[1] << 12,
            z: v[2] << 12,
        };
        match map.point_leaf_index(point) {
            Some(index) => {
                let contents = leaves.get(index).map(|l| l.contents).unwrap_or(0);
                let name = match contents {
                    -1 => "EMPTY",
                    -2 => "SOLID",
                    -3 => "WATER",
                    -4 => "SLIME",
                    -5 => "LAVA",
                    -6 => "SKY",
                    _ => "?",
                };
                let visible_leaf_domain = map
                    .brush_models()
                    .get(0)
                    .map(|world| world.visible_leaves as usize)
                    .unwrap_or(0);
                let mut visibility = vec![0u8; visible_leaf_domain.div_ceil(8)];
                let mut visible_leaf_count = 0usize;
                let mut mark_reference_count = 0usize;
                let mut visible_faces = vec![false; face_count];
                let visible_domain = map
                    .leaf_visibility_into(index, &mut visibility)
                    .unwrap_or(0)
                    .min(visible_leaf_domain);
                for visible_index in 0..visible_domain {
                    if visibility[visible_index >> 3] & (1 << (visible_index & 7)) == 0 {
                        continue;
                    }
                    visible_leaf_count += 1;
                    let leaf_index = visible_index + 1;
                    let Some(leaf) = leaves.get(leaf_index) else {
                        continue;
                    };
                    let start = leaf.first_mark_surface as usize;
                    let end = start.saturating_add(leaf.mark_surface_count as usize);
                    for mark_index in start..end.min(marks.len()) {
                        mark_reference_count += 1;
                        let Some(face) = marks.get(mark_index) else {
                            continue;
                        };
                        let face = face as usize;
                        if let Some(marked) = visible_faces.get_mut(face) {
                            *marked = true;
                        }
                    }
                }
                let unique_face_count = visible_faces.iter().filter(|&&marked| marked).count();
                println!(
                    "({},{},{}) -> leaf {} contents {} {} pvs_leaves={} mark_refs={} unique_faces={}",
                    v[0],
                    v[1],
                    v[2],
                    index,
                    contents,
                    name,
                    visible_leaf_count,
                    mark_reference_count,
                    unique_face_count,
                );
            }
            None => println!("({},{},{}) -> no leaf", v[0], v[1], v[2]),
        }
    }
}
