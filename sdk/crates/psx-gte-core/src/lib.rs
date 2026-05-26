//! Pure-Rust GTE shared math types and host state machine.
//!
//! The math types are register-shaped and are available on every target.
//! The full software GTE state machine is compiled only for non-MIPS
//! builds: on real PS1 hardware the SDK talks to COP2 directly instead
//! of running the simulator on the CPU.
//!
//! No I/O, no inline assembly, no PS1 hardware bindings. Everything is
//! `core::cmp`-only arithmetic; safe to use from any host or target.
//!
//! - [`math`]: fixed-point types ([`Vec3I16`], [`Vec3I32`],
//!   [`Mat3I16`]) shared across the GTE / GPU layers.
//! - [`state::Gte`] (non-MIPS only): full register state + the 21 documented function
//!   opcodes (RTPS / RTPT / MVMVA / NCLIP / NCDS / AVSZ3 / etc.).
//!   Driven via [`Gte::execute`] with a 32-bit command word.

#![no_std]
#![warn(missing_docs)]

pub mod math;
#[cfg(not(target_arch = "mips"))]
pub mod state;
pub mod transform;

pub use math::{Mat3I16, Vec3I16, Vec3I32};
#[cfg(not(target_arch = "mips"))]
pub use state::{Gte, GteProfileSnapshot};
pub use transform::{cos_1_3_12, sin_1_3_12};
