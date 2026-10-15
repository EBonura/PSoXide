//! `psx-seq-cook` command line.

use psx_seq_cook::{rom::Rom, survey};
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
                let name = arg(&args, "--song").unwrap_or_else(|| "KOKIRI".into());
                let id = psx_seq_cook::n64::resolve_seq(&name)
                    .ok_or_else(|| psx_seq_cook::Error::Missing(format!("song {name}")))?;
                let mut o = psx_seq_cook::cook::Options::default();
                if let Some(r) = arg(&args, "--rate") {
                    o.rate = r.parse().unwrap_or(o.rate);
                }
                o.all_instruments = args.iter().any(|a| a == "--all-instruments");
                let out = PathBuf::from(arg(&args, "--out").unwrap_or_else(|| ".".into()));
                let c = psx_seq_cook::cook::cook_song(&data, id, &o)?;
                std::fs::create_dir_all(&out)
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                std::fs::write(out.join("song.psq"), &c.song)
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                std::fs::write(out.join("bank.psqb"), &c.bank)
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                print!("{}", c.report);
                Ok(())
            }
            "render" => {
                let dir = PathBuf::from(arg(&args, "--dir").unwrap_or_else(|| ".".into()));
                let secs: f64 = arg(&args, "--seconds")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(30.0);
                let mut song = std::fs::read(dir.join("song.psq"))
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let bank_bytes = std::fs::read(dir.join("bank.psqb"))
                    .map_err(|e| psx_seq_cook::Error::Io(e.to_string()))?;
                let bank = psx_seq::format::Bank::parse(&bank_bytes)
                    .map_err(|e| psx_seq_cook::Error::Format(format!("{e:?}")))?;
                let voices: usize = arg(&args, "--voices")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(24);
                let mut r = psx_seq_cook::render::Renderer::new(&bank, 0x1010);
                if let Some(m) = arg(&args, "--main") {
                    r.main_volume = i32::from_str_radix(m.trim_start_matches("0x"), 16).unwrap_or(r.main_volume);
                }
                let mask: u32 = (1u32 << voices) - 1;
                macro_rules! go {
                    ($n:literal) => {{
                        let mut seq: psx_seq::Sequencer<'_, 1, $n> = psx_seq::Sequencer::new(mask);
                        seq.set_bank(bank, 0x1010);
                        seq.start(0, &mut song)
                            .map_err(|e| psx_seq_cook::Error::Format(format!("{e:?}")))?;
                        psx_seq_cook::render::render(&mut seq, &mut r, secs, |_, _| {});
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
                println!(
                    "rendered {secs} s to {}; peak voices {}; unsupported ops {}",
                    out.display(),
                    r.peak_voices,
                    unsupported
                );
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
