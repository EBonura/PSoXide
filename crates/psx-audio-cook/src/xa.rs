//! CD-ROM XA ADPCM: an encoder, a reference decoder and the interleaved
//! sector writer.
//!
//! Written from nocash psx-spx ("CDROM XA Audio ADPCM Compression", "CDROM XA
//! Subheader, File, Channel, Interleave"). An audio sector is 18 sound groups
//! of 128 bytes (16 header bytes, 28 data words) plus 20 zero bytes, in a
//! Mode 2 Form 2 sector. A group holds four blocks of 28 samples per channel
//! for stereo, or eight blocks for mono; each block has a header byte with its
//! shift (0..=12, 0 is loudest) and filter (0..=3).
//!
//! * [`encode`] turns PCM into sector payloads. For every block it tries all
//!   four filters at every shift against the decoder's own history and keeps
//!   the pair with the least squared error.
//! * [`decode`] is the reference decoder the tests and `psx-audio-cook xa-decode`
//!   measure against.
//! * [`interleave`] lays several encoded songs out as the channels of one
//!   file, at the stride the drive speed and sample format need, and writes
//!   the raw 2336-byte sectors `mkisopsx --xa-file` takes.

use crate::resample::Sinc;
use psx_hw::cd::xa as hw;

/// Samples per ADPCM block.
pub const BLOCK_SAMPLES: usize = hw::BLOCK_SAMPLES;
/// Bytes of a raw XA sector without sync and header: subheader and its copy
/// (8), 2324 data bytes and the 4-byte EDC (`mkisopsx` rebuilds the EDC).
pub const RAW_SECTOR_BYTES: usize = 2336;
/// Channel number of the filler sectors that pad unused interleave slots.
pub const FILLER_CHANNEL: u8 = 0xFF;

/// `(f0, f1)` per filter, in 1/64: the first four rows of the psx-spx tables.
const FILTERS: [(i32, i32); 4] = [(0, 0), (60, 0), (115, -52), (98, -55)];

/// Sample rate of an XA audio sector.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SampleRate {
    /// 37800 Hz.
    Hz37800,
    /// 18900 Hz.
    Hz18900,
}

impl SampleRate {
    /// The rate in Hz.
    pub const fn hz(self) -> u32 {
        match self {
            SampleRate::Hz37800 => 37_800,
            SampleRate::Hz18900 => 18_900,
        }
    }
}

/// Drive speed the interleave is built for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DriveSpeed {
    /// 75 sectors per second.
    Single,
    /// 150 sectors per second.
    Double,
}

impl DriveSpeed {
    /// Sectors the drive reads per second.
    pub const fn sectors_per_second(self) -> u32 {
        match self {
            DriveSpeed::Single => hw::SECTORS_PER_SECOND,
            DriveSpeed::Double => 2 * hw::SECTORS_PER_SECOND,
        }
    }
}

/// Channel layout and sample rate of an XA audio file.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Format {
    /// Two channels (left and right) rather than one.
    pub stereo: bool,
    /// Sample rate.
    pub rate: SampleRate,
}

impl Format {
    /// The coding-info subheader byte (4-bit samples, no emphasis).
    pub const fn coding_info(self) -> u8 {
        let channels = if self.stereo { hw::CODING_STEREO } else { 0 };
        let rate = match self.rate {
            SampleRate::Hz37800 => 0,
            SampleRate::Hz18900 => hw::CODING_RATE_18900,
        };
        channels | rate
    }

    /// Samples per channel in one sector.
    pub const fn samples_per_sector(self) -> usize {
        let blocks = if self.stereo { 4 } else { 8 };
        hw::GROUPS_PER_SECTOR * blocks * BLOCK_SAMPLES
    }

    /// Sectors one song consumes per second of audio (18.75 for 37.8 kHz
    /// stereo), as a fraction `(numerator, denominator)` of a second.
    pub const fn sectors_per_second_ratio(self) -> (u32, u32) {
        (self.rate.hz(), self.samples_per_sector() as u32)
    }

    /// Interleave stride: one song sector in every `stride` disc sectors
    /// plays at the right speed. 4 for 37.8 kHz stereo at single speed.
    pub const fn stride(self, speed: DriveSpeed) -> usize {
        (speed.sectors_per_second() as usize * self.samples_per_sector()) / self.rate.hz() as usize
    }
}

/// Decoder state of one channel: the last two output samples.
#[derive(Copy, Clone, Default, Debug)]
struct History {
    old: i32,
    older: i32,
}

fn predict(h: History, filter: usize) -> i32 {
    let (f0, f1) = FILTERS[filter];
    (h.old * f0 + h.older * f1 + 32) >> 6
}

/// One encoded block: header byte, signed nibbles and the decoder state after it.
struct Block {
    header: u8,
    nibbles: [u8; BLOCK_SAMPLES],
    end: History,
    error: f64,
}

fn quantize(h: History, x: &[f64], filter: usize, range: u8) -> Block {
    let shift = 12 - range as i32;
    let mut h = h;
    let mut nibbles = [0u8; BLOCK_SAMPLES];
    let mut error = 0.0;
    for (n, &target) in nibbles.iter_mut().zip(x) {
        let pred = predict(h, filter);
        let step = (1i32 << shift) as f64;
        let t = ((target - pred as f64) / step).round().clamp(-8.0, 7.0) as i32;
        let out = ((t << shift) + pred).clamp(-32768, 32767);
        error += (target - out as f64).powi(2);
        *n = (t & 0xF) as u8;
        h = History {
            old: out,
            older: h.old,
        };
    }
    Block {
        header: (filter as u8) << 4 | range,
        nibbles,
        end: h,
        error,
    }
}

/// Best block for `x` from decoder state `h`: every filter and range is
/// tried and the least squared error wins. `filter0_only` restricts the
/// search to filter 0, which ignores the history (the first block of a song,
/// so a drive that restarts mid-stream plays it cleanly).
fn encode_block(h: History, x: &[f64], filter0_only: bool) -> Block {
    let filters = if filter0_only { 1 } else { FILTERS.len() };
    let mut best: Option<Block> = None;
    for filter in 0..filters {
        for range in 0..=12u8 {
            let b = quantize(h, x, filter, range);
            if best.as_ref().is_none_or(|best| b.error < best.error) {
                best = Some(b);
            }
        }
    }
    best.expect("at least one candidate")
}

/// Payload of one Form 2 sector: 18 groups and 20 zero bytes.
pub type SectorData = [u8; hw::FORM2_DATA_BYTES];

/// Samples of `channel` from `at`, zero past the end.
fn block_of(channel: &[f64], at: usize) -> [f64; BLOCK_SAMPLES] {
    let mut x = [0.0; BLOCK_SAMPLES];
    for (slot, v) in x.iter_mut().zip(channel.iter().skip(at)) {
        *slot = *v;
    }
    x
}

/// Sectors needed for `frames` samples per channel (at least one).
pub fn sector_count(format: Format, frames: usize) -> usize {
    frames.div_ceil(format.samples_per_sector()).max(1)
}

/// Encode one song. `channels` holds one sample vector for mono or two for
/// stereo (16-bit range, any length); the result has `min_sectors` sectors,
/// or as many as the song needs when that is more. The song is padded with
/// silence to fill the last sector. The first block of each channel uses
/// filter 0 so the stream decodes from a cold start.
pub fn encode(format: Format, channels: &[Vec<f64>], min_sectors: usize) -> Vec<SectorData> {
    assert_eq!(channels.len(), if format.stereo { 2 } else { 1 });
    let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
    let sectors = sector_count(format, frames).max(min_sectors);
    let mut history = [History::default(); 2];
    let mut out = Vec::with_capacity(sectors);
    for sector in 0..sectors {
        let mut data = [0u8; hw::FORM2_DATA_BYTES];
        for group in 0..hw::GROUPS_PER_SECTOR {
            let g = sector * hw::GROUPS_PER_SECTOR + group;
            let bytes = &mut data[group * hw::GROUP_BYTES..][..hw::GROUP_BYTES];
            for blk in 0..4 {
                for half in 0..2 {
                    let (stream, at) = if format.stereo {
                        (half, g * 4 * BLOCK_SAMPLES + blk * BLOCK_SAMPLES)
                    } else {
                        (0, g * 8 * BLOCK_SAMPLES + (blk * 2 + half) * BLOCK_SAMPLES)
                    };
                    let x = block_of(&channels[stream], at);
                    let first = g == 0 && blk == 0 && (format.stereo || half == 0);
                    let b = encode_block(history[stream], &x, first);
                    history[stream] = b.end;
                    pack_block(bytes, blk, half, &b);
                }
            }
            // Headers are stored twice: bytes 0..4 repeat 4..8, 12..16 repeat 8..12.
            bytes.copy_within(4..8, 0);
            bytes.copy_within(8..12, 12);
        }
        out.push(data);
    }
    out
}

fn pack_block(group: &mut [u8], blk: usize, half: usize, b: &Block) {
    group[4 + blk * 2 + half] = b.header;
    for (j, &nibble) in b.nibbles.iter().enumerate() {
        group[16 + j * 4 + blk] |= nibble << (4 * half);
    }
}

/// Reference decoder from the psx-spx pseudo code: the samples per channel
/// (one vector for mono, two for stereo) of consecutive sector payloads,
/// decoding from zero history.
pub fn decode(format: Format, sectors: &[SectorData]) -> Vec<Vec<i16>> {
    let streams = if format.stereo { 2 } else { 1 };
    let mut out = vec![Vec::new(); streams];
    let mut history = [History::default(); 2];
    for data in sectors {
        for group in data[..hw::SECTOR_ADPCM_BYTES].chunks_exact(hw::GROUP_BYTES) {
            for blk in 0..4 {
                for half in 0..2 {
                    let stream = if format.stereo { half } else { 0 };
                    let header = group[4 + blk * 2 + half];
                    // Shift 13..=15 are reserved and act like 9.
                    let range = match header & 0xF {
                        r @ 0..=12 => r,
                        _ => 9,
                    };
                    let filter = ((header >> 4) & 3) as usize;
                    for j in 0..BLOCK_SAMPLES {
                        let word = group[16 + j * 4 + blk];
                        let nibble = ((word >> (4 * half)) & 0xF) as i32;
                        let t = (nibble ^ 8) - 8;
                        let h = history[stream];
                        let s = ((t << (12 - range)) + predict(h, filter)).clamp(-32768, 32767);
                        history[stream] = History {
                            old: s,
                            older: h.old,
                        };
                        out[stream].push(s as i16);
                    }
                }
            }
        }
    }
    out
}

/// Submode of an audio sector: audio, Form 2, real time.
const SUBMODE_AUDIO: u8 = hw::SUBMODE_AUDIO | hw::SUBMODE_FORM2 | hw::SUBMODE_REAL_TIME;

/// An interleaved XA file ready for `mkisopsx --xa-file`.
pub struct Interleaved {
    /// Raw sectors, [`RAW_SECTOR_BYTES`] each.
    pub bytes: Vec<u8>,
    /// Sectors in the file, including the end guard.
    pub sector_count: u32,
    /// Sectors of the file that carry audio or filler inside the stride
    /// groups (everything before the end guard).
    pub song_span_sectors: u32,
    /// Interleave stride.
    pub stride: usize,
}

fn raw_sector(
    file: u8,
    channel: u8,
    submode: u8,
    coding: u8,
    data: &SectorData,
) -> [u8; RAW_SECTOR_BYTES] {
    let mut sector = [0u8; RAW_SECTOR_BYTES];
    let sub = [file, channel, submode, coding];
    sector[..4].copy_from_slice(&sub);
    sector[4..8].copy_from_slice(&sub);
    sector[8..8 + hw::FORM2_DATA_BYTES].copy_from_slice(data);
    sector
}

/// Lay `songs` out as channels `0..songs.len()` of XA file `file_number`.
///
/// Song sector `k` of channel `c` goes to disc sector `k * stride + c`
/// relative to the file start, where the stride is what `speed` and `format`
/// need for real-time playback. Slots beyond the last song hold filler
/// sectors on channel [`FILLER_CHANNEL`], audio-typed so the drive raises no
/// data IRQ for them. The file ends with [`hw::END_GUARD_SECTORS`] filler
/// sectors and marks its last sector end-of-file. All songs must have the
/// same number of sectors.
pub fn interleave(
    format: Format,
    speed: DriveSpeed,
    file_number: u8,
    songs: &[Vec<SectorData>],
) -> Result<Interleaved, String> {
    let stride = format.stride(speed);
    if songs.is_empty() || songs.len() > stride || songs.len() > 32 {
        return Err(format!(
            "{} songs do not fit {stride} interleave slots (at most 32 channels)",
            songs.len()
        ));
    }
    let length = songs[0].len();
    if songs.iter().any(|s| s.len() != length) {
        return Err("songs must have the same number of sectors".into());
    }
    let coding = format.coding_info();
    let filler: SectorData = [0; hw::FORM2_DATA_BYTES];
    let span = length * stride;
    let total = span + hw::END_GUARD_SECTORS as usize;
    let mut bytes = Vec::with_capacity(total * RAW_SECTOR_BYTES);
    for at in 0..total {
        let (channel, data) = match (at < span).then(|| (at % stride, at / stride)) {
            Some((slot, k)) if slot < songs.len() => (slot as u8, &songs[slot][k]),
            _ => (FILLER_CHANNEL, &filler),
        };
        let last = if at + 1 == total {
            hw::SUBMODE_EOF | hw::SUBMODE_EOR
        } else {
            0
        };
        bytes.extend_from_slice(&raw_sector(
            file_number,
            channel,
            SUBMODE_AUDIO | last,
            coding,
            data,
        ));
    }
    Ok(Interleaved {
        bytes,
        sector_count: total as u32,
        song_span_sectors: span as u32,
        stride,
    })
}

/// One parsed raw XA sector.
pub struct RawSector {
    /// File number.
    pub file: u8,
    /// Channel number.
    pub channel: u8,
    /// Submode byte.
    pub submode: u8,
    /// Coding info byte.
    pub coding: u8,
    /// The 2324 data bytes.
    pub data: SectorData,
}

/// Split a file of raw 2336-byte sectors.
pub fn parse_file(bytes: &[u8]) -> Result<Vec<RawSector>, String> {
    if bytes.is_empty() || bytes.len() % RAW_SECTOR_BYTES != 0 {
        return Err(format!(
            "size is not a multiple of {RAW_SECTOR_BYTES} bytes"
        ));
    }
    Ok(bytes
        .chunks_exact(RAW_SECTOR_BYTES)
        .map(|s| {
            let mut data = [0u8; hw::FORM2_DATA_BYTES];
            data.copy_from_slice(&s[8..8 + hw::FORM2_DATA_BYTES]);
            RawSector {
                file: s[0],
                channel: s[1],
                submode: s[2],
                coding: s[3],
                data,
            }
        })
        .collect())
}

/// The format a coding-info byte describes, if it is 4-bit audio.
pub fn format_of(coding: u8) -> Option<Format> {
    if coding & 0xF0 != 0 || coding & 3 > 1 || coding & 0x08 != 0 {
        return None;
    }
    Some(Format {
        stereo: coding & 3 == hw::CODING_STEREO,
        rate: if coding & hw::CODING_RATE_18900 != 0 {
            SampleRate::Hz18900
        } else {
            SampleRate::Hz37800
        },
    })
}

/// Decode channel `channel` of an interleaved file with the reference
/// decoder: the format found in its sectors and the samples per channel.
pub fn decode_channel(bytes: &[u8], channel: u8) -> Result<(Format, Vec<Vec<i16>>), String> {
    let sectors = parse_file(bytes)?;
    let mine: Vec<&RawSector> = sectors
        .iter()
        .filter(|s| s.channel == channel && s.submode & hw::SUBMODE_AUDIO != 0)
        .collect();
    let first = mine
        .first()
        .ok_or(format!("no audio sectors on channel {channel}"))?;
    let format = format_of(first.coding).ok_or("not 4-bit XA-ADPCM audio")?;
    let data: Vec<SectorData> = mine.iter().map(|s| s.data).collect();
    Ok((format, decode(format, &data)))
}

/// Shape decoded WAV channels into the channels `format` needs and resample
/// them to the format's rate: a mono source is copied to both channels of a
/// stereo format, a stereo source averaged for a mono one, and channels past
/// the second ignored. `peak` scales the loudest sample to that fraction of
/// full scale (`None` keeps the level).
pub fn prepare_pcm(
    format: Format,
    source_rate: u32,
    channels: &[Vec<f64>],
    peak: Option<f64>,
) -> Vec<Vec<f64>> {
    let shaped: Vec<Vec<f64>> = match (format.stereo, channels.len()) {
        (true, 1) => vec![channels[0].clone(), channels[0].clone()],
        (true, _) => channels[..2].to_vec(),
        (false, 1) => channels.to_vec(),
        (false, _) => {
            let len = channels[0].len().min(channels[1].len());
            vec![(0..len)
                .map(|i| 0.5 * (channels[0][i] + channels[1][i]))
                .collect()]
        }
    };
    let sinc = Sinc::new();
    let to = format.rate.hz();
    let mut out: Vec<Vec<f64>> = shaped
        .iter()
        .map(|c| {
            if source_rate == to {
                c.clone()
            } else {
                sinc.resample(c, source_rate, to)
            }
        })
        .collect();
    if let Some(target) = peak {
        let top = out.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        if top > 0.0 {
            let gain = target.clamp(0.0, 1.0) * 32767.0 / top;
            out.iter_mut().flatten().for_each(|v| *v *= gain);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::snr_db;
    use std::f64::consts::PI;

    const STEREO_37K: Format = Format {
        stereo: true,
        rate: SampleRate::Hz37800,
    };
    const MONO_18K: Format = Format {
        stereo: false,
        rate: SampleRate::Hz18900,
    };

    /// A-minor chord with a slow tremolo; `detune` moves the right channel.
    fn chord(rate: u32, seconds: f64, detune: f64) -> Vec<f64> {
        (0..(rate as f64 * seconds) as usize)
            .map(|i| {
                let t = i as f64 / rate as f64;
                let env = 0.7 + 0.3 * (2.0 * PI * 3.0 * t).sin();
                let tone = |f: f64| (2.0 * PI * f * detune * t).sin();
                env * 6000.0 * (tone(220.0) + tone(261.63) + tone(329.63) + 0.5 * tone(880.0))
            })
            .collect()
    }

    fn rounded(x: &[f64]) -> Vec<i16> {
        x.iter().map(|v| v.round() as i16).collect()
    }

    #[test]
    fn stereo_chord_round_trips_above_45_db() {
        let (l, r) = (chord(37_800, 2.0, 1.0), chord(37_800, 2.0, 1.01));
        let sectors = encode(STEREO_37K, &[l.clone(), r.clone()], 0);
        let out = decode(STEREO_37K, &sectors);
        assert_eq!(
            out[0].len(),
            sectors.len() * STEREO_37K.samples_per_sector()
        );
        assert!(snr_db(&rounded(&l), &out[0]) > 45.0);
        assert!(snr_db(&rounded(&r), &out[1]) > 45.0);
        // The padding after the song decodes to silence.
        assert!(out[0][l.len() + 64..].iter().all(|&s| s.abs() < 4));
    }

    #[test]
    fn mono_chord_round_trips_above_45_db() {
        let m = chord(18_900, 2.0, 1.0);
        let sectors = encode(MONO_18K, &[m.clone()], 0);
        let out = decode(MONO_18K, &sectors);
        assert_eq!(out.len(), 1);
        assert!(snr_db(&rounded(&m), &out[0]) > 45.0);
    }

    #[test]
    fn a_loud_square_wave_survives_clipping() {
        let sq: Vec<f64> = (0..8000)
            .map(|i| if (i / 50) % 2 == 0 { 32767.0 } else { -32768.0 })
            .collect();
        let sectors = encode(STEREO_37K, &[sq.clone(), sq.clone()], 0);
        let out = decode(STEREO_37K, &sectors);
        assert!(snr_db(&rounded(&sq), &out[0]) > 15.0);
    }

    /// A group written by hand from the psx-spx layout, decoded without the
    /// encoder: filter 1, range 12 (shift 0), every nibble the same.
    #[test]
    fn hand_built_group_decodes_by_the_spec_formula() {
        let mut data = [0u8; hw::FORM2_DATA_BYTES];
        let header = 0x10 | 12;
        for i in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            data[i] = header;
        }
        // Left blocks take the low nibbles, right the high: left +1, right -2.
        for byte in data[16..128].iter_mut() {
            *byte = 0xE1;
        }
        let out = decode(STEREO_37K, &[data]);
        // s[n] = 1 + (s[n-1] * 60 + 32) >> 6 with s[-1] = 0: 1, 2, 3, ...
        let mut expected = Vec::new();
        let mut old = 0i32;
        for _ in 0..28 {
            old = 1 + ((old * 60 + 32) >> 6);
            expected.push(old as i16);
        }
        assert_eq!(&out[0][..28], &expected[..]);
        assert_eq!(
            out[0][28],
            1 + ((expected[27] as i32 * 60 + 32) >> 6) as i16
        );
        assert!(out[1][..28].iter().all(|&s| s <= 0));
    }

    #[test]
    fn sector_layout_follows_the_spec() {
        let sectors = encode(
            STEREO_37K,
            &[chord(37_800, 0.1, 1.0), chord(37_800, 0.1, 1.0)],
            0,
        );
        let data = &sectors[0];
        for group in data[..hw::SECTOR_ADPCM_BYTES].chunks_exact(hw::GROUP_BYTES) {
            assert_eq!(group[0..4], group[4..8]);
            assert_eq!(group[8..12], group[12..16]);
            assert!(group[4..12].iter().all(|&h| h & 0xC0 == 0 && h & 0xF <= 12));
        }
        assert!(data[hw::SECTOR_ADPCM_BYTES..].iter().all(|&b| b == 0));
        // The first block of each channel is history free (filter 0).
        assert_eq!(data[4] & 0x30, 0);
        assert_eq!(data[5] & 0x30, 0);
    }

    #[test]
    fn strides_match_the_psx_spx_interleave_table() {
        let f = |stereo, rate| Format { stereo, rate };
        let table = [
            (f(true, SampleRate::Hz37800), 4, 8),
            (f(true, SampleRate::Hz18900), 8, 16),
            (f(false, SampleRate::Hz37800), 8, 16),
            (f(false, SampleRate::Hz18900), 16, 32),
        ];
        for (format, single, double) in table {
            assert_eq!(format.stride(DriveSpeed::Single), single);
            assert_eq!(format.stride(DriveSpeed::Double), double);
        }
        assert_eq!(STEREO_37K.coding_info(), 0x01);
        assert_eq!(MONO_18K.coding_info(), 0x04);
        assert_eq!(
            format_of(0x05),
            Some(Format {
                stereo: true,
                rate: SampleRate::Hz18900
            })
        );
        assert_eq!(format_of(0x10), None);
    }

    fn silence(sectors: usize) -> Vec<SectorData> {
        encode(STEREO_37K, &[vec![], vec![]], sectors)
    }

    #[test]
    fn interleave_places_songs_on_their_channels() {
        let songs = vec![silence(3), silence(3), silence(3)];
        let file = interleave(STEREO_37K, DriveSpeed::Single, 7, &songs).unwrap();
        assert_eq!(file.stride, 4);
        assert_eq!(file.song_span_sectors, 12);
        assert_eq!(file.sector_count, 12 + hw::END_GUARD_SECTORS);
        let sectors = parse_file(&file.bytes).unwrap();
        assert_eq!(sectors.len(), file.sector_count as usize);
        for (at, s) in sectors.iter().enumerate() {
            let expected = match at {
                0..=11 if at % 4 < 3 => (at % 4) as u8,
                _ => FILLER_CHANNEL,
            };
            assert_eq!(
                (s.file, s.channel, s.coding),
                (7, expected, 0x01),
                "sector {at}"
            );
            let last = at + 1 == sectors.len();
            let submode = SUBMODE_AUDIO | if last { 0x81 } else { 0 };
            assert_eq!(s.submode, submode, "sector {at}");
        }
        // Subheaders are written twice.
        let raw = &file.bytes[..RAW_SECTOR_BYTES];
        assert_eq!(raw[..4], raw[4..8]);
        assert_eq!(raw[2], 0x64);
    }

    #[test]
    fn interleave_rejects_too_many_or_uneven_songs() {
        let five = vec![silence(1); 5];
        assert!(interleave(STEREO_37K, DriveSpeed::Single, 1, &five).is_err());
        assert!(interleave(STEREO_37K, DriveSpeed::Double, 1, &five).is_ok());
        let uneven = vec![silence(1), silence(2)];
        assert!(interleave(STEREO_37K, DriveSpeed::Single, 1, &uneven).is_err());
        assert!(interleave(STEREO_37K, DriveSpeed::Single, 1, &[]).is_err());
    }

    #[test]
    fn decode_channel_picks_one_song_out_of_the_file() {
        let tone = |f: f64| -> Vec<f64> {
            (0..30_000)
                .map(|i| 9000.0 * (2.0 * PI * f * i as f64 / 37_800.0).sin())
                .collect()
        };
        let songs: Vec<Vec<SectorData>> = [300.0, 700.0, 1500.0, 3100.0]
            .iter()
            .map(|&f| encode(STEREO_37K, &[tone(f), tone(f)], 15))
            .collect();
        let file = interleave(STEREO_37K, DriveSpeed::Single, 1, &songs).unwrap();
        for (channel, f) in [300.0, 700.0, 1500.0, 3100.0].into_iter().enumerate() {
            let (format, pcm) = decode_channel(&file.bytes, channel as u8).unwrap();
            assert_eq!(format, STEREO_37K);
            let snr = snr_db(&rounded(&tone(f)), &pcm[0]);
            assert!(snr > 30.0, "channel {channel}: {snr:.1} dB");
        }
        assert!(decode_channel(&file.bytes, 9).is_err());
    }

    #[test]
    fn prepare_pcm_shapes_and_resamples() {
        let tone: Vec<f64> = (0..44_100)
            .map(|i| 8000.0 * (2.0 * PI * 440.0 * i as f64 / 44_100.0).sin())
            .collect();
        let stereo = prepare_pcm(STEREO_37K, 44_100, &[tone.clone()], None);
        assert_eq!(stereo.len(), 2);
        assert_eq!(stereo[0], stereo[1]);
        assert_eq!(stereo[0].len(), 37_800);
        let mono = prepare_pcm(MONO_18K, 44_100, &[tone.clone(), tone.clone()], None);
        assert_eq!((mono.len(), mono[0].len()), (1, 18_900));
        let loud = prepare_pcm(STEREO_37K, 37_800, &[tone], Some(1.0));
        let top = loud[0].iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!((top - 32767.0).abs() < 1.0);
    }
}
