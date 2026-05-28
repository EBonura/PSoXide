//! Trace BIOS `B(0x07) DeliverEvent` and `B(0x20) UnDeliverEvent`
//! calls for the CDROM event class. This shows which BIOS-visible CD
//! events are actually being signaled.
//!
//! ```bash
//! cargo run --release -p emulator-core --example probe_deliverevent -- 120000000 "/path/to/game.cue"
//! ```

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::path::Path;

fn main() {
    let max_steps: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(120_000_000);
    let disc_path = std::env::args().nth(2);
    let held_buttons = std::env::var("PSOXIDE_PAD1")
        .ok()
        .and_then(|s| parse_u16_mask(&s))
        .unwrap_or(0);
    let pad_pulses = std::env::var("PSOXIDE_PAD1_PULSES")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| parse_pad_pulses(&s).expect("valid PSOXIDE_PAD1_PULSES"))
        .unwrap_or_default();

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS");
    let mut bus = Bus::new(bios).expect("bus");
    if let Some(ref path) = disc_path {
        let disc = disc_support::load_disc_path(Path::new(path)).expect("disc");
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();
        bus.attach_memcard_port1(Vec::new());
    }
    let mut cpu = Cpu::new();

    let mut seen = 0u32;
    let mut current_pad_mask = u16::MAX;

    for step in 0..max_steps {
        let vblank = bus.irq().raise_counts()[0];
        let pad_mask = effective_mask(held_buttons, &pad_pulses, vblank);
        if pad_mask != current_pad_mask {
            bus.set_port1_buttons(emulator_core::ButtonState::from_bits(pad_mask));
            current_pad_mask = pad_mask;
        }

        let pc = cpu.pc();
        let fn_no = cpu.gpr(9) as u8;
        if pc == 0x0000_00b0 && matches!(fn_no, 0x07 | 0x20) && cpu.gpr(4) == 0xf000_0003 {
            let kind = if fn_no == 0x07 {
                "DeliverEvent"
            } else {
                "UnDeliverEvent"
            };
            println!(
                "{kind} call@step={} class={:#010x} spec={:#06x} ra={:#010x} cycles={}",
                step,
                cpu.gpr(4),
                cpu.gpr(5),
                cpu.gpr(31),
                bus.cycles()
            );
            seen = seen.saturating_add(1);
            if seen >= 128 {
                break;
            }
        }

        let was_in_isr = cpu.in_isr();
        cpu.step(&mut bus).expect("step");

        if !was_in_isr && cpu.in_irq_handler() {
            while cpu.in_irq_handler() {
                let vblank = bus.irq().raise_counts()[0];
                let pad_mask = effective_mask(held_buttons, &pad_pulses, vblank);
                if pad_mask != current_pad_mask {
                    bus.set_port1_buttons(emulator_core::ButtonState::from_bits(pad_mask));
                    current_pad_mask = pad_mask;
                }
                cpu.step(&mut bus).expect("isr step");
            }
        }
    }
}
