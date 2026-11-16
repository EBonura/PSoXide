//! Reading an STR back: where its frames and sound sit, and its pictures.
//!
//! Works on the raw 2336-byte sector records `mkisopsx --xa-file` takes (any
//! encoder's), with the SDK's own parsers: [`psx_fmv::stream`] for the
//! chunks and [`psx_fmv::bitstream`] (through [`crate::model`]) for the
//! pictures, and `psx-audio-cook`'s XA decoder for the sound.

use psx_audio_cook::xa;
use psx_fmv::bitstream::{decode_frame, Header};
use psx_fmv::stream::{Chunk, FrameAssembler, CHUNK_PAYLOAD_BYTES, MAX_CHUNKS};
use psx_hw::cd::xa::SUBMODE_AUDIO;

use crate::model::decode_rgb;
use crate::sector::RECORD_BYTES;

/// One frame of a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrameInfo {
    /// Frame number from the chunk headers.
    pub number: u32,
    /// Sectors (chunks) it occupies.
    pub chunks: u16,
    /// Bitstream bytes the chunk header announces.
    pub size: u32,
    /// Quantisation scale of the bitstream.
    pub qscale: u16,
    /// Sector index of its first and last chunk in the stream.
    pub sectors: (usize, usize),
    /// The frame's bitstream, `size` bytes.
    pub bitstream: Vec<u8>,
    /// Width and height.
    pub size_px: (u16, u16),
    /// Run-length words the SDK decoder produces for it (what a player
    /// sizes its MDEC buffer by).
    pub rle_words: usize,
}

/// What a stream holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Layout {
    /// Sector records in the stream.
    pub sectors: usize,
    /// Every complete frame, in stream order.
    pub frames: Vec<FrameInfo>,
    /// Sector index of every audio sector.
    pub audio: Vec<usize>,
    /// Sectors that are neither audio nor a video chunk.
    pub other: usize,
    /// Frames with chunks missing.
    pub dropped: u32,
}

/// Parses a stream of 2336-byte records.
///
/// # Errors
///
/// The stream is not whole records or a frame's bitstream header is short.
pub fn layout(raw: &[u8]) -> Result<Layout, String> {
    if raw.is_empty() || !raw.len().is_multiple_of(RECORD_BYTES) {
        return Err(format!(
            "{} bytes is not whole {RECORD_BYTES}-byte sectors",
            raw.len()
        ));
    }
    let mut out = Layout {
        sectors: raw.len() / RECORD_BYTES,
        ..Layout::default()
    };
    let mut asm = FrameAssembler::new();
    let mut buf = vec![0u8; usize::from(MAX_CHUNKS) * CHUNK_PAYLOAD_BYTES];
    let mut first = None;
    for (i, record) in raw.chunks_exact(RECORD_BYTES).enumerate() {
        if record[2] & SUBMODE_AUDIO != 0 {
            out.audio.push(i);
            continue;
        }
        let user = &record[8..8 + 2048];
        let Some(chunk) = Chunk::parse(user) else {
            out.other += 1;
            continue;
        };
        if chunk.index == 0 || first.is_none_or(|(frame, _)| frame != chunk.frame) {
            first = Some((chunk.frame, i));
        }
        let Some(done) = asm.add(user, &mut buf) else {
            continue;
        };
        let size = (done.size as usize).min(buf.len());
        let header =
            Header::parse(&buf[..size]).map_err(|e| format!("frame {}: {e:?}", done.number))?;
        let macroblocks = u32::from(done.width).div_ceil(16) * u32::from(done.height).div_ceil(16);
        let mut rle = vec![0u16; macroblocks as usize * 6 * 65 + 64];
        let rle_words = decode_frame(&buf[..size], &mut rle, macroblocks, macroblocks, &mut || {})
            .map_err(|e| format!("frame {}: {e:?}", done.number))?;
        out.frames.push(FrameInfo {
            number: done.number,
            chunks: chunk.count,
            size: done.size,
            qscale: header.qscale,
            sectors: (first.map_or(i, |(_, at)| at), i),
            bitstream: buf[..size].to_vec(),
            size_px: (done.width, done.height),
            rle_words,
        });
    }
    out.dropped = asm.dropped;
    Ok(out)
}

impl FrameInfo {
    /// The frame decoded as the MDEC renders it, 24-bit RGB.
    ///
    /// # Errors
    ///
    /// The SDK decoder refused the bitstream.
    pub fn rgb(&self) -> Result<Vec<u8>, String> {
        decode_rgb(
            &self.bitstream,
            usize::from(self.size_px.0),
            usize::from(self.size_px.1),
        )
        .map_err(|e| format!("frame {}: {e:?}", self.number))
    }
}

/// The sound of the audio sectors on `file`/`channel`, decoded with the
/// reference XA decoder, mono or the first channel of stereo.
///
/// # Errors
///
/// No audio on that channel, or not 4-bit XA.
pub fn audio(raw: &[u8], channel: u8) -> Result<(u32, Vec<i16>), String> {
    let (format, pcm) = xa::decode_channel(raw, channel)?;
    let first = pcm.into_iter().next().unwrap_or_default();
    Ok((format.rate.hz(), first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_that_is_not_whole_records_is_refused() {
        assert!(layout(&[0u8; RECORD_BYTES + 1]).is_err());
        assert!(layout(&[]).is_err());
        let blank = layout(&vec![0u8; RECORD_BYTES * 2]).unwrap();
        assert_eq!((blank.other, blank.frames.len()), (2, 0));
    }
}
