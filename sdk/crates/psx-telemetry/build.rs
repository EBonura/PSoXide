//! The architecture gate's feature check (sdk/profile.toml; sdk/docs/GATE.md).
#![allow(dead_code, missing_docs)]
#[path = "../../../crates/psx-gate/src/features.rs"]
mod features;
#[path = "../../../crates/psx-gate/src/profile.rs"]
mod profile;

fn main() {
    features::run_build_script();
}
