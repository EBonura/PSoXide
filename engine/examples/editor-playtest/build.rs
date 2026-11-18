use std::path::PathBuf;

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

    let selected = if cooked_manifest.is_file() {
        cooked_manifest
    } else {
        placeholder_manifest
    };
    println!(
        "cargo:rustc-env=PSXED_PLAYTEST_MANIFEST={}",
        selected.display()
    );

    // The project's link order (tools/build_guest_staged.sh places it from the project's
    // link-order.layout and relinks with it). An ordering file changes no rustflags, so only this
    // crate relinks; ld.lld keeps the last --symbol-ordering-file it is given.
    println!("cargo:rerun-if-env-changed=PSOXIDE_LINK_ORDER");
    if let Some(order) = std::env::var_os("PSOXIDE_LINK_ORDER") {
        println!(
            "cargo:rustc-link-arg=--symbol-ordering-file={}",
            order.to_string_lossy()
        );
    }
}
