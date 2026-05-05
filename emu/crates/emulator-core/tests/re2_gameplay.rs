//! Resident Evil 2 in-game regression route.
//!
//! Ignored because it requires the retail disc image and BIOS and runs
//! a full fastboot-to-gameplay path. Run with:
//!
//! ```bash
//! PSOXIDE_BIOS="/path/to/SCPH1001.BIN" \
//! PSOXIDE_RE2_DISC="/path/to/Resident Evil 2.cue" \
//! cargo test --manifest-path emu/Cargo.toml -p emulator-core \
//!   --test re2_gameplay --release -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use emulator_core::{
    button, fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, ButtonState, Cpu,
    DISC_FAST_BOOT_WARMUP_STEPS,
};

const DEFAULT_BIOS: &str = "/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN";
const DEFAULT_RE2_DISC: &str = "/Users/ebonura/Downloads/ps1 games/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1)/Resident Evil 2 - Dual Shock Ver. (USA) (Disc 1).cue";
const GAMEPLAY_STEPS: u64 = 900_000_000;
const SPU_PUMP_CYCLES: u64 = 560_000;
const SPU_FRAME_SAMPLES: usize = 735;
const EXPECTED_GAMEPLAY_DISPLAY_HASH: u64 = 0x9b42_33cb_7b58_7e5e;

#[test]
#[ignore = "requires local RE2 Disc 1 + BIOS; long-running (~15s in release)"]
fn re2_dualshock_disc1_reaches_first_playable_room_without_gp0_readback_corruption() {
    let bios = std::fs::read(asset_path("PSOXIDE_BIOS", DEFAULT_BIOS)).expect("BIOS readable");
    let disc_path = asset_path("PSOXIDE_RE2_DISC", DEFAULT_RE2_DISC);
    let disc = psoxide_settings::library::load_disc_from_cue(&disc_path).expect("RE2 cue loads");

    let mut cpu = Cpu::new();
    let mut bus = Bus::new(bios).expect("bus");
    warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
        .expect("BIOS warmup");
    fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false).expect("fastboot RE2");
    bus.cdrom.insert_disc(Some(disc));
    bus.attach_digital_pad_port1();
    bus.attach_memcard_port1(Vec::new());

    let mut current_pad_mask = u16::MAX;
    let mut cycles_at_last_pump = bus.cycles();
    for _ in 0..GAMEPLAY_STEPS {
        let vblank = bus.irq().raise_counts()[0];
        let pad_mask = re2_start_skip_mask(vblank);
        if pad_mask != current_pad_mask {
            bus.set_port1_buttons(ButtonState::from_bits(pad_mask));
            current_pad_mask = pad_mask;
        }
        cpu.step(&mut bus).expect("CPU step");
        if bus.cycles().saturating_sub(cycles_at_last_pump) > SPU_PUMP_CYCLES {
            cycles_at_last_pump = bus.cycles();
            bus.run_spu_samples(SPU_FRAME_SAMPLES);
            let _ = bus.spu.drain_audio();
        }
    }

    let area = bus.gpu.display_area();
    assert_eq!(
        (area.x, area.y, area.width, area.height, area.bpp24),
        (0, 240, 320, 240, false)
    );
    assert_eq!(bus.mdec.macroblocks_decoded(), 30_400);
    assert_eq!(bus.mdec.queued_rle_halfwords(), 0);

    let gp0 = bus.gpu.gp0_opcode_histogram();
    assert!(
        gp0[0xA0] >= 5_000,
        "expected room/background uploads, got {}",
        gp0[0xA0]
    );
    assert!(gp0[0xC0] >= 1, "expected RE2 VRAM readback command");
    assert_eq!(
        gp0[0xBB], 0,
        "pixel/readback data leaked into GP0 as 0xBB commands"
    );
    assert_eq!(
        gp0[0xBC], 0,
        "pixel/readback data leaked into GP0 as 0xBC commands"
    );
    assert_eq!(
        gp0[0xD8], 0,
        "pixel/readback data leaked into GP0 as 0xD8 commands"
    );
    assert_eq!(
        gp0[0xDC], 0,
        "pixel/readback data leaked into GP0 as 0xDC commands"
    );

    let (display_hash, width, height, _) = bus.gpu.display_hash();
    assert_eq!((width, height), (320, 240));
    assert_eq!(display_hash, EXPECTED_GAMEPLAY_DISPLAY_HASH);

    if let Ok(path) = std::env::var("PSOXIDE_RE2_VISIBLE_DUMP") {
        dump_visible_ppm(&bus.gpu, &path).expect("visible dump");
        eprintln!("visible dump: {path}");
    }
}

fn asset_path(env_key: &str, fallback: &str) -> PathBuf {
    std::env::var(env_key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(fallback))
}

fn re2_start_skip_mask(vblank: u64) -> u16 {
    const PULSES: &[(u64, u64)] = &[(100, 30), (500, 30), (850, 30)];
    if PULSES
        .iter()
        .any(|&(start, frames)| vblank >= start && vblank < start + frames)
    {
        button::START
    } else {
        0
    }
}

fn dump_visible_ppm(gpu: &emulator_core::Gpu, path: &str) -> std::io::Result<()> {
    use std::io::Write;

    let (rgba, width, height) = gpu.display_rgba8();
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "P6\n{width} {height}\n255")?;
    for px in rgba.chunks_exact(4) {
        file.write_all(&px[..3])?;
    }
    Ok(())
}
