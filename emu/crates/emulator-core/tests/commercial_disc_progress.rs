//! Commercial disc route-progress guards.
//!
//! These are ignored because they require local retail disc images and
//! intentionally document current blockers. Run them when changing the
//! CD-ROM, DMA, GPU, scheduler, BIOS boot, or direct-EXE boot paths:
//!
//! ```bash
//! PSOXIDE_BIOS="/path/to/SCPH1001.BIN" \
//! cargo test --manifest-path emu/Cargo.toml -p emulator-core \
//!   --test commercial_disc_progress --release -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, Cpu, DISC_FAST_BOOT_WARMUP_STEPS,
};

const DEFAULT_BIOS: &str = "/Users/ebonura/Downloads/ps1 bios/SCPH1001.BIN";
const DEFAULT_CTR_DISC: &str = "/Users/ebonura/Downloads/ps1 games/CTR - Crash Team Racing (USA)/CTR - Crash Team Racing (USA).cue";
const DEFAULT_METAL_SLUG_X_DISC: &str =
    "/Users/ebonura/Downloads/ps1 games/Metal Slug X (USA)/Metal Slug X (USA).cue";

const SPU_PUMP_CYCLES: u64 = 560_000;
const SPU_FRAME_SAMPLES: usize = 735;

const CTR_BOOT_STEPS: u64 = 300_000_000;
const CTR_STUCK_SCEA_HASH: u64 = 0xbfb9_bb04_fb70_42d8;

const METAL_SLUG_X_BIOS_BOOT_STEPS: u64 = 300_000_000;
const METAL_SLUG_X_DIRECT_BOOT_STEPS: u64 = 100_000_000;
const METAL_SLUG_X_NO_DATA_HASH: u64 = 0x0936_9767_b12f_c5f2;

#[test]
#[ignore = "requires local CTR disc + BIOS; currently red until BIOS boot leaves the SCEA splash"]
fn ctr_bios_boot_leaves_scea_splash() {
    assert_ctr_leaves_scea(BootMode::Bios);
}

#[test]
#[ignore = "requires local CTR disc + BIOS; currently red until direct boot leaves the SCEA splash"]
fn ctr_direct_exe_boot_leaves_scea_splash() {
    assert_ctr_leaves_scea(BootMode::DirectExeAfterWarmBios);
}

#[test]
#[ignore = "requires local Metal Slug X disc + BIOS; currently red until BIOS boot finds game data"]
fn metal_slug_x_bios_boot_finds_game_data() {
    assert_metal_slug_x_finds_game_data(BootMode::Bios, METAL_SLUG_X_BIOS_BOOT_STEPS);
}

#[test]
#[ignore = "requires local Metal Slug X disc + BIOS; currently red until direct boot finds game data"]
fn metal_slug_x_direct_exe_boot_finds_game_data() {
    assert_metal_slug_x_finds_game_data(
        BootMode::DirectExeAfterWarmBios,
        METAL_SLUG_X_DIRECT_BOOT_STEPS,
    );
}

fn assert_ctr_leaves_scea(mode: BootMode) {
    let snapshot = run_disc_boot_until(
        asset_path("PSOXIDE_CTR_DISC", DEFAULT_CTR_DISC),
        mode,
        CTR_BOOT_STEPS,
        "ctr",
    );

    assert_ne!(
        snapshot.display_hash, CTR_STUCK_SCEA_HASH,
        "CTR is still on the SCEA splash after {CTR_BOOT_STEPS} steps: {snapshot:#?}"
    );
    assert!(
        snapshot.texture_uploads > 1,
        "CTR did not upload post-splash assets: {snapshot:#?}"
    );
}

fn assert_metal_slug_x_finds_game_data(mode: BootMode, steps: u64) {
    let snapshot = run_disc_boot_until(
        asset_path("PSOXIDE_METAL_SLUG_X_DISC", DEFAULT_METAL_SLUG_X_DISC),
        mode,
        steps,
        "metal-slug-x",
    );

    assert_ne!(
        snapshot.display_hash, METAL_SLUG_X_NO_DATA_HASH,
        "Metal Slug X reports that no game data was detected after {steps} steps: {snapshot:#?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum BootMode {
    Bios,
    DirectExeAfterWarmBios,
}

impl BootMode {
    fn label(self) -> &'static str {
        match self {
            Self::Bios => "bios",
            Self::DirectExeAfterWarmBios => "direct-exe",
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct DiscProgressSnapshot {
    boot_mode: &'static str,
    display_hash: u64,
    display_size: (u32, u32),
    visible_nonzero_pixels: usize,
    texture_uploads: u32,
    dma3_starts: u64,
    sector_events: u64,
    cdrom_irq_counts: [u64; 6],
    mdec_commands: u64,
    mdec_macroblocks: u64,
}

fn run_disc_boot_until(
    disc_path: PathBuf,
    mode: BootMode,
    steps: u64,
    dump_stem: &str,
) -> DiscProgressSnapshot {
    let bios = std::fs::read(asset_path("PSOXIDE_BIOS", DEFAULT_BIOS)).expect("BIOS readable");
    let disc = psoxide_settings::library::load_disc_from_cue(&disc_path)
        .unwrap_or_else(|error| panic!("load {}: {error}", disc_path.display()));

    let mut cpu = Cpu::new();
    let mut bus = Bus::new(bios).expect("bus");
    match mode {
        BootMode::Bios => {
            bus.cdrom.insert_disc(Some(disc));
        }
        BootMode::DirectExeAfterWarmBios => {
            warm_bios_for_disc_fast_boot(&mut bus, &mut cpu, DISC_FAST_BOOT_WARMUP_STEPS)
                .expect("BIOS warmup");
            fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, false)
                .unwrap_or_else(|error| panic!("direct boot {}: {error:?}", disc_path.display()));
            bus.cdrom.insert_disc(Some(disc));
        }
    }
    bus.attach_digital_pad_port1();
    bus.attach_memcard_port1(Vec::new());

    let mut cycles_at_last_spu_pump = bus.cycles();
    for _ in 0..steps {
        cpu.step(&mut bus).expect("CPU step");
        if bus.cycles().saturating_sub(cycles_at_last_spu_pump) > SPU_PUMP_CYCLES {
            cycles_at_last_spu_pump = bus.cycles();
            bus.run_spu_samples(SPU_FRAME_SAMPLES);
            let _ = bus.spu.drain_audio();
        }
    }

    maybe_dump_visible(&bus, mode, dump_stem);
    let (display_hash, width, height, _) = bus.gpu.display_hash();
    let gp0 = bus.gpu.gp0_opcode_histogram();
    DiscProgressSnapshot {
        boot_mode: mode.label(),
        display_hash,
        display_size: (width, height),
        visible_nonzero_pixels: visible_nonzero_pixels(&bus),
        texture_uploads: gp0[0xA0],
        dma3_starts: bus.dma_start_triggers()[3],
        sector_events: bus.cdrom.sector_events_scheduled,
        cdrom_irq_counts: bus.cdrom.irq_type_counts,
        mdec_commands: bus.mdec.commands_seen(),
        mdec_macroblocks: bus.mdec.macroblocks_decoded(),
    }
}

fn visible_nonzero_pixels(bus: &Bus) -> usize {
    let (rgba, _, _) = bus.gpu.display_rgba8();
    rgba.chunks_exact(4)
        .filter(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
        .count()
}

fn maybe_dump_visible(bus: &Bus, mode: BootMode, stem: &str) {
    let Some(dir) = std::env::var_os("PSOXIDE_DISC_PROGRESS_DUMP_DIR") else {
        return;
    };
    let path = PathBuf::from(dir).join(format!("{stem}-{}.ppm", mode.label()));
    dump_visible_ppm(bus, &path).unwrap_or_else(|error| panic!("dump {}: {error}", path.display()));
    eprintln!("visible dump: {}", path.display());
}

fn dump_visible_ppm(bus: &Bus, path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;

    let (rgba, width, height) = bus.gpu.display_rgba8();
    let mut file = std::fs::File::create(path)?;
    writeln!(file, "P6\n{width} {height}\n255")?;
    for px in rgba.chunks_exact(4) {
        file.write_all(&px[..3])?;
    }
    Ok(())
}

fn asset_path(env_key: &str, fallback: &str) -> PathBuf {
    std::env::var(env_key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(fallback))
}
