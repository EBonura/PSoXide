use std::path::PathBuf;

/// Cooked grid world files the guest embeds with `include_bytes!`. The cook
/// writes them next to the cooked manifest; they are build outputs, not
/// tracked sources, so a fresh checkout gets empty stand-ins and boots to the
/// clear-colour screen the placeholder manifest describes.
const GRID_WORLD_FILES: [&str; 2] = ["grid_level.pgl", "grid_rooms.pgr"];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let generated_dir = manifest_dir.join("generated");
    let placeholder_manifest = generated_dir.join("level_manifest.rs");
    let cooked_manifest = generated_dir.join("level_manifest.cooked.rs");

    println!("cargo:rerun-if-changed={}", generated_dir.display());
    println!("cargo:rerun-if-changed={}", placeholder_manifest.display());
    println!("cargo:rerun-if-changed={}", cooked_manifest.display());

    for name in GRID_WORLD_FILES {
        let path = generated_dir.join(name);
        if !path.exists() {
            std::fs::write(&path, [])
                .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let selected = if cooked_manifest.is_file() {
        cooked_manifest
    } else {
        placeholder_manifest
    };
    println!(
        "cargo:rustc-env=PSXED_PLAYTEST_MANIFEST={}",
        selected.display()
    );
}
