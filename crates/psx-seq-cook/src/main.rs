//! `psx-seq-cook` command line.

use psx_seq_cook::rom::Rom;
use psx_seq_cook::survey;
use std::path::PathBuf;

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        eprintln!("usage: psx-seq-cook survey --rom ROM --segments CSV");
        std::process::exit(2);
    };
    let run = || -> Result<(), psx_seq_cook::Error> {
        let rom_path =
            PathBuf::from(arg(&args, "--rom").unwrap_or_else(|| "rom/baserom.z64".into()));
        let seg = PathBuf::from(
            arg(&args, "--segments")
                .unwrap_or_else(|| "third_party/oot/baseroms/ntsc-1.2/segments.csv".into()),
        );
        match cmd.as_str() {
            "gen-tables" => {
                print!("{}", psx_seq_cook::gen::tables());
                Ok(())
            }
            "cook" => {
                let rom = Rom::load(&rom_path, &seg)?;
                let data = psx_seq_cook::n64::AudioData::load(&rom)?;
                // `--songs A,B,C` cooks several songs against one bank, each written as
                // `<name>.psq`; `--song A` (the default KOKIRI) writes `song.psq`.
                let names: Vec<String> = match arg(&args, "--songs") {
                    Some(list) => list.split(',').map(str::to_string).collect(),
                    None => vec![arg(&args, "--song").unwrap_or_else(|| "KOKIRI".into())],
                };
                let ids = names
                    .iter()
                    .map(|name| {
                        psx_seq_cook::n64::resolve_seq(name)
                            .ok_or_else(|| psx_seq_cook::Error::Missing(format!("song {name}")))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut o = psx_seq_cook::cook::Options::default();
                if let Some(r) = arg(&args, "--rate") {
                    o.rate = r.parse().unwrap_or(o.rate);
                }
                if let Some(a) = arg(&args, "--align") {
                    o.adpcm_align = a.parse().unwrap_or(o.adpcm_align);
                }
                o.all_instruments = args.iter().any(|a| a == "--all-instruments");
                let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| ".".into()));
                let set = psx_seq_cook::cook::cook_songs(&data, &ids, &o)?;
                std::fs::create_dir_all(&out)
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let single = arg(&args, "--songs").is_none();
                for (name, song) in names.iter().zip(&set.songs) {
                    let file = if single {
                        "song.psq".to_string()
                    } else {
                        format!("{}.psq", name.to_ascii_lowercase())
                    };
                    std::fs::write(out.join(file), song)
                        .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                }
                std::fs::write(out.join("bank.psqb"), &set.bank)
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                print!("{}", set.report);
                Ok(())
            }
            "render" => {
                let dir = PathBuf::from(arg(&args, "--dir").unwrap_or_else(|| ".".into()));
                let secs: f64 = arg(&args, "--seconds")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(30.0);
                let song_file = arg(&args, "--song-file").unwrap_or_else(|| "song.psq".into());
                let mut song = std::fs::read(dir.join(song_file))
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let bank_file = arg(&args, "--bank-file").unwrap_or_else(|| "bank.psqb".into());
                let bank_bytes = std::fs::read(dir.join(bank_file))
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let bank = psx_seq::format::Bank::parse(&bank_bytes)
                    .map_err(|e| psx_seq_cook::Error::Format(format!("{e:?}")))?;
                let live: Option<(u8, u8)> = arg(&args, "--live").and_then(|v| {
                    let (f, i) = v.split_once(':')?;
                    Some((f.parse().ok()?, i.parse().ok()?))
                });
                let voices: usize = arg(&args, "--voices")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(24);
                let mut r = psx_seq_cook::render::Renderer::new(&bank, 0x1010);
                if let Some(m) = arg(&args, "--main") {
                    r.main_volume = i32::from_str_radix(m.trim_start_matches("0x"), 16)
                        .unwrap_or(r.main_volume);
                }
                let mask: u32 = (1u32 << voices) - 1;
                let first: u32 = arg(&args, "--first-voice")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let mask = mask << first;
                let event_hash;
                let event_calls;
                macro_rules! go {
                    ($n:literal) => {{
                        let mut seq: psx_seq::Sequencer<'_, 1, $n> = psx_seq::Sequencer::new(mask);
                        seq.set_bank(bank, 0x1010);
                        seq.start(0, &mut song)
                            .map_err(|e| psx_seq_cook::Error::Format(format!("{e:?}")))?;
                        let mut held: Option<psx_seq::NoteHandle> = None;
                        let trace = psx_seq_cook::render::render(&mut seq, &mut r, secs, |u, seq, r| {
                            let Some((font, inst)) = live else { return };
                            // a short ocarina-like phrase from 10 s: a note every 0.4 s, a little
                            // vibrato, a glide on the last one
                            const PHRASE: [u8; 6] = [57, 60, 64, 57, 60, 64];
                            let start = 10 * psx_seq::UPDATE_HZ;
                            let step = psx_seq::UPDATE_HZ * 2 / 5;
                            if u >= start
                                && (u - start) % step == 0
                                && ((u - start) / step) < PHRASE.len() as u32
                            {
                                if let Some(h) = held.take() {
                                    seq.note_off(h);
                                }
                                let k = PHRASE[((u - start) / step) as usize];
                                held = seq.note_on(r, font, inst, k, 110, 64, 12);
                                if let Some(h) = held {
                                    seq.note_vibrato(h, 12, 24);
                                }
                            }
                            if let Some(h) = held {
                                if u >= start + 5 * step {
                                    let t = (u - start - 5 * step) as u32;
                                    seq.note_bend(h, 65536 + t.min(60) * 120);
                                }
                            }
                        });
                        event_hash = trace.event_hash;
                        event_calls = trace.calls;
                        seq.unsupported_ops()
                    }};
                }
                let unsupported = match voices {
                    8 => go!(8),
                    10 => go!(10),
                    12 => go!(12),
                    14 => go!(14),
                    16 => go!(16),
                    18 => go!(18),
                    20 => go!(20),
                    _ => go!(24),
                };
                let wav = psx_audio_cook::wav::write_pcm16(
                    psx_seq_cook::render::OUT_RATE,
                    &[
                        &r.out.iter().step_by(2).copied().collect::<Vec<_>>(),
                        &r.out.iter().skip(1).step_by(2).copied().collect::<Vec<_>>(),
                    ],
                );
                let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| "render.wav".into()));
                std::fs::write(&out, wav).map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let pcm_hash = r.out.iter().fold(psx_seq_cook::render::FNV_OFFSET, |h, s| {
                    psx_seq_cook::render::fnv1a(h, &s.to_le_bytes())
                });
                println!(
                    "rendered {secs} s to {}; peak voices {}; unsupported ops {}",
                    out.display(),
                    r.peak_voices,
                    unsupported
                );
                println!(
                    "hw calls {event_calls} fnv1a {event_hash:016x}; pcm fnv1a {pcm_hash:016x}"
                );
                Ok(())
            }
            "effect" => {
                // `effect --ids 0x800,0x801 [--wav DIR]`: what the cartridge's effects sequence plays
                // for each sound id, and (with --wav) a rendering of it.
                let rom = Rom::load(&rom_path, &seg)?;
                let data = psx_seq_cook::n64::AudioData::load(&rom)?;
                let ids: Vec<u16> = arg(&args, "--ids")
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        u16::from_str_radix(s.trim_start_matches("0x"), 16)
                            .map_err(|_| psx_seq_cook::Error::Format(format!("bad sound id {s}")))
                    })
                    .collect::<Result<_, _>>()?;
                let set = psx_seq_cook::sfx::cook_effects(&data, &ids)?;
                for (id, voices) in &set.effects {
                    println!("{id:#06x}: {} voice(s)", voices.len());
                    for v in voices {
                        println!(
                            "  sample {} pitch {} level {}/{} looped {} starts {} released {:?}",
                            v.source,
                            v.pitch_q12,
                            v.left,
                            v.right,
                            v.looped,
                            v.starts_after,
                            v.released_after
                        );
                    }
                }
                println!(
                    "{} sample(s), {} bytes of .psau",
                    set.samples.len(),
                    set.samples.iter().map(Vec::len).sum::<usize>()
                );
                if let Some(dir) = arg(&args, "--wav") {
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                    for &id in &ids {
                        let pcm = psx_seq_cook::sfx::render_effect(&set, id, 1.5)?;
                        let wav = psx_audio_cook::wav::write_pcm16(
                            psx_seq_cook::sfx::OUT_RATE,
                            &[
                                &pcm.iter().step_by(2).copied().collect::<Vec<_>>(),
                                &pcm.iter().skip(1).step_by(2).copied().collect::<Vec<_>>(),
                            ],
                        );
                        std::fs::write(PathBuf::from(&dir).join(format!("{id:04x}.wav")), wav)
                            .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                    }
                }
                Ok(())
            }
            "survey" => {
                let rom = Rom::load(&rom_path, &seg)?;
                print!("{}", survey::report(&rom)?);
                Ok(())
            }
            other => Err(psx_seq_cook::Error::Missing(format!("command {other}"))),
        }
    };
    if let Err(e) = run() {
        eprintln!("psx-seq-cook: {e}");
        std::process::exit(1);
    }
}
