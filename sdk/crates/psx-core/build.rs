// SPDX-License-Identifier: GPL-2.0-or-later
//! The architecture gate's feature check (sdk/profile.toml; sdk/docs/GATE.md), and which core
//! image the `link` feature embeds: `prebuilt/psx-core.bin`, or the file named by
//! `PSX_CORE_IMAGE_OVERRIDE` (the hash-mismatch tests build a program on a second image).
#![allow(dead_code, missing_docs)]
#[path = "../../../crates/psx-gate/src/features.rs"]
mod features;
#[path = "../../../crates/psx-gate/src/profile.rs"]
mod profile;

fn main() {
    features::run_build_script();
    println!("cargo:rerun-if-env-changed=PSX_CORE_IMAGE_OVERRIDE");
    let path = match std::env::var("PSX_CORE_IMAGE_OVERRIDE") {
        Ok(path) if !path.is_empty() => path,
        _ => format!("{}/prebuilt/psx-core.bin", env!("CARGO_MANIFEST_DIR")),
    };
    println!("cargo:rerun-if-changed={path}");
    println!("cargo:rustc-env=PSX_CORE_IMAGE={path}");
}
