// SPDX-License-Identifier: GPL-2.0-or-later
//! FMV playback building blocks for PS1 guests.
//!
//! A movie is a `.STR` file: 2048-byte sectors, each carrying a 32-byte
//! chunk header and 2016 bytes of one frame's compressed bitstream (the
//! "BS" format). Playing one is a pipeline:
//!
//! 1. CD sectors stream in (the caller owns the drive; see the
//!    `hello-fmv` example) and [`stream::FrameAssembler`] stitches a frame's
//!    chunks back together.
//! 2. [`bitstream::decode_frame`] runs the variable-length decode on the CPU,
//!    turning the bitstream into the MDEC's run-length halfwords.
//! 3. [`mdec::Mdec`], the driver that owns the MDEC's DMA channels, feeds
//!    those to the MDEC over DMA0 and pulls the decoded frame back
//!    over DMA1 for upload to VRAM.
//!
//! Everything but [`mdec`] is plain logic, built and tested on the host; the
//! driver only touches a register when one of its methods runs.
//!
//! A game that only wants to play a movie calls [`player::play`] (guest only),
//! which runs all three stages overlapped and shows the result; the
//! `hello-fmv` example is the simpler, sector-checking test player.
//!
//! Bitstream versions 1 and 2 (the same coding) are decoded; v3 differs in the DC
//! coding and is rejected with [`bitstream::DecodeError::Version`].
//!
//! The encoder is the SDK's own `psx-str-cook` (`crates/psx-str-cook`), run as a host build tool:
//! `psx-str-cook encode --video in.y4m --audio in.wav --out out.str --size 320x240 --speed 2`.
//!
//! # Boot-only memory
//!
//! On the guest every function and static here names its own section, so a boot program that
//! links `psoxide-boot.ld` runs the player from memory its heap takes afterwards: the entry points
//! (`player::play`, `find_root_file`, `memory_words`; never inlined) and the statics in `.boot`,
//! which resident code may refer to, the rest in `.boot_inner`, which it may not. A program without
//! that script keeps all of it in `.text` and `.data`. Generic code of other crates the player
//! instantiates is not placed: resident code may share it.

#![no_std]

#[doc(alias = "BS")]
pub mod bitstream;
pub mod idct;
pub mod mdec;
#[cfg(target_arch = "mips")]
pub mod player;
pub mod rle;
#[doc(alias = "STR")]
pub mod stream;
