//! Probe the a commercial title intro display state at checkpoints: where is
//! the copyright image, what does GP1 think the display is (bpp24?), and
//! what reached the cmd_log? Chases the missing-intro divergence (CPU VRAM
//! has the Universal/Naughty Dog screen, the HW-replay frame is black).
//!
//! Usage: probe_crash_display <disc.cue> [checkpoints...]

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::{fast_boot_disc, Bus, Cpu};
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let disc_path = args.next().expect("usage: probe_crash_display <disc.cue> [steps...]");
    let mut checkpoints: Vec<u64> = args.filter_map(|s| s.parse().ok()).collect();
    if checkpoints.is_empty() {
        checkpoints = vec![150_000_000, 250_000_000, 300_000_000, 350_000_000, 450_000_000];
    }

    let bios = std::fs::read("bios/SCPH1001.BIN").expect("BIOS at emu/bios/SCPH1001.BIN");
    let mut bus = Bus::new(bios).expect("bus");
    let mut cpu = Cpu::new();
    let disc = disc_support::load_disc_path(Path::new(&disc_path)).expect("disc readable");
    fast_boot_disc(&mut bus, &mut cpu, &disc).expect("fast boot");
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();

    let mut cursor = 0u64;
    let mut cycles_at_last_pump = 0u64;
    for &cp in &checkpoints {
        while cursor < cp {
            if cpu.step(&mut bus).is_err() {
                eprintln!("CPU errored at step {cursor}");
                return;
            }
            cursor += 1;
            // Crash's boot blocks on CD/SPU progress: pump audio at roughly
            // vblank cadence or the game spins on Sync/Ready forever.
            if bus.cycles() - cycles_at_last_pump > 560_000 {
                cycles_at_last_pump = bus.cycles();
                bus.run_spu_samples(735);
                let _ = bus.spu.drain_audio();
            }
        }
        let da = bus.gpu.display_area();
        // Non-black CPU-vram pixels inside the display area = is the image
        // actually there in the software rasterizer's VRAM?
        let mut nonzero = 0u32;
        for y in da.y..da.y.saturating_add(da.height).min(512) {
            for x in da.x..da.x.saturating_add(da.width).min(1024) {
                if bus.gpu.vram.get_pixel(x, y) != 0 {
                    nonzero += 1;
                }
            }
        }
        // cmd_log composition: what command classes reached the GPU so far.
        let (mut draws, mut fills, mut uploads, mut env, mut other) = (0u64, 0u64, 0u64, 0u64, 0u64);
        for e in bus.gpu.cmd_log.iter() {
            match e.opcode {
                0x20..=0x7F => draws += 1,
                0x02 => fills += 1,
                0xA0..=0xBF => uploads += 1,
                0xE0..=0xEF => env += 1,
                _ => other += 1,
            }
        }
        println!(
            "step={cp:>10} pc=0x{pc:08x} display=({x},{y}) {w}x{h} bpp={bpp} vram_nonzero={nonzero} cmd_log[{n}]: draws={draws} fills={fills} uploads={uploads} env={env} other={other}",
            pc = cpu.pc(),
            x = da.x,
            y = da.y,
            w = da.width,
            h = da.height,
            bpp = if da.bpp24 { 24 } else { 15 },
            n = bus.gpu.cmd_log.len(),
        );
    }
}
