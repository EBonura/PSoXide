//! A host model of the SPU, just enough to listen to a cooked song.
//!
//! The sequencer drives a [`Hw`] implementation exactly as it would drive the
//! chip. This one keeps 24 voices, decodes the cooked ADPCM, steps each voice
//! through its samples with the hardware's Gaussian interpolation, applies the
//! left and right levels the sequencer wrote and sums the result at 44.1 kHz.
//! There is no reverb and no hardware envelope: the sequencer keeps the
//! envelope in software and writes its level to the voice every update.

use psx_audio_cook::adpcm;
use psx_audio_cook::spu_play::gauss;
use psx_seq::format::Bank;
use psx_seq::{Sequencer, UPDATE_HZ};
use psx_sfx::hw::Hw;
use psx_sfx::Sample;

/// Output sample rate.
pub const OUT_RATE: u32 = 44_100;

#[derive(Clone, Default)]
struct Voice {
    on: bool,
    sample: usize,
    pos: f64,
    step: f64,
    left: i16,
    right: i16,
    addr: u32,
    rate: u32,
}

struct Decoded {
    addr: u32,
    pcm: Vec<i16>,
    loop_start: Option<usize>,
}

/// The renderer: an [`Hw`] that mixes.
pub struct Renderer {
    samples: Vec<Decoded>,
    voices: Vec<Voice>,
    /// Interleaved stereo output so far.
    pub out: Vec<i16>,
    /// Most voices keyed on at once.
    pub peak_voices: usize,
    /// The SPU main volume register, applied to the sum as the console does
    /// (`0x3FFF`, what `Spu::new` writes, is half scale; `0x7FFF` is full).
    pub main_volume: i32,
}

impl Renderer {
    /// A renderer for `bank` with its ADPCM at `spu_base`.
    pub fn new(bank: &Bank<'_>, spu_base: u32) -> Self {
        let adpcm_all = bank.adpcm();
        let mut samples = Vec::new();
        for i in 0..bank.sample_count() {
            let s = bank.sample(i).unwrap();
            let bytes =
                &adpcm_all[s.adpcm_off as usize..s.adpcm_off as usize + s.blocks as usize * 16];
            samples.push(Decoded {
                addr: spu_base + s.adpcm_off,
                pcm: adpcm::decode(bytes),
                loop_start: (s.loop_block != 0xFFFF).then(|| s.loop_block as usize * 28),
            });
        }
        Self {
            samples,
            voices: vec![Voice::default(); 24],
            out: Vec::new(),
            peak_voices: 0,
            main_volume: 0x3FFF,
        }
    }

    /// Mix `n` output samples with the current voice state.
    pub fn mix(&mut self, n: usize) {
        for _ in 0..n {
            let (mut l, mut r) = (0i32, 0i32);
            for v in self.voices.iter_mut().filter(|v| v.on) {
                let d = &self.samples[v.sample];
                let at = |i: isize| -> i32 {
                    let len = d.pcm.len() as isize;
                    if i < 0 {
                        return 0;
                    }
                    let i = if i < len {
                        i
                    } else if let Some(ls) = d.loop_start {
                        ls as isize + (i - len) % (len - ls as isize).max(1)
                    } else {
                        return 0;
                    };
                    d.pcm[i as usize] as i32
                };
                let idx = v.pos.floor() as isize;
                let phase = (((v.pos - idx as f64) * 65536.0) as u32 >> 8) as usize;
                let s = gauss([at(idx - 3), at(idx - 2), at(idx - 1), at(idx)], phase);
                l += s * v.left as i32 / 0x4000;
                r += s * v.right as i32 / 0x4000;
                v.pos += v.step;
                if d.loop_start.is_none() && v.pos as usize >= d.pcm.len() + 2 {
                    v.on = false;
                } else if let Some(ls) = d.loop_start {
                    let len = d.pcm.len() as f64;
                    if v.pos >= len + 2.0 {
                        v.pos -= len - ls as f64;
                    }
                }
            }
            let (l, r) = (l.clamp(-0x8000, 0x7FFF), r.clamp(-0x8000, 0x7FFF));
            self.out.push(((l * self.main_volume) >> 15) as i16);
            self.out.push(((r * self.main_volume) >> 15) as i16);
        }
    }

    fn find(&self, addr: u32) -> Option<usize> {
        self.samples.iter().position(|s| s.addr == addr)
    }
}

impl Hw for Renderer {
    fn configure(&mut self, voice: u8, sample: &Sample, _looped: bool, pitch_q12: u16) {
        let addr = sample.addr().byte_offset();
        let idx = self.find(addr);
        let v = &mut self.voices[voice as usize];
        v.addr = addr;
        v.rate = sample.sample_rate_hz();
        if let Some(i) = idx {
            v.sample = i;
        }
        v.step = v.rate as f64 * pitch_q12 as f64 / 4096.0 / OUT_RATE as f64;
    }

    fn set_volume(&mut self, voice: u8, left: i16, right: i16) {
        let v = &mut self.voices[voice as usize];
        v.left = left;
        v.right = right;
    }

    fn set_pitch(&mut self, voice: u8, pitch_q12: u16) {
        let v = &mut self.voices[voice as usize];
        v.step = v.rate as f64 * pitch_q12 as f64 / 4096.0 / OUT_RATE as f64;
    }

    fn key_on(&mut self, mask: u32) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                v.on = true;
                v.pos = 2.0;
            }
        }
        let n = self.voices.iter().filter(|v| v.on).count();
        self.peak_voices = self.peak_voices.max(n);
    }

    fn key_off(&mut self, mask: u32) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                v.on = false;
            }
        }
    }
}

/// Run `seq` for `seconds` and render it; returns stereo samples at
/// [`OUT_RATE`]. `each_update` runs before every update with the update
/// number, so a script can write IO ports or play live notes on a timeline.
pub fn render<const P: usize, const N: usize>(
    seq: &mut Sequencer<'_, P, N>,
    r: &mut Renderer,
    seconds: f64,
    mut each_update: impl FnMut(u32, &mut Sequencer<'_, P, N>, &mut Renderer),
) {
    let updates = (seconds * UPDATE_HZ as f64) as u32;
    let per = (OUT_RATE / UPDATE_HZ) as usize;
    for u in 0..updates {
        each_update(u, seq, r);
        seq.update(r);
        r.mix(per);
    }
}
