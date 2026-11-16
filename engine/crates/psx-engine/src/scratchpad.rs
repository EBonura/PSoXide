// SPDX-License-Identifier: GPL-2.0-or-later
//! PS1 CPU scratchpad access: the one module is [`psx_rt::scratchpad`] and this is its name inside
//! the engine.
//!
//! The base pointer, typed access, the named [`Region`]s with their compile-time disjointness
//! check, and running a scratchpad-free phase with its stack here ([`ScratchpadStack`]) are all
//! psx-rt's. Games still own their byte layouts and lifetimes: a shared allocator would add
//! bookkeeping to the console's hottest paths and make otherwise valid overlapping phase
//! reservations impossible. Every guest that runs a [`ScratchpadStack`] must pass `stack-guard` on
//! its linked image; the editor guest build runs it after the hazard patch.
//!
//! The re-export is the engine's public name for it, so callers written against
//! `psx_engine::scratchpad` (psx-goldsrc, psx-bsp, psx-grid, psx-game-runtime, Quake, NitroXide) keep
//! building; a program that links psx-rt names `psx_rt::scratchpad` directly.

pub use psx_rt::scratchpad::{
    assert_disjoint, base_ptr, clear, ptr_at, Region, ScratchpadStack, SIZE,
};
