//! The two containers: a song (`PSQ1`) and a bank set (`PSQB`).
//!
//! A **song** is the sequence bytecode, byte for byte, behind a short header.
//! The bytecode keeps its original big-endian operands and absolute offsets,
//! because jumps, calls, layer loads and envelope pointers all refer to
//! positions inside it. The header is little-endian.
//!
//! ```text
//! 0   "PSQ1"
//! 4   u16 version = 1
//! 6   u16 flags        bit0 patches itself, bit1 reads IO, bit2 uses runseq
//! 8   u8  n_fonts      u8 peak_voices   u16 code_len   u16 channels_used   u16 reserved
//! 16  u8  font_id[n_fonts]               padded with zeros to a multiple of 4
//! ..  bytecode (code_len bytes)
//! ```
//!
//! A **bank** is everything a scene's songs play: instrument and drum
//! tables, envelopes, a sample table and the SPU-ADPCM data, laid out so the
//! ADPCM region goes to SPU RAM in one transfer. All little-endian except the
//! envelope words, which keep the bytecode's big-endian layout so a song's own
//! envelopes and the bank's read through one reader.
//!
//! ```text
//! 0   "PSQB"  u16 version = 1  u16 flags  u32 total_bytes
//! 12  u16 n_fonts  u16 n_samples  u16 n_env_words  u16 reserved
//! 20  u32 off_fonts  u32 off_samples  u32 off_env  u32 off_adpcm  u32 adpcm_bytes
//! FontDesc[n_fonts]  (12 B)  u8 id, u8 n_inst, u8 n_drum, u8 pad, u32 off_inst, u32 off_drum
//! InstDesc  (32 B)   u8 lo, u8 hi, u8 decay, u8 present, u16 env, u16 pad,
//!                    3 x { u16 sample, u16 pad, u32 tuning_q16 }   (low, normal, high)
//! DrumDesc  (16 B)   u8 decay, u8 pan, u16 env, u16 sample, u16 present, u32 tuning_q16, u32 pad
//! SampleDesc (16 B)  u32 adpcm_off, u32 end_samples, u16 blocks, u16 rate_hz, u16 loop_block, u16 flags
//! ```
//!
//! `tuning_q16` already includes the ratio of the original 32 kHz playback
//! rate to the cooked rate and the loop-stretch correction, so the runtime
//! multiplies one number. `0xFFFF` means "none" for sample and envelope
//! indices.

use psx_sfx::Sample;
use psx_spu::SpuAddr;

/// Song magic.
pub const SONG_MAGIC: [u8; 4] = *b"PSQ1";
/// Bank magic.
pub const BANK_MAGIC: [u8; 4] = *b"PSQB";
/// "No sample" / "no envelope" marker.
pub const NONE16: u16 = 0xFFFF;
/// Song flag: the bytecode writes to itself.
pub const FLAG_SELF_MODIFYING: u16 = 1;

/// Reasons a container is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// Wrong magic or version.
    BadHeader,
    /// A table runs past the end of the data.
    Truncated,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
        *b.get(at + 2)?,
        *b.get(at + 3)?,
    ]))
}

/// The parsed header of a song.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SongHeader {
    /// Flag bits.
    pub flags: u16,
    /// Number of soundfont ids.
    pub n_fonts: u8,
    /// Voices the song needs at most (measured by the cook).
    pub peak_voices: u8,
    /// Bytes of bytecode.
    pub code_len: u16,
    /// Soundfont ids, in the original table order (slot `s` is `n - 1 - s`).
    pub font_ids: [u8; 8],
    /// Offset of the bytecode in the file.
    pub code_off: u16,
}

impl SongHeader {
    /// Parse the header of `bytes`.
    ///
    /// ```
    /// use psx_seq::format::*;
    /// let mut f = vec![];
    /// f.extend_from_slice(b"PSQ1");
    /// f.extend_from_slice(&[1, 0, 0, 0, 1, 6, 2, 0, 1, 0, 0, 0]);
    /// f.extend_from_slice(&[7, 0, 0, 0]);
    /// f.extend_from_slice(&[0xFF, 0xFF]);
    /// let h = SongHeader::parse(&f).unwrap();
    /// assert_eq!((h.n_fonts, h.peak_voices, h.code_len, h.font_ids[0]), (1, 6, 2, 7));
    /// assert_eq!(h.code_off, 20);
    /// ```
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.get(0..4) != Some(&SONG_MAGIC) || le16(bytes, 4) != Some(1) {
            return Err(FormatError::BadHeader);
        }
        let flags = le16(bytes, 6).ok_or(FormatError::Truncated)?;
        let n_fonts = *bytes.get(8).ok_or(FormatError::Truncated)?;
        let peak_voices = *bytes.get(9).ok_or(FormatError::Truncated)?;
        let code_len = le16(bytes, 10).ok_or(FormatError::Truncated)?;
        if n_fonts as usize > 8 {
            return Err(FormatError::BadHeader);
        }
        let mut font_ids = [0xFF; 8];
        for (i, f) in font_ids.iter_mut().enumerate().take(n_fonts as usize) {
            *f = *bytes.get(16 + i).ok_or(FormatError::Truncated)?;
        }
        let code_off = 16 + ((n_fonts as usize + 3) & !3);
        if bytes.len() < code_off + code_len as usize {
            return Err(FormatError::Truncated);
        }
        Ok(Self {
            flags,
            n_fonts,
            peak_voices,
            code_len,
            font_ids,
            code_off: code_off as u16,
        })
    }

    /// The bytecode inside `bytes`.
    pub fn code<'a>(&self, bytes: &'a [u8]) -> &'a [u8] {
        &bytes[self.code_off as usize..self.code_off as usize + self.code_len as usize]
    }

    /// The bytecode inside `bytes`, writable (songs that patch themselves
    /// need RAM; the file is normally loaded into RAM anyway).
    pub fn code_mut<'a>(&self, bytes: &'a mut [u8]) -> &'a mut [u8] {
        &mut bytes[self.code_off as usize..self.code_off as usize + self.code_len as usize]
    }
}

/// A sample and its tuning, as an instrument or drum names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuned {
    /// Index into the bank's sample table, or [`NONE16`].
    pub sample: u16,
    /// Pitch multiplier, Q16.
    pub tuning_q16: u32,
}

/// An instrument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instrument {
    /// Keys below this use `low`.
    pub range_lo: u8,
    /// Keys above this use `high`.
    pub range_hi: u8,
    /// Release rate index.
    pub decay: u8,
    /// Envelope start (word index into the bank) or [`NONE16`].
    pub env: u16,
    /// Sample for the low range.
    pub low: Tuned,
    /// Sample for the normal range.
    pub normal: Tuned,
    /// Sample for the high range.
    pub high: Tuned,
}

impl Instrument {
    /// The sample entry for `key`.
    pub fn for_key(&self, key: u8) -> Tuned {
        if key < self.range_lo {
            self.low
        } else if key <= self.range_hi {
            self.normal
        } else {
            self.high
        }
    }
}

/// A drum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drum {
    /// Release rate index.
    pub decay: u8,
    /// Fixed pan.
    pub pan: u8,
    /// Envelope start or [`NONE16`].
    pub env: u16,
    /// Its sample.
    pub tuned: Tuned,
}

/// One cooked sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleInfo {
    /// Offset of its ADPCM data in the ADPCM region.
    pub adpcm_off: u32,
    /// Length of the original sample in samples (loop end), for notes with no
    /// written length.
    pub end_samples: u32,
    /// ADPCM blocks.
    pub blocks: u16,
    /// Cooked sample rate in Hz.
    pub rate_hz: u16,
    /// Block the hardware loop re-enters, or [`NONE16`].
    pub loop_block: u16,
}

/// A parsed bank.
#[derive(Clone, Copy, Debug)]
pub struct Bank<'a> {
    bytes: &'a [u8],
    n_fonts: u16,
    n_samples: u16,
    n_env_words: u16,
    off_fonts: u32,
    off_samples: u32,
    off_env: u32,
    off_adpcm: u32,
    adpcm_bytes: u32,
}

impl<'a> Bank<'a> {
    /// Parse `bytes`.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, FormatError> {
        if bytes.get(0..4) != Some(&BANK_MAGIC) || le16(bytes, 4) != Some(1) {
            return Err(FormatError::BadHeader);
        }
        let t = FormatError::Truncated;
        let b = Self {
            bytes,
            n_fonts: le16(bytes, 12).ok_or(t)?,
            n_samples: le16(bytes, 14).ok_or(t)?,
            n_env_words: le16(bytes, 16).ok_or(t)?,
            off_fonts: le32(bytes, 20).ok_or(t)?,
            off_samples: le32(bytes, 24).ok_or(t)?,
            off_env: le32(bytes, 28).ok_or(t)?,
            off_adpcm: le32(bytes, 32).ok_or(t)?,
            adpcm_bytes: le32(bytes, 36).ok_or(t)?,
        };
        if b.off_adpcm as usize + b.adpcm_bytes as usize > bytes.len() {
            return Err(t);
        }
        Ok(b)
    }

    /// The ADPCM region, to upload to SPU RAM.
    pub fn adpcm(&self) -> &'a [u8] {
        &self.bytes[self.off_adpcm as usize..(self.off_adpcm + self.adpcm_bytes) as usize]
    }

    /// Number of samples.
    pub fn sample_count(&self) -> u16 {
        self.n_samples
    }

    /// Index of the font whose original id is `id`.
    pub fn font_index(&self, id: u8) -> Option<u8> {
        (0..self.n_fonts).find_map(|i| {
            let at = self.off_fonts as usize + 12 * i as usize;
            (*self.bytes.get(at)? == id).then_some(i as u8)
        })
    }

    fn font_field(&self, font: u8, field: usize) -> Option<usize> {
        ((font as u16) < self.n_fonts).then_some(())?;
        Some(self.off_fonts as usize + 12 * font as usize + field)
    }

    /// Instrument `inst` of font index `font`.
    pub fn instrument(&self, font: u8, inst: u8) -> Option<Instrument> {
        let desc = self.font_field(font, 0)?;
        if inst >= *self.bytes.get(desc + 1)? {
            return None;
        }
        let at = le32(self.bytes, desc + 4)? as usize + 32 * inst as usize;
        if *self.bytes.get(at + 3)? == 0 {
            return None;
        }
        let tuned = |o: usize| -> Option<Tuned> {
            Some(Tuned {
                sample: le16(self.bytes, at + o)?,
                tuning_q16: le32(self.bytes, at + o + 4)?,
            })
        };
        Some(Instrument {
            range_lo: *self.bytes.get(at)?,
            range_hi: *self.bytes.get(at + 1)?,
            decay: *self.bytes.get(at + 2)?,
            env: le16(self.bytes, at + 4)?,
            low: tuned(8)?,
            normal: tuned(16)?,
            high: tuned(24)?,
        })
    }

    /// Drum `key` of font index `font`.
    pub fn drum(&self, font: u8, key: u8) -> Option<Drum> {
        let desc = self.font_field(font, 0)?;
        if key >= *self.bytes.get(desc + 2)? {
            return None;
        }
        let at = le32(self.bytes, desc + 8)? as usize + 16 * key as usize;
        if le16(self.bytes, at + 6)? == 0 {
            return None;
        }
        Some(Drum {
            decay: *self.bytes.get(at)?,
            pan: *self.bytes.get(at + 1)?,
            env: le16(self.bytes, at + 2)?,
            tuned: Tuned {
                sample: le16(self.bytes, at + 4)?,
                tuning_q16: le32(self.bytes, at + 8)?,
            },
        })
    }

    /// Sample `idx`.
    pub fn sample(&self, idx: u16) -> Option<SampleInfo> {
        if idx >= self.n_samples {
            return None;
        }
        let at = self.off_samples as usize + 16 * idx as usize;
        Some(SampleInfo {
            adpcm_off: le32(self.bytes, at)?,
            end_samples: le32(self.bytes, at + 4)?,
            blocks: le16(self.bytes, at + 8)?,
            rate_hz: le16(self.bytes, at + 10)?,
            loop_block: le16(self.bytes, at + 12)?,
        })
    }

    /// The resident [`Sample`] for `idx` when the ADPCM region sits at
    /// `base` in SPU RAM.
    pub fn resident(&self, idx: u16, base: u32) -> Option<Sample> {
        let s = self.sample(idx)?;
        Some(Sample::resident(
            SpuAddr::new(base + s.adpcm_off),
            s.rate_hz as u32,
            s.blocks as u32,
        ))
    }

    /// Envelope point `word` as `(delay, arg)`.
    pub fn envelope_point(&self, word: u32) -> Option<(i16, i16)> {
        if word >= self.n_env_words as u32 {
            return None;
        }
        let at = self.off_env as usize + 4 * word as usize;
        let b = self.bytes.get(at..at + 4)?;
        Some((
            i16::from_be_bytes([b[0], b[1]]),
            i16::from_be_bytes([b[2], b[3]]),
        ))
    }
}
