//! A/B measurements for the encoder study. Not part of any build.
//!
//! measure verify <wav> <hsfx> <index> <rate>   legacy replica vs a shipped bank entry
//! measure ablate <wav> <rate> [loop]           encoder/resampler variants at one rate
//! measure ladder <wav>                          band loss and end-to-end LSD per rate
//! measure listen <wav> <name> <outdir> <label=rate:pipeline>...
//!     pipeline: hl (nearest+psxed), new (sinc+trellis)
//! measure match <old_rate> <wav>...             lowest new rate matching hl quality
//! measure compare <sound_dir> <list>            old vs new per bank entry
//! measure debug <wav> <rate>                    alignment check

use psx_audio_cook::{
    self as pac, adpcm, legacy, metrics, rate, resample, spu_play, CookOptions, Cooked, Looping,
    Wav,
};
use std::time::Instant;

fn load(path: &str) -> Wav {
    pac::wav::read(&std::fs::read(path).expect("read wav")).expect("parse wav")
}

fn src_i16(w: &Wav) -> Vec<i16> {
    w.samples
        .iter()
        .map(|&v| v.round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

fn e2e(w: &Wav, reference: &[f64], c: &Cooked) -> (f64, f64) {
    let out = pac::playback(c);
    let n = reference.len().min(out.len());
    let snr = metrics::si_snr_db(&reference[..n], &out[..n]);
    let fw = metrics::fw_snr_seg_db(
        &reference[..n],
        &out[..n],
        (w.rate as f64 / 2.0).min(11_025.0),
    );
    (snr, fw)
}

fn hl_cooked(w: &Wav, rate: u32) -> Cooked {
    let (pcm, adpcm) = legacy::hl_cook(&src_i16(w), w.rate, rate, 0.9);
    Cooked {
        rate,
        pcm,
        adpcm,
        loop_block: None,
    }
}

fn new_cooked(w: &Wav, rate: u32, looping: Looping, effort: adpcm::Effort) -> Cooked {
    new_cooked_c(w, rate, looping, effort, true)
}

fn new_cooked_c(w: &Wav, rate: u32, looping: Looping, effort: adpcm::Effort, comp: bool) -> Cooked {
    let mut o = CookOptions::one_shot(rate);
    o.compensate_gauss = comp;
    o.looping = looping;
    o.encode.effort = effort;
    pac::cook(w, &o)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "verify" => {
            let w = load(&args[1]);
            let bank = std::fs::read(&args[2]).unwrap();
            let idx: usize = args[3].parse().unwrap();
            let rate: u32 = args[4].parse().unwrap();
            let rd = |o: usize| u32::from_le_bytes(bank[o..o + 4].try_into().unwrap()) as usize;
            let (off, len) = (rd(8 + idx * 8), rd(12 + idx * 8));
            let shipped = &bank[off + 32..off + len];
            let c = hl_cooked(&w, rate);
            let mut mine = c.adpcm.clone();
            // map loops are re-flagged after cooking; compare flag-free bytes
            let strip = |v: &[u8]| -> Vec<u8> {
                v.chunks(16)
                    .flat_map(|b| {
                        let mut b = b.to_vec();
                        b[1] = 0;
                        b
                    })
                    .collect()
            };
            let same = strip(shipped) == strip(&mine);
            mine.truncate(shipped.len());
            println!(
                "verify {}: shipped {} B, replica {} B, identical blocks: {same}",
                args[1],
                shipped.len(),
                c.adpcm.len()
            );
        }
        "ablate" => {
            let w = load(&args[1]);
            let r: u32 = args[2].parse().unwrap();
            let looping = if args.get(3).map(String::as_str) == Some("loop") {
                Looping::Whole
            } else {
                Looping::None
            };
            let reference = pac::reference_44k(&w);
            let sinc = resample::Sinc::new();
            // Encoder-only comparison on one shared input (sinc, normalised).
            let base = new_cooked_c(&w, r, looping, adpcm::Effort::Greedy, false);
            let pcm = &base.pcm;
            type Enc<'a> = Box<dyn Fn(&[i16]) -> Vec<u8> + 'a>;
            let variants: [(&str, Enc<'_>); 5] = [
                (
                    "psxed (unclamped greedy)",
                    Box::new(|p: &[i16]| legacy::encode_psxed(p)),
                ),
                (
                    "greedy, SPU-exact",
                    Box::new(|p: &[i16]| {
                        adpcm::encode(
                            p,
                            base.loop_block,
                            &adpcm::EncodeOptions {
                                effort: adpcm::Effort::Greedy,
                                ..Default::default()
                            },
                        )
                    }),
                ),
                (
                    "trellis, no lookahead",
                    Box::new(|p: &[i16]| {
                        adpcm::encode(
                            p,
                            base.loop_block,
                            &adpcm::EncodeOptions {
                                lookahead: 1,
                                ..Default::default()
                            },
                        )
                    }),
                ),
                (
                    "trellis + lookahead",
                    Box::new(|p: &[i16]| {
                        adpcm::encode(p, base.loop_block, &adpcm::EncodeOptions::default())
                    }),
                ),
                (
                    "wide b32 r13 la4 span2",
                    Box::new(|p: &[i16]| {
                        adpcm::encode(
                            p,
                            base.loop_block,
                            &adpcm::EncodeOptions {
                                beam: 32,
                                refine: 13,
                                lookahead: 4,
                                span: 2,
                                ..Default::default()
                            },
                        )
                    }),
                ),
            ];
            for (name, f) in variants.iter() {
                let t = Instant::now();
                let a = f(pcm);
                let dt = t.elapsed().as_secs_f64();
                let d = adpcm::decode(&a);
                println!(
                    "  codec  {:<26} SNR {:6.2} dB   ({:.2} s)",
                    name,
                    metrics::snr_db(pcm, &d),
                    dt
                );
            }
            // End-to-end: resampler x encoder.
            let lin = {
                let mut p = legacy::resample_linear(&src_i16(&w), w.rate, r);
                legacy::normalize_to_peak(&mut p, 0.9);
                let a = legacy::encode_psxed(&p);
                Cooked {
                    rate: r,
                    pcm: p,
                    adpcm: a,
                    loop_block: None,
                }
            };
            let sinc_psxed = {
                let mut p = resample::to_i16(&sinc.resample(&w.samples, w.rate, r));
                legacy::normalize_to_peak(&mut p, 0.9);
                let a = legacy::encode_psxed(&p);
                Cooked {
                    rate: r,
                    pcm: p,
                    adpcm: a,
                    loop_block: None,
                }
            };
            for (name, c) in [
                ("hl: nearest + psxed", hl_cooked(&w, r)),
                ("psxed: linear + psxed", lin),
                ("sinc + psxed", sinc_psxed),
                (
                    "sinc + trellis",
                    new_cooked_c(&w, r, looping, adpcm::Effort::Trellis, false),
                ),
                (
                    "sinc + comp + trellis (new)",
                    new_cooked(&w, r, looping, adpcm::Effort::Trellis),
                ),
            ] {
                let (snr, lsd) = e2e(&w, &reference, &c);
                println!(
                    "  e2e    {:<26} SI-SNR {:6.2} dB  fwSNRseg {:5.2} dB  bytes {}",
                    name,
                    snr,
                    lsd,
                    c.adpcm.len()
                );
            }
        }
        "ladder" => {
            let w = load(&args[1]);
            let reference = pac::reference_44k(&w);
            let rates: Vec<u32> = rate::LADDER
                .iter()
                .copied()
                .filter(|&r| r <= w.rate)
                .collect();
            let loss = rate::band_loss(&w.samples, w.rate, &rates);
            for (i, &r) in rates.iter().enumerate() {
                let (s_old, l_old) = e2e(&w, &reference, &hl_cooked(&w, r));
                let (s_new, l_new) = e2e(
                    &w,
                    &reference,
                    &new_cooked(&w, r, Looping::None, adpcm::Effort::Trellis),
                );
                println!(
                    "  {:5} Hz  band-loss {:5.2}  | hl fwSNRseg {:5.2} SI-SNR {:6.2} | new fwSNRseg {:5.2} SI-SNR {:6.2} | {} B",
                    r, loss[i], l_old, s_old, l_new, s_new, pac::adpcm_bytes(pac::resampled_len(w.samples.len(), w.rate, r))
                );
            }
        }
        "listen" => {
            let w = load(&args[1]);
            let name = &args[2];
            let dir = std::path::Path::new(&args[3]);
            std::fs::create_dir_all(dir).unwrap();
            let reference = pac::reference_44k(&w);
            let orig = resample::to_i16(&reference);
            std::fs::write(
                dir.join(format!("{name}__0-original-{}hz.wav", w.rate)),
                pac::wav::write_mono16(44_100, &orig),
            )
            .unwrap();
            for spec in &args[4..] {
                let (label, rest) = spec.split_once('=').unwrap();
                let (r, pipe) = rest.split_once(':').unwrap();
                let r: u32 = r.parse().unwrap();
                let c = match pipe {
                    "hl" => hl_cooked(&w, r),
                    _ => new_cooked(&w, r, Looping::None, adpcm::Effort::Trellis),
                };
                let (snr, lsd) = e2e(&w, &reference, &c);
                let played = spu_play::play(&c.decoded(), c.rate);
                let file = format!("{name}__{label}-{pipe}-{r}hz.wav");
                std::fs::write(dir.join(&file), pac::wav::write_mono16(44_100, &played)).unwrap();
                println!(
                    "{file}: {} B, SI-SNR {snr:.2} dB, fwSNRseg {lsd:.2} dB",
                    c.adpcm.len()
                );
            }
        }
        "match" => {
            // measure match <old_rate> <wav>... : lowest ladder rate at which the
            // new pipeline's fwSNRseg reaches the hl pipeline's at old_rate.
            // old_rate may be prefixed "psxed:" to use the linear psxed pipeline.
            let (psxed, old_rate): (bool, u32) = match args[1].strip_prefix("psxed:") {
                Some(r) => (true, r.parse().unwrap()),
                None => (false, args[1].parse().unwrap()),
            };
            for path in &args[2..] {
                let w = load(path);
                let reference = pac::reference_44k(&w);
                let old_c = if psxed {
                    let p = legacy::resample_linear(&src_i16(&w), w.rate, old_rate);
                    let a = legacy::encode_psxed(&p);
                    Cooked {
                        rate: old_rate,
                        pcm: p,
                        adpcm: a,
                        loop_block: None,
                    }
                } else {
                    hl_cooked(&w, old_rate)
                };
                let (_, old_q) = e2e(&w, &reference, &old_c);
                let mut best = (
                    old_rate,
                    e2e(
                        &w,
                        &reference,
                        &new_cooked(&w, old_rate, Looping::None, adpcm::Effort::Trellis),
                    )
                    .1,
                );
                for &r in rate::LADDER.iter().filter(|&&r| r < old_rate) {
                    let (_, q) = e2e(
                        &w,
                        &reference,
                        &new_cooked(&w, r, Looping::None, adpcm::Effort::Trellis),
                    );
                    if q >= old_q {
                        best = (r, q);
                    } else {
                        break;
                    }
                }
                println!("{path}|{old_rate}|{old_q:.2}|{}|{:.2}", best.0, best.1);
            }
        }
        "compare" => {
            // measure compare <sound_dir> <list>: lines label|wav+wav|old_rate|new_rate|loop
            // -> label|old_rate|old fwSNRseg|new_rate|new fwSNRseg|seconds
            let dir = std::path::Path::new(&args[1]);
            let list = std::fs::read_to_string(&args[2]).unwrap();
            let sinc = resample::Sinc::new();
            for line in list.lines().filter(|l| !l.is_empty()) {
                let f: Vec<&str> = line.split('|').collect();
                let parts: Vec<Wav> = f[1]
                    .split('+')
                    .map(|p| load(dir.join(p).to_str().unwrap()))
                    .collect();
                let rate = parts.iter().map(|w| w.rate).max().unwrap();
                let mut samples = Vec::new();
                for p in &parts {
                    if p.rate == rate {
                        samples.extend_from_slice(&p.samples)
                    } else {
                        samples.extend(sinc.resample(&p.samples, p.rate, rate))
                    }
                }
                let w = Wav {
                    rate,
                    samples,
                    loop_start: None,
                    loop_end: None,
                    bits: 8,
                };
                let (old, new): (u32, u32) = (f[2].parse().unwrap(), f[3].parse().unwrap());
                // Loops are measured as one-shots: a whole loop is stretched by up
                // to half a block to fill whole blocks, which misaligns it against
                // the reference; the coding is otherwise identical.
                let looping = Looping::None;
                let _ = f[4];
                let reference = pac::reference_44k(&w);
                let (_, qo) = e2e(&w, &reference, &hl_cooked(&w, old));
                let (_, qn) = e2e(
                    &w,
                    &reference,
                    &new_cooked(&w, new, looping, adpcm::Effort::Trellis),
                );
                println!(
                    "{}|{old}|{qo:.2}|{new}|{qn:.2}|{:.2}",
                    f[0],
                    w.samples.len() as f64 / rate as f64
                );
            }
        }
        "pair" => {
            // measure pair <sound_dir> <outdir> <name> <wav[+wav]> <current> <new_rate>
            // current: hl:RATE | psxed:RATE | file:PATH:RATE | none
            // Writes original / current / new playback WAVs (44.1 kHz, through the
            // SPU interpolator model) and prints one index line.
            let dir = std::path::Path::new(&args[1]);
            let out = std::path::Path::new(&args[2]);
            std::fs::create_dir_all(out).unwrap();
            let name = &args[3];
            let sinc = resample::Sinc::new();
            let parts: Vec<Wav> = args[4]
                .split('+')
                .map(|p| load(dir.join(p).to_str().unwrap()))
                .collect();
            let rate = parts.iter().map(|w| w.rate).max().unwrap();
            let mut samples = Vec::new();
            for p in &parts {
                if p.rate == rate {
                    samples.extend_from_slice(&p.samples)
                } else {
                    samples.extend(sinc.resample(&p.samples, p.rate, rate))
                }
            }
            let w = Wav {
                rate,
                samples,
                loop_start: None,
                loop_end: None,
                bits: parts[0].bits,
            };
            let reference = pac::reference_44k(&w);
            let max_hz = (w.rate as f64 / 2.0).min(11_025.0);
            let score = |played: &[f64]| {
                let n = reference.len().min(played.len());
                (
                    metrics::fw_snr_seg_db(&reference[..n], &played[..n], max_hz),
                    metrics::si_snr_db(&reference[..n], &played[..n]),
                )
            };
            std::fs::write(
                out.join(format!("{name}__0-original.wav")),
                pac::wav::write_mono16(44_100, &resample::to_i16(&reference)),
            )
            .unwrap();
            let spec: Vec<&str> = args[5].split(':').collect();
            let current: Option<(String, Cooked)> = match spec[0] {
                "hl" => {
                    let r = spec[1].parse().unwrap();
                    Some((format!("hl-{r}hz"), hl_cooked(&w, r)))
                }
                "psxed" => {
                    let r: u32 = spec[1].parse().unwrap();
                    let p = legacy::resample_linear(&src_i16(&w), w.rate, r);
                    let a = legacy::encode_psxed(&p);
                    Some((
                        format!("psxed-{r}hz"),
                        Cooked {
                            rate: r,
                            pcm: p,
                            adpcm: a,
                            loop_block: None,
                        },
                    ))
                }
                "file" => {
                    let r: u32 = spec[2].parse().unwrap();
                    let a = std::fs::read(spec[1]).unwrap();
                    Some((
                        format!("shipped-{r}hz"),
                        Cooked {
                            rate: r,
                            pcm: Vec::new(),
                            adpcm: a,
                            loop_block: None,
                        },
                    ))
                }
                _ => None,
            };
            let new_rate: u32 = args[6].parse().unwrap();
            let new = new_cooked(&w, new_rate, Looping::None, adpcm::Effort::Trellis);
            let mut line = format!("{name} | {:.2} s |", w.samples.len() as f64 / w.rate as f64);
            if let Some((label, c)) = current {
                let played = spu_play::play(&c.decoded(), c.rate);
                let (fw, si) = score(&played.iter().map(|&v| v as f64).collect::<Vec<_>>());
                std::fs::write(
                    out.join(format!("{name}__A-current-{label}.wav")),
                    pac::wav::write_mono16(44_100, &played),
                )
                .unwrap();
                line += &format!(
                    " current {label} {} B fwSNRseg {fw:.2} SI-SNR {si:.2} |",
                    c.adpcm.len()
                );
            } else {
                line += " current: not in the bank |";
            }
            let played = spu_play::play(&new.decoded(), new.rate);
            let (fw, si) = score(&played.iter().map(|&v| v as f64).collect::<Vec<_>>());
            std::fs::write(
                out.join(format!("{name}__B-new-{new_rate}hz.wav")),
                pac::wav::write_mono16(44_100, &played),
            )
            .unwrap();
            line += &format!(
                " new {new_rate}hz {} B fwSNRseg {fw:.2} SI-SNR {si:.2}",
                new.adpcm.len()
            );
            println!("{line}");
        }
        "debug" => {
            let w = load(&args[1]);
            let r: u32 = args[2].parse().unwrap();
            let reference = pac::reference_44k(&w);
            let sinc = resample::Sinc::new();
            let pcm_sinc = resample::to_i16(&sinc.resample(&w.samples, w.rate, r));
            let pcm_lin = legacy::resample_linear(&src_i16(&w), w.rate, r);
            for (name, pcm) in [("sinc", &pcm_sinc), ("linear", &pcm_lin)] {
                let played: Vec<f64> = spu_play::play(pcm, r).iter().map(|&v| v as f64).collect();
                let up = sinc.resample(
                    &pcm.iter().map(|&v| v as f64).collect::<Vec<_>>(),
                    r,
                    44_100,
                );
                for lag in -4i32..=4 {
                    let sh = |v: &[f64]| -> Vec<f64> {
                        if lag >= 0 {
                            v[lag as usize..].to_vec()
                        } else {
                            let mut o = vec![0.0; (-lag) as usize];
                            o.extend_from_slice(v);
                            o
                        }
                    };
                    let (a, b) = (sh(&played), sh(&up));
                    let n = reference.len().min(a.len()).min(b.len());
                    println!(
                        "{name} lag {lag:+}: gauss SI-SNR {:6.2}  sinc-up SI-SNR {:6.2}",
                        metrics::si_snr_db(&reference[..n], &a[..n]),
                        metrics::si_snr_db(&reference[..n], &b[..n])
                    );
                }
            }
        }
        _ => eprintln!("unknown command"),
    }
}
