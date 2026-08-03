//! One-off probe for the demo-disc chain-load crash: warm-boot a real
//! BIOS, fast-boot the given cue with HLE off (the frontend's default
//! disc path), press CROSS ~150 vblanks in, and the first time the CPU
//! lands on the BEV exception vector (0xBFC00180) dump COP0 state plus
//! the trailing pc ring, so the faulting EPC and cause are visible.
//!
//! ```bash
//! PSOXIDE_BIOS="/path/SCPH1001.BIN" PSOXIDE_DISC="/path/disc.cue" \
//! cargo run -p emulator-core --example chainload_crash_probe --release
//! ```

#[path = "support/disc.rs"]
mod disc_support;

use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, ButtonState, Cpu,
    DISC_FAST_BOOT_WARMUP_STEPS,
};

fn main() {
    let bios_path = std::env::var("PSOXIDE_BIOS").expect("set PSOXIDE_BIOS");
    let disc_path = std::env::var("PSOXIDE_DISC").expect("set PSOXIDE_DISC");
    let steps: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(150_000_000);
    let press_at: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);

    let bios = std::fs::read(&bios_path).expect("BIOS readable");
    let mut bus = Bus::new(bios).expect("bus");
    let mut cpu = Cpu::new();
    let disc = disc_support::load_disc_path(std::path::Path::new(&disc_path)).expect("disc");
    warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS).expect("warmup");
    let info = fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false).expect("fast boot");
    println!(
        "fastboot={} entry=0x{:08x}",
        info.boot_path, info.initial_pc
    );
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();

    const CROSS: u16 = 0x4000;
    let vblank0 = bus.irq().raise_counts()[0];
    let mut ring = [0u32; 64];
    let mut ring_i = 0usize;
    for step in 0..steps {
        let vblank = bus.irq().raise_counts()[0] - vblank0;
        let mask = if (press_at..press_at + 8).contains(&vblank) {
            CROSS
        } else {
            0
        };
        bus.set_port1_buttons(ButtonState::from_bits(mask));

        let pc = cpu.pc();
        ring[ring_i] = pc;
        ring_i = (ring_i + 1) % ring.len();
        // Bits 27,26,24,23 of SR do not exist on the R3000A; any of them
        // set means an mtc0 wrote garbage. Trap the first occurrence.
        let sr = cpu.cop0()[12];
        if sr & 0x0D80_0000 != 0 {
            println!("step {step} vblank {vblank}: GARBAGE SR = 0x{sr:08x}");
            println!("current pc = 0x{pc:08x}");
            println!("last pcs (oldest -> newest):");
            for k in 0..ring.len() {
                println!("  0x{:08x}", ring[(ring_i + k) % ring.len()]);
            }
            return;
        }
        if pc == 0xBFC0_0180 {
            let cop0 = cpu.cop0();
            println!("step {step} vblank {vblank}: pc hit 0xBFC00180");
            println!("SR    = 0x{:08x}", cop0[12]);
            println!(
                "CAUSE = 0x{:08x} (exccode {}, bd={})",
                cop0[13],
                (cop0[13] >> 2) & 0x1F,
                cop0[13] >> 31
            );
            println!("EPC   = 0x{:08x}", cop0[14]);
            println!("BadV  = 0x{:08x}", cop0[8]);
            println!("last pcs (oldest -> newest):");
            for k in 0..ring.len() {
                println!("  0x{:08x}", ring[(ring_i + k) % ring.len()]);
            }
            return;
        }
        if cpu.step(&mut bus).is_err() {
            println!("step error at 0x{:08x} (step {step})", cpu.pc());
            return;
        }
    }
    println!(
        "no BEV hit in {steps} steps; final pc=0x{:08x} vblank={}",
        cpu.pc(),
        bus.irq().raise_counts()[0] - vblank0
    );
}
