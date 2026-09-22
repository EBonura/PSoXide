// SPDX-License-Identifier: GPL-2.0-or-later
//! Write the link map next to the executable, where `make example` hands it
//! to tools/stack_guard.py. Only this crate's link gets the flag, so the
//! shared build-std artifacts keep the same rustflags as every other example.
use std::path::Path;

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    // OUT_DIR is <target>/<triple>/<profile>/build/<pkg>-<hash>/out.
    let profile_dir = Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("OUT_DIR depth");
    let map = profile_dir.join(format!("{}.map", std::env::var("CARGO_PKG_NAME").unwrap()));
    println!("cargo:rustc-link-arg-bins=-Map={}", map.display());
    println!("cargo:rerun-if-changed=build.rs");
}
