//! Capture mixed audio from both PSoXide and PCSX-Redux for the same
//! BIOS/disc/step count, then write side-by-side WAVs plus a compact
//! numeric diff report.
//!
//! ```bash
//! PSOXIDE_DISC="/path/to/game.bin" \
//! cargo run -p emulator-core --example compare_audio_with_redux --release -- 100000000
//!
//! # BIOS-only boot (no disc)
//! cargo run -p emulator-core --example compare_audio_with_redux --release -- --bios-only 100000000
//! ```

use emulator_core::{
    spu::{KON_HI, KON_LO, SAMPLE_CYCLES},
    Bus, Cpu,
};
use parity_oracle::{OracleConfig, ReduxProcess};
use psx_iso::Disc;
use std::env;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_BIOS: &str = "bios/SCPH1001.BIN";
const DEFAULT_DISC: &str =
    "<rom-path>";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SAMPLE_RATE: u32 = 44_100;
const SPUCNT_UNMUTE: u16 = 1 << 14;

fn main() {
    let mut args = env::args().skip(1);
    let mut bios_only = false;
    let mut positional = Vec::new();
    for arg in args.by_ref() {
        match arg.as_str() {
            "--bios-only" => bios_only = true,
            _ => positional.push(arg),
        }
    }

    let steps: u64 = positional
        .first()
        .and_then(|s| s.parse().ok())
        .expect("usage: compare_audio_with_redux [--bios-only] <step_count> [disc_path]");
    let disc_path = if bios_only {
        None
    } else if let Some(path) = positional.get(1) {
        Some(PathBuf::from(path))
    } else {
        match env::var("PSOXIDE_DISC") {
            Ok(value) if value == "-" || value.eq_ignore_ascii_case("none") => None,
            Ok(value) => Some(PathBuf::from(value)),
            Err(_) => Some(PathBuf::from(DEFAULT_DISC)),
        }
    };
    let bios_path = env::var("PSOXIDE_BIOS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_BIOS));
    let out_dir = env::var("PSOXIDE_AUDIO_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let chunk_steps = env::var("PSOXIDE_AUDIO_CHUNK_STEPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_048u64);

    fs::create_dir_all(&out_dir).expect("create output dir");
    let tag = if disc_path.is_some() { "disc" } else { "bios" };
    let redux_raw = out_dir.join(format!("redux_audio_{tag}_{steps}.raw"));
    let redux_wav = out_dir.join(format!("redux_audio_{tag}_{steps}.wav"));
    let ours_raw = out_dir.join(format!("ours_audio_{tag}_{steps}.raw"));
    let ours_wav = out_dir.join(format!("ours_audio_{tag}_{steps}.wav"));
    let report_path = out_dir.join(format!("audio_compare_{tag}_{steps}.txt"));

    let redux_audio = if env::var("PSOXIDE_SKIP_REDUX").is_ok() {
        eprintln!("[redux] skipped (PSOXIDE_SKIP_REDUX set)");
        CapturedAudio {
            tick: 0,
            samples: Vec::new(),
            first_kon_cycle: None,
            first_unmute_cycle: None,
        }
    } else {
        capture_redux_audio(steps, chunk_steps, &bios_path, disc_path.as_deref(), &redux_raw)
    };
    write_wav(&redux_wav, &redux_audio.samples).expect("write redux wav");

    let our_audio = capture_our_audio(steps, &bios_path, disc_path.as_deref());
    write_raw_pcm(&ours_raw, &our_audio.samples).expect("write ours raw");
    write_wav(&ours_wav, &our_audio.samples).expect("write ours wav");

    let report = build_report(steps, &our_audio, &redux_audio, &ours_wav, &redux_wav);
    fs::write(&report_path, &report).expect("write report");

    println!("{report}");
    println!("Report: {}", report_path.display());
    println!("Ours:   {}", ours_wav.display());
    println!("Redux:  {}", redux_wav.display());
}

fn capture_redux_audio(
    steps: u64,
    chunk_steps: u64,
    bios_path: &Path,
    disc_path: Option<&Path>,
    raw_path: &Path,
) -> CapturedAudio {
    let lua = OracleConfig::default_lua_dir().join("oracle.lua");
    let mut config =
        OracleConfig::new(bios_path.to_path_buf(), lua).expect("Redux binary resolves");
    if let Some(disc) = disc_path {
        config = config.with_disc(disc.to_path_buf());
    }

    let mut redux = ReduxProcess::launch(&config).expect("Redux launches");
    redux.handshake(HANDSHAKE_TIMEOUT).expect("handshake");
    let timeout = Duration::from_secs((steps / 300_000).max(60));
    let (tick, frames) = redux
        .run_audio_capture(steps, chunk_steps, raw_path, timeout)
        .expect("redux audio capture");
    eprintln!(
        "[redux] steps={steps} tick={tick} frames={frames} raw={}",
        raw_path.display()
    );
    redux.send_command("quit").ok();
    let _ = redux.wait_for_response(Duration::from_secs(2));
    let _ = redux.terminate();

    CapturedAudio {
        tick,
        samples: read_raw_pcm(raw_path).expect("read redux raw pcm"),
        first_kon_cycle: None,
        first_unmute_cycle: None,
    }
}

struct CapturedAudio {
    tick: u64,
    samples: Vec<(i16, i16)>,
    first_kon_cycle: Option<u64>,
    first_unmute_cycle: Option<u64>,
}

#[derive(Default)]
struct OurTimeline {
    first_kon_cycle: Option<u64>,
    first_unmute_cycle: Option<u64>,
    prev_kon: u32,
    prev_spucnt: u16,
}

fn capture_our_audio(steps: u64, bios_path: &Path, disc_path: Option<&Path>) -> CapturedAudio {
    let bios = fs::read(bios_path).expect("bios");
    let mut bus = Bus::new(bios).expect("bus");
    if let Some(disc_path) = disc_path {
        let disc_bytes = fs::read(disc_path).expect("disc");
        bus.cdrom.insert_disc(Some(Disc::from_bin(disc_bytes)));
    }
    if std::env::var("PSOXIDE_DUMP_VRAM").is_ok() {
        bus.cdrom.enable_command_log(256);
    }

    let mut cpu = Cpu::new();
    let mut audio_cycle_accum = 0u64;
    let mut samples = Vec::new();
    let mut timeline = OurTimeline {
        prev_kon: read_kon(&bus),
        prev_spucnt: bus.spu.spucnt(),
        ..OurTimeline::default()
    };

    use std::collections::HashMap;
    let mut pc_hist: HashMap<u32, u32> = HashMap::new();
    let pc_window_start = steps.saturating_sub(256_000);
    for step_idx in 0..steps {
        // Match Redux's debugger-facing step semantics: one oracle
        // "step" stops at the next user instruction, letting IRQ
        // handlers run through inside the same user-side step.
        let was_in_isr = cpu.in_isr();
        step_and_capture_audio(
            &mut cpu,
            &mut bus,
            &mut audio_cycle_accum,
            &mut samples,
            &mut timeline,
        );
        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                step_and_capture_audio(
                    &mut cpu,
                    &mut bus,
                    &mut audio_cycle_accum,
                    &mut samples,
                    &mut timeline,
                );
            }
        }
        if step_idx >= pc_window_start {
            *pc_hist.entry(cpu.pc()).or_insert(0) += 1;
        }
    }

    // Optional VRAM dump (PSOXIDE_DUMP_VRAM=path) -- boot a disc, step N,
    // and write the full 1024x512 framebuffer as a PPM. Lets us inspect a
    // game's on-screen state through emulator-core without the frontend.
    if let Ok(path) = std::env::var("PSOXIDE_DUMP_VRAM") {
        let vram = bus.gpu.vram.words();
        let (w, h) = (1024usize, 512usize);
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for &px in vram.iter().take(w * h) {
            out.push(((px & 0x1F) << 3) as u8);
            out.push((((px >> 5) & 0x1F) << 3) as u8);
            out.push((((px >> 10) & 0x1F) << 3) as u8);
        }
        std::fs::write(&path, &out).expect("write vram ppm");
        eprintln!("[ours] VRAM -> {path} ({w}x{h}) after {steps} steps");

        // Also dump the on-screen display sub-rect (15bpp) for a clean view.
        let da = bus.gpu.display_area();
        let (dx, dy) = (da.x as usize, da.y as usize);
        let (dw, dh) = ((da.width as usize).max(1), (da.height as usize).max(1));
        let dpath = format!("{path}.display.ppm");
        let mut dout = format!("P6\n{dw} {dh}\n255\n").into_bytes();
        let byte = |off: usize| -> u8 {
            let hw = vram.get(off / 2).copied().unwrap_or(0);
            if off & 1 == 0 { (hw & 0xFF) as u8 } else { (hw >> 8) as u8 }
        };
        for row in 0..dh {
            let vy = dy + row;
            for col in 0..dw {
                if da.bpp24 {
                    // 24bpp FMV: 3 bytes/pixel packed across VRAM halfwords.
                    let base = (vy * w + dx) * 2 + col * 3;
                    dout.push(byte(base));
                    dout.push(byte(base + 1));
                    dout.push(byte(base + 2));
                } else {
                    let vx = dx + col;
                    let px = if vx < w && vy < h { vram[vy * w + vx] } else { 0 };
                    dout.push(((px & 0x1F) << 3) as u8);
                    dout.push((((px >> 5) & 0x1F) << 3) as u8);
                    dout.push((((px >> 10) & 0x1F) << 3) as u8);
                }
            }
        }
        std::fs::write(&dpath, &dout).expect("write display ppm");
        eprintln!("[ours] display {dw}x{dh} @({dx},{dy}) bpp24={} -> {dpath}", da.bpp24);

        // Hang diagnostic: where is the CPU spinning, and what is the CD doing?
        let mut pcs: Vec<(u32, u32)> = pc_hist.iter().map(|(&p, &c)| (p, c)).collect();
        pcs.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("[ours] top spin PCs (last {} steps):", steps.min(256_000));
        for (pc, count) in pcs.iter().take(8) {
            eprintln!("    0x{pc:08x} : {count}");
        }
        let hist = bus.cdrom.command_histogram();
        let cmds: Vec<String> = hist
            .iter()
            .enumerate()
            .filter(|(_, &c)| c > 0)
            .map(|(i, c)| format!("{i:02x}:{c}"))
            .collect();
        eprintln!(
            "[ours] cdrom: cmds={} irq_flag={:02x} irq_mask={:02x} pending={} last_lba={} cmd_hist=[{}]",
            bus.cdrom.commands_dispatched(),
            bus.cdrom.irq_flag(),
            bus.cdrom.irq_mask_value(),
            bus.cdrom.pending_queue_len(),
            bus.cdrom.debug_read_lba(),
            cmds.join(" "),
        );
        let istat = bus.irq_mut().stat();
        let imask = bus.irq_mut().mask();
        eprintln!("[ours] global IRQ: I_STAT={istat:08x} I_MASK={imask:08x} (active={:08x})", istat & imask);
        let t2 = &bus.timers().timers[2];
        eprintln!(
            "[ours] timer2: counter={} mode={:04x} target={} fires={} (irq_on_tgt={} repeat={} reset_at_tgt={} src={})",
            t2.counter, t2.mode, t2.target, bus.timers().dbg_timer2_fires,
            (t2.mode >> 4) & 1, (t2.mode >> 6) & 1, (t2.mode >> 3) & 1, (t2.mode >> 8) & 3,
        );
        if let Ok(addr_s) = std::env::var("PSOXIDE_PEEK") {
            if let Ok(addr) = u32::from_str_radix(addr_s.trim_start_matches("0x"), 16) {
                eprintln!("[ours] peek 0x{addr:08x} = {:08x}", bus.peek_instruction(addr).unwrap_or(0));
            }
        }
        eprintln!(
            "[ours] regs: v0={:08x} v1={:08x} a0={:08x} a1={:08x} a2={:08x} a3={:08x} sp={:08x} ra={:08x}",
            cpu.gpr(2), cpu.gpr(3), cpu.gpr(4), cpu.gpr(5), cpu.gpr(6), cpu.gpr(7), cpu.gpr(29), cpu.gpr(31),
        );
        if let Some(&(top_pc, _)) = pcs.first() {
            let base = top_pc & !0x1F;
            eprintln!("[ours] spin-loop words @0x{base:08x}:");
            for off in 0..32u32 {
                let addr = base + off * 4;
                if let Some(word) = bus.peek_instruction(addr) {
                    eprintln!("    0x{addr:08x}: {word:08x}");
                }
            }
        }
        // Dump the polled table: 24 entries, halfword at +0xC of each, using
        // the entry pointer in $v1 as the current scan position.
        let tbl = cpu.gpr(7); // $a3 = table base
        eprintln!("[ours] table @0x{tbl:08x} (+0xC halfwords, guessed strides):");
        for stride in [0x10u32, 0x14, 0x18, 0x1C, 0x20] {
            let vals: Vec<String> = (0..8u32)
                .map(|i| {
                    let a = tbl.wrapping_add(i * stride + 0xC);
                    bus.peek_instruction(a & !3)
                        .map(|w| format!("{:04x}", (w >> ((a & 2) * 8)) & 0xFFFF))
                        .unwrap_or_else(|| "----".into())
                })
                .collect();
            eprintln!("    stride 0x{stride:02x}: {}", vals.join(" "));
        }
    }

    let (kon, voiced) = bus.spu.voice_debug_counts();
    let (koff, sampstop) = bus.spu.voice_end_counts();
    let cfg = bus.spu.voice_keyon_cfg();
    eprintln!("[ours] per-voice (kon/koff/sampstop, voiced, mean note len, cfg):");
    for v in 0..kon.len() {
        if kon[v] > 0 || voiced[v] > 0 {
            let (sa, alo, ahi, pit, vl, vr) = cfg[v];
            let flag = if kon[v] > 0 && voiced[v] == 0 {
                "  <-- SILENT"
            } else {
                ""
            };
            let note_ms = if kon[v] > 0 {
                (voiced[v] as f64 / kon[v] as f64) * 1000.0 / 44_100.0
            } else {
                0.0
            };
            eprintln!(
                "  v{v:02}: kon={:<3} koff={:<3} sampstop={:<3} voiced={:<8} note~{note_ms:.0}ms start=0x{sa:05x} adsr={alo:04x}/{ahi:04x} pitch={pit:04x} vol={vl}/{vr}{flag}",
                kon[v], koff[v], sampstop[v], voiced[v]
            );
        }
    }

    let (dry, wet, spucnt, reverb_on, pmon, noise_on) = bus.spu.reverb_debug();
    eprintln!(
        "[ours] spu globals: wet/dry={:.3} spucnt={spucnt:04x} reverb_en={} eon={reverb_on:06x} pmon={pmon:06x} noise={noise_on:06x}  (v10 bit set? pmon={} noise={})",
        wet as f64 / dry.max(1) as f64,
        (spucnt >> 7) & 1,
        (pmon >> 10) & 1,
        (noise_on >> 10) & 1
    );
    eprintln!(
        "[ours] dma ch4 (SPU) CHCR start-triggers (kicks): {}",
        bus.dma_start_triggers()[4]
    );

    let trace = bus.spu.voice_trace();
    let tlen = trace.iter().map(|t| t.len()).max().unwrap_or(0);
    let gstep = (tlen / 90).max(1);
    let ms_per_col = gstep * 1024 * 1000 / 44100;
    eprintln!(
        "[ours] per-voice timeline (each col ~{ms_per_col}ms, {tlen} snaps; env/dec scaled 0-9, .=silent; phase /=Atk \\\\=Dec ==Sus v=Rel _=Off):"
    );
    for v in 0..16 {
        let t = &trace[v];
        if t.is_empty() {
            continue;
        }
        let step = gstep;
        let scale = |x: i32| -> char {
            let d = (x.max(0) * 10 / 0x8000).min(9);
            if d == 0 {
                '.'
            } else {
                (b'0' + d as u8) as char
            }
        };
        let env: String = t.iter().step_by(step).map(|&(_, e, _)| scale(e)).collect();
        let dec: String = t
            .iter()
            .step_by(step)
            .map(|&(s, _, _)| scale((s as i32).abs()))
            .collect();
        let pha: String = t
            .iter()
            .step_by(step)
            .map(|&(_, _, p)| match p {
                1 => '/',
                2 => '\\',
                3 => '=',
                4 => 'v',
                _ => '_',
            })
            .collect();
        eprintln!("  v{v:02} env:{env}");
        eprintln!("      dec:{dec}");
        eprintln!("      pha:{pha}");
    }

    CapturedAudio {
        tick: bus.cycles(),
        samples,
        first_kon_cycle: timeline.first_kon_cycle,
        first_unmute_cycle: timeline.first_unmute_cycle,
    }
}

fn step_and_capture_audio(
    cpu: &mut Cpu,
    bus: &mut Bus,
    audio_cycle_accum: &mut u64,
    samples: &mut Vec<(i16, i16)>,
    timeline: &mut OurTimeline,
) {
    let cycles_before = bus.cycles();
    cpu.step(bus).expect("cpu step");
    let cycles_after = bus.cycles();
    update_our_timeline(bus, timeline, cycles_after);
    *audio_cycle_accum = audio_cycle_accum.saturating_add(cycles_after - cycles_before);
    let sample_count = (*audio_cycle_accum / SAMPLE_CYCLES) as usize;
    *audio_cycle_accum %= SAMPLE_CYCLES;
    if sample_count > 0 {
        bus.run_spu_samples(sample_count);
        let drained = bus.spu.drain_audio();
        samples.extend_from_slice(&drained);
    }
}

fn update_our_timeline(bus: &Bus, timeline: &mut OurTimeline, cycle: u64) {
    let kon = read_kon(bus);
    if timeline.first_kon_cycle.is_none() && timeline.prev_kon == 0 && kon != 0 {
        timeline.first_kon_cycle = Some(cycle);
    }
    timeline.prev_kon = kon;

    let spucnt = bus.spu.spucnt();
    if timeline.first_unmute_cycle.is_none()
        && (timeline.prev_spucnt & SPUCNT_UNMUTE) == 0
        && (spucnt & SPUCNT_UNMUTE) != 0
    {
        timeline.first_unmute_cycle = Some(cycle);
    }
    timeline.prev_spucnt = spucnt;
}

fn read_kon(bus: &Bus) -> u32 {
    let lo = bus.spu.read16(KON_LO) as u32;
    let hi = bus.spu.read16(KON_HI) as u32;
    lo | (hi << 16)
}

fn write_raw_pcm(path: &Path, samples: &[(i16, i16)]) -> io::Result<()> {
    let mut f = BufWriter::new(File::create(path)?);
    for &(l, r) in samples {
        f.write_all(&l.to_le_bytes())?;
        f.write_all(&r.to_le_bytes())?;
    }
    f.flush()
}

fn read_raw_pcm(path: &Path) -> io::Result<Vec<(i16, i16)>> {
    let bytes = fs::read(path)?;
    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let l = i16::from_le_bytes([chunk[0], chunk[1]]);
        let r = i16::from_le_bytes([chunk[2], chunk[3]]);
        out.push((l, r));
    }
    Ok(out)
}

fn write_wav(path: &Path, samples: &[(i16, i16)]) -> io::Result<()> {
    let mut f = BufWriter::new(File::create(path)?);
    let data_size = (samples.len() * 4) as u32;
    let file_size = 36 + data_size;
    let channels = 2u16;
    let bits_per_sample = 16u16;
    let byte_rate = SAMPLE_RATE * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;

    f.write_all(b"RIFF")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for &(l, r) in samples {
        f.write_all(&l.to_le_bytes())?;
        f.write_all(&r.to_le_bytes())?;
    }
    f.flush()
}

fn build_report(
    steps: u64,
    ours: &CapturedAudio,
    redux: &CapturedAudio,
    ours_wav: &Path,
    redux_wav: &Path,
) -> String {
    let ours_samples = &ours.samples;
    let redux_samples = &redux.samples;
    let raw = diff_stats(ours_samples, redux_samples);
    let ours_first = first_nonzero_frame(ours_samples);
    let redux_first = first_nonzero_frame(redux_samples);
    let aligned = match (ours_first, redux_first) {
        (Some(o), Some(r)) => diff_stats(&ours_samples[o..], &redux_samples[r..]),
        _ => DiffStats::default(),
    };
    let fine = match (ours_first, redux_first) {
        (Some(o), Some(r)) => best_fine_alignment(&ours_samples[o..], &redux_samples[r..], 8),
        _ => LaggedDiffStats::default(),
    };

    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "=== Audio parity @ step {steps} ===");
    let _ = writeln!(
        out,
        "ticks: ours={} redux={} delta={:+}",
        ours.tick,
        redux.tick,
        ours.tick as i64 - redux.tick as i64
    );
    let _ = writeln!(
        out,
        "frames: ours={} redux={} expected: ours={} redux={}  duration: ours={:.2}s redux={:.2}s",
        ours_samples.len(),
        redux_samples.len(),
        ours.tick / SAMPLE_CYCLES,
        redux.tick / SAMPLE_CYCLES,
        ours_samples.len() as f64 / SAMPLE_RATE as f64,
        redux_samples.len() as f64 / SAMPLE_RATE as f64
    );
    let _ = writeln!(
        out,
        "hashes: ours=0x{:016x} redux=0x{:016x}",
        hash_samples(ours_samples),
        hash_samples(redux_samples)
    );
    let _ = writeln!(
        out,
        "first nonzero frame: ours={} redux={} delta={:+}",
        fmt_opt(ours_first),
        fmt_opt(redux_first),
        delta_opt(ours_first, redux_first)
    );
    let _ = writeln!(
        out,
        "ours timeline: first_kon_cycle={} first_unmute_cycle={} first_nonzero_cycle={}",
        fmt_opt_u64(ours.first_kon_cycle),
        fmt_opt_u64(ours.first_unmute_cycle),
        fmt_opt_u64(ours_first.map(|frame| frame as u64 * SAMPLE_CYCLES))
    );
    let _ = writeln!(
        out,
        "raw overlap: compared={} mean_abs={:.2} rms={:.2} max_abs_delta={} first_diff={}",
        raw.compared_frames,
        raw.mean_abs,
        raw.rms,
        raw.max_abs_delta,
        fmt_opt(raw.first_diff_frame)
    );
    let _ = writeln!(
        out,
        "aligned-from-first-audio: compared={} mean_abs={:.2} rms={:.2} max_abs_delta={} first_diff={}",
        aligned.compared_frames,
        aligned.mean_abs,
        aligned.rms,
        aligned.max_abs_delta,
        fmt_opt(aligned.first_diff_frame)
    );
    let _ = writeln!(
        out,
        "best fine alignment (+/-8 frames): ours_skip={} redux_skip={} compared={} mean_abs={:.2} rms={:.2} max_abs_delta={}",
        fine.ours_skip,
        fine.redux_skip,
        fine.stats.compared_frames,
        fine.stats.mean_abs,
        fine.stats.rms,
        fine.stats.max_abs_delta
    );
    let _ = writeln!(out, "ours wav:  {}", ours_wav.display());
    let _ = writeln!(out, "redux wav: {}", redux_wav.display());
    out
}

#[derive(Default)]
struct LaggedDiffStats {
    ours_skip: usize,
    redux_skip: usize,
    stats: DiffStats,
}

#[derive(Default)]
struct DiffStats {
    compared_frames: usize,
    mean_abs: f64,
    rms: f64,
    max_abs_delta: i32,
    first_diff_frame: Option<usize>,
}

fn diff_stats(a: &[(i16, i16)], b: &[(i16, i16)]) -> DiffStats {
    let compared = a.len().min(b.len());
    if compared == 0 {
        return DiffStats::default();
    }

    let mut abs_sum = 0f64;
    let mut sq_sum = 0f64;
    let mut max_abs = 0i32;
    let mut first_diff = None;

    for (idx, (&(al, ar), &(bl, br))) in a.iter().zip(b.iter()).enumerate() {
        let dl = (al as i32 - bl as i32).abs();
        let dr = (ar as i32 - br as i32).abs();
        let frame_abs = dl.max(dr);
        if frame_abs != 0 && first_diff.is_none() {
            first_diff = Some(idx);
        }
        max_abs = max_abs.max(frame_abs);
        abs_sum += (dl + dr) as f64 * 0.5;
        sq_sum += ((dl * dl + dr * dr) as f64) * 0.5;
    }

    DiffStats {
        compared_frames: compared,
        mean_abs: abs_sum / compared as f64,
        rms: (sq_sum / compared as f64).sqrt(),
        max_abs_delta: max_abs,
        first_diff_frame: first_diff,
    }
}

fn best_fine_alignment(a: &[(i16, i16)], b: &[(i16, i16)], max_skip: usize) -> LaggedDiffStats {
    let mut best = LaggedDiffStats {
        ours_skip: 0,
        redux_skip: 0,
        stats: diff_stats(a, b),
    };
    for skip in 1..=max_skip {
        if skip < a.len() {
            let candidate = LaggedDiffStats {
                ours_skip: skip,
                redux_skip: 0,
                stats: diff_stats(&a[skip..], b),
            };
            if candidate.stats.mean_abs < best.stats.mean_abs {
                best = candidate;
            }
        }
        if skip < b.len() {
            let candidate = LaggedDiffStats {
                ours_skip: 0,
                redux_skip: skip,
                stats: diff_stats(a, &b[skip..]),
            };
            if candidate.stats.mean_abs < best.stats.mean_abs {
                best = candidate;
            }
        }
    }
    best
}

fn first_nonzero_frame(samples: &[(i16, i16)]) -> Option<usize> {
    samples.iter().position(|&(l, r)| l != 0 || r != 0)
}

fn hash_samples(samples: &[(i16, i16)]) -> u64 {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &(l, r) in samples {
        for byte in l.to_le_bytes().into_iter().chain(r.to_le_bytes()) {
            h ^= byte as u64;
            h = h.wrapping_mul(0x0100_0000_01B3);
        }
    }
    h
}

fn fmt_opt(value: Option<usize>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_opt_u64(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn delta_opt(a: Option<usize>, b: Option<usize>) -> String {
    match (a, b) {
        (Some(a), Some(b)) => format!("{:+}", b as isize - a as isize),
        _ => "-".to_string(),
    }
}
