//! BIN/CUE and (eventually) ISO9660 parsing for PS1 disc images.
//!
//! Phase 6d scope: raw-BIN loading. A BIN file is just a sequence of
//! 2352-byte Mode-2 Form-1 sectors, each laid out as:
//!
//! ```text
//!   0..12    sync pattern (0x00, 0xFF × 10, 0x00)
//!  12..16    header: MM SS FF MODE
//!  16..24    sub-header: FILE CHN SUB CI FILE CHN SUB CI
//!  24..2072  user data (2048 bytes)
//! 2072..2076 EDC (optional)
//! 2076..2352 ECC / reserved
//! ```
//!
//! PS1 discs place their data area starting at LBA `0x0000`, which
//! corresponds to MSF `00:02:00` (after the 2-second pre-gap).
//! `Disc::read_sector_raw` here treats LBA 0 as byte offset 0 of the
//! BIN -- adequate for hand-assembled homebrew images. CUE-aware
//! handling (pre-gap skipping, multi-track) lands when a real
//! commercial disc boots.
//!
//! ISO9660 filesystem parsing (primary volume descriptor, path
//! tables, directory records) lands in Phase 6e.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

pub mod boot;
pub mod exe;
pub mod iso9660;
pub use boot::{load_boot_exe_from_disc, BootError, BootExe};
pub use exe::{Exe, ExeError, EXE_HEADER_BYTES};
pub use iso9660::{default_system_cnf, IsoBuilder, IsoFile, XA_SECTOR_SIZE};

/// One raw CD-ROM sector -- always 2352 bytes on a PS1 disc regardless
/// of track mode.
pub const SECTOR_BYTES: usize = 2352;
/// Byte offset of the 2048-byte user-data region within a Mode-2
/// Form-1 sector.
pub const SECTOR_USER_DATA_OFFSET: usize = 24;
/// User-data size per sector.
pub const SECTOR_USER_DATA_BYTES: usize = 2048;

/// Root-directory file used by the editor-playtest CD streaming probe.
pub const CD_STREAM_BENCH_FILE_NAME: &str = "CDTEST.BIN";
/// Default payload size for the editor-playtest CD streaming probe.
pub const CD_STREAM_BENCH_DEFAULT_SECTORS: usize = 32;
/// The first root file starts at LBA 21 in PSoXide's compact ISO layout.
pub const PLAYTEST_FIRST_FILE_LBA: u32 = 21;
/// The boot executable follows `SYSTEM.CNF` immediately, matching the common
/// PS1/homebrew layout and keeping the BIOS boot read close to the start of
/// the data track.
pub const PLAYTEST_BOOT_EXE_START_LBA: u32 = PLAYTEST_FIRST_FILE_LBA + 1;
/// Fixed start LBA for the optional CD streaming probe when it uses the default
/// 32-sector size. The probe sits at the end of the reserved boot area, just
/// before [`WORLD_PACK_FILE_NAME`].
pub const CD_STREAM_BENCH_START_LBA: u32 =
    WORLD_PACK_DEFAULT_START_LBA - CD_STREAM_BENCH_DEFAULT_SECTORS as u32;
/// Header signature at the start of the streaming probe payload.
pub const CD_STREAM_BENCH_MAGIC: [u8; 8] = *b"PSOXSTRM";
/// Root-directory file used for the first real streamed world package.
pub const WORLD_PACK_FILE_NAME: &str = "WORLD.PAK";
/// Root-directory file used for streamed UI image assets. Packed
/// immediately after [`WORLD_PACK_FILE_NAME`] in the playtest ISO
/// layout, sharing the same on-disc pack format and build helpers.
pub const UI_PACK_FILE_NAME: &str = "UI.PAK";
/// Header signature at the start of [`WORLD_PACK_FILE_NAME`].
pub const WORLD_PACK_MAGIC: [u8; 8] = *b"PSOXWPAK";
/// Current on-disc world-pack format version.
pub const WORLD_PACK_VERSION: u32 = 1;
/// Bytes in the fixed world-pack header.
pub const WORLD_PACK_HEADER_BYTES: usize = 28;
/// Bytes per world-pack chunk-table entry.
pub const WORLD_PACK_ENTRY_BYTES: usize = 24;
/// Default start LBA for streamed playtest packs. `SYSTEM.CNF` and `PSX.EXE`
/// live at the front of the disc, then invisible zero padding reserves a fixed
/// boot area until this LBA. The reservation avoids a circular build dependency:
/// runtime pack LBAs no longer depend on the final boot EXE size.
pub const WORLD_PACK_DEFAULT_START_LBA: u32 = 1024;

/// Errors returned when the canonical playtest layout cannot fit the requested
/// boot/debug payloads into the fixed boot area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaytestLayoutError {
    /// The boot executable would overlap the fixed pack region.
    BootExeTooLarge { exe_sectors: u32, max_sectors: u32 },
    /// The optional CD stream-test payload would overlap the boot executable.
    CdtestTooLarge {
        cdtest_sectors: u32,
        max_sectors: u32,
    },
}

/// PS1 track type from a CUE sheet / disc TOC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackType {
    /// Mode-2 data track.
    Data,
    /// Red Book audio track.
    Audio,
}

/// Where a track's raw bytes come from.
///
/// A source can hold the whole image in memory (a `Vec<u8>` is one), read a
/// file on demand, or decode a packed image lazily, so a disc no longer has to
/// live in memory to be played. Reads copy out; nothing borrows the source.
///
/// A source that fetches asynchronously (the web build reads the picked file
/// a slice at a time) answers [`TrackSource::ready`] with `false` for bytes
/// that have not arrived, and fails [`TrackSource::read_at`] on them.
/// Synchronous sources keep the defaults: always ready, nothing to prefetch.
pub trait TrackSource: Send + Sync {
    /// Length in bytes.
    fn len(&self) -> u64;

    /// Whether the source is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copy `out.len()` bytes starting at `offset` into `out`. Returns
    /// `false`, with `out` unspecified, when any of them lie outside the
    /// source or cannot be read right now.
    fn read_at(&self, offset: u64, out: &mut [u8]) -> bool;

    /// Whether [`TrackSource::read_at`] over this range would succeed now.
    fn ready(&self, offset: u64, len: u64) -> bool {
        let _ = (offset, len);
        true
    }

    /// Ask for this range to be made ready soon.
    fn prefetch(&self, offset: u64, len: u64) {
        let _ = (offset, len);
    }

    /// The bytes themselves, when the source is a plain in-memory buffer.
    /// Lets a caller patch a track in place (the web build streams CD-DA
    /// payloads into zero-filled placeholders).
    fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        None
    }
}

impl TrackSource for Vec<u8> {
    fn len(&self) -> u64 {
        self.as_slice().len() as u64
    }

    fn read_at(&self, offset: u64, out: &mut [u8]) -> bool {
        let Ok(start) = usize::try_from(offset) else {
            return false;
        };
        let Some(bytes) = start
            .checked_add(out.len())
            .and_then(|end| self.get(start..end))
        else {
            return false;
        };
        out.copy_from_slice(bytes);
        true
    }

    fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        Some(self.as_mut_slice())
    }
}

/// One track in a loaded disc image.
pub struct Track {
    /// 1-based track number.
    pub number: u8,
    /// Data vs audio.
    pub track_type: TrackType,
    /// Disc LBA where INDEX 01 begins.
    pub start_lba: u32,
    /// Number of addressable INDEX 01+ sectors in this track.
    pub sector_count: u32,
    /// Pregap sectors before INDEX 01.
    pub pregap: u32,
    /// Pregap sectors physically present at the start of `source`.
    /// This differs from `pregap` when a CUE uses `PREGAP` to describe
    /// silence that lives in disc space but not in the track file.
    pub file_pregap: u32,
    /// Raw 2352-byte sectors backing this track.
    pub source: Box<dyn TrackSource>,
}

impl core::fmt::Debug for Track {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Track")
            .field("number", &self.number)
            .field("track_type", &self.track_type)
            .field("start_lba", &self.start_lba)
            .field("sector_count", &self.sector_count)
            .field("pregap", &self.pregap)
            .field("file_pregap", &self.file_pregap)
            .field("source_len", &self.source.len())
            .finish()
    }
}

/// One raw 2352-byte sector, copied out of a disc.
pub type Sector = [u8; SECTOR_BYTES];

/// The 2048-byte user-data payload of one sector.
pub type SectorUserData = [u8; SECTOR_USER_DATA_BYTES];

/// Physical play position for `GetlocP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackPosition {
    /// 1-based track number.
    pub track_number: u8,
    /// 0 = pregap / index 00, 1 = INDEX 01+.
    pub index_number: u8,
    /// Position relative to the current track/index in binary MSF.
    pub relative_msf: (u8, u8, u8),
    /// Absolute disc position in binary MSF.
    pub absolute_msf: (u8, u8, u8),
}

/// Deterministic byte pattern for the editor-playtest streaming
/// probe payload. Shared by the GUI disc builder and `mkisopsx` so
/// the guest can validate it with a tiny no-alloc mirror.
pub fn cd_stream_bench_payload(sectors: usize) -> Vec<u8> {
    let sectors = sectors.max(1);
    let len = sectors.saturating_mul(SECTOR_USER_DATA_BYTES);
    let mut payload = vec![0u8; len];
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte = cd_stream_bench_expected_byte(i, sectors);
    }
    payload
}

/// Expected byte at `index` for [`cd_stream_bench_payload`].
pub const fn cd_stream_bench_expected_byte(index: usize, sectors: usize) -> u8 {
    if index < CD_STREAM_BENCH_MAGIC.len() {
        CD_STREAM_BENCH_MAGIC[index]
    } else if index < 12 {
        ((sectors as u32).to_le_bytes())[index - 8]
    } else {
        let mixed = (index as u32)
            .wrapping_mul(37)
            .wrapping_add((index as u32) >> 3)
            .wrapping_add(0x5D);
        mixed as u8
    }
}

/// FNV-1a checksum over the expected streaming probe payload.
pub fn cd_stream_bench_expected_checksum(sectors: usize) -> u32 {
    let sectors = sectors.max(1);
    let len = sectors.saturating_mul(SECTOR_USER_DATA_BYTES);
    let mut hash = 0x811C_9DC5u32;
    let mut i = 0usize;
    while i < len {
        hash ^= cd_stream_bench_expected_byte(i, sectors) as u32;
        hash = hash.wrapping_mul(0x0100_0193);
        i += 1;
    }
    hash
}

const fn sectors_for_bytes(bytes: usize) -> u32 {
    let sectors = bytes.div_ceil(SECTOR_USER_DATA_BYTES);
    if sectors == 0 {
        1
    } else {
        sectors as u32
    }
}

/// Build a sector-aligned world package from already-cooked chunks.
///
/// Format:
/// - fixed header: magic, version, chunk count, total sectors,
///   header sectors, table bytes;
/// - one 24-byte entry per chunk: id, sector offset, sector count,
///   byte size, FNV checksum, reserved;
/// - each chunk payload starts on a 2048-byte sector.
pub fn build_world_pack(chunks: &[(u32, &[u8])]) -> Vec<u8> {
    let layout = build_world_pack_layout(chunks);
    let total_sectors = layout.total_sectors as usize;
    let mut out = vec![0u8; total_sectors.saturating_mul(SECTOR_USER_DATA_BYTES)];
    out[..WORLD_PACK_MAGIC.len()].copy_from_slice(&WORLD_PACK_MAGIC);
    write_le_u32_at(&mut out, 8, WORLD_PACK_VERSION);
    write_le_u32_at(&mut out, 12, chunks.len() as u32);
    write_le_u32_at(&mut out, 16, layout.total_sectors);
    write_le_u32_at(&mut out, 20, layout.header_sectors);
    write_le_u32_at(&mut out, 24, layout.table_bytes);

    let mut entry_offset = WORLD_PACK_HEADER_BYTES;
    for entry in &layout.entries {
        write_le_u32_at(&mut out, entry_offset, entry.chunk_id);
        write_le_u32_at(&mut out, entry_offset + 4, entry.sector_offset);
        write_le_u32_at(&mut out, entry_offset + 8, entry.sector_count);
        write_le_u32_at(&mut out, entry_offset + 12, entry.byte_size);
        write_le_u32_at(&mut out, entry_offset + 16, entry.checksum);
        write_le_u32_at(&mut out, entry_offset + 20, 0);
        entry_offset += WORLD_PACK_ENTRY_BYTES;
    }

    for ((_, bytes), entry) in chunks.iter().zip(layout.entries.iter()) {
        let start = entry.sector_offset as usize * SECTOR_USER_DATA_BYTES;
        let end = start + bytes.len();
        out[start..end].copy_from_slice(bytes);
    }
    out
}

/// Add the canonical PSoXide playtest-disc file set to `builder`. The hardware
/// conservative order is: `SYSTEM.CNF`, `PSX.EXE`, invisible boot-area padding,
/// optional `CDTEST.BIN`, then `WORLD.PAK` and `UI.PAK`. The cooked
/// `WORLD_PACK_START_LBA` / `UI_PACK_START_LBA` are derived from exactly this
/// order, so every disc builder -- the `mkisopsx` CLI and the editor's
/// in-process embedded Play -- routes through this one function. Defining the
/// layout in a single place keeps the two from drifting (a `UI.PAK` added to one
/// builder but not the other silently breaks streaming).
///
/// The caller sets up `builder` first (volume id, optional system area) and
/// serializes the packs from its own input source (CLI dirs vs the editor's
/// `generated/` tree); only the file set and order are shared here.
pub fn add_playtest_files(
    builder: &mut IsoBuilder,
    exe_bytes: Vec<u8>,
    world_pack: Option<Vec<u8>>,
    ui_pack: Option<Vec<u8>>,
    cdtest_sectors: Option<usize>,
) -> Result<(), PlaytestLayoutError> {
    builder.add_file("SYSTEM.CNF", default_system_cnf());
    let exe_sectors = sectors_for_bytes(exe_bytes.len());
    let next_after_exe = PLAYTEST_BOOT_EXE_START_LBA.saturating_add(exe_sectors);
    let cdtest_sectors_u32 = cdtest_sectors.unwrap_or(0) as u32;
    let pack_start = WORLD_PACK_DEFAULT_START_LBA;
    let cdtest_start = pack_start.saturating_sub(cdtest_sectors_u32);
    let max_exe_sectors = cdtest_start.saturating_sub(PLAYTEST_BOOT_EXE_START_LBA);
    if exe_sectors > max_exe_sectors {
        return Err(PlaytestLayoutError::BootExeTooLarge {
            exe_sectors,
            max_sectors: max_exe_sectors,
        });
    }
    if cdtest_sectors_u32 > pack_start.saturating_sub(next_after_exe) {
        return Err(PlaytestLayoutError::CdtestTooLarge {
            cdtest_sectors: cdtest_sectors_u32,
            max_sectors: pack_start.saturating_sub(next_after_exe),
        });
    }

    builder.add_file("PSX.EXE", exe_bytes);
    builder.add_padding_sectors(cdtest_start - next_after_exe);
    if let Some(sectors) = cdtest_sectors {
        builder.add_file(CD_STREAM_BENCH_FILE_NAME, cd_stream_bench_payload(sectors));
    }
    debug_assert_eq!(
        cdtest_start + cdtest_sectors_u32,
        WORLD_PACK_DEFAULT_START_LBA
    );
    if let Some(world_pack) = world_pack {
        builder.add_file(WORLD_PACK_FILE_NAME, world_pack);
    }
    if let Some(ui_pack) = ui_pack {
        builder.add_file(UI_PACK_FILE_NAME, ui_pack);
    }
    Ok(())
}

/// Cooked placement metadata for one [`WORLD_PACK_FILE_NAME`] chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldPackBuildEntry {
    /// Chunk id supplied to [`build_world_pack`].
    pub chunk_id: u32,
    /// Payload start sector relative to the beginning of `WORLD.PAK`.
    pub sector_offset: u32,
    /// Sector-aligned payload length.
    pub sector_count: u32,
    /// Original unpadded byte size.
    pub byte_size: u32,
    /// FNV-1a checksum of the unpadded payload.
    pub checksum: u32,
}

/// Full cooked layout for a [`WORLD_PACK_FILE_NAME`] payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldPackLayout {
    /// Per-chunk placement entries in payload order.
    pub entries: Vec<WorldPackBuildEntry>,
    /// Header/table sector count.
    pub header_sectors: u32,
    /// Total pack sector count, including header/table.
    pub total_sectors: u32,
    /// Chunk table byte size.
    pub table_bytes: u32,
}

/// Compute the exact [`WORLD_PACK_FILE_NAME`] layout without
/// materialising the full pack bytes.
pub fn build_world_pack_layout(chunks: &[(u32, &[u8])]) -> WorldPackLayout {
    let table_bytes = chunks.len().saturating_mul(WORLD_PACK_ENTRY_BYTES);
    let header_bytes = WORLD_PACK_HEADER_BYTES.saturating_add(table_bytes);
    let header_sectors = header_bytes.div_ceil(SECTOR_USER_DATA_BYTES).max(1) as u32;
    let mut next_sector = header_sectors;
    let mut entries = Vec::with_capacity(chunks.len());
    for &(chunk_id, bytes) in chunks {
        let sector_count = bytes.len().div_ceil(SECTOR_USER_DATA_BYTES).max(1) as u32;
        entries.push(WorldPackBuildEntry {
            chunk_id,
            sector_offset: next_sector,
            sector_count,
            byte_size: bytes.len() as u32,
            checksum: fnv1a32(bytes),
        });
        next_sector = next_sector.saturating_add(sector_count);
    }
    WorldPackLayout {
        entries,
        header_sectors,
        total_sectors: next_sector,
        table_bytes: table_bytes as u32,
    }
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for &byte in bytes {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn write_le_u32_at(dst: &mut [u8], offset: usize, value: u32) {
    dst[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// A loaded disc image: the TOC plus one [`TrackSource`] per track, enough to
/// answer sector reads and SubQ-style position queries.
pub struct Disc {
    tracks: Vec<Track>,
}

impl Disc {
    /// Construct a disc from a raw BIN image held in memory.
    pub fn from_bin(bytes: Vec<u8>) -> Self {
        Self::from_source(Box::new(bytes))
    }

    /// Construct a single-track disc from a raw BIN image behind `source`.
    /// The track-1 pregap is detected from the first sector, which must be
    /// readable now.
    pub fn from_source(source: Box<dyn TrackSource>) -> Self {
        let mut first = [0u8; SECTOR_BYTES];
        let pregap = if source.read_at(0, &mut first) {
            detect_track1_pregap(&first)
        } else {
            0
        };
        let file_sectors = source.len() / SECTOR_BYTES as u64;
        let sector_count = file_sectors
            .saturating_sub(pregap as u64)
            .min(u32::MAX as u64) as u32;
        Self {
            tracks: vec![Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count,
                pregap,
                file_pregap: pregap,
                source,
            }],
        }
    }

    /// Construct a disc from explicit multi-track metadata.
    pub fn from_tracks(mut tracks: Vec<Track>) -> Self {
        tracks.sort_by_key(|track| track.number);
        Self { tracks }
    }

    /// Total addressable sector count up to the disc lead-out.
    pub fn sector_count(&self) -> usize {
        self.leadout_lba() as usize
    }

    /// Number of tracks on the disc.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Tracks in number order.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// First track number on the disc, if any.
    pub fn first_track_number(&self) -> Option<u8> {
        self.tracks.first().map(|track| track.number)
    }

    /// Last track number on the disc, if any.
    pub fn last_track_number(&self) -> Option<u8> {
        self.tracks.last().map(|track| track.number)
    }

    /// Lead-out LBA (first sector after the last track).
    pub fn leadout_lba(&self) -> u32 {
        self.tracks
            .last()
            .map(|track| track.start_lba.saturating_add(track.sector_count))
            .unwrap_or(0)
    }

    /// Track metadata by 1-based track number.
    pub fn track(&self, number: u8) -> Option<&Track> {
        self.tracks.iter().find(|track| track.number == number)
    }

    /// Mutable access to a track's raw sector bytes, for callers that stream
    /// payloads in after the disc is mounted (the web build boots on the data
    /// track and fills the CD-DA tracks as their downloads land). The track's
    /// geometry is fixed at construction; only the bytes may be patched, and
    /// a zero-filled span plays as digital silence until it is. `None` for a
    /// track that is not held in memory.
    pub fn track_bytes_mut(&mut self, number: u8) -> Option<&mut [u8]> {
        self.tracks
            .iter_mut()
            .find(|track| track.number == number)
            .and_then(|track| track.source.bytes_mut())
    }

    /// Track start LBA by 1-based track number.
    pub fn track_start_lba(&self, number: u8) -> Option<u32> {
        self.track(number).map(|track| track.start_lba)
    }

    /// Track metadata for the absolute disc LBA.
    pub fn track_for_lba(&self, lba: u32) -> Option<&Track> {
        self.tracks
            .iter()
            .find(|track| track_contains_lba(track, lba))
    }

    /// Physical play position for an absolute LBA.
    pub fn track_position_for_lba(&self, lba: u32) -> Option<TrackPosition> {
        let track = self.track_for_lba(lba)?;
        let absolute_msf = lba_to_msf(lba);
        let (index_number, relative_msf) = if track.number != 1 && lba < track.start_lba {
            let frames_until_index1 = track.start_lba.saturating_sub(lba).saturating_sub(1);
            (0, frames_to_msf(frames_until_index1))
        } else {
            (1, frames_to_msf(lba.saturating_sub(track.start_lba)))
        };
        Some(TrackPosition {
            track_number: track.number,
            index_number,
            relative_msf,
            absolute_msf,
        })
    }

    /// The track holding `lba` in its source and the sector's byte offset
    /// there. `None` past end-of-disc and for pregap sectors the track's file
    /// does not contain.
    fn locate(&self, lba: u32) -> Option<(&Track, u64)> {
        for track in &self.tracks {
            if !track_contains_lba(track, lba) {
                continue;
            }
            let file_sector = if track.number == 1 {
                lba.checked_sub(track.start_lba)?
                    .checked_add(track.file_pregap)?
            } else {
                let file_start_lba = track.start_lba.saturating_sub(track.file_pregap);
                if lba < file_start_lba {
                    return None;
                }
                lba.checked_sub(file_start_lba)?
            };
            let start = u64::from(file_sector) * SECTOR_BYTES as u64;
            if start + SECTOR_BYTES as u64 <= track.source.len() {
                return Some((track, start));
            }
        }
        None
    }

    /// Read a raw 2352-byte sector. Returns `None` past end-of-disc, and for
    /// a sector an asynchronous source has not delivered yet (see
    /// [`Disc::sectors_ready`]).
    pub fn read_sector_raw(&self, lba: u32) -> Option<Sector> {
        let (track, offset) = self.locate(lba)?;
        let mut sector = [0u8; SECTOR_BYTES];
        track.source.read_at(offset, &mut sector).then_some(sector)
    }

    /// Bytes 12..20 of a raw sector: the header (MSF + mode) and the first
    /// subheader copy. Sources that fetch asynchronously can answer this from
    /// an index before the sector itself has arrived.
    pub fn read_sector_header(&self, lba: u32) -> Option<[u8; 8]> {
        let (track, offset) = self.locate(lba)?;
        let mut header = [0u8; 8];
        track
            .source
            .read_at(offset + 12, &mut header)
            .then_some(header)
    }

    /// Whether sectors `lba..lba + count` that exist on the disc can be read
    /// now. Sectors past the end, or in a pregap the image does not hold,
    /// count as ready: reading them answers `None` without waiting.
    pub fn sectors_ready(&self, lba: u32, count: u32) -> bool {
        (lba..lba.saturating_add(count)).all(|lba| match self.locate(lba) {
            Some((track, offset)) => track.source.ready(offset, SECTOR_BYTES as u64),
            None => true,
        })
    }

    /// Hint that sectors `lba..lba + count` will be read soon.
    pub fn prefetch_sectors(&self, lba: u32, count: u32) {
        let mut next = lba;
        let end = lba.saturating_add(count);
        while next < end {
            let Some((track, offset)) = self.locate(next) else {
                next += 1;
                continue;
            };
            // One request per run of sectors in the same track.
            let mut run = 1u32;
            while next + run < end {
                match self.locate(next + run) {
                    Some((t, o))
                        if core::ptr::eq(t, track)
                            && o == offset + u64::from(run) * SECTOR_BYTES as u64 =>
                    {
                        run += 1
                    }
                    _ => break,
                }
            }
            track
                .source
                .prefetch(offset, u64::from(run) * SECTOR_BYTES as u64);
            next += run;
        }
    }

    /// Read one raw CD-DA audio frame from an audio track.
    ///
    /// CD-DA track files are 2352 bytes per 1/75 s frame: stereo
    /// 16-bit little-endian PCM with no CD-ROM sync/header/subheader.
    pub fn read_cdda_sector(&self, lba: u32) -> Option<Sector> {
        let track = self.track_for_lba(lba)?;
        if track.track_type != TrackType::Audio || lba < track.start_lba {
            return None;
        }
        self.read_sector_raw(lba)
    }

    /// Read the 2048-byte user-data payload of a sector.
    pub fn read_sector_user(&self, lba: u32) -> Option<SectorUserData> {
        let sector = self.read_sector_raw(lba)?;
        let start = if sector[15] == 1 {
            16
        } else {
            SECTOR_USER_DATA_OFFSET
        };
        let mut user = [0u8; SECTOR_USER_DATA_BYTES];
        user.copy_from_slice(&sector[start..start + SECTOR_USER_DATA_BYTES]);
        Some(user)
    }
}

/// Track-1 pregap sectors held at the start of a raw image, from the MSF in
/// its first sector's header (a dump that starts at 00:00:00 holds the full
/// 150-sector pregap; one that starts at 00:02:00 holds none).
pub fn detect_track1_pregap(first_sector: &[u8]) -> u32 {
    if first_sector.len() < SECTOR_BYTES {
        return 0;
    }
    let sector = &first_sector[..SECTOR_BYTES];
    if sector[0] != 0x00 || sector[11] != 0x00 || sector[1..11] != [0xFF; 10] {
        return 0;
    }
    let m = bcd_to_bin(sector[12]);
    let s = bcd_to_bin(sector[13]);
    let f = bcd_to_bin(sector[14]);
    if [m, s, f].contains(&0xFF) {
        return 0;
    }
    let abs_frame = (m as u32) * 60 * 75 + (s as u32) * 75 + (f as u32);
    150u32.saturating_sub(abs_frame)
}

fn track_pregap_start_lba(track: &Track) -> u32 {
    if track.number == 1 {
        track.start_lba
    } else {
        track.start_lba.saturating_sub(track.pregap)
    }
}

fn track_contains_lba(track: &Track, lba: u32) -> bool {
    let start = track_pregap_start_lba(track);
    let end = track.start_lba.saturating_add(track.sector_count);
    lba >= start && lba < end
}

/// Convert a BCD byte pair to binary (used for MSF fields in commands).
/// Returns 0xFF if either nibble is out of range.
pub fn bcd_to_bin(bcd: u8) -> u8 {
    let hi = (bcd >> 4) & 0xF;
    let lo = bcd & 0xF;
    if hi > 9 || lo > 9 {
        0xFF
    } else {
        hi * 10 + lo
    }
}

/// Pack a binary 0..=99 value into a BCD byte. Values above 99
/// clamp to 99 -- hardware drops the high bits rather than
/// corrupting the low nibble.
pub fn bin_to_bcd(v: u8) -> u8 {
    let v = v.min(99);
    ((v / 10) << 4) | (v % 10)
}

/// Convert an absolute LBA to an MSF triple `(minute, second, frame)`
/// in *binary* form. Caller must `bin_to_bcd` each field before
/// sending over the wire. LBA 0 = MSF 00:02:00 per the 150-frame
/// pre-gap convention.
pub fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let abs = lba.saturating_add(150);
    let m = (abs / (60 * 75)) as u8;
    let s = ((abs / 75) % 60) as u8;
    let f = (abs % 75) as u8;
    (m, s, f)
}

/// Convert a frame count (without the 2-second absolute disc pregap)
/// into an MSF triple in binary form.
pub fn frames_to_msf(total_frames: u32) -> (u8, u8, u8) {
    let m = (total_frames / (60 * 75)) as u8;
    let s = ((total_frames / 75) % 60) as u8;
    let f = (total_frames % 75) as u8;
    (m, s, f)
}

/// Convert a 3-byte BCD MSF triple (minute, second, frame) into the
/// absolute LBA the sector lives at.
///
/// PS1 discs start their data at MSF 00:02:00 (after 2-second
/// pre-gap), which we treat as LBA 0 in the BIN layout. So:
///
/// ```text
///     LBA = (minute × 60 + second - 2) × 75 + frame
/// ```
pub fn msf_to_lba(m_bcd: u8, s_bcd: u8, f_bcd: u8) -> u32 {
    let m = bcd_to_bin(m_bcd) as i32;
    let s = bcd_to_bin(s_bcd) as i32;
    let f = bcd_to_bin(f_bcd) as i32;
    let abs = (m * 60 + s) * 75 + f;
    (abs - 150).max(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn bcd_decodes_common_values() {
        assert_eq!(bcd_to_bin(0x00), 0);
        assert_eq!(bcd_to_bin(0x42), 42);
        assert_eq!(bcd_to_bin(0x99), 99);
        assert_eq!(bcd_to_bin(0xAB), 0xFF);
    }

    #[test]
    fn msf_to_lba_data_area_starts_at_zero() {
        assert_eq!(msf_to_lba(0x00, 0x02, 0x00), 0);
    }

    #[test]
    fn bin_to_bcd_packs_correctly() {
        assert_eq!(bin_to_bcd(0), 0x00);
        assert_eq!(bin_to_bcd(42), 0x42);
        assert_eq!(bin_to_bcd(99), 0x99);
        assert_eq!(bin_to_bcd(100), 0x99); // clamped
    }

    #[test]
    fn lba_to_msf_round_trips() {
        assert_eq!(lba_to_msf(0), (0, 2, 0));
        assert_eq!(lba_to_msf(75), (0, 3, 0));
        assert_eq!(lba_to_msf(4500), (1, 2, 0));
    }

    #[test]
    fn disc_sector_count_rounds_down() {
        let bytes = vec![0u8; SECTOR_BYTES * 3 + 100];
        let d = Disc::from_bin(bytes);
        assert_eq!(d.sector_count(), 3);
    }

    #[test]
    fn read_sector_returns_slice() {
        let mut bytes = vec![0u8; SECTOR_BYTES];
        bytes[SECTOR_USER_DATA_OFFSET] = 0xAB;
        let d = Disc::from_bin(bytes);
        let s = d.read_sector_raw(0).unwrap();
        assert_eq!(s[SECTOR_USER_DATA_OFFSET], 0xAB);
        let u = d.read_sector_user(0).unwrap();
        assert_eq!(u[0], 0xAB);
    }

    fn minimal_exe() -> Vec<u8> {
        let mut exe = vec![0u8; EXE_HEADER_BYTES];
        exe[..8].copy_from_slice(b"PS-X EXE");
        exe[0x10..0x14].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        exe[0x18..0x1C].copy_from_slice(&0x8001_0000u32.to_le_bytes());
        exe
    }

    #[test]
    fn playtest_layout_puts_boot_exe_first_and_packs_at_fixed_lba() {
        let world_pack = build_world_pack(&[(0, b"room".as_slice())]);
        let mut builder = IsoBuilder::new();
        add_playtest_files(&mut builder, minimal_exe(), Some(world_pack), None, None)
            .expect("layout fits");

        let disc = Disc::from_bin(builder.build_bin());
        assert_eq!(
            &disc
                .read_sector_user(PLAYTEST_BOOT_EXE_START_LBA)
                .expect("boot exe sector")[..8],
            b"PS-X EXE"
        );
        assert_eq!(
            &disc
                .read_sector_user(WORLD_PACK_DEFAULT_START_LBA)
                .expect("world pack sector")[..WORLD_PACK_MAGIC.len()],
            &WORLD_PACK_MAGIC
        );
    }

    #[test]
    fn playtest_layout_places_default_cdtest_before_world_pack() {
        let mut builder = IsoBuilder::new();
        add_playtest_files(
            &mut builder,
            minimal_exe(),
            Some(build_world_pack(&[(0, b"room".as_slice())])),
            None,
            Some(CD_STREAM_BENCH_DEFAULT_SECTORS),
        )
        .expect("layout fits");

        let disc = Disc::from_bin(builder.build_bin());
        assert_eq!(
            &disc
                .read_sector_user(CD_STREAM_BENCH_START_LBA)
                .expect("cdtest sector")[..CD_STREAM_BENCH_MAGIC.len()],
            &CD_STREAM_BENCH_MAGIC
        );
        assert_eq!(
            &disc
                .read_sector_user(WORLD_PACK_DEFAULT_START_LBA)
                .expect("world pack sector")[..WORLD_PACK_MAGIC.len()],
            &WORLD_PACK_MAGIC
        );
    }

    /// A source whose bytes arrive later: reads fail until `deliver`.
    struct LateSource {
        bytes: Vec<u8>,
        delivered: core::sync::atomic::AtomicBool,
    }

    impl LateSource {
        fn new(bytes: Vec<u8>, delivered: bool) -> Self {
            Self {
                bytes,
                delivered: core::sync::atomic::AtomicBool::new(delivered),
            }
        }
        fn delivered(&self) -> bool {
            self.delivered.load(core::sync::atomic::Ordering::Relaxed)
        }
    }

    impl TrackSource for LateSource {
        fn len(&self) -> u64 {
            self.bytes.len() as u64
        }
        fn read_at(&self, offset: u64, out: &mut [u8]) -> bool {
            let header_only = offset % SECTOR_BYTES as u64 == 12 && out.len() == 8;
            (self.delivered() || header_only) && self.bytes.read_at(offset, out)
        }
        fn ready(&self, _offset: u64, _len: u64) -> bool {
            self.delivered()
        }
    }

    #[test]
    fn late_source_reports_readiness_and_serves_headers_early() {
        let mut bytes = vec![0u8; SECTOR_BYTES * 4];
        bytes[2 * SECTOR_BYTES + 12..2 * SECTOR_BYTES + 20]
            .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        bytes[2 * SECTOR_BYTES + 100] = 0x5A;
        let late = LateSource::new(bytes, false);
        let disc = Disc::from_tracks(vec![Track {
            number: 1,
            track_type: TrackType::Data,
            start_lba: 0,
            sector_count: 4,
            pregap: 0,
            file_pregap: 0,
            source: Box::new(late),
        }]);
        assert!(!disc.sectors_ready(1, 2));
        assert!(disc.read_sector_raw(2).is_none());
        assert_eq!(disc.read_sector_header(2), Some([1, 2, 3, 4, 5, 6, 7, 8]));
        // Past the end counts as ready: nothing will ever arrive there.
        assert!(disc.sectors_ready(4, 10));
    }

    #[test]
    fn prefetch_merges_a_run_into_one_request_per_track() {
        extern crate std;
        use std::sync::{Arc, Mutex};
        type Hints = Arc<Mutex<Vec<(u64, u64)>>>;
        struct Hint(Vec<u8>, Hints);
        impl TrackSource for Hint {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_at(&self, offset: u64, out: &mut [u8]) -> bool {
                self.0.read_at(offset, out)
            }
            fn prefetch(&self, offset: u64, len: u64) {
                self.1.lock().unwrap().push((offset, len));
            }
        }
        let hints = Hints::default();
        let disc = Disc::from_tracks(vec![Track {
            number: 1,
            track_type: TrackType::Data,
            start_lba: 0,
            sector_count: 8,
            pregap: 0,
            file_pregap: 0,
            source: Box::new(Hint(vec![0u8; SECTOR_BYTES * 8], hints.clone())),
        }]);
        disc.prefetch_sectors(2, 20);
        let sector = SECTOR_BYTES as u64;
        assert_eq!(*hints.lock().unwrap(), [(2 * sector, 6 * sector)]);
    }

    #[test]
    fn only_memory_tracks_can_be_patched_in_place() {
        let mut disc = Disc::from_bin(vec![0u8; SECTOR_BYTES * 2]);
        disc.track_bytes_mut(1).unwrap()[0] = 9;
        assert_eq!(disc.read_sector_raw(0).unwrap()[0], 9);
        let late = LateSource::new(vec![0u8; SECTOR_BYTES], true);
        let mut disc = Disc::from_source(Box::new(late));
        assert!(disc.track_bytes_mut(1).is_none());
        assert!(disc.read_sector_raw(0).is_some());
    }

    #[test]
    fn read_past_end_returns_none() {
        let d = Disc::from_bin(vec![0u8; SECTOR_BYTES]);
        assert!(d.read_sector_raw(1).is_none());
    }

    #[test]
    fn mode1_user_reads_from_offset_16() {
        let mut bytes = vec![0u8; SECTOR_BYTES];
        bytes[15] = 1;
        bytes[16] = 0x5D;
        bytes[SECTOR_USER_DATA_OFFSET] = 0xA7;
        let d = Disc::from_bin(bytes);
        assert_eq!(d.read_sector_user(0).unwrap()[0], 0x5D);
    }

    #[test]
    fn track_position_reports_index0_for_pregap() {
        let tracks = vec![
            Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count: 10,
                pregap: 0,
                file_pregap: 0,
                source: Box::new(vec![0u8; SECTOR_BYTES * 10]),
            },
            Track {
                number: 2,
                track_type: TrackType::Audio,
                start_lba: 12,
                sector_count: 4,
                pregap: 2,
                file_pregap: 2,
                source: Box::new(vec![0u8; SECTOR_BYTES * 6]),
            },
        ];
        let disc = Disc::from_tracks(tracks);
        let pos = disc.track_position_for_lba(10).unwrap();
        assert_eq!(pos.track_number, 2);
        assert_eq!(pos.index_number, 0);
        assert_eq!(pos.relative_msf, (0, 0, 1));
        assert_eq!(pos.absolute_msf, (0, 2, 10));
    }

    #[test]
    fn track_position_reports_index1_after_pregap() {
        let tracks = vec![
            Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count: 10,
                pregap: 0,
                file_pregap: 0,
                source: Box::new(vec![0u8; SECTOR_BYTES * 10]),
            },
            Track {
                number: 2,
                track_type: TrackType::Audio,
                start_lba: 12,
                sector_count: 4,
                pregap: 2,
                file_pregap: 2,
                source: Box::new(vec![0u8; SECTOR_BYTES * 6]),
            },
        ];
        let disc = Disc::from_tracks(tracks);
        let pos = disc.track_position_for_lba(12).unwrap();
        assert_eq!(pos.track_number, 2);
        assert_eq!(pos.index_number, 1);
        assert_eq!(pos.relative_msf, (0, 0, 0));
        assert_eq!(pos.absolute_msf, (0, 2, 12));
    }

    #[test]
    fn multitrack_sector_reads_map_through_pregap() {
        let mut track2 = vec![0u8; SECTOR_BYTES * 6];
        track2[2 * SECTOR_BYTES] = 0xAB;
        let disc = Disc::from_tracks(vec![
            Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count: 10,
                pregap: 0,
                file_pregap: 0,
                source: Box::new(vec![0u8; SECTOR_BYTES * 10]),
            },
            Track {
                number: 2,
                track_type: TrackType::Audio,
                start_lba: 12,
                sector_count: 4,
                pregap: 2,
                file_pregap: 2,
                source: Box::new(track2),
            },
        ]);
        assert_eq!(disc.read_sector_raw(12).unwrap()[0], 0xAB);
    }

    #[test]
    fn multitrack_sector_reads_honor_pregap_not_present_in_file() {
        let mut track2 = vec![0u8; SECTOR_BYTES * 4];
        track2[0] = 0xCD;
        let disc = Disc::from_tracks(vec![
            Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count: 10,
                pregap: 0,
                file_pregap: 0,
                source: Box::new(vec![0u8; SECTOR_BYTES * 10]),
            },
            Track {
                number: 2,
                track_type: TrackType::Audio,
                start_lba: 12,
                sector_count: 4,
                pregap: 2,
                file_pregap: 0,
                source: Box::new(track2),
            },
        ]);
        assert!(disc.read_sector_raw(10).is_none());
        assert_eq!(disc.read_sector_raw(12).unwrap()[0], 0xCD);
    }

    #[test]
    fn cdda_sector_reads_only_audio_index1_frames() {
        let mut track2 = vec![0u8; SECTOR_BYTES * 4];
        track2[0] = 0x34;
        let disc = Disc::from_tracks(vec![
            Track {
                number: 1,
                track_type: TrackType::Data,
                start_lba: 0,
                sector_count: 10,
                pregap: 0,
                file_pregap: 0,
                source: Box::new(vec![0u8; SECTOR_BYTES * 10]),
            },
            Track {
                number: 2,
                track_type: TrackType::Audio,
                start_lba: 12,
                sector_count: 4,
                pregap: 2,
                file_pregap: 0,
                source: Box::new(track2),
            },
        ]);

        assert!(disc.read_cdda_sector(0).is_none());
        assert!(disc.read_cdda_sector(10).is_none());
        assert_eq!(disc.read_cdda_sector(12).unwrap()[0], 0x34);
    }

    #[test]
    fn cd_stream_bench_payload_is_deterministic() {
        let payload = cd_stream_bench_payload(2);
        assert_eq!(payload.len(), SECTOR_USER_DATA_BYTES * 2);
        assert_eq!(&payload[..8], &CD_STREAM_BENCH_MAGIC);
        assert_eq!(&payload[8..12], &2u32.to_le_bytes());
        assert_eq!(payload[12], cd_stream_bench_expected_byte(12, 2));
        assert_eq!(
            cd_stream_bench_expected_checksum(2),
            payload.iter().fold(0x811C_9DC5u32, |hash, byte| {
                (hash ^ (*byte as u32)).wrapping_mul(0x0100_0193)
            })
        );
    }

    #[test]
    fn world_pack_aligns_chunk_payloads_to_sectors() {
        let first = vec![0x11; 3000];
        let second = vec![0x22; 17];
        let pack = build_world_pack(&[(7, &first), (9, &second)]);
        assert_eq!(&pack[..8], &WORLD_PACK_MAGIC);
        assert_eq!(le_u32(&pack[8..12]), WORLD_PACK_VERSION);
        assert_eq!(le_u32(&pack[12..16]), 2);
        assert_eq!(le_u32(&pack[20..24]), 1);
        assert_eq!(le_u32(&pack[24..28]), (WORLD_PACK_ENTRY_BYTES * 2) as u32);

        let first_entry = WORLD_PACK_HEADER_BYTES;
        let second_entry = WORLD_PACK_HEADER_BYTES + WORLD_PACK_ENTRY_BYTES;
        assert_eq!(le_u32(&pack[first_entry..first_entry + 4]), 7);
        assert_eq!(le_u32(&pack[first_entry + 4..first_entry + 8]), 1);
        assert_eq!(le_u32(&pack[first_entry + 8..first_entry + 12]), 2);
        assert_eq!(le_u32(&pack[second_entry..second_entry + 4]), 9);
        assert_eq!(le_u32(&pack[second_entry + 4..second_entry + 8]), 3);
        assert_eq!(le_u32(&pack[second_entry + 8..second_entry + 12]), 1);

        let first_start = SECTOR_USER_DATA_BYTES;
        let second_start = 3 * SECTOR_USER_DATA_BYTES;
        assert_eq!(&pack[first_start..first_start + first.len()], &first);
        assert_eq!(&pack[second_start..second_start + second.len()], &second);
    }

    fn le_u32(bytes: &[u8]) -> u32 {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}
