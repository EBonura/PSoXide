//! Sound effects as the original plays them.
//!
//! A game that sequences its music with a bytecode engine usually sequences its sound
//! effects with the same engine: one long sequence (`GENERAL_SFX` in Ocarina of Time) with
//! a channel per concurrent sound, and per sound a small script, picked by number, that
//! chooses the instrument, the key, the velocity and the pan. The number a game passes to
//! its sound call is `bank << 12 | index` (with the bit `0x800` set); the bank picks the
//! channel group, the index the script.
//!
//! The scripts are data, so the way to know what an effect sounds like is to run them.
//! [`EffectProbe`] runs the cartridge's own effects sequence on the host with a bank that
//! describes every sample but converts none (so building it takes microseconds), starts the
//! sound the way the game's audio code does, and records what the sequencer asks the SPU to
//! play: which sample, at what pitch, at what level on each side, and for how long.
//! [`cook_effects`] then cooks exactly those samples as `.psau` one-shots or loops.

use crate::cook::{cook_songs, Options, SampleSource, NATIVE_RATE};
use crate::n64::AudioData;
use crate::Error;
use psx_audio_cook::wav::Wav;
use psx_audio_cook::{cook, CookOptions, Looping};
use psx_seq::format::Bank;
use psx_seq::Sequencer;
use psx_sfx::hw::Hw;
use psx_sfx::Sample;
use std::collections::BTreeMap;

/// The effects sequence's number in the sequence table.
const GENERAL_SFX: usize = 0;
/// Channel groups, by bank: the first channel of each (a sound occupies one channel).
const BANK_CHANNEL: [usize; 7] = [0, 3, 5, 8, 11, 13, 14];
/// Entries in each bank's script table.
const BANK_ENTRIES: [usize; 7] = [224, 80, 250, 499, 72, 8, 181];
/// Updates run before a sound is started, for the sequence to set its channels up.
const WARM_UP: u32 = 40;
/// Updates a sound is followed for (about 0.7 s).
const FOLLOW: u32 = 126;

/// One voice of an effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EffectVoice {
    /// Index into [`EffectProbe::sources`].
    pub source: usize,
    /// Pitch multiplier of the cooked sample rate, `4096` = as cooked.
    pub pitch_q12: u16,
    /// Left level the sequencer reached (an SPU volume).
    pub left: i16,
    /// Right level the sequencer reached.
    pub right: i16,
    /// The sample loops.
    pub looped: bool,
    /// Updates (at 180 Hz) from the start of the sound until the sequencer released the
    /// voice, or `None` if it never did within the time followed.
    pub released_after: Option<u32>,
    /// Updates from the start of the sound until the voice was keyed on.
    pub starts_after: u32,
}

/// Runs the cartridge's effects sequence on the host.
pub struct EffectProbe {
    bank: Vec<u8>,
    song: Vec<u8>,
    sources: Vec<SampleSource>,
}

/// One voice the sequencer keyed on, as the log saw it.
struct Rec {
    voice: u8,
    addr: u32,
    start: u32,
    pitch: u16,
    released: Option<u32>,
    left: i16,
    right: i16,
}

/// An [`Hw`] that follows each voice from key-on to key-off.
#[derive(Default)]
struct Log {
    now: u32,
    configured: BTreeMap<u8, (u32, u16)>,
    /// The latest level written to each voice (a note's first level is written before its key-on).
    levels: BTreeMap<u8, (i16, i16)>,
    recs: Vec<Rec>,
}

impl Log {
    /// The record of the note now sounding on `voice`.
    fn live(&mut self, voice: u8) -> Option<&mut Rec> {
        self.recs
            .iter_mut()
            .rev()
            .find(|r| r.voice == voice && r.released.is_none())
    }
}

impl Hw for Log {
    fn configure(&mut self, voice: u8, sample: &Sample, _looped: bool, pitch_q12: u16) {
        self.configured
            .insert(voice, (sample.addr().byte_offset(), pitch_q12));
    }

    fn set_volume(&mut self, voice: u8, left: i16, right: i16) {
        self.levels.insert(voice, (left, right));
        if let Some(r) = self.live(voice) {
            r.left = r.left.max(left);
            r.right = r.right.max(right);
        }
    }

    fn set_pitch(&mut self, _voice: u8, _pitch_q12: u16) {}

    fn key_on(&mut self, mask: u32) {
        for voice in 0..24u8 {
            if mask & (1 << voice) != 0 {
                if let Some(&(addr, pitch)) = self.configured.get(&voice) {
                    // a voice stolen by a new note ends its old one
                    let now = self.now;
                    if let Some(old) = self.live(voice) {
                        old.released = Some(now);
                    }
                    let (left, right) = self.levels.get(&voice).copied().unwrap_or((0, 0));
                    self.recs.push(Rec {
                        voice,
                        addr,
                        start: now,
                        pitch,
                        released: None,
                        left,
                        right,
                    });
                }
            }
        }
    }

    fn key_off(&mut self, mask: u32) {
        let now = self.now;
        for voice in 0..24u8 {
            if mask & (1 << voice) != 0 {
                if let Some(r) = self.live(voice) {
                    r.released = Some(now);
                }
            }
        }
    }
}

impl EffectProbe {
    /// Describe the effects sequence's soundfonts and set the probe up.
    ///
    /// # Errors
    /// If the cartridge's effects sequence or its soundfonts cannot be read.
    pub fn new(data: &AudioData) -> Result<Self, Error> {
        let options = Options {
            all_instruments: true,
            analysis_only: true,
            ..Options::default()
        };
        let set = cook_songs(data, &[GENERAL_SFX], &options)?;
        let song = set
            .songs
            .into_iter()
            .next()
            .ok_or_else(|| Error::Missing("effects sequence".into()))?;
        Ok(Self {
            bank: set.bank,
            song,
            sources: set.sources,
        })
    }

    /// The cartridge samples the effects can use, in the order [`EffectVoice::source`] indexes.
    #[must_use]
    pub fn sources(&self) -> &[SampleSource] {
        &self.sources
    }

    /// Start the sound `id` and report what the sequencer plays for it.
    ///
    /// # Errors
    /// If `id` names a bank or an entry that does not exist.
    pub fn play(&self, id: u16) -> Result<Vec<EffectVoice>, Error> {
        let bank_no = usize::from((id >> 12) & 0xf);
        let index = usize::from(id & 0x1ff);
        if bank_no >= BANK_CHANNEL.len() || index >= BANK_ENTRIES[bank_no] {
            return Err(Error::Missing(format!("sound effect {id:#06x}")));
        }
        let bank =
            Bank::parse(&self.bank).map_err(|e| Error::Format(format!("effects bank: {e:?}")))?;
        let mut song = self.song.clone();
        let mut seq: Sequencer<'_, 1, 24> = Sequencer::new((1 << 24) - 1);
        // Addresses only identify samples here; nothing is played.
        seq.set_bank(bank, 0x1010);
        seq.start(0, &mut song)
            .map_err(|e| Error::Format(format!("effects sequence: {e:?}")))?;
        let mut log = Log::default();
        for _ in 0..WARM_UP {
            seq.update(&mut log);
        }
        log.recs.clear();
        log.configured.clear();
        log.levels.clear();
        let channel = BANK_CHANNEL[bank_no];
        // The game's audio code starts a sound by writing the channel's IO ports: port 0 raises
        // the request, port 4 carries the low byte of the index and port 5 its ninth bit.
        seq.set_channel_io(0, channel, 0, 1);
        seq.set_channel_io(0, channel, 4, index as u8);
        seq.set_channel_io(0, channel, 5, (index >> 8) as u8);
        for update in 0..FOLLOW {
            log.now = update;
            seq.update(&mut log);
        }
        let mut out = Vec::new();
        for r in &log.recs {
            let off = r.addr - 0x1010;
            let Some(source) = (0..bank.sample_count())
                .find(|&i| bank.sample(i).is_some_and(|s| s.adpcm_off == off))
            else {
                continue;
            };
            let looped = bank.sample(source).is_some_and(|s| s.loop_block != 0xffff);
            out.push(EffectVoice {
                source: usize::from(source),
                pitch_q12: r.pitch,
                left: r.left,
                right: r.right,
                looped,
                released_after: r.released.map(|t| t - r.start),
                starts_after: r.start,
            });
        }
        Ok(out)
    }
}

/// An effect, ready for the game.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CookedEffectSet {
    /// One `.psau` per distinct sample used.
    pub samples: Vec<Vec<u8>>,
    /// For each requested id, in order: its voices, `source` now indexing `samples`.
    pub effects: Vec<(u16, Vec<EffectVoice>)>,
}

/// Cook the effects `ids` name.
///
/// # Errors
/// If an id is not an effect, or a sample cannot be read.
pub fn cook_effects(data: &AudioData, ids: &[u16]) -> Result<CookedEffectSet, Error> {
    let probe = EffectProbe::new(data)?;
    let mut samples: Vec<Vec<u8>> = Vec::new();
    let mut slot: BTreeMap<usize, usize> = BTreeMap::new();
    let mut effects = Vec::new();
    for &id in ids {
        let mut voices = probe.play(id)?;
        for v in &mut voices {
            let at = match slot.get(&v.source) {
                Some(&at) => at,
                None => {
                    samples.push(cook_source(data, probe.sources[v.source])?);
                    slot.insert(v.source, samples.len() - 1);
                    samples.len() - 1
                }
            };
            v.source = at;
        }
        effects.push((id, voices));
    }
    Ok(CookedEffectSet { samples, effects })
}

/// Cook one cartridge sample as a `.psau` at the cartridge's own rate.
fn cook_source(data: &AudioData, src: SampleSource) -> Result<Vec<u8>, Error> {
    let font = data.font(src.font)?;
    let sample = font
        .samples
        .iter()
        .find(|s| s.table_off == src.table_off)
        .ok_or_else(|| {
            Error::Missing(format!(
                "sample {:#x} of soundfont {}",
                src.table_off, src.font
            ))
        })?;
    let pcm = crate::cook::decode_sample(data, &font, sample)?;
    let wav = Wav {
        rate: NATIVE_RATE,
        samples: pcm.iter().map(|&v| f64::from(v)).collect(),
        loop_start: sample.looped.map(|l| l.start as usize),
        loop_end: sample.looped.map(|l| (l.end as usize).min(pcm.len())),
        bits: 16,
    };
    let mut opts = CookOptions::one_shot(NATIVE_RATE);
    opts.normalize_peak = None;
    opts.looping = if sample.looped.is_some() {
        Looping::Source
    } else {
        Looping::None
    };
    let c = cook(&wav, &opts);
    Ok(psx_audio_cook::psau(NATIVE_RATE, c.pcm.len(), &c.adpcm))
}

/// Output rate of [`render_effect`].
pub const OUT_RATE: u32 = 44_100;

/// Mix one cooked effect to stereo at [`OUT_RATE`], for listening to it and for a gate's hash:
/// each voice plays its sample from the start at its pitch and its levels (linear
/// interpolation, no envelope), looped samples until the sequencer released them.
///
/// # Errors
/// If a sample's `.psau` is malformed.
pub fn render_effect(set: &CookedEffectSet, id: u16, seconds: f64) -> Result<Vec<i16>, Error> {
    let Some((_, voices)) = set.effects.iter().find(|(i, _)| *i == id) else {
        return Err(Error::Missing(format!("effect {id:#06x}")));
    };
    let frames = (seconds * f64::from(OUT_RATE)) as usize;
    let mut out = vec![0i32; frames * 2];
    for v in voices {
        let psau = &set.samples[v.source];
        // 12-byte container header, 20-byte audio header, then the blocks.
        let adpcm = psau
            .get(32..)
            .ok_or_else(|| Error::Format("short psau".into()))?;
        let pcm = psx_audio_cook::adpcm::decode(adpcm);
        let loop_block = adpcm
            .chunks(16)
            .position(|b| b.get(1).is_some_and(|f| f & 4 != 0));
        let step = f64::from(NATIVE_RATE) * f64::from(v.pitch_q12) / 4096.0 / f64::from(OUT_RATE);
        let start = (f64::from(v.starts_after) / 180.0 * f64::from(OUT_RATE)) as usize;
        let end = v
            .released_after
            .map(|r| start + (f64::from(r) / 180.0 * f64::from(OUT_RATE)) as usize);
        let mut pos = 0.0f64;
        for n in start..frames {
            if end.is_some_and(|e| n >= e) && loop_block.is_some() {
                break;
            }
            let i = pos as usize;
            let frac = pos - i as f64;
            let at = |k: usize| -> f64 {
                if k < pcm.len() {
                    f64::from(pcm[k])
                } else if let Some(lb) = loop_block {
                    let ls = lb * 28;
                    let span = (pcm.len() - ls).max(1);
                    f64::from(pcm[ls + (k - pcm.len()) % span])
                } else {
                    0.0
                }
            };
            if i >= pcm.len() && loop_block.is_none() {
                break;
            }
            let s = at(i) * (1.0 - frac) + at(i + 1) * frac;
            out[n * 2] += (s * f64::from(v.left) / 16384.0) as i32;
            out[n * 2 + 1] += (s * f64::from(v.right) / 16384.0) as i32;
            pos += step;
        }
    }
    Ok(out
        .into_iter()
        .map(|x| x.clamp(-32768, 32767) as i16)
        .collect())
}
