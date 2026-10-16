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
//!   writer.
//!
//! Docs: `docs/grid/level-format-and-runtime.md`.
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![no_std]
// ponytail: field docs land as the format settles, then this allow goes away.
#![allow(missing_docs)]

extern crate alloc;

pub mod format;
pub mod height;
pub mod view;
pub mod write;

pub use format::*;
pub use view::{CollisionView, FormatError, LevelView, RenderView, RoomView};
