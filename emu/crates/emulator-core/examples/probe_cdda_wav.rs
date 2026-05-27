//! Capture the `hello-cdda` side-load path to a WAV and fail if it is silent.
//!
//! ```bash
//! make probe-cdda-audio
//! # writes /tmp/psoxide_hello_cdda.wav
//! ```

use emulator_core::{spu, Bus, Cpu};
use psx_iso::{Disc, Exe};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const DEFAULT_SECONDS: f32 = 8.0;
const DEFAULT_MIN_PEAK: u16 = 1_000;
const SAMPLE_RATE: u32 = 44_100;

fn main() {
    let root = repo_root();
    let out_dir = root.join("build/examples/mipsel-sony-psx/release");
    let exe_path = env_path("PSOXIDE_EXE").unwrap_or_else(|| out_dir.join("hello-cdda.exe"));
    let disc_path = env_path("PSOXIDE_DISC").unwrap_or_else(|| out_dir.join("hello-cdda.cue"));
    let wav_path =
        env_path("PSOXIDE_WAV").unwrap_or_else(|| PathBuf::from("/tmp/psoxide_hello_cdda.wav"));
    let seconds = env_f32("PSOXIDE_AUDIO_SECONDS")
        .unwrap_or(DEFAULT_SECONDS)
        .max(0.1);
    let min_peak = env_u16("PSOXIDE_MIN_PEAK").unwrap_or(DEFAULT_MIN_PEAK);

    let exe_bytes = std::fs::read(&exe_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", exe_path.display()));
    let exe = Exe::parse(&exe_bytes).expect("parse PSX EXE");
    let disc = load_disc(&disc_path);

    let mut bus = Bus::new_without_bios();
    bus.load_exe_payload(exe.load_addr, &exe.payload);
    bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
    bus.enable_hle_bios();
    bus.attach_digital_pad_port1();
    bus.cdrom.insert_disc(Some(disc));

    let mut cpu = Cpu::new();
    cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());

    let target_samples = (seconds * SAMPLE_RATE as f32).round() as usize;
    let mut samples = Vec::with_capacity(target_samples);
    let mut audio_cycle_accum = 0u64;
    let mut steps = 0u64;

    while samples.len() < target_samples && steps < 250_000_000 {
        let cycles_before = bus.cycles();
        if let Err(error) = cpu.step(&mut bus) {
            eprintln!("[probe-cdda] CPU stopped at step {steps}: {error:?}");
            break;
        }
        steps += 1;
        audio_cycle_accum =
            audio_cycle_accum.saturating_add(bus.cycles().saturating_sub(cycles_before));
        let sample_count = (audio_cycle_accum / spu::SAMPLE_CYCLES) as usize;
        audio_cycle_accum %= spu::SAMPLE_CYCLES;
        if sample_count == 0 {
            continue;
        }

        bus.run_spu_samples(sample_count);
        samples.extend(bus.spu.drain_audio());
        if samples.len() > target_samples {
            samples.truncate(target_samples);
        }
    }

    write_wav(&wav_path, &samples).expect("write WAV");
    let stats = AudioStats::from_samples(&samples);

    println!("=== hello-cdda audio probe ===");
    println!("EXE:      {}", exe_path.display());
    println!("Disc:     {}", disc_path.display());
    println!("WAV:      {}", wav_path.display());
    println!(
        "Duration: {:.2}s",
        samples.len() as f32 / SAMPLE_RATE as f32
    );
    println!("Samples:  {}", samples.len());
    println!(
        "Nonzero:  {} ({:.1}%)",
        stats.nonzero,
        stats.nonzero_percent(samples.len())
    );
    println!("Peak:     L={} R={}", stats.peak_l, stats.peak_r);
    println!("SPUCNT:   0x{:04x}", bus.spu.spucnt());
    println!(
        "CD_VOL:   0x{:04x}/0x{:04x}",
        bus.spu.read16(spu::CD_VOL_L),
        bus.spu.read16(spu::CD_VOL_R)
    );
    println!("CD LBA:   {}", bus.cdrom.debug_read_lba());
    println!("Steps:    {steps}");

    if stats.peak_l.max(stats.peak_r) < min_peak {
        eprintln!(
            "[probe-cdda] audio below threshold: peak {}/{} < {min_peak}",
            stats.peak_l, stats.peak_r
        );
        std::process::exit(1);
    }
}

#[derive(Default)]
struct AudioStats {
    peak_l: u16,
    peak_r: u16,
    nonzero: usize,
}

impl AudioStats {
    fn from_samples(samples: &[(i16, i16)]) -> Self {
        let mut stats = Self::default();
        for &(l, r) in samples {
            stats.peak_l = stats.peak_l.max(l.unsigned_abs());
            stats.peak_r = stats.peak_r.max(r.unsigned_abs());
            if l != 0 || r != 0 {
                stats.nonzero += 1;
            }
        }
        stats
    }

    fn nonzero_percent(&self, total: usize) -> f32 {
        if total == 0 {
            0.0
        } else {
            100.0 * self.nonzero as f32 / total as f32
        }
    }
}

fn load_disc(path: &Path) -> Disc {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("cue") => psoxide_settings::library::load_disc_from_cue(path).expect("load CUE"),
        Some("ccd") => psoxide_settings::library::load_disc_from_ccd(path).expect("load CCD"),
        _ => {
            let bytes = std::fs::read(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            Disc::from_bin(bytes)
        }
    }
}

fn write_wav(path: &Path, samples: &[(i16, i16)]) -> std::io::Result<()> {
    let mut f = BufWriter::new(File::create(path)?);
    let data_size = (samples.len() * 4) as u32;
    let file_size = 36 + data_size;
    let sample_rate = SAMPLE_RATE;
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;

    f.write_all(b"RIFF")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for &(l, r) in samples {
        f.write_all(&l.to_le_bytes())?;
        f.write_all(&r.to_le_bytes())?;
    }
    Ok(())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn env_f32(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.parse().ok()
}

fn env_u16(name: &str) -> Option<u16> {
    std::env::var(name).ok()?.parse().ok()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}
