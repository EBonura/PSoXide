//! Trace every change to a given RAM word, step-by-step (no ISR
//! folding) so the actual writing instruction's PC is captured --
//! including writes from inside an IRQ handler. Set
//! `PSOXIDE_MEM_WRITE_START=<user-steps>` to fast-forward with the
//! normal folded step model before entering the raw watch window.
//!
//! ```bash
//! cargo run --release -p emulator-core --example probe_mem_writes -- 0x800ED294 89184518
//! ```

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, Cpu, DISC_FAST_BOOT_WARMUP_STEPS,
};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::path::Path;

fn parse_hex(s: &str) -> u32 {
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16).expect("bad hex")
    } else {
        s.parse::<u32>().expect("bad decimal")
    }
}

fn main() {
    let mut fastboot = false;
    let mut args = Vec::new();
    for arg in std::env::args().skip(1) {
        if arg == "--fastboot" {
            fastboot = true;
        } else {
            args.push(arg);
        }
    }
    let target_addrs = args
        .first()
        .map(|s| parse_addrs(s))
        .expect("usage: probe_mem_writes <addr[,addr...]> <up_to_step>");
    let up_to_step: u64 = args
        .get(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000_000);
    let start_user_steps = std::env::var("PSOXIDE_MEM_WRITE_START")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let disc_path = args.get(2);
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
    let mut cpu = Cpu::new();
    if let Some(ref path) = disc_path {
        let disc = disc_support::load_disc_path(Path::new(path)).expect("disc");
        if fastboot {
            warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
                .expect("BIOS warmup");
            fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false).expect("fast boot");
        }
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();
        if std::env::var_os("PSOXIDE_NO_MEMCARD").is_none() {
            bus.attach_memcard_port1(Vec::new());
        }
    }

    let mut current_pad_mask = u16::MAX;
    for _ in 0..start_user_steps {
        sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
        let was_in_isr = cpu.in_isr();
        cpu.step(&mut bus).expect("step");
        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
                cpu.step(&mut bus).expect("isr step");
            }
        }
    }

    let mut targets = target_addrs
        .into_iter()
        .map(|addr| {
            let target_word = addr & !3;
            let last = bus.peek_instruction(target_word).unwrap_or(0);
            (target_word, last, 0usize)
        })
        .collect::<Vec<_>>();
    let mut total_hits = 0usize;

    // Pure single-stepping without ISR folding, so we can attribute
    // each write to the exact PC (user or ISR). With no start offset,
    // `up_to_step` keeps its historical user-side-equivalent meaning;
    // with `PSOXIDE_MEM_WRITE_START`, it is the raw watch-window size.
    let max_raw_steps = if start_user_steps == 0 {
        up_to_step.saturating_mul(2)
    } else {
        up_to_step
    };
    for raw_step in 1..=max_raw_steps {
        sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
        let pre_pc = cpu.pc();
        let pre_ra = cpu.gpr(31);
        let pre_args = [cpu.gpr(4), cpu.gpr(5), cpu.gpr(6), cpu.gpr(7)];
        cpu.step(&mut bus).expect("step");
        for (target_word, last, hits) in targets.iter_mut() {
            let w = bus.peek_instruction(*target_word).unwrap_or(0);
            if w == *last {
                continue;
            }
            *hits += 1;
            total_hits += 1;
            let b = w.to_le_bytes();
            let o = last.to_le_bytes();
            println!(
                "raw_step={raw_step:>11}  cyc={:>12}  pc=0x{pre_pc:08x}  \
                 addr=0x{target_word:08x}  \
                 ra=0x{pre_ra:08x}  a0=0x{:08x} a1=0x{:08x} a2=0x{:08x} a3=0x{:08x}  \
                 in_isr={}  word 0x{w:08x} ({:02x} {:02x} {:02x} {:02x}) \
                was 0x{:08x} ({:02x} {:02x} {:02x} {:02x})",
                bus.cycles(),
                pre_args[0],
                pre_args[1],
                pre_args[2],
                pre_args[3],
                cpu.in_irq_handler(),
                b[0],
                b[1],
                b[2],
                b[3],
                *last,
                o[0],
                o[1],
                o[2],
                o[3],
            );
            *last = w;
            if total_hits >= 500 {
                println!("... stopping after 500 total hits");
                return;
            }
        }
    }
    println!("done.");
    for (target_word, _, hits) in targets {
        println!("  {target_word:#010x}: {hits} writes");
    }
}

fn parse_addrs(text: &str) -> Vec<u32> {
    text.split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| parse_hex(part.trim()))
        .collect()
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
