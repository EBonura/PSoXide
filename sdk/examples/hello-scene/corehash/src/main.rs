//! `corehash CORE.bin`: the core hash a SCENES.TBL row carries for that image.
fn main() {
    let path = std::env::args().nth(1).expect("usage: corehash CORE.bin");
    let bytes = std::fs::read(&path).expect("read the core image");
    println!("{:08x}", psx_scene_abi::word_fnv(&bytes));
}
