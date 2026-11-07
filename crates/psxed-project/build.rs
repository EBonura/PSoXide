//! Selects the RON the built-in starter project is parsed from.
//!
//! The starter is the live `projects/default/project.ron` of the tree that
//! hydrates this crate (the editor repository), so edits to that project keep
//! showing up in `ProjectDocument::starter()`. A tree without a `projects/`
//! directory (the SDK repository) falls back to the committed snapshot in
//! `starter/project.ron`.
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let live = manifest.join("../../projects/default/project.ron");
    let snapshot = manifest.join("starter/project.ron");
    println!("cargo:rerun-if-changed={}", live.display());
    println!("cargo:rerun-if-changed={}", snapshot.display());
    println!("cargo:rerun-if-changed=build.rs");
    let chosen = if live.is_file() { live } else { snapshot };
    let chosen = chosen.canonicalize().expect("starter project.ron resolves");
    println!("cargo:rustc-env=PSXED_STARTER_PROJECT_RON={}", chosen.display());
}
