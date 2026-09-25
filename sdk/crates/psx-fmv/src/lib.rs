// SPDX-License-Identifier: GPL-2.0-or-later
//! FMV playback building blocks for PS1 guests.
//!
//! A movie is a `.STR` file: 2048-byte sectors, each carrying a 32-byte
//! chunk header and 2016 bytes of one frame's compressed bitstream (the
//! "BS" format). Playing one is a pipeline:
//!
//! 1. CD sectors stream in (the caller owns the drive; see the
//!    `hello-fmv` example) and [`str::FrameAssembler`] stitches a frame's
//!    chunks back together.
//! 2. [`bs::decode_frame`] runs the variable-length decode on the CPU,
//!    turning the bitstream into the MDEC's run-length halfwords.
//! 3. [`mdec`] (guest only) feeds those to the MDEC over DMA0 and pulls
//!    decoded 16-pixel-wide columns back over DMA1 for upload to VRAM.
//!
//! Everything but [`mdec`] is plain logic, built and tested on the host.
//! Only bitstream version 2 is decoded so far; v3 differs in the DC
//! coding and is rejected with [`bs::BsError::Version`].
//!
//! The encoder is `psxavenc` (zlib license), run as a host build tool:
//! `psxavenc -t strv -v v2 -s 320x240 -r 15 -x 2 in.mp4 out.str`.

#![no_std]

pub mod bs;
pub mod iso;
#[cfg(target_arch = "mips")]
pub mod mdec;
pub mod str;
