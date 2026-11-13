//! The architecture gate's shared parts (see `sdk/profile.toml` and sdk/docs/GATE.md).
//!
//! `profile` and `features` are std-only and are compiled straight into the build script of
//! every SDK crate that has features; `press` is the check the disc builders run; the program
//! scan lives in `psoxide-hazard` (`psx-gate`).

pub mod features;
pub mod press;
pub mod profile;
