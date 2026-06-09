//! Capture per-op GTE samples (MVMVA / RTPT / NCLIP / ...) from the gameplay
//! scene -- inputs, outputs, and FLAG -- to seed the hardware-tests GTE
//! conformance battery with REAL scene operations instead of synthetic ones.
//!
//! Unlike `probe_gameplay_gte_trace` (which captures only FLAG-firing ops
//! across all mnemonics, 24 total), this buckets by mnemonic and keeps a
//! handful of DEDUPED samples per op, including non-FLAG ones -- because a
//! divergence (e.g. the on-hardware vertex explosion) can come from an op
//! whose result is wrong without ever setting the saturation/overflow FLAG.
//! Each bucket prefers variety: distinct instruction encodings (for MVMVA the
//! mx/vx/cv/sf/lm selector bits live in the instr) and distinct inputs.
//!
//! Usage: probe_gameplay_gte_ops <steps> <disc.cue>

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::{fast_boot_disc, Bus, Cpu};
use std::collections::{HashMap, HashSet};
use std::path::Path;

const PER_OP_FLAG: usize = 4;
const PER_OP_NOFLAG: usize = 4;

fn main() {
    let mut args = std::env::args().skip(1);
    let steps: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(300_000_000);
    let disc_path = args.next().expect("usage: probe_gameplay_gte_ops <steps> <disc.cue>");

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS at emu/bios/SCPH1001.BIN");
    let mut bus = Bus::new(bios).expect("bus");
    let mut cpu = Cpu::new();
    let disc = disc_support::load_disc_path(Path::new(&disc_path)).expect("disc readable");
    fast_boot_disc(&mut bus, &mut cpu, &disc).expect("fast boot");
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    // Past the "ANALOG MODE REQUIRED" gate into actual gameplay.
    let _ = bus.force_port1_analog_mode();

    let mut by_op = [0u64; 64];
    let mut flag_by_op = [0u64; 64];
    let mut buckets: Vec<Bucket> = (0..64).map(|_| Bucket::default()).collect();
    let mut pc_sites: HashMap<(u32, usize), u64> = HashMap::new();
    let mut total_func = 0u64;

    for _ in 1..=steps {
        let pc = cpu.pc();
        let instr = bus.peek_instruction(pc).unwrap_or(0);
        let is_cop2 = (instr >> 26) == 0x12;
        let is_gte_func = is_cop2 && (instr & (1 << 25)) != 0;

        let inputs = is_gte_func.then(|| snapshot(&cpu));

        if cpu.step(&mut bus).is_err() {
            break;
        }

        if let Some(input) = inputs {
            total_func += 1;
            let func = (instr & 0x3F) as usize;
            by_op[func] += 1;
            *pc_sites.entry((pc, func)).or_insert(0) += 1;
            let flag = cpu.cop2().read_control(31);
            let flag_master = flag & 0x8000_0000 != 0;
            if flag_master {
                flag_by_op[func] += 1;
            }
            buckets[func].offer(Captured {
                instr,
                input,
                out_data: read_data_all(&cpu),
                flag,
            });
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

    // Confirm we actually reached the gameplay render.
    let (rgba, w, h) = bus.gpu.display_rgba8();
    let nonzero = rgba.chunks_exact(4).filter(|p| p[0] | p[1] | p[2] != 0).count();
    let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in rgba.chunks_exact(4) {
        ppm.extend_from_slice(&px[..3]);
    }
    let _ = std::fs::write("/tmp/gte-ops-frame.ppm", ppm);

    println!("=== gameplay GTE op capture: {disc_path} ({steps} steps) ===");
    println!("final display: {w}x{h}  nonzero_px={nonzero}  -> /tmp/gte-ops-frame.ppm");
    println!("total COP2 function-ops: {total_func}");
    println!();
    println!("--- op distribution (count / FLAG-firing) ---");
    for func in 0..64 {
        if by_op[func] > 0 {
            println!(
                "  {:<8} (0x{:02x}): {:>8}  flag={}",
                mnemonic(func as u32),
                func,
                by_op[func],
                flag_by_op[func]
            );
        }
    }

    println!();
    println!("--- top GTE call-sites (PC : mnemonic = count) -- tells player from world ---");
    let mut sites: Vec<((u32, usize), u64)> = pc_sites.into_iter().collect();
    sites.sort_by(|a, b| b.1.cmp(&a.1));
    for ((pc, func), count) in sites.into_iter().take(24) {
        println!("  0x{pc:08x}  {:<8} {count}", mnemonic(func as u32));
    }

    // Print buckets, MVMVA / RTPT / NCLIP first (the bug-relevant ops).
    let priority = [0x12u32, 0x30, 0x06];
    let mut order: Vec<u32> = priority.to_vec();
    for func in 0..64u32 {
        if !priority.contains(&func) {
            order.push(func);
        }
    }
    for func in order {
        let bucket = &buckets[func as usize];
        if bucket.len() == 0 {
            continue;
        }
        println!();
        println!(
            "=== {} (0x{:02x}) -- {} deduped samples ({} flag / {} noflag) ===",
            mnemonic(func),
            func,
            bucket.len(),
            bucket.flag.len(),
            bucket.noflag.len()
        );
        for (i, s) in bucket.samples().enumerate() {
            print_sample(func, i, s);
        }
    }
}

#[derive(Default)]
struct Bucket {
    seen: HashSet<u64>,
    flag: Vec<Captured>,
    noflag: Vec<Captured>,
}

impl Bucket {
    fn offer(&mut self, c: Captured) {
        let fires = c.flag & 0x8000_0000 != 0;
        let cap = if fires { PER_OP_FLAG } else { PER_OP_NOFLAG };
        let bucket = if fires { &mut self.flag } else { &mut self.noflag };
        if bucket.len() >= cap {
            return;
        }
        // Dedup by encoding + inputs so we keep distinct ops, not the same
        // op replayed thousands of times.
        let key = mix(c.instr as u64)
            ^ hash_regs(&c.input.0).rotate_left(1)
            ^ hash_regs(&c.input.1).rotate_left(2);
        if self.seen.insert(key) {
            bucket.push(c);
        }
    }

    fn samples(&self) -> impl Iterator<Item = &Captured> {
        // FLAG-firing first: those are the overflow/saturation cases most
        // likely to diverge on real silicon.
        self.flag.iter().chain(self.noflag.iter())
    }

    fn len(&self) -> usize {
        self.flag.len() + self.noflag.len()
    }
}

struct Captured {
    instr: u32,
    input: ([u32; 32], [u32; 32]), // (data, control) before
    out_data: [u32; 32],
    flag: u32,
}

fn print_sample(func: u32, i: usize, s: &Captured) {
    println!(
        "[{i}] {} instr=0x{:08x} sf={} mx={} vx={} cv={} lm={} FLAG=0x{:08x}",
        mnemonic(func),
        s.instr,
        (s.instr >> 19) & 1,
        (s.instr >> 17) & 3,
        (s.instr >> 15) & 3,
        (s.instr >> 13) & 3,
        (s.instr >> 10) & 1,
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

fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x
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
