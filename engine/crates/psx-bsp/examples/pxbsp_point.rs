//! Leaf lookup for world points in a cooked .pxbsp: prints the leaf index and
//! contents for each `x,y,z` (engine world units) given on the command line.
//! usage: pxbsp_point <brush_world.pxbsp> x,y,z [x,y,z ...]
use psx_bsp::pxbsp_resident::PxbspResidentMap;
use psx_bsp::{SliceReader, Vec3I32};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bytes = std::fs::read(&args[0]).expect("read pxbsp");
    let mut map = PxbspResidentMap::default();
    let mut reader = SliceReader::new(&bytes);
    map.load(0, &mut reader).ok().expect("load pxbsp");
    let leaves = map.leaves();
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
                println!(
                    "({},{},{}) -> leaf {} contents {} {}",
                    v[0], v[1], v[2], index, contents, name
                );
            }
            None => println!("({},{},{}) -> no leaf", v[0], v[1], v[2]),
        }
    }
}
