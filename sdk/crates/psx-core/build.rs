// SPDX-License-Identifier: GPL-2.0-or-later
//! Which core image the `link` feature embeds: `prebuilt/psx-core.bin`, or the file named by
//! `PSX_CORE_IMAGE_OVERRIDE` (the hash-mismatch tests build a program on a second image).
fn main() {
    println!("cargo:rerun-if-env-changed=PSX_CORE_IMAGE_OVERRIDE");
    let path = match std::env::var("PSX_CORE_IMAGE_OVERRIDE") {
        Ok(path) if !path.is_empty() => path,
        _ => format!("{}/prebuilt/psx-core.bin", env!("CARGO_MANIFEST_DIR")),
    };
    println!("cargo:rerun-if-changed={path}");
    println!("cargo:rustc-env=PSX_CORE_IMAGE={path}");
}
