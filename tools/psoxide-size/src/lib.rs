//! Link-map based size accounting and fault records for PS1 guests.
//!
//! `map` parses the lld link map, `classify` assigns every input section to a
//! class (panic and fmt machinery, Debug impls, strings, tables, SDK, game),
//! and `fault` reads psx-rt's `__psx_rt_fault_*` words out of a RAM dump so a
//! build run with `-Cpanic=immediate-abort` can still report where it stopped.
//! `ram` is the free-RAM headline and the per-section, per-crate and per-module
//! tables a game's budget gate reads; `block` finds a versioned state block in
//! a PS-X EXE.

pub mod block;
pub mod classify;
pub mod fault;
pub mod map;
pub mod policy;
pub mod ram;
