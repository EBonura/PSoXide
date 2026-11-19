//! The sector records an STR is made of.
//!
//! Each record is a Mode 2 sector without sync and header, as
//! `mkisopsx --xa-file` takes it: the 4-byte subheader twice, then the data.
//! A video sector is Form 1: 2048 user bytes (the 32-byte chunk header of
//! [`psx_fmv::stream`] and 2016 bytes of the frame) and 280 bytes of EDC and
//! ECC. An audio sector is Form 2: 2324 bytes of XA-ADPCM (`psx-audio-cook`)
//! and the 4-byte EDC. The disc builder computes the EDC and ECC at the
//! sector's final address, so they are written here as zeros.

use psx_fmv::stream::{CHUNK_HEADER_BYTES, CHUNK_PAYLOAD_BYTES};
use psx_hw::cd::xa::{SUBMODE_AUDIO, SUBMODE_DATA, SUBMODE_FORM2, SUBMODE_REAL_TIME};

/// Bytes of a sector record: 8-byte subheader and 2328 bytes.
pub const RECORD_BYTES: usize = psx_audio_cook::xa::RAW_SECTOR_BYTES;
/// Submode of a video sector: data, real time (Form 1).
pub const SUBMODE_VIDEO_SECTOR: u8 = SUBMODE_DATA | SUBMODE_REAL_TIME;
/// Submode of a sound sector: audio, Form 2, real time.
pub const SUBMODE_AUDIO_SECTOR: u8 = SUBMODE_AUDIO | SUBMODE_FORM2 | SUBMODE_REAL_TIME;
/// STR chunk magic and the MDEC video chunk type.
const CHUNK_MAGIC: u16 = 0x0160;
const CHUNK_TYPE_VIDEO: u16 = 0x8001;

/// Where a stream is filed on the disc: XA file and channel numbers, which
/// the player's drive filter selects.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Channel {
    /// XA file number.
    pub file: u8,
    /// XA channel number (0..=31).
    pub channel: u8,
}

impl Channel {
    /// XA file `file`, channel `channel`.
    #[must_use]
    pub const fn new(file: u8, channel: u8) -> Channel {
        Channel { file, channel }
    }
}

/// The video sectors of one frame: `bitstream` cut into 2016-byte chunks,
/// each behind its chunk header, padded with zeros to `chunks` sectors.
///
/// # Panics
///
/// If the bitstream needs more than `chunks` sectors or is shorter than its
/// 8-byte header.
#[must_use]
pub fn video_sectors(
    at: Channel,
    number: u32,
    size_px: (u16, u16),
    bitstream: &[u8],
    chunks: u16,
) -> Vec<[u8; RECORD_BYTES]> {
    assert!(
        bitstream.len() <= usize::from(chunks) * CHUNK_PAYLOAD_BYTES,
        "{} bytes in {chunks} chunks",
        bitstream.len()
    );
    (0..chunks)
        .map(|index| {
            let mut record = [0u8; RECORD_BYTES];
            let sub = [at.file, at.channel, SUBMODE_VIDEO_SECTOR, 0];
            record[..4].copy_from_slice(&sub);
            record[4..8].copy_from_slice(&sub);
            let user = &mut record[8..8 + 2048];
            user[0..2].copy_from_slice(&CHUNK_MAGIC.to_le_bytes());
            user[2..4].copy_from_slice(&CHUNK_TYPE_VIDEO.to_le_bytes());
            user[4..6].copy_from_slice(&index.to_le_bytes());
            user[6..8].copy_from_slice(&chunks.to_le_bytes());
            user[8..12].copy_from_slice(&number.to_le_bytes());
            user[12..16].copy_from_slice(&(bitstream.len() as u32).to_le_bytes());
            user[16..18].copy_from_slice(&size_px.0.to_le_bytes());
            user[18..20].copy_from_slice(&size_px.1.to_le_bytes());
            // A copy of the bitstream header.
            user[20..28].copy_from_slice(&bitstream[..8]);
            let start = usize::from(index) * CHUNK_PAYLOAD_BYTES;
            let piece = bitstream.get(start..).unwrap_or(&[]);
            let piece = &piece[..piece.len().min(CHUNK_PAYLOAD_BYTES)];
            user[CHUNK_HEADER_BYTES..CHUNK_HEADER_BYTES + piece.len()].copy_from_slice(piece);
            record
        })
        .collect()
}

/// One XA audio sector record.
#[must_use]
pub fn audio_sector(
    at: Channel,
    coding: u8,
    data: &psx_audio_cook::xa::SectorData,
) -> [u8; RECORD_BYTES] {
    let mut record = [0u8; RECORD_BYTES];
    let sub = [at.file, at.channel, SUBMODE_AUDIO_SECTOR, coding];
    record[..4].copy_from_slice(&sub);
    record[4..8].copy_from_slice(&sub);
    record[8..8 + data.len()].copy_from_slice(data);
    record
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_fmv::stream::FrameAssembler;

    #[test]
    fn chunks_reassemble_through_the_sdk_assembler() {
        let mut bitstream: Vec<u8> = (0..5000u32).map(|i| (i * 7) as u8).collect();
        bitstream[..8].copy_from_slice(&[0x20, 0, 0, 0x38, 1, 0, 2, 0]);
        let at = Channel {
            file: 1,
            channel: 0,
        };
        let sectors = video_sectors(at, 3, (320, 240), &bitstream, 4);
        assert_eq!(sectors.len(), 4);
        let mut asm = FrameAssembler::new();
        let mut buf = vec![0u8; 4 * CHUNK_PAYLOAD_BYTES];
        let mut done = None;
        for s in &sectors {
            assert_eq!(&s[..4], &[1, 0, SUBMODE_VIDEO_SECTOR, 0]);
            done = asm.add(&s[8..8 + 2048], &mut buf).or(done);
        }
        let frame = done.unwrap();
        assert_eq!(
            (frame.number, frame.size, frame.width, frame.height),
            (3, 5000, 320, 240)
        );
        assert_eq!(&buf[..5000], &bitstream[..]);
        assert!(buf[5000..].iter().all(|&b| b == 0));
    }
}
