//! Boot a retail disc with a pad attached, run for a while, then dump
//! the recent SIO0 controller transactions.
//!
//! Built for the exact "SDK demos see input, commercial games don't"
//! investigation: it tells us whether a game is raw-polling with
//! `0x42`, doing the full DualShock config dance (`0x43`/`0x44`/`0x45`
//! / `0x46` / `0x47` / `0x4C` / `0x4D`), or never getting a sane pad
//! response at all.
//!
//! Supports two input modes:
//! - `PSOXIDE_PAD1=0x0008` keeps a mask held for the whole run.
//! - `PSOXIDE_PAD1_PULSES='0x0008@1200+4,0x4000@1250+1'` presses one
//!   or more masks for a fixed number of VBlanks starting at the given
//!   VBlank count. Format per entry: `<mask>@<start_vblank>+<frames>`.
//! - `PSOXIDE_VISIBLE_DUMP=/tmp/frame.ppm` dumps the final display frame.
//! - `PSOXIDE_REQUIRE_CDDA=1` fails unless the run issued Play and mixed
//!   audible CD-DA samples above `PSOXIDE_MIN_PEAK` (default 256).
//! - `PSOXIDE_REQUIRE_CDROM_READS=1` fails unless the game read sectors
//!   through the BIOS-facing CD-ROM FIFO/DMA path.
//!
//! Best used with MMIO tracing enabled:
//!
//! ```bash
//! PSOXIDE_DISC="/path/to/game.bin" \
//! cargo run -p emulator-core --example probe_disc_pad_trace \
//!   --features emulator-core/trace-mmio --release -- 120000000
//! ```

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::{
    fast_boot_disc_with_hle, spu, warm_bios_for_disc_fast_boot, Bus, Cpu,
    DISC_FAST_BOOT_WARMUP_STEPS,
};
use std::path::PathBuf;

#[cfg(feature = "trace-mmio")]
use emulator_core::mmio_trace::MmioEntry;
#[cfg(feature = "trace-mmio")]
use emulator_core::{MmioKind, Sio0};
#[cfg(feature = "trace-mmio")]
use std::collections::BTreeMap;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PadPulse {
    mask: u16,
    start_vblank: u64,
    frames: u64,
}

#[derive(Default)]
struct AudioStats {
    samples: usize,
    nonzero: usize,
    peak_l: u16,
    peak_r: u16,
}

#[derive(Copy, Clone)]
struct DisplayHashChange {
    vblank: u64,
    cycle: u64,
    hash: u64,
    width: u32,
    height: u32,
}

impl AudioStats {
    fn add_samples(&mut self, samples: &[(i16, i16)]) {
        self.samples += samples.len();
        for &(l, r) in samples {
            self.peak_l = self.peak_l.max(l.unsigned_abs());
            self.peak_r = self.peak_r.max(r.unsigned_abs());
            if l != 0 || r != 0 {
                self.nonzero += 1;
            }
        }
    }
}

fn dump_cdrom_state(bus: &Bus, audio: &AudioStats) {
    println!("\n=== CD-ROM / CD-DA state ===");
    println!("mode:       0x{:02x}", bus.cdrom.debug_mode());
    let (xa_file, xa_channel) = bus.cdrom.debug_xa_filter();
    println!("xa filter:  file={xa_file} channel={xa_channel}");
    println!("read_lba:   {}", bus.cdrom.debug_read_lba());
    println!("fifo len:   {}", bus.cdrom.data_fifo_len());
    println!("fifo pops:  {}", bus.cdrom.data_fifo_pops());
    println!("cd queue:   {}", bus.cdrom.cd_audio_queue_len());
    println!(
        "audio:      samples={} nonzero={} peak_l={} peak_r={}",
        audio.samples, audio.nonzero, audio.peak_l, audio.peak_r
    );
    if let Some((header, subheader)) = bus.cdrom.debug_last_sector() {
        println!(
            "last hdr:   {:02x}:{:02x}:{:02x} mode={} sub=[{:02x} {:02x} {:02x} {:02x}]",
            header[0],
            header[1],
            header[2],
            header[3],
            subheader[0],
            subheader[1],
            subheader[2],
            subheader[3]
        );
    } else {
        println!("last hdr:   (none)");
    }
    println!("debug:      {}", bus.cdrom.debug_state(bus.cycles()));
    println!("\n=== CD-ROM command histogram ===");
    for (cmd, &count) in bus.cdrom.command_histogram().iter().enumerate() {
        if count != 0 {
            println!("  0x{cmd:02x} {:<8} {count}", cdrom_cmd_name(cmd as u8));
        }
    }
    println!("\n=== Recent CD-ROM commands ===");
    for entry in bus.cdrom.command_log() {
        println!(
            "  cyc={:>12} op=0x{:02x} {:<8} params=[{}]",
            entry.cycle,
            entry.command,
            cdrom_cmd_name(entry.command),
            fmt_bytes(&entry.params[..entry.param_len as usize])
        );
    }
    println!("\n=== Recent CD-ROM responses ===");
    for entry in bus.cdrom.response_log() {
        println!(
            "  cyc={:>12} irq={:?} bytes=[{}]",
            entry.cycle,
            entry.irq,
            fmt_bytes(&entry.bytes[..entry.len as usize])
        );
    }
}

fn push_display_hash_change(changes: &mut Vec<DisplayHashChange>, change: DisplayHashChange) {
    const MAX_DISPLAY_HASH_CHANGES: usize = 32;
    changes.push(change);
    if changes.len() > MAX_DISPLAY_HASH_CHANGES {
        changes.remove(0);
    }
}

fn dump_display_hash_changes(changes: &[DisplayHashChange]) {
    println!("\n=== Recent display hash changes ===");
    if changes.is_empty() {
        println!("  (none)");
        return;
    }
    for change in changes {
        println!(
            "  vblank={:>5} cyc={:>12} display={}x{} hash=0x{:016x}",
            change.vblank, change.cycle, change.width, change.height, change.hash
        );
    }
}

fn fmt_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn cdrom_cmd_name(op: u8) -> &'static str {
    match op {
        0x00 => "Sync",
        0x01 => "GetStat",
        0x02 => "SetLoc",
        0x03 => "Play",
        0x04 => "Forward",
        0x05 => "Backward",
        0x06 => "ReadN",
        0x07 => "MotorOn",
        0x08 => "Stop",
        0x09 => "Pause",
        0x0A => "Init",
        0x0B => "Mute",
        0x0C => "Demute",
        0x0D => "SetFilter",
        0x0E => "SetMode",
        0x0F => "GetParam",
        0x10 => "GetLocL",
        0x11 => "GetLocP",
        0x12 => "SetSession",
        0x13 => "GetTN",
        0x14 => "GetTD",
        0x15 => "SeekL",
        0x16 => "SeekP",
        0x17 => "SetClock",
        0x18 => "GetClock",
        0x19 => "Test",
        0x1A => "GetID",
        0x1B => "ReadS",
        0x1C => "Reset",
        0x1D => "GetQ",
        0x1E => "ReadTOC",
        0x1F => "VideoCD",
        _ => "?",
    }
}

fn env_bool(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PadMaskChange {
    vblank: u64,
    cycles: u64,
    mask: u16,
}

fn main() {
    let mut fastboot = false;
    let mut positional = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--fastboot" {
            fastboot = true;
        } else {
            positional.push(arg);
        }
    }

    let steps: u64 = positional
        .first()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120_000_000);

    let bios_path = std::env::var("PSOXIDE_BIOS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("bios/SCPH1001.BIN"));
    let disc_path = std::env::var("PSOXIDE_DISC").unwrap_or_else(|_| "<rom-path>".into());
    let held_buttons = std::env::var("PSOXIDE_PAD1")
        .ok()
        .and_then(|s| parse_u16_mask(&s))
        .unwrap_or(0);
    let pad_pulses = std::env::var("PSOXIDE_PAD1_PULSES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| {
            parse_pad_pulses(&s).unwrap_or_else(|| {
                panic!(
                    "PSOXIDE_PAD1_PULSES must be comma-separated \
                     <mask>@<start_vblank>+<frames> entries"
                )
            })
        })
        .unwrap_or_default();
    let require_cdda = env_bool("PSOXIDE_REQUIRE_CDDA");
    let require_cdrom_reads = env_bool("PSOXIDE_REQUIRE_CDROM_READS");
    let min_peak = std::env::var("PSOXIDE_MIN_PEAK")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(256);

    let bios = std::fs::read(&bios_path).expect("BIOS readable");
    let mut bus = Bus::new(bios).expect("bus");
    let mut cpu = Cpu::new();
    let disc =
        disc_support::load_disc_path(std::path::Path::new(&disc_path)).expect("disc readable");
    if fastboot {
        warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
            .expect("BIOS warmup");
        let info = fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false).expect("fast boot");
        println!(
            "fastboot={} entry=0x{:08x} payload={}B",
            info.boot_path, info.initial_pc, info.payload_len
        );
    }
    bus.cdrom.insert_disc(Some(disc));
    bus.cdrom.enable_command_log(128);
    bus.cdrom.enable_response_log(128);
    bus.attach_digital_pad_port1();

    let mut audio_cycle_accum = 0u64;
    let mut audio_stats = AudioStats::default();
    let mut current_pad_mask = None;
    let mut pad_mask_changes = Vec::new();
    let (initial_display_hash, initial_display_width, initial_display_height, _) =
        bus.gpu.display_hash();
    let mut last_display_hash = initial_display_hash;
    let mut display_hash_changes = Vec::new();
    push_display_hash_change(
        &mut display_hash_changes,
        DisplayHashChange {
            vblank: bus.irq().raise_counts()[0],
            cycle: bus.cycles(),
            hash: initial_display_hash,
            width: initial_display_width,
            height: initial_display_height,
        },
    );
    let mut last_sampled_vblank = bus.irq().raise_counts()[0];
    sync_pad_mask(
        &mut bus,
        held_buttons,
        &pad_pulses,
        &mut current_pad_mask,
        &mut pad_mask_changes,
    );
    for _ in 0..steps {
        let cycles_before = bus.cycles();
        if cpu.step(&mut bus).is_err() {
            break;
        }
        audio_cycle_accum =
            audio_cycle_accum.saturating_add(bus.cycles().saturating_sub(cycles_before));
        sync_pad_mask(
            &mut bus,
            held_buttons,
            &pad_pulses,
            &mut current_pad_mask,
            &mut pad_mask_changes,
        );
        let sample_count = (audio_cycle_accum / spu::SAMPLE_CYCLES) as usize;
        audio_cycle_accum %= spu::SAMPLE_CYCLES;
        if sample_count != 0 {
            bus.run_spu_samples(sample_count);
            audio_stats.add_samples(&bus.spu.drain_audio());
        }
        let vblank = bus.irq().raise_counts()[0];
        if vblank != last_sampled_vblank {
            last_sampled_vblank = vblank;
            let (hash, width, height, _) = bus.gpu.display_hash();
            if hash != last_display_hash {
                last_display_hash = hash;
                push_display_hash_change(
                    &mut display_hash_changes,
                    DisplayHashChange {
                        vblank,
                        cycle: bus.cycles(),
                        hash,
                        width,
                        height,
                    },
                );
            }
        }
    }

    let (display_hash, w, h, _) = bus.gpu.display_hash();
    println!("disc: {disc_path}");
    println!("steps:      {steps}");
    println!("pad mask:   0x{held_buttons:04x}");
    println!("pad pulses: {}", format_pad_pulses(&pad_pulses));
    println!("cpu.tick:   {}", cpu.tick());
    println!("bus.cycles: {}", bus.cycles());
    println!("final pc:   0x{:08x}", cpu.pc());
    println!("vblank:     {}", bus.irq().raise_counts()[0]);
    println!("display:    {w}x{h}  hash=0x{display_hash:016x}");
    dump_display_hash_changes(&display_hash_changes);
    dump_cdrom_state(&bus, &audio_stats);
    dump_pad_mask_changes(&pad_mask_changes);
    dump_pad_histogram(&bus);
    if let Ok(path) = std::env::var("PSOXIDE_VISIBLE_DUMP") {
        dump_visible_ppm(&bus, &path).expect("visible dump");
        println!("\nvisible dump: {path}");
    }

    #[cfg(feature = "trace-mmio")]
    dump_trace(&bus);
    #[cfg(not(feature = "trace-mmio"))]
    println!("\nMMIO tracing is disabled. Re-run with --features emulator-core/trace-mmio.");

    let hist = bus.cdrom.command_histogram();
    let peak = audio_stats.peak_l.max(audio_stats.peak_r);
    let mut failed = false;
    if require_cdda {
        if hist[0x03] == 0 {
            eprintln!("[probe-disc-pad] required CD-DA Play command was not observed");
            failed = true;
        }
        if peak < min_peak {
            eprintln!("[probe-disc-pad] required CD-DA peak {peak} < {min_peak}");
            failed = true;
        }
    }
    if require_cdrom_reads {
        let read_commands = hist[0x06].saturating_add(hist[0x1B]);
        if read_commands == 0 {
            eprintln!("[probe-disc-pad] required ReadN/ReadS command was not observed");
            failed = true;
        }
        if bus.cdrom.data_fifo_pops() == 0 {
            eprintln!("[probe-disc-pad] required CD-ROM data FIFO consumption was not observed");
            failed = true;
        }
    }
    if failed {
        std::process::exit(1);
    }
}

fn parse_u16_mask(text: &str) -> Option<u16> {
    let s = text.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

fn parse_pad_pulses(text: &str) -> Option<Vec<PadPulse>> {
    let mut pulses = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        pulses.push(parse_pad_pulse(entry)?);
    }
    Some(pulses)
}

fn parse_pad_pulse(text: &str) -> Option<PadPulse> {
    let (mask_text, rest) = text.split_once('@')?;
    let mask = parse_u16_mask(mask_text)?;
    let (start_text, frames_text) = match rest.split_once('+') {
        Some((start, frames)) => (start.trim(), frames.trim()),
        None => (rest.trim(), "1"),
    };
    let start_vblank = start_text.parse().ok()?;
    let frames = frames_text.parse().ok()?;
    if frames == 0 {
        return None;
    }
    Some(PadPulse {
        mask,
        start_vblank,
        frames,
    })
}

fn format_pad_pulses(pulses: &[PadPulse]) -> String {
    if pulses.is_empty() {
        return "(none)".into();
    }
    pulses
        .iter()
        .map(|pulse| {
            format!(
                "0x{:04x}@{}+{}",
                pulse.mask, pulse.start_vblank, pulse.frames
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn effective_pad_mask(base_mask: u16, pulses: &[PadPulse], current_vblank: u64) -> u16 {
    let mut mask = base_mask;
    for pulse in pulses {
        let end_vblank = pulse.start_vblank.saturating_add(pulse.frames);
        if current_vblank >= pulse.start_vblank && current_vblank < end_vblank {
            mask |= pulse.mask;
        }
    }
    mask
}

fn sync_pad_mask(
    bus: &mut Bus,
    base_mask: u16,
    pulses: &[PadPulse],
    current_mask: &mut Option<u16>,
    changes: &mut Vec<PadMaskChange>,
) {
    let vblank = bus.irq().raise_counts()[0];
    let next_mask = effective_pad_mask(base_mask, pulses, vblank);
    if current_mask.is_some_and(|mask| mask == next_mask) {
        return;
    }

    bus.set_port1_buttons(emulator_core::ButtonState::from_bits(next_mask));
    if current_mask.is_some() || next_mask != 0 {
        changes.push(PadMaskChange {
            vblank,
            cycles: bus.cycles(),
            mask: next_mask,
        });
    }
    *current_mask = Some(next_mask);
}

fn dump_pad_mask_changes(changes: &[PadMaskChange]) {
    println!("\n=== Pad-1 mask changes ===");
    if changes.is_empty() {
        println!("  (none)");
        return;
    }
    for change in changes {
        println!(
            "  vblank={}  cycles={}  mask=0x{:04x}",
            change.vblank, change.cycles, change.mask
        );
    }
}

fn dump_pad_histogram(bus: &Bus) {
    println!("\n=== Port-1 pad command histogram ===");
    match bus.port1_pad_command_histogram() {
        Some(hist) => {
            let mut any = false;
            for (cmd, &count) in hist.iter().enumerate() {
                if count == 0 {
                    continue;
                }
                any = true;
                println!("  cmd 0x{cmd:02x}: {count}");
            }
            if !any {
                println!("  (no controller commands observed)");
            }
        }
        None => println!("  (no pad attached)"),
    }

    println!("\n=== Port-1 first-byte histogram ===");
    let mut any = false;
    for (byte, &count) in bus.port1_first_byte_histogram().iter().enumerate() {
        if count == 0 {
            continue;
        }
        any = true;
        println!("  first 0x{byte:02x}: {count}");
    }
    if !any {
        println!("  (no transactions observed)");
    }

    let recent = bus.port1_pad_recent_commands();
    println!("\n=== Recent port-1 pad commands ===");
    if recent.is_empty() {
        println!("  (none)");
    } else {
        let cmds = recent
            .iter()
            .map(|cmd| format!("0x{cmd:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("  {cmds}");
    }

    let recent_first = bus.port1_recent_first_bytes();
    println!("\n=== Recent port-1 first bytes ===");
    if recent_first.is_empty() {
        println!("  (none)");
    } else {
        let bytes = recent_first
            .iter()
            .map(|b| format!("0x{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("  {bytes}");
    }

    let polls = bus.port1_recent_polls();
    println!("\n=== Recent port-1 0x42 polls ===");
    if polls.is_empty() {
        println!("  (none)");
    } else {
        let vblank_period = bus.vblank_period().max(1);
        for poll in polls {
            let tx = poll
                .tx
                .iter()
                .take(poll.len as usize)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            let rx = poll
                .rx
                .iter()
                .take(poll.len as usize)
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "  cyc={:>12} approx_vblank={:>5}  {}  tx=[{tx}]  rx=[{rx}]",
                poll.cycle,
                poll.cycle / vblank_period,
                if poll.complete {
                    "complete"
                } else {
                    "partial "
                }
            );
        }
    }
}

fn dump_visible_ppm(bus: &Bus, path: &str) -> std::io::Result<()> {
    use std::io::Write;

    let (rgba, width, height) = bus.gpu.display_rgba8();
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "P6\n{width} {height}\n255")?;
    for px in rgba.chunks_exact(4) {
        file.write_all(&px[..3])?;
    }
    Ok(())
}

#[cfg(feature = "trace-mmio")]
fn dump_trace(bus: &Bus) {
    let entries: Vec<_> = bus
        .mmio_trace
        .iter_chronological()
        .filter(|e| Sio0::contains(e.addr))
        .collect();
    if entries.is_empty() {
        println!("\nNo SIO0 MMIO accesses recorded.");
        return;
    }

    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            matches!(
                (e.addr - Sio0::BASE, e.kind),
                (0x0, MmioKind::R8 | MmioKind::R16 | MmioKind::R32)
                    | (0x0, MmioKind::W8 | MmioKind::W16 | MmioKind::W32)
                    | (0xA, MmioKind::W16 | MmioKind::W32)
                    | (0x8, MmioKind::W16 | MmioKind::W32)
                    | (0xE, MmioKind::W16 | MmioKind::W32)
            )
        })
        .collect();
    if filtered.is_empty() {
        println!("\nNo relevant SIO0 accesses captured.");
        return;
    }

    let txns = decode_transactions(&filtered);
    println!("\n=== SIO0 command histogram ===");
    let mut cmd_hist: BTreeMap<u8, u32> = BTreeMap::new();
    let mut id_hist: BTreeMap<u8, u32> = BTreeMap::new();
    for txn in &txns {
        if let Some(&cmd) = txn.tx_bytes.get(1) {
            *cmd_hist.entry(cmd).or_insert(0) += 1;
        }
        if let Some(&id) = txn.rx_bytes.first() {
            *id_hist.entry(id).or_insert(0) += 1;
        }
    }
    for (cmd, count) in &cmd_hist {
        println!("  cmd 0x{cmd:02x}: {count}");
    }

    println!("\n=== SIO0 ID histogram ===");
    for (id, count) in &id_hist {
        println!("  id  0x{id:02x}: {count}");
    }

    println!("\n=== Last 24 controller transactions ===");
    let skip = txns.len().saturating_sub(24);
    for txn in &txns[skip..] {
        println!(
            "  cyc={:>12}  ctrl={:#06x}  {}",
            txn.start_cycle,
            txn.ctrl,
            format_transaction(txn),
        );
    }

    println!("\n=== Last 48 relevant SIO0 accesses ===");
    let skip = filtered.len().saturating_sub(48);
    for e in &filtered[skip..] {
        println!(
            "  cyc={:>12}  {}  {:08x}  {:08x}",
            e.cycle,
            e.kind.tag(),
            e.addr,
            e.value
        );
    }
}

#[cfg(feature = "trace-mmio")]
#[derive(Default)]
struct Transaction {
    start_cycle: u64,
    ctrl: u16,
    tx_bytes: Vec<u8>,
    rx_bytes: Vec<u8>,
}

#[cfg(feature = "trace-mmio")]
fn decode_transactions(entries: &[&MmioEntry]) -> Vec<Transaction> {
    let mut txns = Vec::new();
    let mut current = Transaction::default();
    let mut joyn_selected = false;

    for e in entries {
        match (e.addr - Sio0::BASE, e.kind) {
            (0xA, MmioKind::W16 | MmioKind::W32) => {
                let ctrl = e.value as u16;
                let new_joyn = ctrl & (1 << 1) != 0;
                if joyn_selected && !new_joyn && !current.tx_bytes.is_empty() {
                    txns.push(std::mem::take(&mut current));
                }
                if new_joyn && !joyn_selected {
                    current = Transaction {
                        start_cycle: e.cycle,
                        ctrl,
                        ..Transaction::default()
                    };
                } else if new_joyn {
                    current.ctrl = ctrl;
                }
                joyn_selected = new_joyn;
            }
            (0x0, MmioKind::W8 | MmioKind::W16 | MmioKind::W32) => {
                if current.tx_bytes.is_empty() {
                    current.start_cycle = e.cycle;
                }
                current.tx_bytes.push(e.value as u8);
            }
            (0x0, MmioKind::R8 | MmioKind::R16 | MmioKind::R32) => {
                current.rx_bytes.push(e.value as u8);
            }
            _ => {}
        }
    }

    if !current.tx_bytes.is_empty() {
        txns.push(current);
    }
    txns
}

#[cfg(feature = "trace-mmio")]
fn format_transaction(txn: &Transaction) -> String {
    let mut out = String::new();
    let len = txn.tx_bytes.len().max(txn.rx_bytes.len());
    for i in 0..len {
        if i > 0 {
            out.push(' ');
        }
        let tx = txn
            .tx_bytes
            .get(i)
            .map(|b| format!(">{b:02x}"))
            .unwrap_or_else(|| ">--".into());
        let rx = txn
            .rx_bytes
            .get(i)
            .map(|b| format!("<{b:02x}"))
            .unwrap_or_else(|| "<--".into());
        out.push_str(&format!("{tx}/{rx}"));
    }
    out
}
