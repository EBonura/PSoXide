// SPDX-License-Identifier: GPL-2.0-or-later
//! An in-memory [`Block`] device: a full 128 KiB card image in RAM.
//!
//! Used by the host test-suite (the whole filesystem is exercised against it)
//! and usable on-device as a virtual/scratch card or a staging buffer before a
//! single bulk flush to hardware.

use crate::{Block, Error, Result, CARD_SIZE, FRAME_COUNT, FRAME_SIZE};

/// A 128 KiB card held entirely in memory.
///
/// The struct owns the full image inline (128 KiB), so place it in a `static`
/// or box it on the heap rather than passing it around by value on the small
/// PS1 stack.
#[repr(C)]
pub struct RamCard {
    bytes: [u8; CARD_SIZE],
}

impl RamCard {
    /// A blank (all-zero, unformatted) card. Call [`crate::Card::format`] to lay
    /// down a valid directory.
    pub const fn new() -> Self {
        RamCard {
            bytes: [0u8; CARD_SIZE],
        }
    }

    /// Build from an existing 128 KiB image (e.g. a `.mcd` file loaded on a host).
    pub fn from_image(image: &[u8]) -> Result<Self> {
        if image.len() != CARD_SIZE {
            return Err(Error::OutOfRange);
        }
        let mut c = RamCard::new();
        c.bytes.copy_from_slice(image);
        Ok(c)
    }

    /// Borrow the raw 128 KiB image (for persisting to a host file).
    pub fn image(&self) -> &[u8; CARD_SIZE] {
        &self.bytes
    }
}

impl Default for RamCard {
    fn default() -> Self {
        Self::new()
    }
}

impl Block for RamCard {
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()> {
        let f = frame as usize;
        if f >= FRAME_COUNT {
            return Err(Error::OutOfRange);
        }
        let base = f * FRAME_SIZE;
        out.copy_from_slice(&self.bytes[base..base + FRAME_SIZE]);
        Ok(())
    }

    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()> {
        let f = frame as usize;
        if f >= FRAME_COUNT {
            return Err(Error::OutOfRange);
        }
        let base = f * FRAME_SIZE;
        self.bytes[base..base + FRAME_SIZE].copy_from_slice(data);
        Ok(())
    }
}
