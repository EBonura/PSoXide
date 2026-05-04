//! Log visits to selected PCs while booting a disc.

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::path::Path;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let limit = args
        .first()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(100_000_000);
    let pcs = args
        .get(1)
        .map(|s| parse_addrs(s))
        .filter(|v| !v.is_empty())
        .expect("usage: probe_pc_hits <steps> <pc[,pc...]> <disc.cue|disc.bin>");
    let disc_path = args
        .get(2)
        .expect("usage: probe_pc_hits <steps> <pc[,pc...]> <disc.cue|disc.bin>");
    let held_buttons = std::env::var("PSOXIDE_PAD1")
        .ok()
        .and_then(|s| parse_u16_mask(&s))
        .unwrap_or(0);
    let pad_pulses = std::env::var("PSOXIDE_PAD1_PULSES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_pad_pulses(&s).expect("valid PSOXIDE_PAD1_PULSES"))
        .unwrap_or_default();
    let log_start_step = std::env::var("PSOXIDE_PC_HIT_START")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);

    let bios = std::fs::read("/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN").expect("BIOS");
    let disc = disc_support::load_disc_path(Path::new(disc_path)).expect("disc");
    let mut bus = Bus::new(bios).expect("bus");
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    bus.attach_memcard_port1(Vec::new());
    let mut cpu = Cpu::new();
    let mut current_pad_mask = u16::MAX;

    let mut hits = 0usize;
    let hit_limit = std::env::var("PSOXIDE_PC_HIT_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200);

    let mut step = 1u64;
    while step <= limit {
        sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);

        if maybe_log_hit(step, log_start_step, &pcs, &cpu, &bus, &mut hits, hit_limit) {
            return;
        }
        let was_in_isr = cpu.in_isr();
        cpu.step(&mut bus).expect("step");
        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
                if maybe_log_hit(step, log_start_step, &pcs, &cpu, &bus, &mut hits, hit_limit) {
                    return;
                }
                cpu.step(&mut bus).expect("isr step");
            }
        }
        step += 1;
    }

    println!("done. hits={hits}");
}

fn maybe_log_hit(
    step: u64,
    log_start_step: u64,
    pcs: &[u32],
    cpu: &Cpu,
    bus: &Bus,
    hits: &mut usize,
    hit_limit: usize,
) -> bool {
    let pc = cpu.pc();
    if step < log_start_step || !pcs.contains(&pc) {
        return false;
    }
    *hits += 1;
    println!(
        "hit={:>4} step={step:>10} cyc={:>12} pc=0x{pc:08x} instr=0x{:08x} \
         ra=0x{:08x} sp=0x{:08x} a0=0x{:08x} a1=0x{:08x} a2=0x{:08x} a3=0x{:08x} \
         v0=0x{:08x} v1=0x{:08x} in_isr={}",
        *hits,
        bus.cycles(),
        bus.peek_instruction(pc).unwrap_or(0),
        cpu.gpr(31),
        cpu.gpr(29),
        cpu.gpr(4),
        cpu.gpr(5),
        cpu.gpr(6),
        cpu.gpr(7),
        cpu.gpr(2),
        cpu.gpr(3),
        cpu.in_irq_handler(),
    );
    if *hits >= hit_limit {
        println!("stopping after {hits} hits");
        return true;
    }
    false
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

fn parse_addrs(spec: &str) -> Vec<u32> {
    spec.split(',')
        .filter_map(|part| parse_u32(part.trim()))
        .collect()
}

fn parse_u32(text: &str) -> Option<u32> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        text.parse().ok()
    }
}
