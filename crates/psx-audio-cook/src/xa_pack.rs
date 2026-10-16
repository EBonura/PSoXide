//! Many songs into as few XA files as the interleave allows.
//!
//! [`crate::xa::interleave`] lays up to `stride` songs into one file, every
//! song padded to the file's longest. A game with a dozen songs therefore
//! has to decide which songs share a file, and every game that did so grew
//! its own copy of the rule and its own table that tells the guest where a
//! song went. This module is the one copy:
//!
//! * [`assign`] groups songs by length, longest first, `stride` to a file, so
//!   the padding each song carries is as small as the lengths allow.
//! * [`pack`] encodes the songs, interleaves each group and returns the files
//!   with a [`Placement`] per song, in the order the songs were given.
//! * [`manifest_json`] and [`rust_source`] describe the result: the first for
//!   build tools, the second as constants a guest `include!`s, so no game
//!   keeps a hand-written song table.
//!
//! The command line is `psx-audio-cook xa-pack` ([`crate::xa_cli::pack`]).

use crate::xa::{self, DriveSpeed, Format, Interleaved};

/// Longest `NAME` the file names allow: an 8.3 name keeps room for the file
/// index, and ISO 9660 level 1 allows eight characters before the dot.
pub const MAX_NAME_CHARS: usize = 6;

/// One song, already shaped to the file format ([`xa::prepare_pcm`]).
pub struct Song {
    /// What the song is called in the manifest.
    pub name: String,
    /// One sample vector for a mono format, two for stereo, at the format's rate.
    pub pcm: Vec<Vec<f64>>,
}

/// Where a song ended up.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    /// The song's name.
    pub name: String,
    /// Index into [`Packed::files`].
    pub file: usize,
    /// Channel of that file: the number the player filters on.
    pub channel: usize,
    /// Samples per channel at the file's rate, before the padding every song
    /// of the file receives.
    pub samples: usize,
}

/// One interleaved file of the pack.
pub struct PackedFile {
    /// Name on the disc, e.g. `MUSIC1.XA`.
    pub name: String,
    /// Subheader file number the player filters on.
    pub number: u8,
    /// The file's sectors and layout.
    pub data: Interleaved,
    /// Songs sharing the file.
    pub songs: usize,
}

/// The result of [`pack`].
pub struct Packed {
    /// Format every file is encoded in.
    pub format: Format,
    /// Drive speed the files are interleaved for.
    pub speed: DriveSpeed,
    /// The files, in file order.
    pub files: Vec<PackedFile>,
    /// One placement per input song, in input order.
    pub placements: Vec<Placement>,
}

impl Packed {
    /// Sectors of every file, guard sectors included.
    pub fn total_sectors(&self) -> u64 {
        self.files
            .iter()
            .map(|f| u64::from(f.data.sector_count))
            .sum()
    }

    /// Interleave slots that carry no song, summed over the files. Each costs
    /// disc space exactly like music.
    pub fn empty_slots(&self) -> usize {
        self.files.iter().map(|f| f.data.stride - f.songs).sum()
    }
}

/// Which file and channel each song goes to: songs sorted by length, longest
/// first, `stride` to a file; within a file the songs keep their input order.
/// Ties keep input order too, so the result is stable. Returns, per song,
/// `(file, channel)`.
pub fn assign(lengths: &[usize], stride: usize) -> Vec<(usize, usize)> {
    assert!(stride > 0, "stride is at least 1");
    let mut order: Vec<usize> = (0..lengths.len()).collect();
    order.sort_by(|&a, &b| lengths[b].cmp(&lengths[a]).then(a.cmp(&b)));
    let mut slot = vec![(0, 0); lengths.len()];
    for (file, group) in order.chunks(stride).enumerate() {
        let mut group = group.to_vec();
        group.sort_unstable();
        for (channel, song) in group.into_iter().enumerate() {
            slot[song] = (file, channel);
        }
    }
    slot
}

/// Whether `name` can be the stem of the file names: upper-case letters and
/// digits, short enough for `NAME` + index + `.XA` to stay an 8.3 name.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_CHARS {
        return Err(format!(
            "pack name {name:?} must be 1 to {MAX_NAME_CHARS} characters"
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Err(format!(
            "pack name {name:?} must be upper-case letters and digits"
        ));
    }
    Ok(())
}

/// Encode `songs` and interleave them. File `i` is named `{name}{i + 1}.XA`
/// and carries subheader file number `first_file + i`. Songs encode in
/// parallel, one thread each.
pub fn pack(
    name: &str,
    format: Format,
    speed: DriveSpeed,
    first_file: u8,
    songs: &[Song],
) -> Result<Packed, String> {
    check_name(name)?;
    if songs.is_empty() {
        return Err("no songs to pack".into());
    }
    let stride = format.stride(speed);
    let frames = |s: &Song| s.pcm.iter().map(Vec::len).max().unwrap_or(0);
    let lengths: Vec<usize> = songs.iter().map(frames).collect();
    let slots = assign(&lengths, stride);
    let file_count = slots.iter().map(|&(f, _)| f + 1).max().unwrap_or(0);
    if usize::from(first_file) + file_count > 256 {
        return Err(format!(
            "{file_count} files from file number {first_file} overflow the 8-bit file number"
        ));
    }

    let mut files = Vec::with_capacity(file_count);
    for file in 0..file_count {
        let mut members: Vec<(usize, usize)> = slots
            .iter()
            .enumerate()
            .filter(|(_, &(f, _))| f == file)
            .map(|(song, &(_, channel))| (channel, song))
            .collect();
        members.sort_unstable();
        // Every song of a file has the file's length, so encode to the
        // longest member's sector count.
        let sectors = members
            .iter()
            .map(|&(_, song)| xa::sector_count(format, lengths[song]))
            .max()
            .unwrap_or(1);
        let encoded: Vec<_> = std::thread::scope(|scope| {
            let jobs: Vec<_> = members
                .iter()
                .map(|&(_, song)| {
                    let pcm = &songs[song].pcm;
                    scope.spawn(move || xa::encode(format, pcm, sectors))
                })
                .collect();
            jobs.into_iter()
                .map(|j| j.join().expect("encoder thread panicked"))
                .collect()
        });
        let number = first_file + file as u8;
        let data = xa::interleave(format, speed, number, &encoded)?;
        files.push(PackedFile {
            name: format!("{name}{}.XA", file + 1),
            number,
            data,
            songs: members.len(),
        });
    }

    let placements = songs
        .iter()
        .zip(&slots)
        .zip(&lengths)
        .map(|((song, &(file, channel)), &samples)| Placement {
            name: song.name.clone(),
            file,
            channel,
            samples,
        })
        .collect();
    Ok(Packed {
        format,
        speed,
        files,
        placements,
    })
}

fn json_escape(text: &str) -> String {
    text.replace(['\\', '"'], "_")
}

/// The pack as one line of JSON for build tools.
pub fn manifest_json(packed: &Packed) -> String {
    let (num, den) = packed.format.sectors_per_second_ratio();
    let files: Vec<String> = packed
        .files
        .iter()
        .map(|f| {
            format!(
                "{{\"name\":\"{}\",\"number\":{},\"sectors\":{},\"song_span_sectors\":{},\"songs\":{}}}",
                f.name, f.number, f.data.sector_count, f.data.song_span_sectors, f.songs
            )
        })
        .collect();
    let songs: Vec<String> = packed
        .placements
        .iter()
        .map(|p| {
            format!(
                "{{\"name\":\"{}\",\"file\":{},\"channel\":{},\"samples\":{},\"seconds\":{:.3}}}",
                json_escape(&p.name),
                p.file,
                p.channel,
                p.samples,
                p.samples as f64 / packed.format.rate.hz() as f64
            )
        })
        .collect();
    format!(
        "{{\"stereo\":{},\"sample_rate\":{},\"drive_speed\":{},\"stride\":{},\"sectors_per_second\":\"{num}/{den}\",\"empty_slots\":{},\"total_sectors\":{},\"files\":[{}],\"songs\":[{}]}}",
        packed.format.stereo,
        packed.format.rate.hz(),
        packed.speed.sectors_per_second() / 75,
        packed.format.stride(packed.speed),
        packed.empty_slots(),
        packed.total_sectors(),
        files.join(","),
        songs.join(",")
    )
}

/// Constants a guest `include!`s: the files to look up by name and the place
/// of every song, in input order. `XA_SPEED_DOUBLE` says which
/// `psx_io::cd::xa::DriveSpeed` to build the files with.
pub fn rust_source(packed: &Packed) -> String {
    let mut out = String::from(
        "// Generated by `psx-audio-cook xa-pack`; do not edit.\n\
         // XA_SONGS is (file index, channel, milliseconds) per song, in input order.\n",
    );
    let count = packed.files.len();
    out.push_str(&format!("pub const XA_FILE_COUNT: usize = {count};\n"));
    out.push_str(&format!(
        "pub const XA_FILE_NAMES: [&str; {count}] = [{}];\n",
        packed
            .files
            .iter()
            .map(|f| format!("\"{}\"", f.name))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "pub const XA_FILE_NUMBERS: [u8; {count}] = [{}];\n",
        packed
            .files
            .iter()
            .map(|f| f.number.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "pub const XA_FILE_SECTORS: [u32; {count}] = [{}];\n",
        packed
            .files
            .iter()
            .map(|f| f.data.sector_count.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "pub const XA_SPEED_DOUBLE: bool = {};\n",
        matches!(packed.speed, DriveSpeed::Double)
    ));
    out.push_str(&format!(
        "pub const XA_SONGS: [(u8, u8, u32); {}] = [{}];\n",
        packed.placements.len(),
        packed
            .placements
            .iter()
            .map(|p| {
                let ms = (p.samples as u64 * 1000 / u64::from(packed.format.rate.hz())) as u32;
                format!("({}, {}, {ms})", p.file, p.channel)
            })
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::snr_db;
    use crate::xa::SampleRate;
    use std::f64::consts::PI;

    const MONO_18K: Format = Format {
        stereo: false,
        rate: SampleRate::Hz18900,
    };
    const STEREO_37K: Format = Format {
        stereo: true,
        rate: SampleRate::Hz37800,
    };

    fn tone(rate: u32, seconds: f64, hz: f64) -> Vec<f64> {
        (0..(f64::from(rate) * seconds) as usize)
            .map(|i| 8000.0 * (2.0 * PI * hz * i as f64 / f64::from(rate)).sin())
            .collect()
    }

    fn song(format: Format, name: &str, seconds: f64, hz: f64) -> Song {
        let plane = tone(format.rate.hz(), seconds, hz);
        let channels = if format.stereo { 2 } else { 1 };
        Song {
            name: name.into(),
            pcm: vec![plane; channels],
        }
    }

    fn rounded(x: &[f64]) -> Vec<i16> {
        x.iter().map(|v| v.round() as i16).collect()
    }

    #[test]
    fn longest_songs_share_a_file_and_keep_input_order() {
        // Sorted longest first: 60, 50, 40, 30 | 20, 10, 5.
        let lengths = [10, 50, 30, 40, 20, 60, 5];
        let slots = assign(&lengths, 4);
        assert_eq!(slots[1], (0, 0));
        assert_eq!(slots[2], (0, 1));
        assert_eq!(slots[3], (0, 2));
        assert_eq!(slots[5], (0, 3));
        assert_eq!(slots[0], (1, 0));
        assert_eq!(slots[4], (1, 1));
        assert_eq!(slots[6], (1, 2));
    }

    #[test]
    fn equal_lengths_fall_back_to_input_order() {
        let slots = assign(&[7, 7, 7, 7, 7], 4);
        assert_eq!(slots, [(0, 0), (0, 1), (0, 2), (0, 3), (1, 0)]);
    }

    #[test]
    fn fewer_songs_than_a_stride_make_one_file() {
        assert_eq!(assign(&[3, 9], 8), [(0, 0), (0, 1)]);
        assert!(assign(&[], 4).is_empty());
    }

    #[test]
    fn names_must_fit_an_8_3_file_name() {
        assert!(check_name("MUSIC").is_ok());
        assert!(check_name("MUSIC1").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("TOOLONGNAME").is_err());
        assert!(check_name("music").is_err());
        assert!(check_name("MU-SIC").is_err());
    }

    #[test]
    fn five_stereo_songs_make_two_files_and_every_channel_decodes_to_its_song() {
        let format = STEREO_37K;
        let songs = [
            song(format, "a", 0.30, 440.0),
            song(format, "b", 0.10, 660.0),
            song(format, "c", 0.25, 880.0),
            song(format, "d", 0.20, 1100.0),
            song(format, "e", 0.28, 1320.0),
        ];
        let packed = pack("TEST", format, DriveSpeed::Single, 2, &songs).expect("pack");
        assert_eq!(packed.files.len(), 2);
        assert_eq!(packed.files[0].name, "TEST1.XA");
        assert_eq!(packed.files[1].name, "TEST2.XA");
        assert_eq!(packed.files[0].number, 2);
        assert_eq!(packed.files[1].number, 3);
        // The four longest (a, e, c, d) share the first file.
        assert_eq!(packed.files[0].songs, 4);
        assert_eq!(packed.files[1].songs, 1);
        assert_eq!(packed.placements[1].file, 1);
        assert_eq!(packed.empty_slots(), 3);
        for (placement, source) in packed.placements.iter().zip(&songs) {
            let file = &packed.files[placement.file];
            let (decoded_format, planes) =
                xa::decode_channel(&file.data.bytes, placement.channel as u8).expect("decode");
            assert_eq!(decoded_format, format);
            let n = source.pcm[0].len();
            assert!(planes[0].len() >= n);
            let snr = snr_db(&rounded(&source.pcm[0]), &planes[0][..n]);
            assert!(
                snr > 35.0,
                "{} on file {} channel {}: {snr} dB",
                placement.name,
                placement.file,
                placement.channel
            );
        }
    }

    #[test]
    fn mono_18k_fits_sixteen_songs_in_one_file() {
        let format = MONO_18K;
        let songs: Vec<Song> = (0..16)
            .map(|i| {
                song(
                    format,
                    &format!("s{i}"),
                    0.1 + 0.01 * i as f64,
                    300.0 + 40.0 * i as f64,
                )
            })
            .collect();
        let packed = pack("MONO", format, DriveSpeed::Single, 1, &songs).expect("pack");
        assert_eq!(packed.files.len(), 1);
        assert_eq!(packed.empty_slots(), 0);
        assert_eq!(packed.files[0].data.stride, 16);
        // The file is as long as its longest song, plus the end guard.
        let longest = xa::sector_count(format, songs[15].pcm[0].len());
        assert_eq!(
            packed.files[0].data.song_span_sectors as usize,
            longest * 16
        );
    }

    #[test]
    fn a_seventeenth_mono_song_opens_a_second_file() {
        let format = MONO_18K;
        let songs: Vec<Song> = (0..17)
            .map(|i| song(format, &format!("s{i}"), 0.05, 400.0 + 30.0 * i as f64))
            .collect();
        let packed = pack("MONO", format, DriveSpeed::Single, 1, &songs).expect("pack");
        assert_eq!(packed.files.len(), 2);
        assert_eq!(packed.empty_slots(), 15);
    }

    #[test]
    fn manifest_and_constants_describe_the_same_placement() {
        let format = STEREO_37K;
        let songs = [
            song(format, "title", 0.20, 440.0),
            song(format, "boss", 0.10, 880.0),
        ];
        let packed = pack("MUSIC", format, DriveSpeed::Single, 2, &songs).expect("pack");
        let json = manifest_json(&packed);
        assert!(json.contains("\"name\":\"MUSIC1.XA\""));
        assert!(json.contains("\"number\":2"));
        assert!(json.contains("\"name\":\"title\",\"file\":0,\"channel\":0"));
        assert!(json.contains("\"name\":\"boss\",\"file\":0,\"channel\":1"));
        assert!(json.contains("\"empty_slots\":2"));
        let rust = rust_source(&packed);
        assert!(rust.contains("pub const XA_FILE_COUNT: usize = 1;"));
        assert!(rust.contains("pub const XA_FILE_NAMES: [&str; 1] = [\"MUSIC1.XA\"];"));
        assert!(rust.contains("pub const XA_FILE_NUMBERS: [u8; 1] = [2];"));
        assert!(rust.contains("pub const XA_SPEED_DOUBLE: bool = false;"));
        assert!(
            rust.contains("pub const XA_SONGS: [(u8, u8, u32); 2] = [(0, 0, 200), (0, 1, 100)];")
        );
    }

    #[test]
    fn too_many_files_for_the_file_number_are_refused() {
        let format = MONO_18K;
        let songs: Vec<Song> = (0..17)
            .map(|i| song(format, &format!("s{i}"), 0.02, 300.0 + 20.0 * i as f64))
            .collect();
        // Two mono 18.9 kHz files from file number 255 would need 255 and 256.
        assert!(pack("MONO", format, DriveSpeed::Single, 255, &songs).is_err());
        assert!(pack("MONO", format, DriveSpeed::Single, 1, &[]).is_err());
    }
}
