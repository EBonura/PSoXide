//! Reading the N64 libultra-style audio data in an OoT cartridge: the audio
//! tables, soundfonts (instrument, drum and effect definitions), the sample
//! bank and the sequences.
//!
//! The layout is a data format, documented by the community and checked
//! against the cartridge here; this module reads it and holds no code taken
//! from any decompilation. Offsets of the tables inside the `code` file are
//! the NTSC 1.2 ones.

pub mod aseq;
pub mod vadpcm;

use crate::rom::{be_u16, be_u32, Rom};
use crate::Error;

/// Virtual address of the first byte of the `code` file (NTSC 1.2).
pub const CODE_VRAM: u32 = 0x8001_16E0;
/// Soundfont table in `code`.
pub const SOUNDFONT_TABLE_VRAM: u32 = 0x8011_3DF0;
/// Per-sequence list of soundfont ids in `code`.
pub const SEQ_FONT_TABLE_VRAM: u32 = 0x8011_4060;
/// Sequence table in `code`.
pub const SEQ_TABLE_VRAM: u32 = 0x8011_4220;
/// Sample bank table in `code`.
pub const SAMPLE_BANK_TABLE_VRAM: u32 = 0x8011_4910;

/// Sequence names by id (the game's background music numbering).
pub const SEQ_NAMES: [&str; 110] = [
    "GENERAL_SFX",
    "NATURE_AMBIENCE",
    "FIELD_LOGIC",
    "FIELD_INIT",
    "FIELD_DEFAULT_1",
    "FIELD_DEFAULT_2",
    "FIELD_DEFAULT_3",
    "FIELD_DEFAULT_4",
    "FIELD_DEFAULT_5",
    "FIELD_DEFAULT_6",
    "FIELD_DEFAULT_7",
    "FIELD_DEFAULT_8",
    "FIELD_DEFAULT_9",
    "FIELD_DEFAULT_A",
    "FIELD_DEFAULT_B",
    "FIELD_ENEMY_INIT",
    "FIELD_ENEMY_1",
    "FIELD_ENEMY_2",
    "FIELD_ENEMY_3",
    "FIELD_ENEMY_4",
    "FIELD_STILL_1",
    "FIELD_STILL_2",
    "FIELD_STILL_3",
    "FIELD_STILL_4",
    "DUNGEON",
    "KAKARIKO_ADULT",
    "ENEMY",
    "BOSS",
    "INSIDE_DEKU_TREE",
    "MARKET",
    "TITLE",
    "LINK_HOUSE",
    "GAME_OVER",
    "BOSS_CLEAR",
    "ITEM_GET",
    "OPENING_GANON",
    "HEART_GET",
    "OCA_LIGHT",
    "JABU_JABU",
    "KAKARIKO_KID",
    "GREAT_FAIRY",
    "ZELDA_THEME",
    "FIRE_TEMPLE",
    "OPEN_TRE_BOX",
    "FOREST_TEMPLE",
    "COURTYARD",
    "GANON_TOWER",
    "LONLON",
    "GORON_CITY",
    "FIELD_MORNING",
    "SPIRITUAL_STONE",
    "OCA_BOLERO",
    "OCA_MINUET",
    "OCA_SERENADE",
    "OCA_REQUIEM",
    "OCA_NOCTURNE",
    "MINI_BOSS",
    "SMALL_ITEM_GET",
    "TEMPLE_OF_TIME",
    "EVENT_CLEAR",
    "KOKIRI",
    "OCA_FAIRY_GET",
    "SARIA_THEME",
    "SPIRIT_TEMPLE",
    "HORSE",
    "HORSE_GOAL",
    "INGO",
    "MEDALLION_GET",
    "OCA_SARIA",
    "OCA_EPONA",
    "OCA_ZELDA",
    "OCA_SUNS",
    "OCA_TIME",
    "OCA_STORM",
    "NAVI_OPENING",
    "DEKU_TREE_CS",
    "WINDMILL",
    "HYRULE_CS",
    "MINI_GAME",
    "SHEIK",
    "ZORA_DOMAIN",
    "APPEAR",
    "ADULT_LINK",
    "MASTER_SWORD",
    "INTRO_GANON",
    "SHOP",
    "CHAMBER_OF_SAGES",
    "FILE_SELECT",
    "ICE_CAVERN",
    "DOOR_OF_TIME",
    "OWL",
    "SHADOW_TEMPLE",
    "WATER_TEMPLE",
    "BRIDGE_TO_GANONS",
    "OCARINA_OF_TIME",
    "GERUDO_VALLEY",
    "POTION_SHOP",
    "KOTAKE_KOUME",
    "ESCAPE",
    "UNDERGROUND",
    "GANONDORF_BATTLE",
    "GANON_BATTLE",
    "END_DEMO",
    "STAFF_1",
    "STAFF_2",
    "STAFF_3",
    "STAFF_4",
    "FIRE_BOSS",
    "TIMED_MINI_GAME",
    "CUTSCENE_EFFECTS",
];

/// Resolve a sequence name (case-insensitive) or decimal id to an id.
pub fn resolve_seq(name: &str) -> Option<usize> {
    let s = name.trim();
    if let Ok(n) = s.parse::<usize>() {
        return (n < SEQ_NAMES.len()).then_some(n);
    }
    let up = s.to_ascii_uppercase();
    SEQ_NAMES.iter().position(|n| *n == up)
}

/// One row of an audio table (16 bytes in the cartridge).
#[derive(Clone, Copy, Debug)]
pub struct TableEntry {
    /// Offset of the data inside its file; for an alias, the index it
    /// refers to.
    pub rom_addr: u32,
    /// Size in bytes; zero marks an alias.
    pub size: u32,
    /// Raw `shortData1`.
    pub s1: u16,
    /// Raw `shortData2`.
    pub s2: u16,
    /// Raw `shortData3`.
    pub s3: u16,
}

/// A parsed audio table.
#[derive(Clone, Debug)]
pub struct AudioTable {
    /// The rows.
    pub entries: Vec<TableEntry>,
}

impl AudioTable {
    fn parse(code: &[u8], vram: u32) -> Result<Self, Error> {
        let off = (vram - CODE_VRAM) as usize;
        let n = be_u16(code, off)? as usize;
        if n == 0 || n > 1024 {
            return Err(Error::Format(format!(
                "audio table at {vram:#x}: {n} entries"
            )));
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let at = off + 0x10 + i * 0x10;
            entries.push(TableEntry {
                rom_addr: be_u32(code, at)?,
                size: be_u32(code, at + 4)?,
                s1: be_u16(code, at + 10)?,
                s2: be_u16(code, at + 12)?,
                s3: be_u16(code, at + 14)?,
            });
        }
        Ok(Self { entries })
    }

    /// The row `id` names after following one alias.
    pub fn resolve(&self, id: usize) -> Option<&TableEntry> {
        let e = self.entries.get(id)?;
        if e.size == 0 {
            self.entries.get(e.rom_addr as usize)
        } else {
            Some(e)
        }
    }
}

/// Everything needed from the cartridge's audio files.
pub struct AudioData {
    /// The `code` file (holds the tables).
    pub code: Vec<u8>,
    /// All sequences.
    pub audioseq: Vec<u8>,
    /// All soundfonts.
    pub audiobank: Vec<u8>,
    /// All sample data.
    pub audiotable: Vec<u8>,
    /// Sequence table.
    pub seq_table: AudioTable,
    /// Soundfont table.
    pub font_table: AudioTable,
    /// Sample bank table.
    pub bank_table: AudioTable,
}

impl AudioData {
    /// Load the audio files and tables from `rom`.
    pub fn load(rom: &Rom) -> Result<Self, Error> {
        let code = rom.file("code")?;
        let audioseq = rom.file("Audioseq")?;
        let audiobank = rom.file("Audiobank")?;
        let audiotable = rom.file("Audiotable")?;
        let seq_table = AudioTable::parse(&code, SEQ_TABLE_VRAM)?;
        let font_table = AudioTable::parse(&code, SOUNDFONT_TABLE_VRAM)?;
        let bank_table = AudioTable::parse(&code, SAMPLE_BANK_TABLE_VRAM)?;
        Ok(Self {
            code,
            audioseq,
            audiobank,
            audiotable,
            seq_table,
            font_table,
            bank_table,
        })
    }

    /// The bytes of sequence `id`.
    pub fn sequence(&self, id: usize) -> Result<&[u8], Error> {
        let e = self
            .seq_table
            .resolve(id)
            .ok_or_else(|| Error::Missing(format!("sequence {id}")))?;
        let s = e.rom_addr as usize;
        self.audioseq
            .get(s..s + e.size as usize)
            .ok_or(Error::Truncated(s))
    }

    /// Soundfont ids sequence `id` lists, in table order.
    pub fn fonts_of_sequence(&self, id: usize) -> Result<Vec<u8>, Error> {
        let base = (SEQ_FONT_TABLE_VRAM - CODE_VRAM) as usize;
        let rel = be_u16(&self.code, base + 2 * id)? as usize;
        let at = base + rel;
        let n = *self.code.get(at).ok_or(Error::Truncated(at))? as usize;
        self.code
            .get(at + 1..at + 1 + n)
            .map(<[u8]>::to_vec)
            .ok_or(Error::Truncated(at))
    }

    /// Parse soundfont `id`.
    pub fn font(&self, id: usize) -> Result<Font, Error> {
        Font::parse(self, id)
    }
}

/// An envelope point: `delay` in envelope units, `arg` the target level
/// (0..=32767) or, for the special delays, the jump target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvPoint {
    /// Signed delay; `0` ends, `-1` holds, `-2` jumps to `arg`, `-3` restarts.
    pub delay: i16,
    /// Target level or jump index.
    pub arg: i16,
}

/// A sample's loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loop {
    /// First looped sample.
    pub start: u32,
    /// Sample after the loop.
    pub end: u32,
    /// Repeats; `-1` is forever, `0` plays once.
    pub count: i32,
}

/// One sample of a soundfont.
#[derive(Clone, Debug)]
pub struct Sample {
    /// Offset of the VADPCM data in the audiotable file.
    pub table_off: u32,
    /// Size of the VADPCM data in bytes.
    pub size: u32,
    /// Its loop, when it has one.
    pub looped: Option<Loop>,
    /// Sample count the sound ends at: the loop header's end, else the
    /// decoded length.
    pub end_samples: u32,
    /// Offset of its codebook inside the soundfont.
    pub book_off: u32,
}

impl Sample {
    /// Number of PCM samples the VADPCM data decodes to.
    pub fn pcm_len(&self) -> usize {
        (self.size as usize / 9) * 16
    }
}

/// A sample with its tuning.
#[derive(Clone, Debug)]
pub struct Tuned {
    /// Index into [`Font::samples`].
    pub sample: usize,
    /// Pitch multiplier relative to the sample's rate.
    pub tuning: f32,
}

/// An instrument: up to three samples by key range.
#[derive(Clone, Debug)]
pub struct Instrument {
    /// Highest key of the low sample's range (exclusive of the normal one).
    pub range_lo: u8,
    /// Highest key of the normal range.
    pub range_hi: u8,
    /// Release rate index.
    pub decay_index: u8,
    /// Envelope points.
    pub env: Vec<EnvPoint>,
    /// Sample below `range_lo`.
    pub low: Option<Tuned>,
    /// Sample for the normal range.
    pub normal: Option<Tuned>,
    /// Sample above `range_hi`.
    pub high: Option<Tuned>,
}

/// A drum: one sample, a fixed pan.
#[derive(Clone, Debug)]
pub struct Drum {
    /// Release rate index.
    pub decay_index: u8,
    /// Pan, 0..=127.
    pub pan: u8,
    /// The sample.
    pub tuned: Tuned,
    /// Envelope points.
    pub env: Vec<EnvPoint>,
}

/// A parsed soundfont.
#[derive(Clone, Debug)]
pub struct Font {
    /// Table index.
    pub id: usize,
    /// Sample bank 1 and 2 ids (0xFF = none).
    pub banks: [u8; 2],
    /// Instruments by program number.
    pub instruments: Vec<Option<Instrument>>,
    /// Drums by key.
    pub drums: Vec<Option<Drum>>,
    /// Effects by index.
    pub sfx: Vec<Option<Tuned>>,
    /// Every distinct sample.
    pub samples: Vec<Sample>,
    /// The raw soundfont bytes (codebooks and loop states are read from here).
    pub raw: Vec<u8>,
}

struct Builder<'a> {
    data: &'a AudioData,
    raw: &'a [u8],
    banks: [u8; 2],
    samples: Vec<Sample>,
    by_off: Vec<(u32, usize)>,
}

impl Builder<'_> {
    fn sample(&mut self, off: u32) -> Result<usize, Error> {
        if let Some(&(_, i)) = self.by_off.iter().find(|(o, _)| *o == off) {
            return Ok(i);
        }
        let at = off as usize;
        let w0 = be_u32(self.raw, at)?;
        let medium = (w0 >> 26) & 3;
        let size = w0 & 0x00FF_FFFF;
        let addr = be_u32(self.raw, at + 4)?;
        let loop_off = be_u32(self.raw, at + 8)? as usize;
        let book_off = be_u32(self.raw, at + 12)?;
        let bank_id = *self
            .banks
            .get(medium as usize)
            .ok_or_else(|| Error::Format(format!("sample medium {medium}")))?;
        let bank = self
            .data
            .bank_table
            .resolve(bank_id as usize)
            .ok_or_else(|| Error::Missing(format!("sample bank {bank_id}")))?;
        let mut end_samples = (size / 9) * 16;
        if loop_off != 0 && loop_off + 16 <= self.raw.len() {
            end_samples = be_u32(self.raw, loop_off + 4).unwrap_or(end_samples);
        }
        let looped = if loop_off != 0 && loop_off + 16 <= self.raw.len() {
            let count = be_u32(self.raw, loop_off + 8)? as i32;
            (count != 0).then(|| Loop {
                start: be_u32(self.raw, loop_off).unwrap_or(0),
                end: be_u32(self.raw, loop_off + 4).unwrap_or(0),
                count,
            })
        } else {
            None
        };
        let i = self.samples.len();
        self.samples.push(Sample {
            table_off: bank.rom_addr + addr,
            size,
            looped,
            end_samples,
            book_off,
        });
        self.by_off.push((off, i));
        Ok(i)
    }

    fn tuned(&mut self, at: usize) -> Result<Option<Tuned>, Error> {
        let sample_off = be_u32(self.raw, at)?;
        let tuning = f32::from_bits(be_u32(self.raw, at + 4)?);
        if sample_off == 0 {
            return Ok(None);
        }
        Ok(Some(Tuned {
            sample: self.sample(sample_off)?,
            tuning,
        }))
    }

    fn envelope(&self, off: u32) -> Vec<EnvPoint> {
        let mut out = Vec::new();
        let mut at = off as usize;
        for _ in 0..32 {
            let (Ok(d), Ok(a)) = (be_u16(self.raw, at), be_u16(self.raw, at + 2)) else {
                break;
            };
            let p = EnvPoint {
                delay: d as i16,
                arg: a as i16,
            };
            out.push(p);
            at += 4;
            if matches!(p.delay, 0 | -1 | -3) {
                break;
            }
            // A jump can only be followed by data already seen.
            if p.delay == -2 {
                break;
            }
        }
        out
    }
}

impl Font {
    fn parse(data: &AudioData, id: usize) -> Result<Font, Error> {
        let e = data
            .font_table
            .entries
            .get(id)
            .ok_or_else(|| Error::Missing(format!("soundfont {id}")))?;
        let start = e.rom_addr as usize;
        let raw = data
            .audiobank
            .get(start..start + e.size as usize)
            .ok_or(Error::Truncated(start))?
            .to_vec();
        let banks = [(e.s1 >> 8) as u8, (e.s1 & 0xFF) as u8];
        let n_inst = ((e.s2 >> 8) as usize).min(126);
        let n_drum = (e.s2 & 0xFF) as usize;
        let n_sfx = e.s3 as usize;
        let mut b = Builder {
            data,
            raw: &raw,
            banks,
            samples: Vec::new(),
            by_off: Vec::new(),
        };

        let mut drums = Vec::new();
        let drum_list = be_u32(&raw, 0)? as usize;
        if drum_list != 0 {
            for i in 0..n_drum {
                let off = be_u32(&raw, drum_list + 4 * i)? as usize;
                if off == 0 {
                    drums.push(None);
                    continue;
                }
                let tuned = b.tuned(off + 4)?;
                let env = b.envelope(be_u32(&raw, off + 12)?);
                drums.push(tuned.map(|tuned| Drum {
                    decay_index: raw[off],
                    pan: raw[off + 1],
                    tuned,
                    env,
                }));
            }
        }

        let mut sfx = Vec::new();
        let sfx_list = be_u32(&raw, 4)? as usize;
        if sfx_list != 0 {
            for i in 0..n_sfx {
                sfx.push(b.tuned(sfx_list + 8 * i)?);
            }
        }

        let mut instruments = Vec::new();
        for i in 0..n_inst {
            let off = be_u32(&raw, 8 + 4 * i)? as usize;
            if off == 0 {
                instruments.push(None);
                continue;
            }
            let lo = raw[off + 1];
            let hi = raw[off + 2];
            let env = b.envelope(be_u32(&raw, off + 4)?);
            let low = if lo != 0 { b.tuned(off + 8)? } else { None };
            let normal = b.tuned(off + 16)?;
            let high = if hi != 0x7F { b.tuned(off + 24)? } else { None };
            instruments.push(Some(Instrument {
                range_lo: lo,
                range_hi: hi,
                decay_index: raw[off + 3],
                env,
                low,
                normal,
                high,
            }));
        }
        let samples = b.samples;
        Ok(Font {
            id,
            banks,
            instruments,
            drums,
            sfx,
            samples,
            raw,
        })
    }

    /// Distinct audiotable offsets of every sample in the font.
    pub fn sample_keys(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.samples.iter().map(|s| (s.table_off, s.size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_names_resolve_by_name_and_id() {
        assert_eq!(resolve_seq("kokiri"), Some(60));
        assert_eq!(resolve_seq("60"), Some(60));
        assert_eq!(resolve_seq("OCA_SUNS"), Some(71));
        assert_eq!(resolve_seq("nope"), None);
        assert_eq!(resolve_seq("999"), None);
        assert_eq!(SEQ_NAMES.len(), 110);
    }

    #[test]
    fn an_alias_entry_resolves_to_the_row_it_names() {
        let t = AudioTable {
            entries: vec![
                TableEntry {
                    rom_addr: 100,
                    size: 50,
                    s1: 0,
                    s2: 0,
                    s3: 0,
                },
                TableEntry {
                    rom_addr: 0,
                    size: 0,
                    s1: 0,
                    s2: 0,
                    s3: 0,
                },
            ],
        };
        assert_eq!(t.resolve(1).unwrap().rom_addr, 100);
        assert_eq!(t.resolve(0).unwrap().size, 50);
        assert!(t.resolve(2).is_none());
    }
}
