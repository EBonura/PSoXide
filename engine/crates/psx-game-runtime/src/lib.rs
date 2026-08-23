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

pub mod actor_pose;
pub mod arch_props;
#[cfg(feature = "cd-stream-bench")]
pub mod asset_streaming;
pub mod box_props;
pub mod cd_stream;
pub mod character;
pub mod combat;
pub mod cylinder_props;
pub mod entities;
pub mod image_props;
pub mod logic;
pub mod model_rendering;
pub mod particles;
pub mod room_cache;
pub mod room_lighting;
pub mod room_streaming;
pub mod room_visibility;
pub mod room_window;
pub mod save;
pub mod schedule;
pub mod sky;
pub mod vitality;
pub mod vram;
pub mod water;
pub mod world_cells;
pub mod world_visibility;
