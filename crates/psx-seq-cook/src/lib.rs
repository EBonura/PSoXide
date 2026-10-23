//! Cook and render sequenced music for `psx-seq`.
//!
//! The importer reads an N64 cartridge the player owns ([`n64`]), the cook
//! turns it into the `psx-seq` containers, and the renderer plays a cooked
//! song to a WAV on the host so it can be compared with a capture of the
//! original.

pub mod cook;
pub mod gen;
pub mod n64;
pub mod render;
pub mod rom;
pub mod survey;

/// Errors from reading and cooking.
#[derive(Debug)]
pub enum Error {
    /// A file could not be read or written.
    Io(String),
    /// Data ended early at this offset.
    Truncated(usize),
    /// Data does not have the expected layout.
    Format(String),
    /// A named thing is not there.
    Missing(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(m) => write!(f, "io: {m}"),
            Error::Truncated(at) => write!(f, "data ends early at {at:#x}"),
            Error::Format(m) => write!(f, "format: {m}"),
            Error::Missing(m) => write!(f, "missing: {m}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    /// Cook the Kokiri Forest theme from a real cartridge and check the
    /// result parses and is self-consistent. Runs only when
    /// `PSX_SEQ_TEST_ROM` and `PSX_SEQ_TEST_SEGMENTS` name the player's own
    /// files; nothing from a cartridge is ever committed.
    #[test]
    fn a_cooked_song_and_bank_parse_and_agree() {
        let (Ok(rom), Ok(seg)) = (
            std::env::var("PSX_SEQ_TEST_ROM"),
            std::env::var("PSX_SEQ_TEST_SEGMENTS"),
        ) else {
            return;
        };
        let rom =
            crate::rom::Rom::load(std::path::Path::new(&rom), std::path::Path::new(&seg)).unwrap();
        let data = crate::n64::AudioData::load(&rom).unwrap();
        let c = crate::cook::cook_song(&data, 60, &crate::cook::Options::default()).unwrap();
        let bank = psx_seq::format::Bank::parse(&c.bank).unwrap();
        let header = psx_seq::format::SongHeader::parse(&c.song).unwrap();
        assert_eq!(header.n_fonts, 1);
        assert_eq!(header.code(&c.song).len(), 3984);
        assert_eq!(bank.sample_count(), 16);
        let font = bank.font_index(15).expect("font 15 is in the bank");
        let mut found = 0;
        for i in 0..=125u8 {
            if let Some(ins) = bank.instrument(font, i) {
                found += 1;
                assert!(bank.sample(ins.normal.sample).is_some(), "instrument {i}");
            }
        }
        assert!(found >= 12, "the song's instruments are present: {found}");
    }

    /// Cook two songs of one area against one bank: the bank holds both
    /// songs' instruments once, the ADPCM starts on the requested boundary,
    /// and the tables alone parse without the samples.
    #[test]
    fn songs_of_one_area_share_a_bank() {
        let (Ok(rom), Ok(seg)) = (
            std::env::var("PSX_SEQ_TEST_ROM"),
            std::env::var("PSX_SEQ_TEST_SEGMENTS"),
        ) else {
            return;
        };
        let rom =
            crate::rom::Rom::load(std::path::Path::new(&rom), std::path::Path::new(&seg)).unwrap();
        let data = crate::n64::AudioData::load(&rom).unwrap();
        let options = crate::cook::Options {
            adpcm_align: 2048,
            ..crate::cook::Options::default()
        };
        // KOKIRI (60) and LINK_HOUSE (31) draw on the same sample banks.
        let set = crate::cook::cook_songs(&data, &[60, 31], &options).unwrap();
        assert_eq!(set.songs.len(), 2);
        let bank = psx_seq::format::Bank::parse(&set.bank).unwrap();
        let (off, len) = bank.adpcm_range();
        assert_eq!(off % 2048, 0, "ADPCM starts on a sector boundary");
        assert_eq!(bank.adpcm().len(), len as usize);
        let tables = psx_seq::format::Bank::parse_tables(&set.bank[..bank.tables_used()]).unwrap();
        assert!(tables.adpcm().is_empty());
        assert_eq!(tables.sample_count(), bank.sample_count());
        // Shared samples are cooked once: the pair costs less than the two alone.
        let alone: usize = [60usize, 31]
            .iter()
            .map(|&id| {
                let one = crate::cook::cook_song(&data, id, &options).unwrap();
                psx_seq::format::Bank::parse(&one.bank)
                    .unwrap()
                    .adpcm()
                    .len()
            })
            .sum();
        assert!(bank.adpcm().len() < alone);
    }
}
