#![no_std]

//! Empty placeholder.
//!
//! The XBSP/PXBSP readers lived here until the brush world was removed. The
//! pinned emulator frontend's manifest still names this crate as an optional
//! dependency of its `editor` feature, and that manifest is hydrated from a
//! pinned component revision, so the crate stays as an empty name to keep the
//! workspace resolvable. Delete it together with that dependency when the
//! frontend pin moves.
