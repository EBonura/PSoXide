//! Log visits to selected PCs while booting a disc.

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::path::Path;

#[derive(Clone)]
struct WatchWord {
    label: String,
    addr: u32,
    deref: bool,
}

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

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS");
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
    let watch_words = std::env::var("PSOXIDE_PC_HIT_WATCH_WORDS")
        .ok()
        .map(|s| parse_watch_words(&s))
        .unwrap_or_default();
    let spu_pump_cycles = std::env::var("PSOXIDE_SPU_PUMP_CYCLES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| std::env::var_os("PSOXIDE_SPU_PUMP").map(|_| 560_000));
    let spu_pump_samples = std::env::var("PSOXIDE_SPU_PUMP_SAMPLES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(735);
    let mut cycles_at_last_pump = 0u64;

    let mut step = 1u64;
    while step <= limit {
        sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);

        if maybe_log_hit(
            step,
            log_start_step,
            &pcs,
            &cpu,
            &bus,
            &watch_words,
            &mut hits,
            hit_limit,
        ) {
            return;
        }
        let was_in_isr = cpu.in_isr();
        cpu.step(&mut bus).expect("step");
        maybe_pump_spu(
            &mut bus,
            spu_pump_cycles,
            spu_pump_samples,
            &mut cycles_at_last_pump,
        );
        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
                if maybe_log_hit(
                    step,
                    log_start_step,
                    &pcs,
                    &cpu,
                    &bus,
                    &watch_words,
                    &mut hits,
                    hit_limit,
                ) {
                    return;
                }
                cpu.step(&mut bus).expect("isr step");
                maybe_pump_spu(
                    &mut bus,
                    spu_pump_cycles,
                    spu_pump_samples,
                    &mut cycles_at_last_pump,
                );
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
    watch_words: &[WatchWord],
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
         v0=0x{:08x} v1=0x{:08x} s0=0x{:08x} s1=0x{:08x} s2=0x{:08x} s3=0x{:08x} \
         in_isr={}",
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
        cpu.gpr(16),
        cpu.gpr(17),
        cpu.gpr(18),
        cpu.gpr(19),
        cpu.in_irq_handler(),
    );
    for (label, value) in [
        ("ra", cpu.gpr(31)),
        ("a0", cpu.gpr(4)),
        ("a1", cpu.gpr(5)),
        ("a2", cpu.gpr(6)),
        ("a3", cpu.gpr(7)),
    ] {
        if let Some(text) = peek_c_string(bus, value, 96) {
            println!("         {label}_str: {:?}", text);
        }
    }
    if !watch_words.is_empty() {
        println!("         watch: {}", format_watch_words(bus, watch_words));
    }
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

fn maybe_pump_spu(
    bus: &mut Bus,
    pump_cycles: Option<u64>,
    pump_samples: usize,
    cycles_at_last_pump: &mut u64,
) {
    let Some(pump_cycles) = pump_cycles else {
        return;
    };
    if bus.cycles().saturating_sub(*cycles_at_last_pump) > pump_cycles {
        *cycles_at_last_pump = bus.cycles();
        bus.run_spu_samples(pump_samples);
        let _ = bus.spu.drain_audio();
    }
}

fn parse_addrs(spec: &str) -> Vec<u32> {
    spec.split(',')
        .filter_map(|part| parse_u32(part.trim()))
        .collect()
}

fn parse_watch_words(spec: &str) -> Vec<WatchWord> {
    spec.split(',')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let (label, addr_text) = match part.split_once(':') {
                Some((label, addr)) => (label.trim().to_string(), addr.trim()),
                None => (part.to_string(), part),
            };
            let (deref, addr_text) = match addr_text.strip_prefix('*') {
                Some(rest) => (true, rest.trim()),
                None => (false, addr_text),
            };
            let addr = parse_u32(addr_text)?;
            Some(WatchWord { label, addr, deref })
        })
        .collect()
}

fn format_watch_words(bus: &Bus, watch_words: &[WatchWord]) -> String {
    watch_words
        .iter()
        .map(|watch| {
            let value = peek_word(bus, watch.addr);
            if watch.deref {
                let pointed = peek_word(bus, value);
                format!(
                    "{}@0x{:08x}=0x{:08x}->0x{:08x}",
                    watch.label, watch.addr, value, pointed
                )
            } else {
                format!("{}@0x{:08x}=0x{:08x}", watch.label, watch.addr, value)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn peek_word(bus: &Bus, addr: u32) -> u32 {
    bus.peek_instruction(addr).unwrap_or(0)
}

fn peek_c_string(bus: &Bus, addr: u32, max_len: usize) -> Option<String> {
    let phys = addr & 0x1fff_ffff;
    if !(phys < 0x0020_0000 || (0x1fc0_0000..0x1fc8_0000).contains(&phys)) {
        return None;
    }
    let mut bytes = Vec::new();
    for offset in 0..max_len {
        let byte = bus.try_read8(addr.wrapping_add(offset as u32))?;
        if byte == 0 {
            break;
        }
        if byte == b'\n' || byte == b'\r' || byte == b'\t' || (0x20..=0x7e).contains(&byte) {
            bytes.push(byte);
        } else {
            return None;
        }
    }
    if bytes.len() < 2 {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn parse_u32(text: &str) -> Option<u32> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        text.parse().ok()
    }
}
