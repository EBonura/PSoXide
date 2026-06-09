//! Trace the GTE (COP2) function-ops the gameplay scene actually runs, with
//! their full input/output register state, so we can replay the *real* inputs
//! on hardware instead of synthetic ones.
//!
//! Usage: probe_gameplay_gte_trace <steps> <disc.cue>
//!
//! For each COP2 function-op (RTPS/RTPT/NCLIP/...) it snapshots all 32 data +
//! 32 control registers BEFORE the op (the inputs the guest set up), runs it,
//! then reads the outputs + FLAG (control reg 31). It reports:
//!   - a per-mnemonic distribution (how many of each op the scene runs),
//!   - how many fire the GTE FLAG (overflow/saturation = the divergence-prone
//!     cases),
//!   - a deduped sample of FLAG-firing ops with full state, ready to paste into
//!     the hardware-tests GTE battery.

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::{fast_boot_disc, Bus, Cpu};
use std::collections::HashSet;
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let steps: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(300_000_000);
    let disc_path = args.next().expect("usage: probe_gameplay_gte_trace <steps> <disc.cue>");

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS at emu/bios/SCPH1001.BIN");
    let mut bus = Bus::new(bios).expect("bus");
    let mut cpu = Cpu::new();
    let disc = disc_support::load_disc_path(Path::new(&disc_path)).expect("disc readable");
    fast_boot_disc(&mut bus, &mut cpu, &disc).expect("fast boot");
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    // Get past the "ANALOG MODE REQUIRED" gate into actual gameplay, matching
    // the frontend's headless playtest pad (attach_headless_playtest_pad).
    let _ = bus.force_port1_analog_mode();

    let mut total_func = 0u64;
    let mut reg_moves = 0u64; // MTC2/MFC2/CTC2/CFC2 (COP2 register moves)
    let mut by_op = [0u64; 64]; // count per function id (instr & 0x3F)
    let mut flag_hits = 0u64;
    let mut seen = HashSet::new(); // dedup captured samples by (instr, input hash)
    let mut samples: Vec<Captured> = Vec::new();
    const MAX_SAMPLES: usize = 24;

    for _ in 1..=steps {
        let pc = cpu.pc();
        let instr = bus.peek_instruction(pc).unwrap_or(0);
        let is_cop2 = (instr >> 26) == 0x12;
        let is_gte_func = is_cop2 && (instr & (1 << 25)) != 0;
        if is_cop2 && !is_gte_func {
            reg_moves += 1;
        }

        let inputs = is_gte_func.then(|| snapshot(&cpu));

        if cpu.step(&mut bus).is_err() {
            break;
        }

        if let Some(input) = inputs {
            total_func += 1;
            let func = (instr & 0x3F) as usize;
            by_op[func] += 1;
            let flag = cpu.cop2().read_control(31);
            let flag_master = flag & 0x8000_0000 != 0;
            if flag_master {
                flag_hits += 1;
            }
            // Capture FLAG-firing ops (divergence-prone) until we have enough.
            if flag_master && samples.len() < MAX_SAMPLES {
                let key = (instr, hash_regs(&input.0), hash_regs(&input.1));
                if seen.insert(key) {
                    samples.push(Captured {
                        instr,
                        input,
                        out_data: read_data_all(&cpu),
                        flag,
                    });
                }
            }
        }

        // Drain the IRQ handler so streaming/vblank progress.
        let mut guard = 0;
        while cpu.in_irq_handler() && guard < 100_000 {
            if cpu.step(&mut bus).is_err() {
                break;
            }
            guard += 1;
        }
    }

    // Confirm the probe actually reached the gameplay render: dump the final
    // display + report nonzero pixels. A black frame means we never got there.
    let (rgba, w, h) = bus.gpu.display_rgba8();
    let nonzero = rgba.chunks_exact(4).filter(|p| p[0] | p[1] | p[2] != 0).count();
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    let _ = std::fs::write("/tmp/gte-probe-frame.ppm", ppm);

    println!("=== gameplay GTE trace: {disc_path} ({steps} steps) ===");
    println!("final display: {w}x{h}  nonzero_px={nonzero}  -> /tmp/gte-probe-frame.ppm");
    println!("COP2 register moves (MTC2/MFC2/CTC2/CFC2): {reg_moves}");
    println!("total COP2 function-ops: {total_func}");
    println!("FLAG-firing (overflow/saturation) ops: {flag_hits}");
    println!();
    println!("--- op distribution ---");
    for (func, &count) in by_op.iter().enumerate() {
        if count > 0 {
            println!("  {:<10} (0x{:02x}): {count}", mnemonic(func as u32), func);
        }
    }
    println!();
    println!("--- {} deduped FLAG-firing samples (full state) ---", samples.len());
    for (i, s) in samples.iter().enumerate() {
        println!(
            "[{i}] {} instr=0x{:08x} FLAG=0x{:08x}",
            mnemonic(s.instr & 0x3F),
            s.instr,
            s.flag
        );
        print!("  ctrl:");
        for (j, v) in s.input.1.iter().enumerate() {
            if *v != 0 {
                print!(" c{j}=0x{v:08x}");
            }
        }
        println!();
        print!("  data_in:");
        for (j, v) in s.input.0.iter().enumerate() {
            if *v != 0 {
                print!(" d{j}=0x{v:08x}");
            }
        }
        println!();
        print!("  data_out:");
        for (j, v) in s.out_data.iter().enumerate() {
            if *v != 0 {
                print!(" d{j}=0x{v:08x}");
            }
        }
        println!();
    }
}

struct Captured {
    instr: u32,
    input: ([u32; 32], [u32; 32]), // (data, control) before
    out_data: [u32; 32],
    flag: u32,
}

fn snapshot(cpu: &Cpu) -> ([u32; 32], [u32; 32]) {
    (read_data_all(cpu), read_control_all(cpu))
}

fn read_data_all(cpu: &Cpu) -> [u32; 32] {
    let mut d = [0u32; 32];
    for (i, slot) in d.iter_mut().enumerate() {
        *slot = cpu.cop2().read_data(i as u8);
    }
    d
}

fn read_control_all(cpu: &Cpu) -> [u32; 32] {
    let mut c = [0u32; 32];
    for (i, slot) in c.iter_mut().enumerate() {
        *slot = cpu.cop2().read_control(i as u8);
    }
    c
}

fn hash_regs(regs: &[u32; 32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &v in regs {
        h = (h ^ v as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

fn mnemonic(func: u32) -> &'static str {
    match func {
        0x01 => "RTPS",
        0x06 => "NCLIP",
        0x0C => "OP",
        0x10 => "DPCS",
        0x11 => "INTPL",
        0x12 => "MVMVA",
        0x13 => "NCDS",
        0x14 => "CDP",
        0x16 => "NCDT",
        0x1B => "NCCS",
        0x1C => "CC",
        0x1E => "NCS",
        0x20 => "NCT",
        0x28 => "SQR",
        0x29 => "DCPL",
        0x2A => "DPCT",
        0x2D => "AVSZ3",
        0x2E => "AVSZ4",
        0x30 => "RTPT",
        0x3D => "GPF",
        0x3E => "GPL",
        0x3F => "NCCT",
        _ => "?",
    }
}
