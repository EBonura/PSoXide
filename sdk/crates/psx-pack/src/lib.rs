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
//! Scope: this root module is PURE logic (parsing + decompression), building
//! and testing on the host. The CD-ROM command/DMA state machine that feeds
//! sectors in lives in [`cd`] ([`cd::SectorReader`] + [`cd::load_chunk`]),
//! `cfg(target_arch = "mips")`-gated; the reader is `psx_io::cd::reader`, on
//! the `Cd` token. Note [`cd::SectorReader::prepare`] sets `I_MASK` to
//! VBlank-only until `stop` puts it back; read its docs before adopting.
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
#![cfg_attr(target_arch = "mips", feature(asm_experimental_arch))]
#![warn(missing_docs)]

pub mod cd;
/// Allocation-free Quake and GoldSrc visibility row codecs.
pub mod visibility;

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
    parse_entry_at(bytes, HEADER_BYTES + index * ENTRY_BYTES)
}

/// Parse one [`ENTRY_BYTES`]-sized table entry starting at an arbitrary byte
/// `offset`. This is the sector-local primitive behind [`parse_entry`]: a
/// caller holding a single 2048-byte header sector (not the whole pack) can
/// parse the entries it contains directly, and stitch the one entry that
/// straddles into the next sector through a small [`ENTRY_BYTES`] buffer
/// (see [`entry_location`]). `None` when the slice is too short.
pub fn parse_entry_at(bytes: &[u8], offset: usize) -> Option<PackEntry> {
    Some(PackEntry {
        chunk_id: le32(bytes, offset)?,
        sector_offset: le32(bytes, offset + 4)?,
        sector_count: le32(bytes, offset + 8)?,
        byte_size: le32(bytes, offset + 12)?,
        checksum: le32(bytes, offset + 16)?,
    })
}

/// Locate table entry `index` on disc: `(header_sector, byte_offset_within_it)`.
///
/// The chunk table is packed right after the 28-byte header with no sector
/// alignment, so every 85th entry straddles a sector boundary
/// (`offset + ENTRY_BYTES > SECTOR_BYTES`, first at index 84). Sector-by-sector
/// readers must detect that case and stitch the entry from the tail of this
/// sector plus the head of the next one.
pub const fn entry_location(index: u32) -> (u32, usize) {
    let byte = HEADER_BYTES + index as usize * ENTRY_BYTES;
    ((byte / SECTOR_BYTES) as u32, byte % SECTOR_BYTES)
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
#[inline]
#[must_use]
pub fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = Fnv32::new();
    hash.update(bytes);
    hash.finish()
}

/// Resumable FNV-1a over bytes: the same function as [`fnv1a32`], for data
/// that arrives in pieces (a save record written field by field, a stream
/// read sector by sector). Feeding the pieces in order gives exactly the
/// one-shot hash of their concatenation.
///
/// The guest build multiplies by the prime with shifts and adds: the R3000A's
/// `mult` takes 13 cycles on a full-width operand and LLVM turns any
/// shift-and-add form back into one, so the sequence is written out in
/// assembly. [`update`](Self::update) is the small form (about 19 cycles a
/// byte, one shared body for `fnv1a32`, `push` loops and the ends of `update_bulk`);
/// [`update_bulk`](Self::update_bulk) reads aligned words and is the one for large buffers
/// (about 13.7 cycles a byte).
///
/// ```
/// use psx_pack::{fnv1a32, Fnv32};
/// let mut hash = Fnv32::new();
/// hash.update(b"PSOX");
/// hash.update(b"WPAK");
/// assert_eq!(hash.finish(), fnv1a32(b"PSOXWPAK"));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv32(u32);

impl Fnv32 {
    /// FNV-1a 32-bit offset basis: the state of a hash that has seen no bytes.
    pub const OFFSET_BASIS: u32 = 0x811c_9dc5;

    /// A hash that has seen no bytes; `finish` returns the offset basis.
    #[must_use]
    pub const fn new() -> Self {
        Self(Self::OFFSET_BASIS)
    }

    /// A hash resumed from the `state` an earlier [`finish`](Self::finish) returned, for a
    /// caller that keeps the running value in its own struct between pieces.
    #[must_use]
    pub const fn from_state(state: u32) -> Self {
        Self(state)
    }

    /// Fold one byte into the hash (for a caller that maps each byte first,
    /// such as an ASCII case fold).
    #[inline(always)]
    pub fn push(&mut self, byte: u8) {
        self.0 = mul_prime(self.0 ^ u32::from(byte));
    }

    /// Fold `bytes` into the hash, one byte at a time.
    #[inline(never)]
    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.push(byte);
        }
    }

    /// [`update`](Self::update) for a large buffer: the bytes up to a word boundary and the last
    /// few go through `update`, the whole words between are fetched one load each (the R3000 has
    /// no data cache, so four byte loads are four bus accesses). Same hash; use it where bytes
    /// are many.
    #[inline(never)]
    pub fn update_bulk(&mut self, bytes: &[u8]) {
        let head = ((bytes.as_ptr() as usize).wrapping_neg() & 3).min(bytes.len());
        let (head, rest) = bytes.split_at(head);
        self.update(head);
        let words = rest.len() / 4;
        let mut hash = self.0;
        let mut p = rest.as_ptr().cast::<u32>();
        // SAFETY: one past the last whole word of `rest`.
        let end = unsafe { p.add(words) };
        while p != end {
            // SAFETY: `p` is word-aligned (the head was consumed) and inside `rest`.
            let word = u32::from_le(unsafe { p.read() });
            hash = mul_prime(hash ^ (word & 0xff));
            hash = mul_prime(hash ^ ((word >> 8) & 0xff));
            hash = mul_prime(hash ^ ((word >> 16) & 0xff));
            hash = mul_prime(hash ^ (word >> 24));
            // SAFETY: stays within one past the last whole word.
            p = unsafe { p.add(1) };
        }
        self.0 = hash;
        self.update(&rest[words * 4..]);
    }

    /// The hash of everything fed so far. The hash stays usable: more bytes
    /// may follow.
    #[must_use]
    pub const fn finish(&self) -> u32 {
        self.0
    }
}

/// `x * 0x0100_0193` (the FNV prime, 2^24 + 403) as eight shifts and adds on
/// the guest, where the multiply unit is the slower way.
#[cfg(target_arch = "mips")]
#[inline(always)]
fn mul_prime(mut x: u32) -> u32 {
    // SAFETY: register-only arithmetic; no memory, no stack, no flags.
    unsafe {
        core::arch::asm!(
            ".set push",
            ".set noat",
            "sll {t0}, {x}, 1",
            "addu {t0}, {t0}, {x}",
            "sll {t0}, {t0}, 2",
            "addu {t0}, {t0}, {x}",
            "sll {t1}, {t0}, 5",
            "subu {t0}, {t1}, {t0}",
            "sll {t1}, {x}, 24",
            "addu {x}, {t0}, {t1}",
            ".set pop",
            x = inout(reg) x,
            t0 = out(reg) _,
            t1 = out(reg) _,
            options(pure, nomem, nostack),
        );
    }
    x
}

/// `x * 0x0100_0193`.
#[cfg(not(target_arch = "mips"))]
#[inline(always)]
fn mul_prime(x: u32) -> u32 {
    x.wrapping_mul(0x0100_0193)
}

impl Default for Fnv32 {
    fn default() -> Self {
        Self::new()
    }
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

    #[test]
    fn fnv32_pieces_equal_one_shot_for_every_split() {
        let data: Vec<u8> = (0..300u32).map(|i| (i * 37 + 11) as u8).collect();
        let whole = fnv1a32(&data);
        for cut in 0..=data.len() {
            let mut h = Fnv32::new();
            h.update(&data[..cut]);
            h.update(&data[cut..]);
            assert_eq!(h.finish(), whole, "split at {cut}");
        }
        let mut bytewise = Fnv32::default();
        for b in &data {
            bytewise.update(core::slice::from_ref(b));
        }
        assert_eq!(bytewise.finish(), whole);
    }

    /// The game-side loops this type replaced, kept as oracles.
    fn game_fold(bytes: &[u8]) -> u32 {
        bytes.iter().fold(0x811c_9dc5u32, |h, b| {
            (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
        })
    }

    /// Hollow Knight's room_decode and ClipCheck form: the prime as shifts and adds.
    fn hk_shift_add(mut hash: u32, bytes: &[u8]) -> u32 {
        for &byte in bytes {
            let h = hash ^ u32::from(byte);
            hash = h
                .wrapping_add(h << 1)
                .wrapping_add(h << 4)
                .wrapping_add(h << 7)
                .wrapping_add(h << 8)
                .wrapping_add(h << 24);
        }
        hash
    }

    #[test]
    fn fnv32_equals_every_game_copy_at_every_alignment_and_length() {
        // 4 KiB of pseudo-random bytes in a word-aligned buffer.
        let mut backing = vec![0u32; 1100];
        let mut x = 0x1234_5678u32;
        for w in backing.iter_mut() {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *w = x;
        }
        // SAFETY: a u32 buffer viewed as its bytes.
        let all = unsafe { core::slice::from_raw_parts(backing.as_ptr().cast::<u8>(), 4400) };
        for offset in 0..8 {
            for len in (0..=70).chain([255, 256, 257, 1000, 4000]) {
                let bytes = &all[offset..offset + len];
                let want = game_fold(bytes);
                assert_eq!(fnv1a32(bytes), want, "one-shot {offset}+{len}");
                assert_eq!(hk_shift_add(0x811c_9dc5, bytes), want, "hk {offset}+{len}");
                let mut bulk = Fnv32::new();
                bulk.update_bulk(bytes);
                assert_eq!(bulk.finish(), want, "bulk {offset}+{len}");
                let mut pushed = Fnv32::new();
                for &b in bytes {
                    pushed.push(b);
                }
                assert_eq!(pushed.finish(), want, "push {offset}+{len}");
                // Resumed in the middle, the way a save record is fed.
                let cut = len / 3;
                let mut parts = Fnv32::new();
                parts.update_bulk(&bytes[..cut]);
                parts.update_bulk(&bytes[cut..]);
                assert_eq!(parts.finish(), want, "resumed {offset}+{len}");
            }
        }
    }

    #[test]
    fn the_prime_as_shifts_and_adds_is_the_multiply() {
        let shifts = |x: u32| {
            let t = x
                .wrapping_shl(1)
                .wrapping_add(x)
                .wrapping_shl(2)
                .wrapping_add(x);
            t.wrapping_shl(5)
                .wrapping_sub(t)
                .wrapping_add(x.wrapping_shl(24))
        };
        let mut x = 1u32;
        for _ in 0..2_000_000 {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            assert_eq!(shifts(x), x.wrapping_mul(0x0100_0193));
        }
        for x in [0, 1, 0xff, 0x8000_0000, 0xffff_ffff, 0x0100_0193] {
            assert_eq!(shifts(x), x.wrapping_mul(0x0100_0193));
        }
    }

    #[test]
    fn fnv32_resumes_from_a_kept_state() {
        let data: Vec<u8> = (0..200u32).map(|i| (i * 91 + 3) as u8).collect();
        for cut in 0..=data.len() {
            let mut first = Fnv32::new();
            first.update_bulk(&data[..cut]);
            let mut second = Fnv32::from_state(first.finish());
            second.update_bulk(&data[cut..]);
            assert_eq!(second.finish(), fnv1a32(&data));
        }
        assert_eq!(Fnv32::from_state(Fnv32::OFFSET_BASIS), Fnv32::new());
    }

    #[test]
    fn fnv32_known_values() {
        assert_eq!(Fnv32::new().finish(), 0x811c_9dc5);
        assert_eq!(fnv1a32(b""), 0x811c_9dc5);
        // Published FNV-1a 32 test vectors.
        assert_eq!(fnv1a32(b"a"), 0xe40c_292c);
        assert_eq!(fnv1a32(b"foobar"), 0xbf9c_f968);
    }
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
        let a = vec![0xaau8; 100];
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
        let raw: Vec<u8> = (0..999u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let mut buf = raw.clone();
        assert_eq!(
            decompress_hlzc_in_place(&mut buf, raw.len()),
            Some(raw.len())
        );
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
    fn entry_location_matches_flat_table_offsets() {
        for index in 0..300u32 {
            let flat = HEADER_BYTES + index as usize * ENTRY_BYTES;
            let (sector, within) = entry_location(index);
            assert_eq!(sector as usize * SECTOR_BYTES + within, flat);
            assert!(within < SECTOR_BYTES);
        }
        // The first straddling entry: 28 + 84*24 = 2044, crossing into sector 1.
        assert_eq!(entry_location(84), (0, 2044));
        assert_eq!(entry_location(85), (1, 20));
    }

    /// Scan a real psx-iso pack the way the CD reader does: one buffered
    /// sector at a time, stitching entries that straddle sector boundaries
    /// through a 24-byte staging buffer. Every entry must parse identically
    /// to the flat whole-pack parse.
    #[test]
    fn sector_local_scan_with_straddles_matches_flat_parse() {
        // 90 chunks make the table span three sectors' worth of entries is
        // not needed; two sectors with one straddle (index 84) is the case
        // that matters. Non-contiguous ids prove index != id.
        let payloads: Vec<(u32, Vec<u8>)> = (0..90u32)
            .map(|i| (i * 3 + 1, vec![i as u8; 5 + (i as usize % 40)]))
            .collect();
        let refs: Vec<(u32, &[u8])> = payloads
            .iter()
            .map(|(id, bytes)| (*id, bytes.as_slice()))
            .collect();
        let pack = psx_iso::build_world_pack(&refs);

        let header = parse_header(&pack).expect("header");
        assert_eq!(header.chunk_count, 90);
        assert!(
            header.header_sectors >= 2,
            "fixture must produce a multi-sector table"
        );

        let sector = |s: u32| -> &[u8] {
            let at = s as usize * SECTOR_BYTES;
            &pack[at..at + SECTOR_BYTES]
        };

        let mut straddles = 0;
        for index in 0..header.chunk_count {
            let (sec, within) = entry_location(index);
            assert!(sec < header.header_sectors);
            let entry = if within + ENTRY_BYTES <= SECTOR_BYTES {
                parse_entry_at(sector(sec), within).expect("in-sector entry")
            } else {
                straddles += 1;
                let first = SECTOR_BYTES - within;
                let mut stitched = [0u8; ENTRY_BYTES];
                stitched[..first].copy_from_slice(&sector(sec)[within..]);
                stitched[first..].copy_from_slice(&sector(sec + 1)[..ENTRY_BYTES - first]);
                parse_entry_at(&stitched, 0).expect("stitched entry")
            };
            let flat = parse_entry(&pack, index as usize).expect("flat entry");
            assert_eq!(entry, flat, "entry {index} diverged");
        }
        assert_eq!(straddles, 1, "index 84 must straddle sectors 0 and 1");
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
