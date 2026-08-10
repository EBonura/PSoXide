use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let generated_dir = manifest_dir.join("generated");
    let placeholder_manifest = generated_dir.join("level_manifest.rs");
    let cooked_manifest = generated_dir.join("level_manifest.cooked.rs");
    let placeholder_brush_manifest = generated_dir.join("brush_manifest.rs");
    let cooked_brush_manifest = generated_dir.join("brush_manifest.cooked.rs");

    println!("cargo:rerun-if-changed={}", generated_dir.display());
    println!("cargo:rerun-if-changed={}", placeholder_manifest.display());
    println!("cargo:rerun-if-changed={}", cooked_manifest.display());
    println!(
        "cargo:rerun-if-changed={}",
        placeholder_brush_manifest.display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        cooked_brush_manifest.display()
    );

    let selected = if cooked_manifest.is_file() {
        cooked_manifest
    } else {
        placeholder_manifest
    };
    println!(
        "cargo:rustc-env=PSXED_PLAYTEST_MANIFEST={}",
        selected.display()
    );
    let selected_brush = if cooked_brush_manifest.is_file() {
        cooked_brush_manifest
    } else {
        placeholder_brush_manifest
    };
    println!(
        "cargo:rustc-env=PSXED_BRUSH_PLAYTEST_MANIFEST={}",
        selected_brush.display()
    );
}
