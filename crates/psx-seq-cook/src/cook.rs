//! From a cartridge to the `psx-seq` containers: pick the instruments a
//! song uses, convert their samples to SPU-ADPCM, and write the song and the
//! bank.

use crate::n64::aseq::scan;
use crate::n64::vadpcm::{self, Book};
use crate::n64::{AudioData, EnvPoint, Font, Sample, Tuned};
use crate::Error;
use psx_audio_cook::wav::Wav;
use psx_audio_cook::{cook, CookOptions, Looping};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Playback rate of the original sample data: every N64 sample is a 32 kHz
/// signal whose tuning says how far from that it was recorded.
pub const NATIVE_RATE: u32 = 32_000;

/// Settings for [`cook_song`].
#[derive(Clone, Debug)]
pub struct Options {
    /// Cooked sample rate in Hz (32000 keeps the original).
    pub rate: u32,
    /// Include every instrument of the song's soundfonts, not just the ones
    /// the static scan found.
    pub all_instruments: bool,
    /// Voices the song may need (written to the header).
    pub peak_voices: u8,
    /// Alignment of the ADPCM region inside the bank, in bytes (a power of
    /// two, at least 16). A game that streams the bank from disc in whole
    /// sectors passes 2048 so the samples start on a sector boundary and
    /// the tables never share a sector with them.
    pub adpcm_align: usize,
    /// Describe the samples without converting them: the bank has the right
    /// tables (so a host sequencer makes every decision the console would)
    /// but its ADPCM is silence. Cooking a whole soundfont this way takes
    /// milliseconds instead of seconds.
    pub analysis_only: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rate: NATIVE_RATE,
            all_instruments: false,
            peak_voices: 16,
            adpcm_align: 16,
            analysis_only: false,
        }
    }
}

/// The cooked pair and what was done.
pub struct Cooked {
    /// A `PSQ1` song.
    pub song: Vec<u8>,
    /// A `PSQB` bank.
    pub bank: Vec<u8>,
    /// Human-readable cook report.
    pub report: String,
}

/// Several songs cooked against one shared bank.
#[non_exhaustive]
pub struct CookedSet {
    /// One `PSQ1` song per requested sequence, in request order.
    pub songs: Vec<Vec<u8>>,
    /// The `PSQB` bank all of them play from.
    pub bank: Vec<u8>,
    /// Where each of the bank's samples came from, in bank order.
    pub sources: Vec<SampleSource>,
    /// Human-readable cook report.
    pub report: String,
}

/// The cartridge sample behind one of a bank's samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SampleSource {
    /// Soundfont that defined it.
    pub font: usize,
    /// Offset of its VADPCM data in the audiotable file (its identity).
    pub table_off: u32,
    /// Samples it decodes to.
    pub pcm_len: usize,
    /// The original loops it.
    pub looped: bool,
}

/// A sound effect cooked as a one-shot `.psau` sample.
#[non_exhaustive]
pub struct CookedEffect {
    /// The `.psau` container (mono SPU-ADPCM, one-shot).
    pub psau: Vec<u8>,
    /// Playback rate written into the container: the sample's native rate
    /// times the effect's tuning, so playing it at pitch 1.0 sounds as the
    /// original did.
    pub rate_hz: u32,
    /// Length in ADPCM blocks.
    pub blocks: usize,
    /// The original loops this sample (a one-shot cook plays it once).
    pub looped: bool,
}

struct CookedSample {
    adpcm: Vec<u8>,
    blocks: u16,
    rate: u16,
    loop_block: u16,
    end_samples: u32,
    stretch_q16: u32,
}

fn le16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn le32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

pub(crate) fn decode_sample(data: &AudioData, font: &Font, s: &Sample) -> Result<Vec<i16>, Error> {
    decode(data, font, s)
}

fn decode(data: &AudioData, font: &Font, s: &Sample) -> Result<Vec<i16>, Error> {
    let start = s.table_off as usize;
    let raw = data
        .audiotable
        .get(start..start + s.size as usize)
        .ok_or(Error::Truncated(start))?;
    let book = Book::parse(&font.raw, s.book_off as usize)?;
    Ok(vadpcm::decode(raw, &book))
}

/// What [`cook_sample`] would produce, without converting anything: the same
/// block count and loop block, over a single block of silence.
fn describe_sample(s: &Sample, rate: u32) -> CookedSample {
    let pcm_len = s.pcm_len();
    let out_len = pcm_len * rate as usize / NATIVE_RATE as usize;
    let blocks = out_len.div_ceil(28).max(1);
    let loop_block = s
        .looped
        .map(|l| (l.start as usize * rate as usize / NATIVE_RATE as usize / 28) as u16);
    // One block stands in for the data: every sample still gets an address of its own.
    let adpcm = vec![0u8; 16];
    CookedSample {
        adpcm,
        blocks: blocks as u16,
        rate: rate as u16,
        loop_block: loop_block.unwrap_or(0xffff),
        end_samples: s.end_samples,
        stretch_q16: 65536,
    }
}

fn cook_sample(pcm: &[i16], s: &Sample, rate: u32) -> CookedSample {
    let looped = s.looped;
    let wav = Wav {
        rate: NATIVE_RATE,
        samples: pcm.iter().map(|&v| v as f64).collect(),
        loop_start: looped.map(|l| l.start as usize),
        loop_end: looped.map(|l| (l.end as usize).min(pcm.len())),
        bits: 16,
    };
    let mut opts = CookOptions::one_shot(rate);
    opts.normalize_peak = None;
    opts.looping = if looped.is_some() {
        Looping::Source
    } else {
        Looping::None
    };
    let c = cook(&wav, &opts);
    let mut stretch = 1.0f64;
    if let (Some(lb), Some(l)) = (c.loop_block, looped) {
        let body = c.pcm.len() - lb * 28;
        let ideal =
            (l.end.min(pcm.len() as u32) - l.start) as f64 * rate as f64 / NATIVE_RATE as f64;
        if ideal > 0.0 {
            stretch = body as f64 / ideal;
        }
    }
    let mut adpcm = c.adpcm.clone();
    let blocks = (adpcm.len() / 16) as u16;
    if c.loop_block.is_none() {
        adpcm.extend_from_slice(&psx_sfx_parking_tail());
    }
    CookedSample {
        adpcm,
        blocks,
        rate: rate as u16,
        loop_block: c.loop_block.map_or(0xffff, |b| b as u16),
        end_samples: s.end_samples,
        stretch_q16: (stretch * 65536.0).round() as u32,
    }
}

/// The silent block the SPU parks on after a one-shot (mirrors
/// `psx_sfx::PARKING_TAIL`; the cook does not depend on the device crates).
fn psx_sfx_parking_tail() -> [u8; 16] {
    [0x00, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

struct EnvPool {
    words: Vec<EnvPoint>,
    seen: Vec<(Vec<EnvPoint>, u16)>,
}

impl EnvPool {
    fn add(&mut self, env: &[EnvPoint]) -> u16 {
        if env.is_empty() {
            return 0xffff;
        }
        if let Some((_, at)) = self.seen.iter().find(|(e, _)| e == env) {
            return *at;
        }
        let at = self.words.len() as u16;
        self.words.extend_from_slice(env);
        self.seen.push((env.to_vec(), at));
        at
    }
}

/// Cook sequence `seq_id`.
pub fn cook_song(data: &AudioData, seq_id: usize, opts: &Options) -> Result<Cooked, Error> {
    let set = cook_songs(data, &[seq_id], opts)?;
    Ok(Cooked {
        song: set.songs.into_iter().next().unwrap_or_default(),
        bank: set.bank,
        report: set.report,
    })
}

/// One requested song: its bytecode, scan and soundfont list.
struct SongPlan {
    id: usize,
    code: Vec<u8>,
    scan: crate::n64::aseq::Scan,
    font_ids: Vec<u8>,
}

/// Cook several sequences against one bank: the union of the instruments,
/// drums and samples they use, each sample once. A scene that changes its
/// song without leaving its area keeps this bank in SPU RAM.
///
/// # Errors
/// If a sequence or one of its soundfonts is missing or malformed.
pub fn cook_songs(data: &AudioData, seq_ids: &[usize], opts: &Options) -> Result<CookedSet, Error> {
    let mut plans: Vec<SongPlan> = Vec::new();
    for &seq_id in seq_ids {
        let code = data.sequence(seq_id)?.to_vec();
        let scan = scan(&code);
        let font_ids = data.fonts_of_sequence(seq_id)?;
        let n = font_ids.len();
        if n == 0 || n > 8 {
            return Err(Error::Format(format!("sequence {seq_id} lists {n} fonts")));
        }
        plans.push(SongPlan {
            id: seq_id,
            code,
            scan,
            font_ids,
        });
    }

    let mut fonts: Vec<Font> = Vec::new();
    for p in &plans {
        for &id in &p.font_ids {
            if !fonts.iter().any(|f| f.id == id as usize) {
                fonts.push(data.font(id as usize)?);
            }
        }
    }

    // which instruments and drums to carry, over all the songs
    let mut want_inst: BTreeMap<(usize, u8), ()> = BTreeMap::new();
    let mut want_drum: BTreeMap<(usize, u8), ()> = BTreeMap::new();
    let mut drums_unknown = false;
    for p in &plans {
        let n = p.font_ids.len();
        let slot_font = |slot: u8| -> Option<u8> {
            ((slot as usize) < n).then(|| p.font_ids[n - 1 - slot as usize])
        };
        for &(slot, inst) in &p.scan.instruments {
            if let Some(f) = slot_font(slot) {
                want_inst.insert((f as usize, inst), ());
            }
        }
        for &(slot, key) in &p.scan.drum_keys {
            if let Some(f) = slot_font(slot) {
                want_drum.insert((f as usize, key), ());
            }
        }
        drums_unknown |= p.scan.drum_keys.is_empty()
            && p.scan
                .ops
                .keys()
                .any(|&(l, o)| l == crate::n64::aseq::Level::Layer && o == 0xc6);
    }
    let mut report = String::new();
    for p in &plans {
        let sc = &p.scan;
        writeln!(
            report,
            "song {} ({}) {} bytes, fonts {:?}",
            p.id,
            crate::n64::SEQ_NAMES.get(p.id).copied().unwrap_or("?"),
            p.code.len(),
            p.font_ids
        )
        .ok();
        writeln!(
            report,
            "scan: channels {} layers {} instruments {} drum keys {} selfmod {} io {} dynamic {}",
            sc.channels.len(),
            sc.layers.len(),
            sc.instruments.len(),
            sc.drum_keys.len(),
            sc.self_modifying,
            sc.io,
            sc.dynamic_targets
        )
        .ok();
        writeln!(
            report,
            "instruments used (font slot, id): {:?}",
            sc.instruments
        )
        .ok();
    }

    // samples, deduplicated by audiotable offset
    let mut samples: Vec<CookedSample> = Vec::new();
    let mut sources: Vec<SampleSource> = Vec::new();
    let mut by_off: BTreeMap<u32, u16> = BTreeMap::new();
    let mut sample_index = |data: &AudioData,
                            font: &Font,
                            tuned: &Tuned,
                            report: &mut String|
     -> Result<(u16, u32), Error> {
        let s = &font.samples[tuned.sample];
        let idx = if let Some(&i) = by_off.get(&s.table_off) {
            i
        } else {
            let cs = if opts.analysis_only {
                describe_sample(s, opts.rate)
            } else {
                let pcm = decode(data, font, s)?;
                cook_sample(&pcm, s, opts.rate)
            };
            sources.push(SampleSource {
                font: font.id,
                table_off: s.table_off,
                pcm_len: s.pcm_len(),
                looped: s.looped.is_some(),
            });
            writeln!(
                report,
                "  sample @{:#08x}: {} pcm -> {} blocks{} stretch {:+.1} cents",
                s.table_off,
                s.pcm_len(),
                cs.blocks,
                if cs.loop_block == 0xffff {
                    String::new()
                } else {
                    format!(", loop at block {}", cs.loop_block)
                },
                1731.234 * (cs.stretch_q16 as f64 / 65536.0).ln()
            )
            .ok();
            samples.push(cs);
            let i = (samples.len() - 1) as u16;
            by_off.insert(s.table_off, i);
            i
        };
        let stretch = samples[idx as usize].stretch_q16;
        let tuning = (tuned.tuning as f64 * stretch as f64).round() as u32;
        Ok((idx, tuning))
    };

    let mut env = EnvPool {
        words: Vec::new(),
        seen: Vec::new(),
    };
    // per font: instruments and drums
    struct FontOut {
        id: u8,
        insts: Vec<Option<[u8; 32]>>,
        drums: Vec<Option<[u8; 16]>>,
    }
    let mut outs: Vec<FontOut> = Vec::new();
    for f in &fonts {
        let mut insts: Vec<Option<[u8; 32]>> = Vec::new();
        for (i, ins) in f.instruments.iter().enumerate() {
            let used = opts.all_instruments || want_inst.contains_key(&(f.id, i as u8));
            let Some(ins) = ins.as_ref().filter(|_| used) else {
                insts.push(None);
                continue;
            };
            let mut b = Vec::new();
            b.extend_from_slice(&[ins.range_lo, ins.range_hi, ins.decay_index, 1]);
            le16(&mut b, env.add(&ins.env));
            le16(&mut b, 0);
            for t in [&ins.low, &ins.normal, &ins.high] {
                match t {
                    Some(t) if t.tuning.is_finite() && t.tuning > 0.0 => {
                        let (si, tuning) = sample_index(data, f, t, &mut report)?;
                        le16(&mut b, si);
                        le16(&mut b, 0);
                        le32(&mut b, tuning);
                    }
                    _ => {
                        le16(&mut b, 0xffff);
                        le16(&mut b, 0);
                        le32(&mut b, 0);
                    }
                }
            }
            insts.push(Some(b.try_into().unwrap()));
        }
        let mut drums: Vec<Option<[u8; 16]>> = Vec::new();
        for (i, d) in f.drums.iter().enumerate() {
            let used =
                opts.all_instruments || drums_unknown || want_drum.contains_key(&(f.id, i as u8));
            let Some(d) = d.as_ref().filter(|_| used) else {
                drums.push(None);
                continue;
            };
            let (si, tuning) = sample_index(data, f, &d.tuned, &mut report)?;
            let mut b = Vec::new();
            b.push(d.decay_index);
            b.push(d.pan);
            le16(&mut b, env.add(&d.env));
            le16(&mut b, si);
            le16(&mut b, 1);
            le32(&mut b, tuning);
            le32(&mut b, 0);
            drums.push(Some(b.try_into().unwrap()));
        }
        // Trailing unused entries cost resident table bytes for nothing.
        while matches!(insts.last(), Some(None)) {
            insts.pop();
        }
        while matches!(drums.last(), Some(None)) {
            drums.pop();
        }
        outs.push(FontOut {
            id: f.id as u8,
            insts,
            drums,
        });
    }

    // write the bank
    let n_fonts = outs.len();
    let off_fonts = 40usize;
    let mut off = off_fonts + 12 * n_fonts;
    let mut font_tbl = Vec::new();
    let mut tables = Vec::new();
    for o in &outs {
        let off_inst = off + tables.len();
        for i in &o.insts {
            match i {
                Some(b) => tables.extend_from_slice(b),
                None => tables.extend_from_slice(&[0u8; 32]),
            }
        }
        let off_drum = off + tables.len();
        for d in &o.drums {
            match d {
                Some(b) => tables.extend_from_slice(b),
                None => tables.extend_from_slice(&[0u8; 16]),
            }
        }
        font_tbl.extend_from_slice(&[o.id, o.insts.len() as u8, o.drums.len() as u8, 0]);
        le32(&mut font_tbl, off_inst as u32);
        le32(&mut font_tbl, off_drum as u32);
    }
    off += tables.len();
    let off_samples = off;
    let mut sample_tbl = Vec::new();
    let mut adpcm = Vec::new();
    for s in &samples {
        le32(&mut sample_tbl, adpcm.len() as u32);
        le32(&mut sample_tbl, s.end_samples);
        le16(&mut sample_tbl, s.blocks);
        le16(&mut sample_tbl, s.rate);
        le16(&mut sample_tbl, s.loop_block);
        le16(&mut sample_tbl, 0);
        adpcm.extend_from_slice(&s.adpcm);
    }
    off += sample_tbl.len();
    let off_env = off;
    let mut env_bytes = Vec::new();
    for p in &env.words {
        env_bytes.extend_from_slice(&p.delay.to_be_bytes());
        env_bytes.extend_from_slice(&p.arg.to_be_bytes());
    }
    off += env_bytes.len();
    let align = opts.adpcm_align.max(16).next_power_of_two();
    let off_adpcm = (off + align - 1) & !(align - 1);
    let mut bank = Vec::new();
    bank.extend_from_slice(b"PSQB");
    le16(&mut bank, 1);
    le16(&mut bank, 0);
    le32(&mut bank, 0);
    le16(&mut bank, n_fonts as u16);
    le16(&mut bank, samples.len() as u16);
    le16(&mut bank, env.words.len() as u16);
    le16(&mut bank, 0);
    le32(&mut bank, off_fonts as u32);
    le32(&mut bank, off_samples as u32);
    le32(&mut bank, off_env as u32);
    le32(&mut bank, off_adpcm as u32);
    le32(&mut bank, adpcm.len() as u32);
    bank.extend_from_slice(&font_tbl);
    bank.extend_from_slice(&tables);
    bank.extend_from_slice(&sample_tbl);
    bank.extend_from_slice(&env_bytes);
    while bank.len() < off_adpcm {
        bank.push(0);
    }
    bank.extend_from_slice(&adpcm);
    let total = bank.len() as u32;
    bank[8..12].copy_from_slice(&total.to_le_bytes());

    // write the songs
    let mut songs = Vec::new();
    for p in &plans {
        let sc = &p.scan;
        let mut flags = 0u16;
        if sc.self_modifying {
            flags |= 1;
        }
        if sc.io {
            flags |= 2;
        }
        let mut song = Vec::new();
        song.extend_from_slice(b"PSQ1");
        le16(&mut song, 1);
        le16(&mut song, flags);
        song.push(p.font_ids.len() as u8);
        song.push(opts.peak_voices);
        le16(&mut song, p.code.len() as u16);
        le16(&mut song, sc.channels.len() as u16);
        le16(&mut song, 0);
        song.extend_from_slice(&p.font_ids);
        while song.len() % 4 != 0 {
            song.push(0);
        }
        song.extend_from_slice(&p.code);
        songs.push(song);
    }

    writeln!(
        report,
        "bank: {} fonts, {} samples, {} envelope points, {} bytes (ADPCM {} bytes = {:.1} KiB at {} Hz); songs {:?} bytes",
        n_fonts,
        samples.len(),
        env.words.len(),
        bank.len(),
        adpcm.len(),
        adpcm.len() as f64 / 1024.0,
        opts.rate,
        songs.iter().map(Vec::len).collect::<Vec<_>>()
    )
    .ok();
    Ok(CookedSet {
        songs,
        bank,
        sources,
        report,
    })
}

/// Cook sound effect `index` of soundfont `font` as a one-shot `.psau`.
///
/// Effects are the soundfont's own list of tuned samples (what a game's
/// sound-effect sequence plays), not instruments. The tuning is folded into
/// the sample rate so the runtime plays the result at pitch 1.0.
///
/// # Errors
/// If the soundfont or the effect is missing, or its tuning is not a positive number.
pub fn cook_effect(data: &AudioData, font: usize, index: usize) -> Result<CookedEffect, Error> {
    let f = data.font(font)?;
    let tuned = f
        .sfx
        .get(index)
        .and_then(Option::as_ref)
        .ok_or_else(|| Error::Missing(format!("effect {index} of soundfont {font}")))?;
    if !(tuned.tuning.is_finite() && tuned.tuning > 0.0) {
        return Err(Error::Format(format!(
            "effect {index} of soundfont {font} has tuning {}",
            tuned.tuning
        )));
    }
    let s = &f.samples[tuned.sample];
    let pcm = decode(data, &f, s)?;
    let wav = Wav {
        rate: NATIVE_RATE,
        samples: pcm.iter().map(|&v| f64::from(v)).collect(),
        loop_start: None,
        loop_end: None,
        bits: 16,
    };
    let mut opts = CookOptions::one_shot(NATIVE_RATE);
    opts.normalize_peak = None;
    let c = cook(&wav, &opts);
    let rate_hz = (f64::from(NATIVE_RATE) * f64::from(tuned.tuning)).round() as u32;
    Ok(CookedEffect {
        psau: psx_audio_cook::psau(rate_hz, c.pcm.len(), &c.adpcm),
        rate_hz,
        blocks: c.adpcm.len() / 16,
        looped: s.looped.is_some(),
    })
}
