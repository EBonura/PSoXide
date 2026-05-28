//! Dump emitted instructions from an ISR that just ran, by
//! single-stepping around a known user step and printing each
//! instruction that executes inside `in_irq_handler() == true`.
//!
//! Usage: `cargo run --release -p emulator-core --example dump_isr_window -- <user_step_at_isr_entry> <post_fold_cycle>`
//!
//! Run ours up to the user step that triggers the ISR, then
//! log every in-ISR instruction we retire until we exit the ISR,
//! with PC + instr + key GPR state so we can cross-reference against
//! the BIOS ISR code.

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use std::path::Path;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};

fn main() {
    let trigger_step: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: dump_isr_window <trigger_user_step> [disc.cue|disc.bin]");
    let disc_path = std::env::args().nth(2);

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS");
    let mut bus = Bus::new(bios).expect("bus");
    if let Some(ref p) = disc_path {
        let disc = disc_support::load_disc_path(Path::new(p)).expect("disc");
        bus.cdrom.insert_disc(Some(disc));
        if std::env::var_os("PSOXIDE_NO_PAD").is_none() {
            bus.attach_digital_pad_port1();
        }
        if std::env::var_os("PSOXIDE_NO_MEMCARD").is_none() {
            bus.attach_memcard_port1(Vec::new());
        }
    }
    let mut cpu = Cpu::new();
    let held_buttons = std::env::var("PSOXIDE_PAD1")
        .ok()
        .and_then(|s| parse_u16_mask(&s))
        .unwrap_or(0);
    let pad_pulses = std::env::var("PSOXIDE_PAD1_PULSES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_pad_pulses(&s).expect("valid PSOXIDE_PAD1_PULSES"))
        .unwrap_or_default();
    let wants_pad = std::env::var_os("PSOXIDE_NO_PAD").is_none()
        && (held_buttons != 0 || !pad_pulses.is_empty());
    let mut current_pad_mask = u16::MAX;
    let max_isr_steps = std::env::var("PSOXIDE_ISR_DUMP_MAX")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2000);

    // Run folded steps up to the trigger step (one user step retires
    // per iteration, ISR body counted in the fold).
    let mut user_step = 0u64;
    while user_step < trigger_step - 1 {
        if wants_pad {
            sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
        }
        let was_in_isr = cpu.in_isr();
        cpu.step(&mut bus).expect("step");
        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                if wants_pad {
                    sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
                }
                cpu.step(&mut bus).expect("isr step");
            }
        }
        user_step += 1;
    }

    // Now one more user-side step that's expected to trigger the ISR.
    // Single-step through it + the ISR body, logging everything.
    let was_in_isr = cpu.in_isr();
    let trigger_pc = cpu.pc();
    let trigger_instr = bus.peek_instruction(trigger_pc).unwrap_or(0);
    let trigger_istat = bus.irq().stat();
    let trigger_imask = bus.irq().mask();
    let trigger_dicr = bus.read32(0x1f80_10f4);
    println!(
        "[trigger user step {trigger_step}]  pc=0x{trigger_pc:08x} instr=0x{trigger_instr:08x}  cyc_pre={} istat=0x{trigger_istat:03x} imask=0x{trigger_imask:03x} dicr=0x{trigger_dicr:08x}",
        bus.cycles(),
    );
    if wants_pad {
        sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
    }
    cpu.step(&mut bus).expect("step");
    println!(
        "  after user step: pc=0x{:08x} cyc={}  in_irq={}  in_isr={} istat=0x{:03x} imask=0x{:03x} dicr=0x{:08x}",
        cpu.pc(),
        bus.cycles(),
        cpu.in_irq_handler(),
        cpu.in_isr(),
        bus.irq().stat(),
        bus.irq().mask(),
        bus.read32(0x1f80_10f4),
    );

    if !was_in_isr && cpu.in_irq_handler() {
        let mut isr_n = 0;
        while cpu.in_irq_handler() {
            if wants_pad {
                sync_pad_mask(&mut bus, held_buttons, &pad_pulses, &mut current_pad_mask);
            }
            let pc = cpu.pc();
            let instr = bus.peek_instruction(pc).unwrap_or(0);
            let gpr_a0 = cpu.gpr(4);
            let gpr_t0 = cpu.gpr(8);
            let gpr_t2 = cpu.gpr(10);
            cpu.step(&mut bus).expect("isr step");
            println!(
                "  isr[{isr_n:>4}]  pc=0x{pc:08x} instr=0x{instr:08x}  cyc={}  a0=0x{gpr_a0:08x} t0=0x{gpr_t0:08x} t2=0x{gpr_t2:08x}",
                bus.cycles(),
            );
            isr_n += 1;
            if isr_n > max_isr_steps {
                println!("  ... truncating after {max_isr_steps} isr steps");
                break;
            }
        }
        println!(
            "isr length: {isr_n} instructions; final pc=0x{:08x} cyc={} istat=0x{:03x} imask=0x{:03x} dicr=0x{:08x}",
            cpu.pc(),
            bus.cycles(),
            bus.irq().stat(),
            bus.irq().mask(),
            bus.read32(0x1f80_10f4),
        );
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
