//! Sample the CPU PC at a few checkpoints to see whether we're in
//! BIOS ROM, RAM user code, or spinning somewhere. For debugging games
//! that hang past a menu transition (MGS).
//!
//! ```bash
//! PSOXIDE_PC_CHECKPOINTS=1085000000,1100000000 \
//! PSOXIDE_PAD1_PULSES="0x0008@600+8,0x4000@650+8" \
//! cargo run --release -p emulator-core --example probe_pc_progression -- "/path/to/game.cue"
//! ```

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::{collections::HashMap, path::Path};

const DEFAULT_CHECKPOINTS: &[u64] = &[
    100_000_000,
    200_000_000,
    250_000_000,
    300_000_000,
    350_000_000,
    400_000_000,
    450_000_000,
    500_000_000,
];

fn main() {
    let disc_path = std::env::args().nth(1);
    let checkpoints = std::env::var("PSOXIDE_PC_CHECKPOINTS")
        .ok()
        .map(|s| parse_checkpoints(&s).expect("valid PSOXIDE_PC_CHECKPOINTS"))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CHECKPOINTS.to_vec());
    let sample_window = std::env::var("PSOXIDE_PC_SAMPLE_WINDOW")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10_000);
    let top_limit = std::env::var("PSOXIDE_PC_TOP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(12);
    let held_buttons = std::env::var("PSOXIDE_PAD1")
        .ok()
        .and_then(|s| parse_u16_mask(&s))
        .unwrap_or(0);
    let pad_pulses = std::env::var("PSOXIDE_PAD1_PULSES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_pad_pulses(&s).expect("valid PSOXIDE_PAD1_PULSES"))
        .unwrap_or_default();

    let bios = std::fs::read("/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN").expect("BIOS");
    let mut bus = Bus::new(bios).expect("bus");
    if let Some(ref p) = disc_path {
        let disc = disc_support::load_disc_path(Path::new(p)).expect("disc");
        bus.cdrom.insert_disc(Some(disc));
    }
    bus.attach_digital_pad_port1();
    if std::env::var_os("PSOXIDE_NO_MEMCARD").is_none() {
        bus.attach_memcard_port1(Vec::new());
    }
    let mut cpu = Cpu::new();
    let mut current_pad_mask = u16::MAX;

    let mut cur = 0u64;
    let mut recent_pcs: HashMap<u32, u64> = HashMap::new();
    let mut recent_samples = 0u64;
    let mut last_sector_events = 0u64;
    for target in checkpoints {
        while cur < target {
            sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
            if target.saturating_sub(cur) <= sample_window {
                record_pc(&mut recent_pcs, &mut recent_samples, cpu.pc());
            }
            let was_in_isr = cpu.in_isr();
            cpu.step(&mut bus).expect("step");
            if !was_in_isr && cpu.in_irq_handler() {
                while cpu.in_irq_handler() {
                    sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
                    if target.saturating_sub(cur) <= sample_window {
                        record_pc(&mut recent_pcs, &mut recent_samples, cpu.pc());
                    }
                    cpu.step(&mut bus).expect("isr step");
                }
            }
            cur += 1;
        }
        let pc = cpu.pc();
        let region = match pc {
            p if p >= 0xBFC0_0000 => "BIOS-ROM",
            p if (0x8000_0000..0x8020_0000).contains(&p) => "RAM-user",
            p if (0xA000_0000..0xA020_0000).contains(&p) => "RAM-user(A)",
            _ => "???",
        };
        let sec_delta = bus.cdrom.sector_events_scheduled - last_sector_events;
        last_sector_events = bus.cdrom.sector_events_scheduled;
        println!(
            "step={target:>12}  cyc={:>12}  vblank={:>5}  pc=0x{pc:08x} [{region}]  \
             samples={} unique_pcs={} sec_events+{}",
            bus.cycles(),
            bus.irq().raise_counts()[0],
            recent_samples,
            recent_pcs.len(),
            sec_delta,
        );
        print_top_pcs(&recent_pcs, recent_samples, top_limit);
        recent_pcs.clear();
        recent_samples = 0;
    }
}

fn sync_pad_mask(
    bus: &mut Bus,
    held_buttons: u16,
    pad_pulses: &[pad_support::PadPulse],
    current_pad_mask: &mut u16,
) {
    let vblank = bus.irq().raise_counts()[0];
    let pad_mask = effective_mask(held_buttons, pad_pulses, vblank);
    if pad_mask != *current_pad_mask {
        bus.set_port1_buttons(emulator_core::ButtonState::from_bits(pad_mask));
        *current_pad_mask = pad_mask;
    }
}

fn record_pc(recent_pcs: &mut HashMap<u32, u64>, recent_samples: &mut u64, pc: u32) {
    *recent_pcs.entry(pc).or_insert(0) += 1;
    *recent_samples += 1;
}

fn print_top_pcs(recent_pcs: &HashMap<u32, u64>, recent_samples: u64, top_limit: usize) {
    let mut top = recent_pcs
        .iter()
        .map(|(&pc, &count)| (pc, count))
        .collect::<Vec<_>>();
    top.sort_by_key(|&(pc, count)| (std::cmp::Reverse(count), pc));
    for (pc, count) in top.into_iter().take(top_limit) {
        let pct = if recent_samples == 0 {
            0.0
        } else {
            (count as f64 * 100.0) / recent_samples as f64
        };
        println!("  pc=0x{pc:08x} count={count:>8} pct={pct:>5.2}%");
    }
}

fn parse_checkpoints(text: &str) -> Result<Vec<u64>, String> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let checkpoint = part
            .parse()
            .map_err(|_| format!("invalid checkpoint `{part}`"))?;
        out.push(checkpoint);
    }
    out.sort_unstable();
    Ok(out)
}
