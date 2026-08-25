//! Cook one rigid GLB/glTF/FBX source to a native model bundle.
//!
//! Usage:
//!   cargo run -p psxed-gltf --example cook_rigid_model -- \
//!     <source> <output-dir> <stem> <texture-width> <texture-height> <world-height>

use std::path::PathBuf;

fn parse_u16(value: Option<String>, label: &str) -> u16 {
    value
        .unwrap_or_else(|| panic!("missing {label}"))
        .parse()
        .unwrap_or_else(|_| panic!("invalid {label}"))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let source = PathBuf::from(args.next().expect("missing source"));
    let output_dir = PathBuf::from(args.next().expect("missing output directory"));
    let stem = args.next().expect("missing output stem");
    let texture_width = parse_u16(args.next(), "texture width");
    let texture_height = parse_u16(args.next(), "texture height");
    let world_height = parse_u16(args.next(), "world height");
    assert!(args.next().is_none(), "unexpected trailing argument");

    let config = psxed_gltf::RigidModelConfig {
        texture_width,
        texture_height,
        world_height,
        ..Default::default()
    };
    let package = psxed_gltf::convert_rigid_model_path(&source, &config).expect("cook model");
    std::fs::create_dir_all(&output_dir).expect("create output directory");
    std::fs::write(output_dir.join(format!("{stem}.psxmdl")), &package.model).expect("write model");
    if let Some(texture) = &package.texture {
        std::fs::write(output_dir.join(format!("{stem}.psxt")), texture).expect("write texture");
    }
    for clip in &package.clips {
        std::fs::write(
            output_dir.join(format!("{}.psxanim", clip.sanitized_name)),
            &clip.bytes,
        )
        .expect("write animation");
    }

    println!(
        "{}: {} vertices, {} faces, {} joints, {}x{} {:?}",
        stem,
        package.report.cooked_vertices,
        package.report.faces,
        package.report.joints,
        texture_width,
        texture_height,
        config.texture_depth,
    );
}
