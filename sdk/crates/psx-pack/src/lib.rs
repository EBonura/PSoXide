// SPDX-License-Identifier: GPL-2.0-or-later
//! Guest-side WORLD.PAK parsing and in-place chunk decompression.
//!
//! `psx-iso::build_world_pack` (behind the `mkisopsx` CLI and the editor's
//! embedded Play) writes the pack; this crate is the matching reader logic.
//! Before it existed, every streaming game re-derived the parser and the
//! LZ4 decoder from the engine's `editor-playtest` example: oot-psx
//! `loader.rs`, zelda3-psx `loader.rs` (a copy of oot's), hl-psx
//! `cdstream.rs`. Reader and writer now live in one repo so the format
//! cannot drift.
//!
//! Scope: PURE logic only. Feeding sectors in (the CD-ROM command/DMA state
//! machine) stays with the caller for now; the proven reference is
//! `engine/examples/editor-playtest/src/cd_stream/hw.rs`. Folding that state
//! machine into the SDK is the planned follow-up; it needs emulator + silicon
//! verification that pure parsing does not.
//!
//! Pack layout (all little-endian, from `psx-iso`):
//!
//! ```text
//! header, 28 bytes:  "PSOXWPAK" | u32 version=1 | u32 chunk_count
//!                    | u32 total_sectors | u32 header_sectors | u32 table_bytes
//! table:             chunk_count x 24-byte entries:
//!                    u32 chunk_id | u32 sector_offset | u32 sector_count
//!                    | u32 byte_size | u32 checksum (FNV-1a) | u32 reserved
//! payloads:          each chunk starts on a 2048-byte sector boundary
//! ```
//!
//! Chunks written with `--world-pack-compress-rooms` carry an `HLZC` frame
//! (`"HLZC" | u32 raw_len | LZ4 block`); [`decompress_hlzc_in_place`] undoes
//! it inside the destination buffer, no scratch memory needed.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

/// Pack magic, first 8 header bytes.
pub const MAGIC: [u8; 8] = *b"PSOXWPAK";
/// The one pack version this parser understands.
pub const VERSION: u32 = 1;
/// Fixed header size in bytes.
pub const HEADER_BYTES: usize = 28;
/// Size of one chunk-table entry in bytes.
pub const ENTRY_BYTES: usize = 24;
/// User-data bytes per CD sector (MODE2 form 1).
pub const SECTOR_BYTES: usize = 2048;
/// Magic framing a compressed chunk payload.
pub const HLZC_MAGIC: [u8; 4] = *b"HLZC";

/// Parsed fixed header.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PackHeader {
    /// Number of entries in the chunk table.
    pub chunk_count: u32,
    /// Whole pack size in sectors.
    pub total_sectors: u32,
    /// Sectors covering header + chunk table (payloads start after them).
    pub header_sectors: u32,
    /// Chunk-table size in bytes (`chunk_count * ENTRY_BYTES`).
    pub table_bytes: u32,
}

/// One chunk-table entry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PackEntry {
    /// Caller-chosen chunk id (the pack is keyed by these, not by index).
    pub chunk_id: u32,
    /// Payload start, in sectors from the beginning of the pack.
    pub sector_offset: u32,
    /// Payload length in whole sectors.
    pub sector_count: u32,
    /// Exact payload length in bytes.
    pub byte_size: u32,
    /// FNV-1a over the payload bytes (see [`fnv1a32`]).
    pub checksum: u32,
}

fn le32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Parse and validate the fixed header from the first pack bytes
/// (any slice covering at least [`HEADER_BYTES`], e.g. the first sector).
/// `None` on wrong magic or version.
pub fn parse_header(bytes: &[u8]) -> Option<PackHeader> {
    if bytes.get(..MAGIC.len())? != MAGIC {
        return None;
    }
    if le32(bytes, 8)? != VERSION {
        return None;
    }
    Some(PackHeader {
        chunk_count: le32(bytes, 12)?,
        total_sectors: le32(bytes, 16)?,
        header_sectors: le32(bytes, 20)?,
        table_bytes: le32(bytes, 24)?,
    })
}

/// Parse the table entry at `index`. `bytes` must start at the beginning of
/// the pack (header included) and cover the entry; read
/// `header.header_sectors * SECTOR_BYTES` up front and every entry is in
/// range. `None` when the slice is too short.
pub fn parse_entry(bytes: &[u8], index: usize) -> Option<PackEntry> {
    let at = HEADER_BYTES + index * ENTRY_BYTES;
    Some(PackEntry {
        chunk_id: le32(bytes, at)?,
        sector_offset: le32(bytes, at + 4)?,
        sector_count: le32(bytes, at + 8)?,
        byte_size: le32(bytes, at + 12)?,
        checksum: le32(bytes, at + 16)?,
    })
}

/// Linear-scan the table for `chunk_id`. Same slice contract as
/// [`parse_entry`]. Packs are written sorted by id unless an explicit order
/// file reorders them, so callers doing many lookups should build their own
/// cache (hl-psx keeps a 512-entry table) rather than rescanning.
pub fn find_chunk(bytes: &[u8], header: &PackHeader, chunk_id: u32) -> Option<PackEntry> {
    for i in 0..header.chunk_count as usize {
        let entry = parse_entry(bytes, i)?;
        if entry.chunk_id == chunk_id {
            return Some(entry);
        }
    }
    None
}

/// FNV-1a over `bytes`; the checksum `psx-iso` stores per entry.
pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Undo the `HLZC` compression frame in place.
///
/// On entry `buf[..loaded]` holds a chunk payload as read from disc. A chunk
/// without the frame (mkisopsx keeps raw bytes when compression does not
/// shrink them) is returned untouched as `Some(loaded)`. A framed chunk is
/// staged at the buffer tail and LZ4-decoded back to the head; `buf` only
/// needs to be large enough for the RAW payload plus the standard in-place
/// margin (the cooked-side buffer sizing already guarantees this; a too-small
/// buffer fails cleanly rather than corrupting).
///
/// `None` means a corrupt stream, a truncated buffer, or a decode that did
/// not produce exactly `raw_len` bytes. Callers should treat it as a failed
/// load, not a partial one.
///
/// Caveat inherited from the format: a RAW chunk whose first four bytes
/// happen to be `HLZC` would be misread as compressed. Cooked chunk types
/// all start with their own magics (`HLMA`, `PSXT`, ...), which is what
/// makes the sniff safe in practice.
pub fn decompress_hlzc_in_place(buf: &mut [u8], loaded: usize) -> Option<usize> {
    if loaded > buf.len() {
        return None;
    }
    if loaded < 8 || buf[..4] != HLZC_MAGIC {
        return Some(loaded);
    }
    let raw_len = le32(buf, 4)? as usize;
    let comp_len = loaded - 8;
    let cap = buf.len();
    if raw_len > cap {
        return None;
    }
    // Stage the compressed stream at the buffer tail, then decode tail ->
    // head. Writes are checked against the read cursor each step, so even a
    // buffer without enough in-place margin fails instead of corrupting.
    let src_start = cap - comp_len;
    buf.copy_within(8..loaded, src_start);
    lz4_block_decode_in_place(buf, src_start, raw_len)
}

/// Decode one raw LZ4 block that has been staged at `buf[src_start..]`,
/// writing the output from `buf[0]`. Returns the decoded length, which must
/// equal `raw_len`.
fn lz4_block_decode_in_place(buf: &mut [u8], src_start: usize, raw_len: usize) -> Option<usize> {
    let cap = buf.len();
    let mut si = src_start;
    let mut di = 0usize;
    while si < cap {
        let token = buf[si];
        si += 1;
        // Literal run (token high nibble, 15 = extended).
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            loop {
                let b = *buf.get(si)?;
                si += 1;
                lit += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        if lit > 0 {
            // In-place safety: the write may not start past the read cursor.
            if si.checked_add(lit)? > cap || di + lit > raw_len || di > si {
                return None;
            }
            buf.copy_within(si..si + lit, di);
            si += lit;
            di += lit;
        }
        if si >= cap {
            break; // the final sequence is literals-only
        }
        // Match: 2-byte little-endian offset into the decoded history,
        // then a 4-based extendable length (token low nibble).
        if si + 2 > cap {
            return None;
        }
        let off = buf[si] as usize | ((buf[si + 1] as usize) << 8);
        si += 2;
        if off == 0 || off > di {
            return None;
        }
        let mut mlen = (token & 15) as usize;
        if mlen == 15 {
            loop {
                let b = *buf.get(si)?;
                si += 1;
                mlen += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        mlen += 4;
        // Writes must stay below the unread source.
        if di + mlen > raw_len || di + mlen > si {
            return None;
        }
        // Byte-at-a-time forward copy: replicates the window when
        // off < mlen, exactly LZ4's overlap semantics.
        for k in 0..mlen {
            buf[di + k] = buf[di + k - off];
        }
        di += mlen;
    }
    if di == raw_len {
        Some(di)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;
    use std::vec::Vec;

    fn frame_hlzc(raw: &[u8]) -> Vec<u8> {
        let comp = lz4_flex::block::compress(raw);
        let mut out = Vec::new();
        out.extend_from_slice(&HLZC_MAGIC);
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp);
        out
    }

    #[test]
    fn parses_a_pack_the_real_writer_built() {
        let a = vec![0xAAu8; 100];
        let b: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        let pack = psx_iso::build_world_pack(&[(10, a.as_slice()), (42, b.as_slice())]);

        let header = parse_header(&pack).expect("header");
        assert_eq!(header.chunk_count, 2);
        assert_eq!(header.table_bytes, 2 * ENTRY_BYTES as u32);

        let e = find_chunk(&pack, &header, 42).expect("chunk 42");
        assert_eq!(e.byte_size as usize, b.len());
        assert_eq!(e.sector_count, 3); // 5000 bytes = 3 sectors
        let start = e.sector_offset as usize * SECTOR_BYTES;
        let payload = &pack[start..start + e.byte_size as usize];
        assert_eq!(payload, b.as_slice());
        assert_eq!(fnv1a32(payload), e.checksum);

        assert!(find_chunk(&pack, &header, 7).is_none());
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let pack = psx_iso::build_world_pack(&[(1, &[1, 2, 3])]);
        let mut bad = pack.clone();
        bad[0] = b'X';
        assert!(parse_header(&bad).is_none());
        let mut v2 = pack;
        v2[8] = 2;
        assert!(parse_header(&v2).is_none());
    }

    #[test]
    fn decodes_real_lz4_in_place() {
        // Compressible payload with structure (runs + repeats + a text tail).
        let mut raw = Vec::new();
        for i in 0..6000u32 {
            raw.push((i / 64) as u8);
        }
        raw.extend_from_slice(b"the quick brown fox jumps over the lazy dog");
        let framed = frame_hlzc(&raw);
        assert!(framed.len() < raw.len(), "test payload must compress");

        // Generous margin, as the cook-side sizing provides.
        let mut buf = vec![0u8; raw.len() + 1024];
        buf[..framed.len()].copy_from_slice(&framed);
        let n = decompress_hlzc_in_place(&mut buf, framed.len()).expect("decode");
        assert_eq!(n, raw.len());
        assert_eq!(&buf[..n], raw.as_slice());
    }

    #[test]
    fn incompressible_payload_passes_through_raw() {
        // mkisopsx stores raw bytes when compression does not help; the
        // decoder must hand them back untouched.
        let raw: Vec<u8> = (0..999u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let mut buf = raw.clone();
        assert_eq!(decompress_hlzc_in_place(&mut buf, raw.len()), Some(raw.len()));
        assert_eq!(buf, raw);
    }

    #[test]
    fn overlapping_matches_replicate() {
        // 1 literal + long overlapping match (off=1): classic RLE-via-LZ4.
        let raw = vec![7u8; 300];
        let framed = frame_hlzc(&raw);
        let mut buf = vec![0u8; 512];
        buf[..framed.len()].copy_from_slice(&framed);
        let n = decompress_hlzc_in_place(&mut buf, framed.len()).expect("decode");
        assert_eq!(&buf[..n], raw.as_slice());
    }

    #[test]
    fn corrupt_streams_fail_cleanly() {
        let raw: Vec<u8> = (0..2000u32).map(|i| (i / 32) as u8).collect();
        let framed = frame_hlzc(&raw);

        // Truncated compressed stream.
        let mut buf = vec![0u8; raw.len() + 1024];
        buf[..framed.len() - 5].copy_from_slice(&framed[..framed.len() - 5]);
        assert_eq!(decompress_hlzc_in_place(&mut buf, framed.len() - 5), None);

        // raw_len larger than the buffer.
        let mut huge = framed.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut buf2 = vec![0u8; raw.len() + 1024];
        buf2[..huge.len()].copy_from_slice(&huge);
        assert_eq!(decompress_hlzc_in_place(&mut buf2, huge.len()), None);

        // Buffer without in-place margin: must fail, not corrupt.
        let mut tight = vec![0u8; framed.len()];
        tight.copy_from_slice(&framed);
        assert_eq!(decompress_hlzc_in_place(&mut tight, framed.len()), None);
    }

    #[test]
    fn fnv_matches_the_writer() {
        // Golden value: the writer's fnv1a32 over "PSOXWPAK".
        let pack = psx_iso::build_world_pack(&[(1, &MAGIC[..])]);
        let header = parse_header(&pack).unwrap();
        let e = find_chunk(&pack, &header, 1).unwrap();
        assert_eq!(e.checksum, fnv1a32(&MAGIC));
    }
}
