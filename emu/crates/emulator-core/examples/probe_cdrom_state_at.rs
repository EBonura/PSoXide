//! Run our CDROM to step N, dump its state: current LBA, mode,
//! pending events. Used to cross-check against Redux when sector
//! data differs between them.

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use emulator_core::{Bus, Cpu};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask};
use std::path::Path;

fn main() {
    let n: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(90_146_543);
    let disc_path = std::env::args().nth(2);

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS");
    let mut bus = Bus::new(bios).expect("bus");
    let irq_log_cap = std::env::var("PSOXIDE_CDROM_IRQ_LOG_CAP")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(2048);
    bus.cdrom.enable_irq_log(irq_log_cap);
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
    let mut current_pad_mask = u16::MAX;
    for _ in 0..n {
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

    println!("=== CDROM state at step {n} (cycles={}) ===", bus.cycles());
    println!("irq_flag         : {}", bus.cdrom.irq_flag());
    println!("index            : {}", bus.cdrom.index_value());
    println!("irq_mask         : 0x{:02x}", bus.cdrom.irq_mask_value());
    println!("data_fifo_pops   : {}", bus.cdrom.data_fifo_pops());
    println!(
        "dataready_suppressed: {}",
        bus.cdrom.data_ready_suppressed()
    );
    println!(
        "data_fifo        : len={} words={} ready={} armed={}",
        bus.cdrom.data_fifo_len(),
        bus.cdrom.data_fifo_words(),
        bus.cdrom.data_fifo_ready(),
        bus.cdrom.data_transfer_armed(),
    );
    let (file, channel) = bus.cdrom.debug_xa_filter();
    println!(
        "read_state       : mode=0x{:02x} filter=({file},{channel}) read_lba={}",
        bus.cdrom.debug_mode(),
        bus.cdrom.debug_read_lba(),
    );
    if let Some((header, subheader)) = bus.cdrom.debug_last_sector() {
        println!(
            "last_sector      : header=[{}] subheader=[{}]",
            fmt_bytes(&header),
            fmt_bytes(&subheader),
        );
    } else {
        println!("last_sector      : none");
    }
    println!("cd_audio_queue   : {}", bus.cdrom.cd_audio_queue_len());
    println!("sector_events    : {}", bus.cdrom.sector_events_scheduled);
    println!("pending_queue_len: {}", bus.cdrom.pending_queue_len());
    if let Some((deadline, irq)) = bus.cdrom.next_pending_event() {
        println!(
            "next_pending     : {:?} at {deadline} (in {})",
            irq,
            deadline as i64 - bus.cycles() as i64
        );
    }
    println!("irq_stat         : 0x{:03x}", bus.irq().stat());
    println!("irq_mask         : 0x{:03x}", bus.irq().mask());

    let irq_names = [
        "VBlank",
        "Gpu",
        "Cdrom",
        "Dma",
        "Timer0",
        "Timer1",
        "Timer2",
        "Controller",
        "Sio",
        "Spu",
        "Lightpen",
    ];
    println!();
    println!("irq raises:");
    for (name, count) in irq_names.iter().zip(bus.irq().raise_counts()) {
        if count > 0 {
            println!("  {name:<10} {count}");
        }
    }

    println!();
    println!("scheduler:");
    use emulator_core::scheduler::EventSlot;
    for slot in [
        EventSlot::Sio,
        EventSlot::Sio1,
        EventSlot::Cdr,
        EventSlot::CdRead,
        EventSlot::GpuDma,
        EventSlot::MdecOutDma,
        EventSlot::SpuDma,
        EventSlot::MdecInDma,
        EventSlot::GpuOtcDma,
        EventSlot::CdrDma,
        EventSlot::CdrPlay,
        EventSlot::CdrDbuf,
        EventSlot::CdrLid,
        EventSlot::SpuAsync,
        EventSlot::VBlank,
    ] {
        if let Some(target) = bus.scheduler.target(slot) {
            println!(
                "  {slot:?}: target={target} in {}",
                target as i64 - bus.cycles() as i64
            );
        }
    }

    println!();
    println!(
        "=== CDROM IRQ log ({} entries) ===",
        bus.cdrom.cdrom_irq_log.len()
    );
    let names = [
        "None",
        "DataReady",
        "Complete",
        "Acknowledge",
        "DataEnd",
        "Error",
    ];
    for (i, &(cyc, ty)) in bus.cdrom.cdrom_irq_log.iter().enumerate() {
        let name = names.get(ty as usize).copied().unwrap_or("?");
        println!("  #{i:>3} cyc={cyc:>12} type={ty} ({name})");
    }
    let cmd_hist = bus.cdrom.command_histogram();
    println!();
    println!("command histogram:");
    for (i, c) in cmd_hist.iter().enumerate().take(32) {
        if *c > 0 {
            println!("  0x{i:02X}: {c}");
        }
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

fn fmt_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
