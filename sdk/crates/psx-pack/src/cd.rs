// SPDX-License-Identifier: GPL-2.0-or-later
//! The pack-table scan and chunk streaming, on top of the polled sector
//! reader.
//!
//! The CD controller has one driver, `psx_io::cd`: the `Cd` token, its
//! register steps, and [`SectorReader`], the polled reader that sequences
//! SetMode, Setloc, ReadN and Pause the way the pack loader has run on
//! silicon. This module holds what the pack format adds: finding a chunk's
//! table entry and copying its sectors into a caller's buffer.
//!
//! Everything that touches the drive is `cfg(target_arch = "mips")`; on the
//! host only the constants and the [`SectorReader`] re-export compile, so
//! `cargo test` keeps covering the pure pieces.
//!
//! # What loading does to the machine
//!
//! [`load_chunk`] and [`find_entry`] bracket each read with
//! [`SectorReader::prepare`] and [`SectorReader::stop`]. Between the two,
//! `I_MASK` is VBlank-only (the reader polls the controller's own IRQ flags,
//! so a CD-ROM CPU interrupt with no handler cannot storm). `stop` puts the
//! previous mask back, so a caller's timer or controller interrupts survive a
//! chunk load. The reader needs the `Cd` token: `SectorReader::with_cd`.
//!
//! No caching happens at this layer. hl-psx's 512-entry table cache (skip
//! re-scanning the header on every chunk load) is a game-side optimization:
//! keep it in the game, keyed to its own pack, where the entry count and the
//! RAM budget are known.

use crate::SECTOR_BYTES;
#[cfg(target_arch = "mips")]
use crate::{entry_location, parse_entry_at, parse_header, PackEntry, ENTRY_BYTES};

/// Moved to `psx_io::cd::reader`. Kept at this path for one stage so
/// `psx_pack::cd::SectorReader` still resolves; a re-export cannot carry a
/// deprecation.
pub use psx_io::cd::reader::{
    SectorReader, DIAG_CD_ERROR, DIAG_PARAM_STUCK, DIAG_SITE_READ, DIAG_TIMEOUT,
};

/// One CD sector's user data in 32-bit words (`SECTOR_BYTES / 4`).
pub const SECTOR_WORDS: usize = SECTOR_BYTES / 4;

const _: () = assert!(SECTOR_WORDS == psx_io::cd::reader::SECTOR_WORDS);

/// Where `mkisopsx` / the editor's embedded Play place `WORLD.PAK`: the pack's
/// first sector, as an absolute data-track LBA. Mirrors
/// `psx_iso::WORLD_PACK_DEFAULT_START_LBA` (the writer reserves a fixed boot
/// area so runtime LBAs never depend on the boot EXE size); a host test in
/// this crate asserts the two constants stay equal.
pub const WORLD_PACK_DEFAULT_LBA: u32 = 1024;

/// The sector currently in `scratch`, as bytes (little-endian DMA words are
/// exactly the on-disc byte order).
#[cfg(target_arch = "mips")]
fn scratch_bytes(scratch: &[u32; SECTOR_WORDS]) -> &[u8] {
    // SAFETY: [u32; N] reinterpreted as its own bytes; alignment shrinks.
    unsafe { core::slice::from_raw_parts(scratch.as_ptr() as *const u8, SECTOR_BYTES) }
}

/// Read pack header sector `sector` (relative to `pack_lba`) into `scratch`,
/// unless `*loaded` says it is already there. Each miss is a full
/// prepare/seek/read/stop cycle, which is why sequential entry scans track
/// `loaded` (ports hl-psx's `load_pack_header_sector`).
#[cfg(target_arch = "mips")]
fn load_header_sector(
    rd: &mut SectorReader,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    sector: u32,
) -> bool {
    if *loaded == sector {
        return true;
    }
    if !rd.prepare() || !rd.start_read(pack_lba + sector) {
        rd.stop();
        return false;
    }
    let ok = rd.read_sector(scratch);
    rd.stop();
    if ok {
        *loaded = sector;
    }
    ok
}

/// Read table entry `index` while scanning, stitching an entry that straddles
/// two header sectors (ports hl-psx's `read_pack_entry`; the sector math and
/// byte parsing are the crate's host-tested [`entry_location`] /
/// [`parse_entry_at`]).
#[cfg(target_arch = "mips")]
fn read_entry(
    rd: &mut SectorReader,
    pack_lba: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    loaded: &mut u32,
    header_sectors: u32,
    index: u32,
) -> Option<PackEntry> {
    let (sector, within) = entry_location(index);
    if sector >= header_sectors {
        return None;
    }
    if !load_header_sector(rd, pack_lba, scratch, loaded, sector) {
        return None;
    }
    if within + ENTRY_BYTES <= SECTOR_BYTES {
        parse_entry_at(scratch_bytes(scratch), within)
    } else {
        // Entry spans this sector and the next; stitch the 24 bytes together.
        if sector + 1 >= header_sectors {
            return None;
        }
        let first = SECTOR_BYTES - within;
        let mut stitched = [0u8; ENTRY_BYTES];
        stitched[..first].copy_from_slice(&scratch_bytes(scratch)[within..]);
        if !load_header_sector(rd, pack_lba, scratch, loaded, sector + 1) {
            return None;
        }
        stitched[first..].copy_from_slice(&scratch_bytes(scratch)[..ENTRY_BYTES - first]);
        parse_entry_at(&stitched, 0)
    }
}

/// Scan the pack table at `pack_lba` for `chunk_id` and return its entry
/// (including `byte_size` and the FNV `checksum` the writer stored).
///
/// Reads the header/table sectors through `scratch`, one at a time. `None`
/// on read failure, bad magic/version, or id not present. `I_MASK` is
/// VBlank-only while it reads and restored when it returns.
#[cfg(target_arch = "mips")]
pub fn find_entry(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
) -> Option<PackEntry> {
    let mut loaded = u32::MAX;
    if !load_header_sector(rd, pack_lba, scratch, &mut loaded, 0) {
        return None;
    }
    let header = parse_header(scratch_bytes(scratch))?;
    let mut index = 0u32;
    while index < header.chunk_count {
        let entry = read_entry(
            rd,
            pack_lba,
            scratch,
            &mut loaded,
            header.header_sectors,
            index,
        )?;
        if entry.chunk_id == chunk_id {
            return Some(entry);
        }
        index += 1;
    }
    None
}

/// Stream chunk `chunk_id` from the pack at `pack_lba` into `dst`.
///
/// Scans the table for the entry (sector-by-sector through `scratch`,
/// straddle-safe), verifies the payload fits `dst`, then seeks to the payload
/// and copies it in whole sectors via `scratch`. Returns the chunk's exact
/// byte size, or `None` (no disc / not found / too big / read error). The
/// payload lands at `dst[0]` still compressed if the writer compressed it;
/// see [`load_chunk_decompressed`].
///
/// Reads only as many sectors as `byte_size` needs: the table's padded
/// `sector_count` could be garbage and looping on it would hang the loader.
///
/// `I_MASK` is VBlank-only while the reads run and back to what it was when
/// this returns. No caching; scan cost is linear in the table, so games
/// loading many chunks should keep their own id table (see module doc).
#[cfg(target_arch = "mips")]
pub fn load_chunk(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let entry = find_entry(rd, pack_lba, chunk_id, scratch)?;
    let byte_size = entry.byte_size as usize;
    if byte_size > dst.len() * 4 {
        return None;
    }
    if !rd.prepare() || !rd.start_read(pack_lba + entry.sector_offset) {
        rd.stop();
        return None;
    }
    let dst_ptr = dst.as_mut_ptr() as *mut u8;
    let needed = byte_size.div_ceil(SECTOR_BYTES);
    let mut s = 0usize;
    while s < needed {
        if !rd.read_sector(scratch) {
            rd.stop();
            return None;
        }
        let off = s * SECTOR_BYTES;
        let copy = byte_size.saturating_sub(off).min(SECTOR_BYTES);
        if copy > 0 {
            // SAFETY: `off + copy <= byte_size <= dst.len() * 4` (checked
            // above), so the write stays inside `dst`; `scratch` is a whole
            // sector and `copy <= SECTOR_BYTES`; the two are distinct borrows.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    scratch.as_ptr() as *const u8,
                    dst_ptr.add(off),
                    copy,
                );
            }
        }
        s += 1;
    }
    rd.stop();
    Some(byte_size)
}

/// [`load_chunk`], then [`crate::decompress_hlzc_in_place`] on the result.
///
/// Returns the chunk's RAW byte length: the decompressed size for an `HLZC`
/// chunk, the stored size for an uncompressed one (raw passthrough). `None`
/// on any load or decode failure. `dst` must hold the raw payload plus the
/// in-place LZ4 margin (see the decompressor's docs); a too-small buffer
/// fails cleanly.
#[cfg(target_arch = "mips")]
pub fn load_chunk_decompressed(
    rd: &mut SectorReader,
    pack_lba: u32,
    chunk_id: u32,
    scratch: &mut [u32; SECTOR_WORDS],
    dst: &mut [u32],
) -> Option<usize> {
    let loaded = load_chunk(rd, pack_lba, chunk_id, scratch, dst)?;
    // SAFETY: [u32] viewed as its own bytes for the in-place decoder.
    let bytes =
        unsafe { core::slice::from_raw_parts_mut(dst.as_mut_ptr() as *mut u8, dst.len() * 4) };
    crate::decompress_hlzc_in_place(bytes, loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pack_lba_seeks_where_the_disc_math_says() {
        // The default pack LBA: 1024 + 150 lead-in sectors = 1174 = 15 * 75 + 49,
        // so Setloc gets 00:15:49. (The BCD arithmetic itself is psx-io's, and
        // tested there.)
        assert_eq!(
            psx_io::cd::lba_to_bcd_msf(WORLD_PACK_DEFAULT_LBA),
            [0x00, 0x15, 0x49]
        );
    }

    #[test]
    fn default_pack_lba_matches_the_writer() {
        // psx-iso (the pack writer) is a host-only crate, so the guest-side
        // constant is mirrored here; this pins them together.
        assert_eq!(
            WORLD_PACK_DEFAULT_LBA,
            psx_iso::WORLD_PACK_DEFAULT_START_LBA
        );
    }
}
