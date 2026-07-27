// SPDX-License-Identifier: GPL-2.0-or-later
//! PlayStation 1 memory-card driver.
//!
//! Three layers, each usable on its own:
//!
//! 1. **Transport** -- a [`Block`] device exposes the card as 1024 raw 128-byte
//!    frames. [`HardwareCard`] talks to a physical card over SIO0 (feature
//!    `hw`, on by default); [`RamCard`] is an in-memory image for host tests and
//!    virtual saves.
//! 2. **Filesystem** -- [`Card`] wraps any [`Block`] and implements the standard
//!    PS1 directory: named files, block allocation with the real link-chain,
//!    BIOS-visible title + icon frames, and per-frame XOR checksums. Saves show
//!    up in the console's memory-card manager like any retail game.
//! 3. **Container** -- [`Card::write`] wraps payloads in a small self-describing
//!    header so [`Card::read`] can transparently handle both plain and (feature
//!    `compress`) LZSS-compressed saves.
//!
//! ## Why talk to SIO0 directly
//!
//! Like [`psx_pad`](../psx_pad), this avoids BIOS syscalls so the same code runs
//! under an HLE BIOS side-load or a real boot. The card protocol is a fixed
//! request/response the [`sio`] module drives byte by byte.
//!
//! ## Example
//!
//! ```ignore
//! use psx_mc::{Card, HardwareCard, Slot};
//!
//! let mut card = Card::new(HardwareCard::new(Slot::One));
//! if !card.is_formatted()? {
//!     card.format()?;
//! }
//! card.write("BESLES-00000MYGAME01", "MY GAME", &save_bytes)?;
//!
//! let mut buf = [0u8; 8192];
//! let len = card.read("BESLES-00000MYGAME01", &mut buf)?;
//! ```
//!
//! All logic below the transport is pure and covered by host unit tests
//! (`cargo test --no-default-features`, optionally `--features compress`).

#![no_std]
#![allow(clippy::result_unit_err)]

mod fs;
mod ram;

#[cfg(test)]
mod tests;

#[cfg(feature = "compress")]
pub mod compress;

#[cfg(feature = "hw")]
pub mod sio;

pub use fs::Icon;
pub use ram::RamCard;

#[cfg(feature = "hw")]
pub use sio::{AckMode, HardwareCard, ReadDiag, Slot, Timing};

// --------------------------------------------------------------------------
// Card geometry (fixed by the hardware).
// --------------------------------------------------------------------------

/// Bytes per frame (the transfer unit of the serial protocol).
pub const FRAME_SIZE: usize = 128;
/// Frames per 8 KiB block.
pub const FRAMES_PER_BLOCK: usize = 64;
/// Total blocks on a standard card (block 0 is the directory).
pub const BLOCK_COUNT: usize = 16;
/// Total frames on a standard card.
pub const FRAME_COUNT: usize = BLOCK_COUNT * FRAMES_PER_BLOCK; // 1024
/// Total card size in bytes (128 KiB).
pub const CARD_SIZE: usize = FRAME_COUNT * FRAME_SIZE;
/// Usable save blocks (blocks 1..=15; block 0 holds the directory).
pub const DATA_BLOCKS: usize = BLOCK_COUNT - 1;
/// Maximum file-name length (region+product code + name), excluding the NUL.
pub const MAX_NAME: usize = 20;

// --------------------------------------------------------------------------
// Errors.
// --------------------------------------------------------------------------

/// Everything that can go wrong talking to a card or its filesystem.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No card responded on the port (no `/ACK`), or it was removed mid-transfer.
    NoCard,
    /// The card broke protocol (bad ID/ACK/terminator byte).
    Protocol,
    /// A frame's stored checksum did not match its data (read); likely a
    /// corrupt or half-written card.
    BadChecksum,
    /// Frame index past the end of the card.
    OutOfRange,
    /// The card has no valid `MC` directory header (needs [`Card::format`]).
    NotFormatted,
    /// No file with the requested name exists.
    NotFound,
    /// Not enough free blocks for the save.
    NoSpace,
    /// A file with that name already exists and `overwrite` was not set.
    Exists,
    /// The directory link-chain is inconsistent (corrupt card).
    Corrupt,
    /// The caller's read buffer is smaller than the stored payload.
    BufferTooSmall,
    /// The save is not a `psx-mc` container, or its header is malformed.
    BadContainer,
    /// The payload is compressed but this build lacks the `compress` feature,
    /// or decompression failed.
    Compression,
    /// A supplied name/title was empty or too long.
    BadName,
}

/// Convenience alias.
pub type Result<T> = core::result::Result<T, Error>;

// --------------------------------------------------------------------------
// Block transport abstraction.
// --------------------------------------------------------------------------

/// A card as a flat array of [`FRAME_COUNT`] 128-byte frames.
///
/// This is the seam between the hardware and the filesystem: implement it once
/// per backing (real card, RAM image, a file on a host) and the whole [`Card`]
/// filesystem works unchanged.
pub trait Block {
    /// Read frame `frame` (0..[`FRAME_COUNT`]) into `out`.
    fn read_frame(&mut self, frame: u16, out: &mut [u8; FRAME_SIZE]) -> Result<()>;
    /// Write `data` to frame `frame` (0..[`FRAME_COUNT`]).
    fn write_frame(&mut self, frame: u16, data: &[u8; FRAME_SIZE]) -> Result<()>;
}

// --------------------------------------------------------------------------
// Directory listing entry.
// --------------------------------------------------------------------------

/// One file as reported by [`Card::list`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// File name bytes (ASCII), `name_len` valid.
    pub name: [u8; MAX_NAME + 1],
    /// Valid length of `name`.
    pub name_len: u8,
    /// Number of 8 KiB blocks the file occupies.
    pub blocks: u8,
}

impl Entry {
    /// The file name as a `&str` (names are ASCII by construction).
    pub fn name(&self) -> &str {
        // SAFETY: names only ever contain the ASCII bytes validated on write.
        unsafe { core::str::from_utf8_unchecked(&self.name[..self.name_len as usize]) }
    }
}

// --------------------------------------------------------------------------
// The filesystem handle. Method bodies live in `fs.rs`.
// --------------------------------------------------------------------------

/// A memory card with the standard PS1 filesystem on top of a [`Block`] device.
pub struct Card<B: Block> {
    pub(crate) dev: B,
}

impl<B: Block> Card<B> {
    /// Wrap a block device. Does no I/O; call [`Card::is_formatted`] /
    /// [`Card::format`] as needed.
    pub fn new(dev: B) -> Self {
        Card { dev }
    }

    /// Recover the underlying block device.
    pub fn into_inner(self) -> B {
        self.dev
    }

    /// Borrow the underlying block device.
    pub fn device(&mut self) -> &mut B {
        &mut self.dev
    }
}

/// The save-container header written ahead of every payload so reads are
/// self-describing. 16 bytes, little-endian.
///
/// ```text
/// 0  "PMC1"          magic
/// 4  flags (u8)      bit0 = payload is LZSS-compressed
/// 5  reserved[3]
/// 8  raw_len  (u32)  original payload length
/// 12 stored_len(u32) bytes stored after this header
/// ```
pub(crate) const CONTAINER_MAGIC: [u8; 4] = *b"PMC1";
pub(crate) const CONTAINER_LEN: usize = 16;
pub(crate) const FLAG_COMPRESSED: u8 = 1 << 0;
