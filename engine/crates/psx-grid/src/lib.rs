//! The PGL grid level format, its zero-copy reader, native sector collision
//! and the grid world renderer.
//!
//! One crate, three jobs:
//!
//! * [`format`], [`view`], [`write`]: the file layout and its readers and
//!   writers. The cook (host) writes it, the guest casts it in place.
//! * [`height`] and [`collision`]: integer height functions and an O(1)
//!   per-sector collision provider behind `psx_engine::CollisionTraceProvider`.
//! * [`render`]: the row-span cull, shared-vertex projection and packet
//!   writer, with the camera, frustum and polygon clip it draws through.
//!
//! Docs: `docs/grid/level-format-and-runtime.md`.
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![no_std]
// ponytail: field docs land as the format settles, then this allow goes away.
#![allow(missing_docs)]

extern crate alloc;

pub mod activity;
pub mod collision;
pub mod contents;
pub mod format;
pub mod height;
pub mod lattice;
pub mod render;
pub mod resident;
pub mod toolchain_probe;
pub mod view;
pub mod walk;
pub mod write;

#[cfg(test)]
mod format_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod walk_tests;

pub use activity::RoomActivity;
pub use format::*;
pub use lattice::{Lattice, LatticeTables, SMALL_WORLD_ROOMS};
pub use resident::{ResidentError, ResidentRooms, RoomSlot, NO_SLOT};
pub use view::{CollisionView, FormatError, LevelView, RenderView, RoomView};
pub use walk::WalkSet;
