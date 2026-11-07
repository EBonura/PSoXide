//! Reading the header of `WORLD.PAK` and the chunks it names.
//!
//! The grid level core is chunk 0 and the first room chunk is chunk 1 (see
//! `psx_grid::stream`). The guest finds both in the first sector of the pack,
//! reads the core into RAM and keeps the sector of chunk 1 as the base every
//! room record's `pack_sector` is relative to. Nothing about the level's disc
//! layout is baked into the executable except where the pack starts.

use super::{
    read_chunk_blocking_with, CdController, RoomChunkLoadResult, Transport, WorldChunkInfo,
    SECTOR_BYTES, STATUS_CHUNK_NOT_FOUND, STATUS_OK, STATUS_UNSUPPORTED,
};
use psx_level::{LevelWorldPackEntryRecord, RoomIndex};

use super::Console;

/// `WORLD.PAK` magic.
const MAGIC: [u8; 8] = *b"PSOXWPAK";
/// Supported pack version.
const VERSION: u32 = 1;
/// Bytes before the entry table.
const HEADER_BYTES: usize = 28;
/// Bytes per table entry: id, sector offset, sector count, byte size,
/// checksum, reserved.
const ENTRY_BYTES: usize = 24;
/// Status: the first sector is not a `WORLD.PAK` header.
pub const STATUS_PACK_HEADER_INVALID: u32 = 9;

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let word = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
}

/// Look `ids` up in the header sector `sector` of a `WORLD.PAK`, filling
/// `out[i]` for `ids[i]`. Only entries that fit in the first sector are
/// searched, which is where the cook puts the core and the first room.
/// Returns [`STATUS_OK`], [`STATUS_PACK_HEADER_INVALID`] or
/// [`STATUS_CHUNK_NOT_FOUND`].
pub fn parse_world_pack_entries(sector: &[u8], ids: &[u32], out: &mut [WorldChunkInfo]) -> u32 {
    if sector.len() < HEADER_BYTES || sector[..MAGIC.len()] != MAGIC {
        return STATUS_PACK_HEADER_INVALID;
    }
    let (Some(version), Some(count)) = (le_u32(sector, 8), le_u32(sector, 12)) else {
        return STATUS_PACK_HEADER_INVALID;
    };
    if version != VERSION || count == 0 {
        return STATUS_PACK_HEADER_INVALID;
    }
    let searchable = (count as usize).min((sector.len() - HEADER_BYTES) / ENTRY_BYTES);
    for (id, slot) in ids.iter().zip(out.iter_mut()) {
        let mut found = false;
        for entry in 0..searchable {
            let at = HEADER_BYTES + entry * ENTRY_BYTES;
            if le_u32(sector, at) != Some(*id) {
                continue;
            }
            let (Some(sector_offset), Some(sector_count), Some(byte_size), Some(checksum)) = (
                le_u32(sector, at + 4),
                le_u32(sector, at + 8),
                le_u32(sector, at + 12),
                le_u32(sector, at + 16),
            ) else {
                return STATUS_PACK_HEADER_INVALID;
            };
            *slot = WorldChunkInfo {
                sector_offset,
                sector_count,
                byte_size: byte_size as usize,
                checksum,
            };
            found = true;
            break;
        }
        if !found {
            return STATUS_CHUNK_NOT_FOUND;
        }
    }
    STATUS_OK
}

/// Read the header sector of the `WORLD.PAK` at `pack_lba` and look `ids` up
/// in it (see [`parse_world_pack_entries`]). Blocking, bounded by the
/// transport's own no-progress watchdog.
pub fn read_world_pack_entries_blocking(
    cd: &mut CdController,
    pack_lba: u32,
    ids: &[u32],
    out: &mut [WorldChunkInfo],
) -> u32 {
    read_world_pack_entries_with(&mut Console, cd, pack_lba, ids, out)
}

pub(super) fn read_world_pack_entries_with<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    pack_lba: u32,
    ids: &[u32],
    out: &mut [WorldChunkInfo],
) -> u32 {
    if !transport.available() {
        return STATUS_UNSUPPORTED;
    }
    cd.begin_run(transport, pack_lba, 1);
    if let Err(status) = cd.wait_sector(transport) {
        cd.abort_run(transport);
        return status;
    }
    // SAFETY: the sector buffer of the controller holds a whole sector.
    let sector = unsafe { core::slice::from_raw_parts(cd.sector_bytes(), SECTOR_BYTES) };
    parse_world_pack_entries(sector, ids, out)
}

/// Read one chunk of the pack at `pack_lba`, whose placement `info` came from
/// the pack header, into `dst`, and verify its size and checksum. Blocking,
/// bounded by the transport's watchdog.
pub fn read_world_chunk_blocking(
    cd: &mut CdController,
    pack_lba: u32,
    info: WorldChunkInfo,
    dst: &mut [u32],
) -> RoomChunkLoadResult {
    read_world_chunk_with(&mut Console, cd, pack_lba, info, dst)
}

pub(super) fn read_world_chunk_with<T: Transport>(
    transport: &mut T,
    cd: &mut CdController,
    pack_lba: u32,
    info: WorldChunkInfo,
    dst: &mut [u32],
) -> RoomChunkLoadResult {
    // One synthetic table entry; the chunk id is whatever matches.
    let toc = [LevelWorldPackEntryRecord {
        room: RoomIndex(0),
        sector_offset: info.sector_offset,
        sector_count: info.sector_count,
        byte_size: info.byte_size as u32,
        checksum: info.checksum,
    }];
    read_chunk_blocking_with(transport, cd, pack_lba, &toc, 0, dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pack header sector for chunks `(id, sector_offset, sector_count,
    /// byte_size, checksum)`.
    fn header(entries: &[(u32, u32, u32, u32, u32)]) -> [u8; SECTOR_BYTES] {
        let mut sector = [0u8; SECTOR_BYTES];
        sector[..8].copy_from_slice(&MAGIC);
        sector[8..12].copy_from_slice(&VERSION.to_le_bytes());
        sector[12..16].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        for (i, entry) in entries.iter().enumerate() {
            let at = HEADER_BYTES + i * ENTRY_BYTES;
            for (n, word) in [entry.0, entry.1, entry.2, entry.3, entry.4]
                .iter()
                .enumerate()
            {
                sector[at + n * 4..at + n * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
        }
        sector
    }

    #[test]
    fn finds_the_core_and_the_first_room_by_id() {
        let sector = header(&[(0, 1, 1, 392, 7), (1, 2, 4, 6208, 8), (2, 6, 4, 6148, 9)]);
        let mut out = [WorldChunkInfo::EMPTY; 2];
        assert_eq!(
            parse_world_pack_entries(&sector, &[0, 1], &mut out),
            STATUS_OK
        );
        assert_eq!(
            (out[0].sector_offset, out[0].sector_count, out[0].byte_size),
            (1, 1, 392)
        );
        assert_eq!(out[0].checksum, 7);
        assert_eq!((out[1].sector_offset, out[1].sector_count), (2, 4));
    }

    #[test]
    fn refuses_a_sector_that_is_not_a_pack_header() {
        let mut out = [WorldChunkInfo::EMPTY; 1];
        assert_eq!(
            parse_world_pack_entries(&[0u8; SECTOR_BYTES], &[0], &mut out),
            STATUS_PACK_HEADER_INVALID
        );
        let mut sector = header(&[(0, 1, 1, 8, 0)]);
        sector[8] = 2;
        assert_eq!(
            parse_world_pack_entries(&sector, &[0], &mut out),
            STATUS_PACK_HEADER_INVALID
        );
    }

    #[test]
    fn a_missing_id_is_reported_not_guessed() {
        let sector = header(&[(0, 1, 1, 392, 7)]);
        let mut out = [WorldChunkInfo::EMPTY; 2];
        assert_eq!(
            parse_world_pack_entries(&sector, &[0, 1], &mut out),
            STATUS_CHUNK_NOT_FOUND
        );
    }
}
