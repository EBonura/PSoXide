//! Dump the raw CPU instructions hidden inside one folded user step.
//! `probe_fine_divergence` folds IRQ handlers into the user-side
//! record to match Redux's oracle protocol; this diagnostic shows the
//! local ISR path when a folded record has a cycle-only mismatch.

#[path = "support/disc.rs"]
mod disc_support;
#[path = "support/pad.rs"]
mod pad_support;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use emulator_core::scheduler::EventSlot;
use emulator_core::{Bus, ButtonState, Cpu};
#[cfg(feature = "trace-mmio")]
use emulator_core::{MmioKind, Sio0};
use pad_support::{effective_mask, parse_pad_pulses, parse_u16_mask, PadPulse};

const DEFAULT_BIOS: &str = "/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN";

fn main() {
    let start: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: probe_raw_irq_trace <completed_user_steps> <disc.cue|disc.bin>");
    let disc_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .expect("usage: probe_raw_irq_trace <completed_user_steps> <disc.cue|disc.bin>");

    let bios = std::fs::read(DEFAULT_BIOS).expect("BIOS");
    let disc = disc_support::load_disc_path(&disc_path).expect("disc");
    let mut bus = Bus::new(bios).expect("bus");
    bus.cdrom.insert_disc(Some(disc));
    let pad = PadRoute::from_env();
    let mut current_pad_mask = None;
    if pad.enabled() {
        bus.attach_digital_pad_port1();
        sync_pad_mask(&mut bus, &pad, &mut current_pad_mask);
    }
    let mut cpu = Cpu::new();

    for _ in 0..start {
        step_user(&mut cpu, &mut bus, &pad, &mut current_pad_mask);
    }
    bus.set_dma_log_enabled(std::env::var("PSOXIDE_RAW_DMA_LOG").is_ok());

    println!(
        "at user_step={start} pc=0x{:08x} cycles={} istat=0x{:03x} imask=0x{:03x} dicr=0x{:08x} cdflag=0x{:02x} lastcmd=0x{:02x}",
        cpu.pc(),
        bus.cycles(),
        bus.irq().stat(),
        bus.irq().mask(),
        bus.read32(0x1f80_10f4),
        bus.cdrom.irq_flag(),
        bus.cdrom.last_command()
    );

    let before_cycles = bus.cycles();
    let rec = cpu.step_traced(&mut bus).expect("next user step");
    sync_pad_mask(&mut bus, &pad, &mut current_pad_mask);
    println!(
        "user raw pc=0x{:08x} instr=0x{:08x} tick={} next_pc=0x{:08x} in_irq={} istat=0x{:03x} cdflag=0x{:02x} lastcmd=0x{:02x}",
        rec.pc,
        rec.instr,
        rec.tick,
        cpu.pc(),
        cpu.in_irq_handler() as u8,
        bus.irq().stat(),
        bus.cdrom.irq_flag(),
        bus.cdrom.last_command()
    );

    let max_raw = std::env::var("PSOXIDE_RAW_IRQ_MAX")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(2_000);
    let print_range = std::env::var("PSOXIDE_RAW_IRQ_RANGE")
        .ok()
        .and_then(|s| parse_range(&s));
    let print_sio = std::env::var_os("PSOXIDE_RAW_SIO").is_some();
    let summarize = std::env::var("PSOXIDE_RAW_SUMMARY").is_ok();
    let mut pc_counts = HashMap::<u32, u64>::new();
    let mut mem_summary = MemSummary::default();
    let mut raw = 0u64;
    while cpu.in_irq_handler() {
        let pc = cpu.pc();
        if summarize {
            *pc_counts.entry(pc).or_insert(0) += 1;
        }
        let instr = bus.peek_instruction(pc).unwrap_or(0xdead_beef);
        if summarize {
            mem_summary.observe(instr, &cpu);
        }
        let cycles_before = bus.cycles();
        let rec = cpu.step_traced(&mut bus).expect("isr step");
        sync_pad_mask(&mut bus, &pad, &mut current_pad_mask);
        raw += 1;
        if print_range.map_or(true, |(lo, hi)| (lo..=hi).contains(&pc)) {
            println!(
                "isr {raw:>5} pc=0x{pc:08x} instr=0x{instr:08x} before={cycles_before} after={} rec_pc=0x{:08x} istat=0x{:03x} imask=0x{:03x} dicr=0x{:08x} cdflag=0x{:02x} v0=0x{:08x} a0=0x{:08x} t0=0x{:08x} t1=0x{:08x} ra=0x{:08x}{}",
                bus.cycles(),
                rec.pc,
                bus.irq().stat(),
                bus.irq().mask(),
                bus.read32(0x1f80_10f4),
                bus.cdrom.irq_flag(),
                cpu.gpr(2),
                cpu.gpr(4),
                cpu.gpr(8),
                cpu.gpr(9),
                cpu.gpr(31),
                if print_sio {
                    sio_debug(&bus)
                } else {
                    String::new()
                },
            );
        }
        for (slot, scheduled_at, delay, target) in bus.drain_dma_log() {
            println!(
                "dma raw={raw:>5} slot={slot} scheduled_at={scheduled_at} delay={delay} target={target}"
            );
        }
        if raw > max_raw {
            println!("stopping after {max_raw} raw ISR instructions");
            break;
        }
    }

    println!(
        "done raw_isr={raw} pc=0x{:08x} cycles={} delta={} istat=0x{:03x} imask=0x{:03x}",
        cpu.pc(),
        bus.cycles(),
        bus.cycles().saturating_sub(before_cycles),
        bus.irq().stat(),
        bus.irq().mask()
    );
    if summarize {
        let mut top = pc_counts.into_iter().collect::<Vec<_>>();
        top.sort_by_key(|&(pc, count)| (std::cmp::Reverse(count), pc));
        println!("top_pcs:");
        for (pc, count) in top.into_iter().take(32) {
            let instr = bus.peek_instruction(pc).unwrap_or(0);
            println!("  pc=0x{pc:08x} instr=0x{instr:08x} count={count}");
        }
        mem_summary.print();
    }
    dump_relevant_mmio(&bus);
}

fn sio_debug(bus: &Bus) -> String {
    let sio = bus.sio0();
    let sio_target = bus
        .scheduler
        .target(EventSlot::Sio)
        .map(|target| target.to_string())
        .unwrap_or_else(|| "-".to_string());
    format!(
        " sio_stat=0x{:08x} sio_ctrl=0x{:04x} sio_irq={} sio_pending={} sio_rx={} sio_tx={} sio_busy={} sio_wait_ack={} sio_tfer={} sio_ack={} sio_ack_end={} sio_target={}",
        sio.debug_stat(),
        sio.debug_ctrl(),
        sio.debug_irq_latched() as u8,
        sio.debug_pending_irq() as u8,
        opt_u8_hex(sio.debug_rx()),
        opt_u8_hex(sio.debug_queued_tx()),
        sio.debug_transfer_busy() as u8,
        sio.debug_awaiting_ack() as u8,
        opt_u64(sio.debug_transfer_deadline()),
        opt_u64(sio.debug_ack_deadline()),
        opt_u64(sio.debug_ack_end_deadline()),
        sio_target,
    )
}

fn opt_u8_hex(value: Option<u8>) -> String {
    value
        .map(|v| format!("0x{v:02x}"))
        .unwrap_or_else(|| "-".to_string())
}

fn opt_u64(value: Option<u64>) -> String {
    value
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(feature = "trace-mmio")]
fn dump_relevant_mmio(bus: &Bus) {
    let entries = bus
        .mmio_trace
        .iter_chronological()
        .filter(|e| {
            let is_sio = Sio0::contains(e.addr)
                && matches!(
                    (e.addr - Sio0::BASE, e.kind),
                    (0x0, MmioKind::R8 | MmioKind::R16 | MmioKind::R32)
                        | (0x0, MmioKind::W8 | MmioKind::W16 | MmioKind::W32)
                        | (0x4, MmioKind::R16 | MmioKind::R32)
                        | (0x8, MmioKind::W16 | MmioKind::W32)
                        | (0xA, MmioKind::W16 | MmioKind::W32)
                        | (0xE, MmioKind::W16 | MmioKind::W32)
                );
            let is_cdrom = (0x1f80_1800..=0x1f80_1803).contains(&e.addr);
            let is_irq = matches!(e.addr, 0x1f80_1070 | 0x1f80_1074);
            let is_dma_irq = e.addr == 0x1f80_10f4;
            is_sio || is_cdrom || is_irq || is_dma_irq
        })
        .collect::<Vec<_>>();
    let skip = entries.len().saturating_sub(160);
    println!(
        "mmio_tail count={} showing={}",
        entries.len(),
        entries.len() - skip
    );
    for e in &entries[skip..] {
        println!(
            "  mmio cyc={:>12} {} addr=0x{:08x} value=0x{:08x}",
            e.cycle,
            e.kind.tag(),
            e.addr,
            e.value
        );
    }
}

#[cfg(not(feature = "trace-mmio"))]
fn dump_relevant_mmio(_bus: &Bus) {}

fn parse_range(text: &str) -> Option<(u32, u32)> {
    let (lo, hi) = text.split_once('-')?;
    Some((parse_u32(lo)?, parse_u32(hi)?))
}

fn parse_u32(text: &str) -> Option<u32> {
    let s = text.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        u32::from_str_radix(s, 16)
            .or_else(|_| s.parse::<u32>())
            .ok()
    }
}

struct PadRoute {
    base_mask: u16,
    pulses: Vec<PadPulse>,
}

impl PadRoute {
    fn from_env() -> Self {
        let base_mask = std::env::var("PSOXIDE_PAD1")
            .ok()
            .and_then(|s| parse_u16_mask(&s))
            .unwrap_or(0);
        let pulses = std::env::var("PSOXIDE_PAD1_PULSES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| parse_pad_pulses(&s).expect("PSOXIDE_PAD1_PULSES parses"))
            .unwrap_or_default();
        Self { base_mask, pulses }
    }

    fn enabled(&self) -> bool {
        self.base_mask != 0 || !self.pulses.is_empty()
    }
}

fn sync_pad_mask(bus: &mut Bus, route: &PadRoute, current_mask: &mut Option<u16>) {
    if !route.enabled() {
        return;
    }
    let next = effective_mask(route.base_mask, &route.pulses, bus.irq().raise_counts()[0]);
    if current_mask.is_some_and(|mask| mask == next) {
        return;
    }
    bus.set_port1_buttons(ButtonState::from_bits(next));
    *current_mask = Some(next);
}

fn step_user(cpu: &mut Cpu, bus: &mut Bus, pad: &PadRoute, current_pad_mask: &mut Option<u16>) {
    let was_in_isr = cpu.in_isr();
    cpu.step(bus).expect("step");
    sync_pad_mask(bus, pad, current_pad_mask);
    if !was_in_isr && cpu.in_irq_handler() {
        while cpu.in_irq_handler() {
            cpu.step(bus).expect("isr step");
            sync_pad_mask(bus, pad, current_pad_mask);
        }
    }
}

#[derive(Default)]
struct MemSummary {
    access_cycles: u64,
    by_op: BTreeMap<&'static str, u64>,
    by_region: BTreeMap<&'static str, u64>,
    by_op_region: BTreeMap<(&'static str, &'static str), u64>,
}

impl MemSummary {
    fn observe(&mut self, instr: u32, cpu: &Cpu) {
        let Some(access) = decode_mem_access(instr, cpu) else {
            return;
        };
        if access.cycles == 0 {
            return;
        }
        self.access_cycles += access.cycles as u64;
        *self.by_op.entry(access.op).or_default() += access.cycles as u64;
        *self.by_region.entry(access.region).or_default() += access.cycles as u64;
        *self
            .by_op_region
            .entry((access.op, access.region))
            .or_default() += access.cycles as u64;
    }

    fn print(&self) {
        println!("mem_access_cycles={}", self.access_cycles);
        println!("mem_by_op:");
        for (op, count) in &self.by_op {
            println!("  {op:<5} {count}");
        }
        println!("mem_by_region:");
        for (region, count) in &self.by_region {
            println!("  {region:<10} {count}");
        }
        println!("mem_by_op_region:");
        for ((op, region), count) in &self.by_op_region {
            println!("  {op:<5} {region:<10} {count}");
        }
    }
}

struct MemAccess {
    op: &'static str,
    region: &'static str,
    cycles: u8,
}

fn decode_mem_access(instr: u32, cpu: &Cpu) -> Option<MemAccess> {
    let opcode = instr >> 26;
    let rs = ((instr >> 21) & 0x1f) as u8;
    let addr = cpu.gpr(rs).wrapping_add((instr as i16) as i32 as u32);
    let aligned = match opcode {
        0x21 | 0x25 | 0x29 => addr & 1 == 0, // LH/LHU/SH
        0x23 | 0x2b => addr & 3 == 0,        // LW/SW
        _ => true,
    };
    if !aligned {
        return None;
    }
    let (op, cycles) = match opcode {
        0x20 => ("LB", 1),
        0x21 => ("LH", 1),
        0x22 => ("LWL", 1),
        0x23 => ("LW", 1),
        0x24 => ("LBU", 1),
        0x25 => ("LHU", 1),
        0x26 => ("LWR", 1),
        0x28 => ("SB", 1),
        0x29 => ("SH", 1),
        0x2a => ("SWL", 2),
        0x2b => ("SW", 1),
        0x2e => ("SWR", 2),
        0x32 => ("LWC2", 1),
        0x3a => ("SWC2", 1),
        _ => return None,
    };
    Some(MemAccess {
        op,
        region: region_name(to_physical_probe(addr)),
        cycles,
    })
}

fn to_physical_probe(virt: u32) -> u32 {
    match virt >> 29 {
        0b100 | 0b101 => virt & 0x1fff_ffff,
        _ => virt,
    }
}

fn region_name(phys: u32) -> &'static str {
    match phys {
        0x0000_0000..=0x007f_ffff => "ram",
        0x1f00_0000..=0x1f7f_ffff => "exp1",
        0x1f80_0000..=0x1f80_03ff => "scratch",
        0x1f80_1000..=0x1f80_1fff => "io",
        0x1f80_2000..=0x1f80_ffff => "exp2",
        0x1fc0_0000..=0x1fc7_ffff => "bios",
        _ => "unmapped",
    }
}
