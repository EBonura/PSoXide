//! The playtest/export disc's music: Music-node WAVs cooked to XA-ADPCM files.
//!
//! Each distinct WAV a Music node names becomes one song. Songs share files
//! four to a file (stereo, 37.8 kHz, single speed), longest first so the
//! padding every song carries up to its file's length stays small, and the
//! cooked `MUSIC_TRACKS` table tells the engine where each one went: entry `n`
//! is the song of Music-node track `n + 2` (the numbering CD-DA gave cooked
//! projects, see [`psx_level::MUSIC_FIRST_TRACK`]).
//!
//! The files are raw XA sectors for `mkisopsx --xa-file` or
//! `IsoBuilder::add_xa_file`; both place them after UI.PAK, the last file of
//! the shared playtest layout, which is how the table's sector numbers are
//! known before the image exists.
//!
//! Layout is a function of sample counts alone ([`layout`]), so the manifest
//! is rendered without encoding anything; [`encode`] does the encoding and
//! checks it against the layout.

use psx_audio_cook::xa::{self, DriveSpeed, Format, SampleRate};
use std::fmt::Write as _;

/// Every file is stereo at 37.8 kHz, the best XA quality, and single speed:
/// four songs to a file.
const FORMAT: Format = Format {
    stereo: true,
    rate: SampleRate::Hz37800,
};
const SPEED: DriveSpeed = DriveSpeed::Single;
/// File `n` (1-based) is `MUSIC{n}.XA` with subheader file number `n`.
const FILE_STEM: &str = "MUSIC";

/// Where one song went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Placement {
    /// Index of the file, from 0.
    pub file: usize,
    /// Channel of that file.
    pub channel: u8,
    /// Sectors the song spans from the file's start, at the file's own
    /// stride: the song's length, not the file's.
    pub span_sectors: u32,
}

/// Where every song went, and how big each file is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    /// One entry per song, in the order the songs were given.
    pub placements: Vec<Placement>,
    /// Sectors of each file, end guard included.
    pub file_sectors: Vec<u32>,
}

/// Samples per channel a source of `source_len` samples at `source_rate` Hz
/// has at the XA rate once `playback_speed_q12` (`4096` = 1.0x) is baked in.
/// A faster song plays as if its source rate were higher, so it comes out
/// shorter.
pub(crate) fn frames_at_xa_rate(
    source_len: usize,
    source_rate: u32,
    playback_speed_q12: u16,
) -> usize {
    psx_audio_cook::resampled_len(
        source_len,
        effective_rate(source_rate, playback_speed_q12),
        FORMAT.rate.hz(),
    )
}

fn effective_rate(source_rate: u32, playback_speed_q12: u16) -> u32 {
    ((u64::from(source_rate) * u64::from(playback_speed_q12.max(1)) + 2048) / 4096).max(1) as u32
}

/// Group songs of `frames` samples per channel into files. Songs sorted by
/// length, longest first, go `stride` to a file; within a file they keep their
/// input order, and ties keep input order too, so the result is stable.
fn assign(frames: &[usize]) -> Vec<(usize, u8)> {
    let stride = FORMAT.stride(SPEED);
    let mut order: Vec<usize> = (0..frames.len()).collect();
    order.sort_by(|&a, &b| frames[b].cmp(&frames[a]).then(a.cmp(&b)));
    let mut slot = vec![(0, 0); frames.len()];
    for (file, group) in order.chunks(stride).enumerate() {
        let mut group = group.to_vec();
        group.sort_unstable();
        for (channel, song) in group.into_iter().enumerate() {
            slot[song] = (file, channel as u8);
        }
    }
    slot
}

/// Lay songs of `frames` samples per channel (at the XA rate) out in files.
pub(crate) fn layout(frames: &[usize]) -> Layout {
    let stride = FORMAT.stride(SPEED);
    let slots = assign(frames);
    let files = slots.iter().map(|&(file, _)| file + 1).max().unwrap_or(0);
    let mut file_sectors = vec![0u32; files];
    let placements = frames
        .iter()
        .zip(&slots)
        .map(|(&frames, &(file, channel))| {
            let span = (xa::sector_count(FORMAT, frames) * stride) as u32;
            file_sectors[file] = file_sectors[file].max(span);
            Placement {
                file,
                channel,
                span_sectors: span,
            }
        })
        .collect();
    for sectors in &mut file_sectors {
        *sectors += psx_hw::cd::xa::END_GUARD_SECTORS;
    }
    Layout {
        placements,
        file_sectors,
    }
}

impl Layout {
    /// The `MUSIC_TRACKS` table the cooked manifest carries, for files laid
    /// down back to back from sector `first_lba` of the image.
    pub fn render_tracks(&self, first_lba: u32) -> String {
        let mut file_lba = Vec::with_capacity(self.file_sectors.len());
        let mut next = first_lba;
        for sectors in &self.file_sectors {
            file_lba.push(next);
            next += sectors;
        }
        let mut out = String::from(
            "/// XA songs of the project's Music nodes. Entry `n` is track `n + 2`.\n\
             pub static MUSIC_TRACKS: &[psx_level::LevelMusicTrack] = &[\n",
        );
        for p in &self.placements {
            let _ = writeln!(
                out,
                "    psx_level::LevelMusicTrack {{ file_lba: {}, span_sectors: {}, file_number: {}, channel: {} }},",
                file_lba[p.file],
                p.span_sectors,
                p.file + 1,
                p.channel,
            );
        }
        out.push_str("];\n");
        out
    }

    /// Names the files go by on the disc.
    pub fn file_names(&self) -> Vec<String> {
        (1..=self.file_sectors.len())
            .map(|n| format!("{FILE_STEM}{n}.XA"))
            .collect()
    }
}

/// One song to cook.
pub(crate) struct Source<'a> {
    /// The WAV file's bytes.
    pub wav: &'a [u8],
    /// Baked playback speed in Q12 (`4096` = 1.0x).
    pub playback_speed_q12: u16,
}

/// Encode `sources` into the files `layout` describes: raw XA sectors for
/// `mkisopsx --xa-file`, one `Vec` per file.
pub(crate) fn encode(sources: &[Source<'_>], layout: &Layout) -> Result<Vec<Vec<u8>>, String> {
    let mut pcm = Vec::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        let (rate, channels) = psx_audio_cook::wav::read_channels(source.wav)
            .map_err(|e| format!("music source {index}: {e}"))?;
        let rate = effective_rate(rate, source.playback_speed_q12);
        let shaped = xa::prepare_pcm(FORMAT, rate, &channels, None);
        let frames = shaped.iter().map(Vec::len).max().unwrap_or(0);
        let expected = xa::sector_count(FORMAT, frames) * FORMAT.stride(SPEED);
        if layout
            .placements
            .get(index)
            .map(|p| p.span_sectors as usize)
            != Some(expected)
        {
            return Err(format!(
                "music source {index} changed since the manifest was laid out"
            ));
        }
        pcm.push(shaped);
    }
    let mut files = Vec::with_capacity(layout.file_sectors.len());
    for file in 0..layout.file_sectors.len() {
        let mut members: Vec<(u8, usize)> = layout
            .placements
            .iter()
            .enumerate()
            .filter(|(_, p)| p.file == file)
            .map(|(song, p)| (p.channel, song))
            .collect();
        members.sort_unstable();
        // Every song of a file has the file's length.
        let sectors = members
            .iter()
            .map(|&(_, song)| {
                xa::sector_count(FORMAT, pcm[song].iter().map(Vec::len).max().unwrap_or(0))
            })
            .max()
            .unwrap_or(1);
        let encoded: Vec<_> = std::thread::scope(|scope| {
            let jobs: Vec<_> = members
                .iter()
                .map(|&(_, song)| {
                    let pcm = &pcm[song];
                    scope.spawn(move || xa::encode(FORMAT, pcm, sectors))
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().expect("encoder thread panicked"))
                .collect()
        });
        let interleaved = xa::interleave(FORMAT, SPEED, (file + 1) as u8, &encoded)?;
        if interleaved.sector_count != layout.file_sectors[file] {
            return Err(format!("music file {} is not the size laid out", file + 1));
        }
        files.push(interleaved.bytes);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mono 16-bit WAV of `seconds` of a quiet tone at `rate`.
    fn tone(rate: u32, seconds: f64) -> Vec<u8> {
        let samples: Vec<i16> = (0..(f64::from(rate) * seconds) as usize)
            .map(|i| ((i as f64 * 0.05).sin() * 8000.0) as i16)
            .collect();
        psx_audio_cook::wav::write_mono16(rate, &samples)
    }

    #[test]
    fn songs_share_files_four_to_a_file_longest_first() {
        let frames = [100, 500, 300, 400, 200];
        let layout = layout(&frames);
        // 500, 400, 300, 200 fill file 0; 100 is alone in file 1.
        assert_eq!(layout.placements[1].file, 0);
        assert_eq!(layout.placements[3].file, 0);
        assert_eq!(layout.placements[2].file, 0);
        assert_eq!(layout.placements[4].file, 0);
        assert_eq!(layout.placements[0].file, 1);
        // Within a file the input order decides the channel.
        let channels: Vec<_> = layout.placements.iter().map(|p| p.channel).collect();
        assert_eq!(channels, [0, 0, 1, 2, 3]);
    }

    #[test]
    fn a_short_song_ends_at_its_own_length_not_its_files() {
        let layout = layout(&[37_800 * 20, 37_800 * 8]);
        assert_eq!(layout.placements[0].file, layout.placements[1].file);
        // A sector holds 2016 samples per channel: 18.75 sectors a second,
        // 75 disc sectors a second once interleaved four wide.
        assert_eq!(layout.placements[0].span_sectors, 20 * 75);
        assert_eq!(layout.placements[1].span_sectors, 8 * 75);
        assert_eq!(
            layout.file_sectors,
            [20 * 75 + psx_hw::cd::xa::END_GUARD_SECTORS]
        );
    }

    #[test]
    fn a_faster_song_is_shorter_and_a_slower_one_longer() {
        let normal = frames_at_xa_rate(44_100 * 10, 44_100, 4096);
        assert_eq!(normal, 37_800 * 10);
        assert_eq!(frames_at_xa_rate(44_100 * 10, 44_100, 8192), 37_800 * 5);
        assert_eq!(frames_at_xa_rate(44_100 * 10, 44_100, 2048), 37_800 * 20);
    }

    #[test]
    fn render_tracks_places_files_back_to_back_from_the_first_sector() {
        let layout = layout(&[37_800 * 8, 37_800 * 4, 37_800 * 4, 37_800 * 4, 37_800 * 2]);
        let text = layout.render_tracks(5000);
        let first_file = 8 * 75 + psx_hw::cd::xa::END_GUARD_SECTORS;
        // 2 s is 37.5 sectors of audio, 38 whole ones, 152 interleaved.
        assert!(text.contains(&format!(
            "LevelMusicTrack {{ file_lba: 5000, span_sectors: {}, file_number: 1, channel: 0 }}",
            8 * 75
        )));
        assert!(text.contains(&format!(
            "file_lba: {}, span_sectors: {}, file_number: 2, channel: 0",
            5000 + first_file,
            152
        )));
        assert_eq!(layout.file_names(), ["MUSIC1.XA", "MUSIC2.XA"]);
    }

    #[test]
    fn encoded_files_match_the_layout_and_carry_each_song_on_its_channel() {
        let a = tone(44_100, 1.0);
        let b = tone(22_050, 0.5);
        let sources = [
            Source {
                wav: &a,
                playback_speed_q12: 4096,
            },
            Source {
                wav: &b,
                playback_speed_q12: 4096,
            },
        ];
        let frames = [
            frames_at_xa_rate(44_100, 44_100, 4096),
            frames_at_xa_rate(11_025, 22_050, 4096),
        ];
        let layout = layout(&frames);
        let files = encode(&sources, &layout).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].len(),
            layout.file_sectors[0] as usize * xa::RAW_SECTOR_BYTES
        );
        let sectors = xa::parse_file(&files[0]).unwrap();
        assert!(sectors.iter().all(|s| s.file == 1));
        for channel in [0u8, 1] {
            assert!(
                sectors.iter().any(|s| s.channel == channel),
                "channel {channel} has no sectors"
            );
        }
        // The 0.5 s song stops carrying audio after its own span and the file
        // is padded with silence to the longer song's length.
        assert_eq!(layout.placements[0].span_sectors, 19 * 4);
        assert_eq!(layout.placements[1].span_sectors, 10 * 4);
    }

    #[test]
    fn a_changed_source_is_refused() {
        let a = tone(44_100, 1.0);
        let layout = layout(&[frames_at_xa_rate(44_100 * 2, 44_100, 4096)]);
        let err = encode(
            &[Source {
                wav: &a,
                playback_speed_q12: 4096,
            }],
            &layout,
        )
        .unwrap_err();
        assert!(err.contains("changed since"), "{err}");
    }
}
