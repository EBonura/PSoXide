// SPDX-License-Identifier: GPL-2.0-or-later
//! The scene core: the one piece of code that stays in RAM while one program on a disc hands
//! over to the next.
//!
//! The core is built once per SDK pin (the `image` crate next to this one) into a flat image that
//! runs at [`abi::WINDOW`], 0x801FDF00..0x801FFF00. Every program that runs on the core embeds
//! those exact bytes (feature `link`), so the window is byte-identical in every program by
//! construction, and calls the core with direct `jal`s to the fixed slots of its jump table.
//!
//! What the core owns: the CD drive and its console rules for every read and every switch, the
//! scene table, the switch itself and the handoff block. Everything a program needs while it
//! runs (exception handler, presenter, pad, streaming engines, XA) stays in the program.
//!
//! This crate is three things:
//! - [`abi`]: the addresses, layouts and codes both sides agree on;
//! - [`fnv`] and [`fatal`]: the two routines both sides run (the program runs its own copies
//!   before it may trust the window);
//! - with feature `link`, the program side: the embedded image, `__core_boot` and the calls.
#![no_std]
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]

pub mod abi;
pub mod fatal;
pub mod fnv;

#[cfg(feature = "link")]
pub mod link;
