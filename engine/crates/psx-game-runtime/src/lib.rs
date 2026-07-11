//! PSoXide's game layer -- the runtime between `psx-engine`'s
//! primitives and a game. Phase 1 (see `docs/game-runtime-plan.md`):
//! CD streaming and, next, the room residency/VRAM stack carved out
//! of the `editor-playtest` example so every downstream game stops
//! re-implementing them.
//!
//! Boundary rules (the short version): this crate owns streaming and
//! gameplay POLICY; `psx-engine` owns drawing/scheduling MECHANISM;
//! capacities arrive through generated budgets, cooked data arrives
//! as `&'static` typed records, and state lives in owned arenas.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

// Carried verbatim from editor-playtest in the phase-1 carve. The
// example was an excluded package and never inherited the workspace
// lints, so its `unsafe fn` bodies predate unsafe_op_in_unsafe_fn
// hygiene (108 sites) and it manages state through `static mut`.
// Both get fixed together in the phase-1.5 API pass (see
// docs/game-runtime-plan.md boundary rules) -- wrapping 108 unsafe
// sites by hand mid-carve invites exactly the drift the carve's
// bit-identical gate exists to prevent.
#[allow(unsafe_op_in_unsafe_fn)]
#[allow(missing_docs)]
pub mod cd_stream;
pub mod room_cache;
pub mod room_visibility;
