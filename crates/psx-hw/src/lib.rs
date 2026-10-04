// SPDX-License-Identifier: GPL-2.0-or-later
//! PlayStation 1 hardware model.
//!
//! This crate is the single source of truth for PS1 hardware details:
//! register addresses, bitfield layouts, and command packet formats.
//! Both the emulator and the SDK depend on this crate; they cannot drift
//! from each other because they read the same constants.
//!
//! **This crate defines data, not behavior.** Nothing here allocates,
//! performs I/O, or implements traits beyond basic `#[derive]`s. If a
//! symbol requires `#[cfg(feature = "std")]` or a runtime allocator, it
//! does not belong here.
//!
//! Hardware references used throughout:
//! - nocash PSX-SPX (<https://psx-spx.consoledev.net/>), cited per module

#![no_std]
#![forbid(unsafe_code)]

pub mod cd;
pub mod cop0;
pub mod dma;
pub mod gpu;
pub mod gte;
pub mod hash;
pub mod irq;
pub mod mdec;
pub mod memory;
pub mod sio;
pub mod spu;
pub mod timers;
