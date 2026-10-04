// SPDX-License-Identifier: GPL-2.0-or-later
//! GTE (COP2) access -- PS1 Geometry Transformation Engine.
//!
//! The GTE isn't memory-mapped. It's MIPS coprocessor 2, accessed via
//! a dedicated instruction class (`MTC2`/`MFC2`/`CTC2`/`CFC2` to move
//! data between CPU and COP2 registers, `COP2 cofun` to run function
//! ops like RTPS/RTPT/MVMVA/NCDS).
//!
//! This crate exposes two layers:
//!
//! - **Low-level register macros**: [`write_data!`], [`read_data!`],
//!   [`write_control!`], [`read_control!`]. Each takes a literal
//!   register index (0..31) so the assembler emits the correct
//!   immediate field. Use these when you need direct access for
//!   performance-sensitive paths.
//!
//! - **High-level operation wrappers**: zero-argument inline functions
//!   for the common GTE commands with their typical options baked in
//!   (e.g. [`ops::project_single`] uses `sf=1, lm=0`). On MIPS each
//!   compiles to a single 4-byte `.word`. On host they dispatch to a per-thread
//!   software GTE living in [`host`].
//!
//! All function-op wrappers are `unsafe fn` -- they assume the caller
//! has loaded the required input registers via the register macros.
//!
//! # Same simulation, two backends
//!
//! - On `target_arch = "mips"` everything compiles down to direct
//!   coprocessor instructions, identical to writing the assembly by
//!   hand.
//! - On host the macros and ops route through [`psx_gte_core::Gte`]
//!   stored in a `thread_local!` cell. The simulation is the
//!   bit-faithful one the emulator already runs against PCSX-Redux's
//!   parity oracle, so editor previews match what the hardware draws.
//!
//! # Example
//!
//! ```ignore
//! use psx_gte::{ops::project_single, read_data, write_control, write_data};
//!
//! unsafe {
//!     write_control!(0, 0x0000_1000); // RT[0][0]=0x1000, RT[0][1]=0
//!     // … fill in the rest of the rotation matrix and TR …
//!     write_data!(0, (10 & 0xFFFF) | (20 << 16));
//!     write_data!(1, 30);
//!     project_single();
//!     let sxy2: u32 = read_data!(14);
//! }
//! ```

#![cfg_attr(target_arch = "mips", no_std)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

pub mod lighting;
pub mod math;
pub mod ops;
pub mod regs;
pub mod scene;
pub mod transform;

/// The COP2 instruction encodings the wrappers emit (`psx_hw::gte`).
pub use psx_hw::gte as encoding;

#[cfg(not(target_arch = "mips"))]
pub mod host;
