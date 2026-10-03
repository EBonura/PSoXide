// SPDX-License-Identifier: GPL-2.0-or-later
//! Renamed to [`crate::stream`].
//!
//! Every item here forwards to its new path and is deprecated.

use crate::stream;

/// Moved to [`stream::CHUNK_HEADER_BYTES`].
#[deprecated(note = "moved to `psx_fmv::stream::CHUNK_HEADER_BYTES`")]
pub const CHUNK_HEADER_BYTES: usize = stream::CHUNK_HEADER_BYTES;
/// Moved to [`stream::CHUNK_PAYLOAD_BYTES`].
#[deprecated(note = "moved to `psx_fmv::stream::CHUNK_PAYLOAD_BYTES`")]
pub const CHUNK_PAYLOAD_BYTES: usize = stream::CHUNK_PAYLOAD_BYTES;
/// Moved to [`stream::MAX_CHUNKS`].
#[deprecated(note = "moved to `psx_fmv::stream::MAX_CHUNKS`")]
pub const MAX_CHUNKS: u16 = stream::MAX_CHUNKS;

/// Moved to [`stream::Chunk`].
#[deprecated(note = "moved to `psx_fmv::stream::Chunk`")]
pub type Chunk = stream::Chunk;
/// Moved to [`stream::Frame`].
#[deprecated(note = "moved to `psx_fmv::stream::Frame`")]
pub type Frame = stream::Frame;
/// Moved to [`stream::FrameAssembler`].
#[deprecated(note = "moved to `psx_fmv::stream::FrameAssembler`")]
pub type FrameAssembler = stream::FrameAssembler;
