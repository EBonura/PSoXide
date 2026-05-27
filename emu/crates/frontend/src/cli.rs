//! Headless CLI -- exercises every stateful path the GUI exposes
//! without opening a window.
//!
//! Existed for three reasons:
//!
//! 1. **Verification substrate.** Every feature added to the GUI
//!    should land first here as a subcommand -- then the UI is a
//!    thin layer over a tested CLI. "Does the game library scan
//!    find my games?" becomes a deterministic test instead of a
//!    click-test.
//! 2. **Regression scripts.** `frontend launch <game> --steps 100M
//!    --dump-hash` is a one-liner you can wrap in a shell test to
//!    pin BIOS / SDK behaviour without rebuilding the GUI.
//! 3. **CI.** No display server → `cargo test` on Linux boxes
//!    without Xvfb. The existing milestone tests already run
//!    headless; this extends the same principle to the
//!    user-facing features.
//!
//! When the frontend binary is run with a subcommand argument
//! (`scan`, `list`, `launch`, `info`), this module handles it and
//! returns -- `main()` never spins up winit/wgpu. Without a
//! subcommand, the GUI runs as normal.

use std::collections::HashSet;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use emulator_core::{
    button, fast_boot_disc_with_hle, spu::SAMPLE_CYCLES, telemetry, warm_bios_for_disc_fast_boot,
    Bus, ButtonState, Cpu, Gpu, GteProfileSnapshot, DISC_FAST_BOOT_WARMUP_STEPS,
};
use psoxide_settings::{
    library::{GameKind, LibraryEntry},
    ConfigPaths, Library, Settings,
};
use psx_iso::{Disc, Exe};
use psxed_project::{NodeId, ProjectDocument};
use psxed_ui::{ViewportCameraMode, ViewportCameraState};

use crate::app::{
    build_embedded_playtest_disc, bus_from_configured_bios, copy_project_disc,
    fast_boot_embedded_playtest_disc,
};
use crate::playtest_input::read_input_tape;

const NTSC_CPU_CYCLES_PER_VBLANK: u64 = 33_868_800 / 60;
const GUEST_RENDER_BREAKDOWN_STAGES: &[(u16, &str)] = &[
    (telemetry::stage::SKY, "sky"),
    (telemetry::stage::FAR_VISTA, "far vista"),
    (telemetry::stage::ROOM, "room"),
    (telemetry::stage::ENTITY_MARKERS, "markers"),
    (telemetry::stage::IMAGE_PROPS, "image props"),
    (telemetry::stage::MODEL_INSTANCES, "models"),
    (telemetry::stage::PLAYER, "player"),
    (telemetry::stage::EQUIPMENT, "equipment"),
    (telemetry::stage::WORLD_FLUSH, "flush/sort"),
    (telemetry::stage::OT_SUBMIT, "ot submit"),
];

/// Top-level argument parser. Passed to `clap::Parser::parse()`
/// from `main.rs`.
#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    /// Path to an alternate config directory (defaults to the
    /// platform config dir). Useful for portable installs and
    /// keeping tests from touching your real settings.
    #[arg(long, global = true)]
    pub config_dir: Option<PathBuf>,

    /// Launch the GUI in a regular floating window instead of the
    /// default borderless-fullscreen mode. Useful when developing
    /// with the editor side-by-side with a terminal or docs. Only
    /// meaningful when no headless subcommand is given --
    /// subcommands always run windowless.
    #[arg(long)]
    pub windowed: bool,

    /// Run the experimental compute-shader rasterizer in parallel
    /// with the CPU rasterizer (Phase C). Per-frame the frontend
    /// drains the CPU's `cmd_log` and replays each GP0 packet
    /// through the GPU compute path. Off by default -- opt-in until
    /// parity is confirmed in a wide enough test set. Press F12 in
    /// the GUI to toggle at runtime once the bus is wired up.
    #[arg(long)]
    pub gpu_compute: bool,

    /// Headless subcommand. Omit to launch the GUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Every headless operation the frontend exposes. Add new variants
/// as UI features are built so each one has a scriptable
/// equivalent.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print resolved config paths + effective settings values.
    Info,
    /// Walk the configured (or given) library root and refresh the
    /// on-disk game cache.
    Scan(ScanArgs),
    /// Print the cached library contents (one line per game).
    List,
    /// Run the emulator headlessly on a specific game or EXE and
    /// emit final state info.
    Launch(LaunchArgs),
    /// Build the in-editor Play disc image from the current generated package.
    BuildEditorPlaytestDisc,
    /// Cook, build, and export a project CUE/BIN disc without opening the GUI.
    BuildProjectDisc(BuildProjectDiscArgs),
    /// Render an editor 3D preview screenshot without opening the GUI.
    DumpEditorPreview(DumpEditorPreviewArgs),
}

/// Arguments for `scan`.
#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Library root to scan. Overrides `settings.paths.game_library`
    /// if set; otherwise uses the configured value.
    #[arg(long)]
    pub root: Option<PathBuf>,
}

/// Arguments for `launch`.
#[derive(Debug, Args)]
pub struct LaunchArgs {
    /// Path to a `.cue`, `.bin`, `.iso`, `.ccd`, or `.exe` to run.
    /// Either this or `--game-id` must be provided.
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Alternative to `--path`: pick a game from the cached library
    /// by its stable ID (16-hex-char fingerprint).
    #[arg(long)]
    pub game_id: Option<String>,
    /// Number of CPU instructions to retire before stopping.
    #[arg(long, default_value_t = 100_000_000)]
    pub steps: u64,
    /// Stop once instrumented homebrew has emitted this many guest
    /// frame-begin telemetry markers. `--steps` still acts as a safety cap.
    #[arg(long)]
    pub guest_frames: Option<u64>,
    /// Stop once instrumented homebrew has emitted this many rendered
    /// visual frames through the VISUAL_FRAMES telemetry counter. Pair
    /// with `--guest-frames` as a fallback while cadence telemetry rolls out.
    #[arg(long)]
    pub guest_visual_frames: Option<u64>,
    /// Replay a saved editor playtest input tape, applying one sample per
    /// emitted guest frame-begin marker.
    #[arg(long)]
    pub input_tape: Option<PathBuf>,
    /// Treat an authored disc as an embedded editor Play disc and boot it
    /// through the same no-BIOS HLE path used by the editor viewport.
    #[arg(long)]
    pub embedded_playtest: bool,
    /// Force the real BIOS disc boot path instead of direct
    /// SYSTEM.CNF fast boot.
    #[arg(long)]
    pub bios_boot: bool,
    /// Print an FNV-1a-64 VRAM hash at the end. Same algorithm the
    /// milestone regression tests use, so a CLI run + a unit test
    /// should produce identical numbers.
    #[arg(long)]
    pub dump_hash: bool,
    /// Write visible-display FNV-1a hashes at rendered visual-frame
    /// checkpoints. The CSV is stable enough to diff across performance
    /// experiments.
    #[arg(long)]
    pub visual_hash_log: Option<PathBuf>,
    /// Capture every Nth rendered visual frame when `--visual-hash-log`
    /// is enabled. Defaults to every frame.
    #[arg(long, default_value_t = 1)]
    pub visual_hash_interval: u64,
    /// Write visible-display hashes at guest frame-begin checkpoints.
    /// This is useful when performance changes alter visual cadence but
    /// the simulation path should still render the same checkpoint image.
    #[arg(long)]
    pub guest_hash_log: Option<PathBuf>,
    /// Capture every Nth guest frame when `--guest-hash-log` is enabled.
    #[arg(long, default_value_t = 60)]
    pub guest_hash_interval: u64,
    /// Optional path to dump the final VRAM as a raw PPM image.
    /// Lets you eyeball the boot state without firing up the GUI.
    #[arg(long)]
    pub dump_vram: Option<PathBuf>,
    /// Optional path to dump the HW renderer's output as a PPM. Spins
    /// up a headless wgpu device, replays the cumulative `cmd_log`
    /// through the same pipeline the live GUI uses, and writes the
    /// result. Use this to regression-test the HW pipeline without
    /// a window or screen-capture permission.
    #[arg(long)]
    pub dump_hw: Option<PathBuf>,
    /// Print a guest-runtime telemetry summary captured out-of-band.
    #[arg(long)]
    pub dump_guest_profile: bool,
    /// Hold the left analog stick fully forward during the headless run.
    #[arg(long)]
    pub hold_forward: bool,
    /// Hold the game run button during the headless run.
    #[arg(long)]
    pub hold_run: bool,
}

/// Arguments for `build-project-disc`.
#[derive(Debug, Args)]
pub struct BuildProjectDiscArgs {
    /// Project directory containing `project.ron`, or a direct path to a project file.
    #[arg(long, default_value = "editor/projects/default")]
    pub project: PathBuf,
}

/// Arguments for `dump-editor-preview`.
#[derive(Debug, Args)]
pub struct DumpEditorPreviewArgs {
    /// Project directory containing `project.ron`, or a direct path to a project file.
    #[arg(long, default_value = "editor/projects/default")]
    pub project: PathBuf,
    /// Output PPM path.
    #[arg(long)]
    pub out: PathBuf,
    /// Orbit camera yaw in editor 4096-units-per-turn convention.
    #[arg(long, default_value_t = 320)]
    pub yaw: u16,
    /// Orbit camera pitch in editor 4096-units-per-turn convention.
    #[arg(long, default_value_t = 300)]
    pub pitch: u16,
    /// Orbit camera distance in editor/world units.
    #[arg(long, default_value_t = 8192)]
    pub radius: i32,
    /// Orbit target X in editor/world units.
    #[arg(long, default_value_t = 2048)]
    pub target_x: i32,
    /// Orbit target Y in editor/world units.
    #[arg(long, default_value_t = 512)]
    pub target_y: i32,
    /// Orbit target Z in editor/world units.
    #[arg(long, default_value_t = 2048)]
    pub target_z: i32,
    /// Hide the streaming grid overlay.
    #[arg(long)]
    pub no_grid: bool,
}

/// Entry point. Dispatches on `cli.command`; returns `Ok(())` on
/// success, `Err` with a user-visible message on failure. `main()`
/// prints the error and exits non-zero.
pub fn run(cli: Cli) -> Result<(), String> {
    let paths = resolve_paths(cli.config_dir.as_deref())?;
    match cli.command.expect("CLI dispatch called without a command") {
        Command::Info => cmd_info(&paths),
        Command::Scan(args) => cmd_scan(&paths, args),
        Command::List => cmd_list(&paths),
        Command::Launch(args) => cmd_launch(&paths, args),
        Command::BuildEditorPlaytestDisc => cmd_build_editor_playtest_disc(),
        Command::BuildProjectDisc(args) => cmd_build_project_disc(args),
        Command::DumpEditorPreview(args) => cmd_dump_editor_preview(args),
    }
}

/// Dedicated resolver because the `--config-dir` override + the
/// platform-default path need consistent "one place to ask" logic
/// both here and in the GUI.
fn resolve_paths(override_dir: Option<&std::path::Path>) -> Result<ConfigPaths, String> {
    match override_dir {
        Some(p) => {
            let paths = ConfigPaths::rooted(p);
            paths.ensure_dir(paths.root()).map_err(|e| e.to_string())?;
            Ok(paths)
        }
        None => ConfigPaths::platform_default().map_err(|e| e.to_string()),
    }
}

fn cmd_info(paths: &ConfigPaths) -> Result<(), String> {
    let settings_path = paths.settings_file();
    let library_path = paths.library_file();
    let settings = Settings::load(&settings_path).unwrap_or_default();

    println!("# PSoXide headless");
    println!();
    println!("Paths:");
    println!("  config dir       : {}", paths.root().display());
    println!("  settings.ron     : {}", settings_path.display());
    println!("  library.ron      : {}", library_path.display());
    println!();
    println!("Settings:");
    println!("  version          : {}", settings.version);
    println!("  paths.bios       : {}", fmt_empty(&settings.paths.bios));
    println!(
        "  paths.library    : {}",
        fmt_empty(&settings.paths.game_library)
    );
    println!("  video.int.scale  : {}", settings.video.integer_scale);
    println!(
        "  emu.hle-bios-exe : {}",
        settings.emulator.hle_bios_for_side_load
    );
    println!(
        "  input.port1.cross: {}",
        settings.input.port1.cross.label()
    );
    Ok(())
}

fn cmd_scan(paths: &ConfigPaths, args: ScanArgs) -> Result<(), String> {
    let mut settings = Settings::load(&paths.settings_file()).unwrap_or_default();
    let explicit_root = args.root.clone();
    let root = args.root.map(Ok).unwrap_or_else(|| {
        if settings.paths.game_library.is_empty() {
            Err(
                "No library root. Pass --root <dir> or set paths.game_library in settings.ron."
                    .to_string(),
            )
        } else {
            Ok(PathBuf::from(&settings.paths.game_library))
        }
    })?;
    if !root.exists() {
        return Err(format!("library root does not exist: {}", root.display()));
    }

    let mut lib = Library::load_or_empty(&paths.library_file());
    let before = lib.entries.len();
    let changed = lib.scan(&root).map_err(|e| e.to_string())?;
    lib.save(&paths.library_file()).map_err(|e| e.to_string())?;
    println!(
        "scanned {} → {} entries ({} parsed / re-parsed, {} reused)",
        root.display(),
        lib.entries.len(),
        changed,
        lib.entries.len().saturating_sub(changed),
    );
    if before != lib.entries.len() {
        println!("(cache size changed: {} → {})", before, lib.entries.len());
    }

    // Persist the root into settings.ron whenever `--root` was passed
    // explicitly. A fresh config dir that never had settings.ron
    // written stays empty otherwise -- the GUI would find the library
    // but wouldn't know where to rescan from, so the next GUI-triggered
    // rescan would fail. Writing here keeps the "scan once on the CLI,
    // then use the GUI" path frictionless.
    if let Some(new_root) = explicit_root {
        let new_str = new_root.to_string_lossy().into_owned();
        if settings.paths.game_library != new_str {
            settings.paths.game_library = new_str;
            if let Err(e) = settings.save(&paths.settings_file()) {
                eprintln!("warning: could not save settings.ron: {e}");
            } else {
                println!(
                    "settings.paths.game_library updated -> {}",
                    new_root.display()
                );
            }
        }
    }
    Ok(())
}

fn cmd_list(paths: &ConfigPaths) -> Result<(), String> {
    let lib = Library::load_or_empty(&paths.library_file());
    if lib.entries.is_empty() {
        println!("(library is empty — run `scan` first)");
        return Ok(());
    }
    // Sort alphabetically by title for stable output.
    let mut sorted = lib.entries.clone();
    sorted.sort_by_key(|a| a.title.to_lowercase());
    for e in &sorted {
        println!(
            "{:<16}  {:<10}  {:<7}  {:>8} MiB  {}",
            e.id,
            kind_label(e.kind),
            region_label(e),
            e.size / (1024 * 1024),
            e.title,
        );
    }
    println!();
    println!("{} entries", sorted.len());
    Ok(())
}

fn cmd_launch(paths: &ConfigPaths, args: LaunchArgs) -> Result<(), String> {
    let settings = Settings::load(&paths.settings_file()).unwrap_or_default();
    if args.input_tape.is_some() && (args.hold_forward || args.hold_run) {
        return Err(
            "--input-tape cannot be combined with --hold-forward or --hold-run".to_string(),
        );
    }

    // Resolve `path`: direct flag or lookup by game-id.
    let game_path = match (args.path, args.game_id) {
        (Some(p), _) => p,
        (None, Some(id)) => {
            let lib = Library::load_or_empty(&paths.library_file());
            lib.entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.path.clone())
                .ok_or_else(|| format!("no game with id={id} in library.ron"))?
        }
        (None, None) => {
            return Err("Provide --path or --game-id".to_string());
        }
    };
    let tape_samples = match args.input_tape.as_deref() {
        Some(path) => {
            let samples = read_input_tape(path)?;
            if samples.is_empty() {
                return Err(format!("input tape has no frames: {}", path.display()));
            }
            eprintln!(
                "[cli] loaded input tape {} ({} frames)",
                path.display(),
                samples.len()
            );
            Some(samples)
        }
        None => None,
    };
    let guest_frame_limit = args
        .guest_frames
        .or_else(|| tape_samples.as_ref().map(|samples| samples.len() as u64));

    let mut cpu = Cpu::new();

    // Dispatch on extension: discs boot through the CD path, EXEs use
    // the legacy homebrew side-load path.
    let ext = game_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let mut bus = match ext.as_str() {
        "exe" => {
            let mut bus = Bus::new_without_bios();
            if args.dump_hw.is_some() {
                bus.gpu.enable_cmd_log();
            }
            let bytes = std::fs::read(&game_path).map_err(|e| e.to_string())?;
            let exe = Exe::parse(&bytes).map_err(|e| format!("parse EXE: {e:?}"))?;
            bus.load_exe_payload(exe.load_addr, &exe.payload);
            bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
            cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());
            // Match the GUI launch path: side-loaded EXEs need the
            // HLE syscall tables. No user BIOS is needed because the CPU
            // starts in the homebrew payload and BIOS table calls are
            // intercepted by HLE dispatch.
            bus.enable_hle_bios();
            attach_headless_playtest_pad(&mut bus);
            eprintln!(
                "[cli] side-loaded {} — entry=0x{:08x} payload={}B",
                game_path.display(),
                exe.initial_pc,
                exe.payload.len()
            );
            bus
        }
        "bin" | "iso" => {
            let mut bus = if args.embedded_playtest {
                Bus::new_without_bios()
            } else {
                bus_from_configured_bios(&settings)?
            };
            if args.dump_hw.is_some() {
                bus.gpu.enable_cmd_log();
            }
            let bytes = std::fs::read(&game_path).map_err(|e| e.to_string())?;
            let disc = Disc::from_bin(bytes);
            if args.embedded_playtest {
                fast_boot_embedded_playtest_disc(&mut bus, &mut cpu, &disc, &game_path);
            } else {
                maybe_fast_boot_disc(
                    &mut bus,
                    &mut cpu,
                    &disc,
                    &game_path,
                    settings.emulator.fast_boot_disc && !args.bios_boot,
                );
            }
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus);
            eprintln!("[cli] mounted disc {}", game_path.display());
            bus
        }
        "cue" => {
            let mut bus = if args.embedded_playtest {
                Bus::new_without_bios()
            } else {
                bus_from_configured_bios(&settings)?
            };
            if args.dump_hw.is_some() {
                bus.gpu.enable_cmd_log();
            }
            let disc = psoxide_settings::library::load_disc_from_cue(&game_path)?;
            if args.embedded_playtest {
                fast_boot_embedded_playtest_disc(&mut bus, &mut cpu, &disc, &game_path);
            } else {
                maybe_fast_boot_disc(
                    &mut bus,
                    &mut cpu,
                    &disc,
                    &game_path,
                    settings.emulator.fast_boot_disc && !args.bios_boot,
                );
            }
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus);
            eprintln!("[cli] mounted cue-backed disc {}", game_path.display());
            bus
        }
        "ccd" => {
            if args.embedded_playtest {
                return Err("--embedded-playtest does not support .ccd".to_string());
            }
            let mut bus = bus_from_configured_bios(&settings)?;
            if args.dump_hw.is_some() {
                bus.gpu.enable_cmd_log();
            }
            let disc = psoxide_settings::library::load_disc_from_ccd(&game_path)?;
            maybe_fast_boot_disc(
                &mut bus,
                &mut cpu,
                &disc,
                &game_path,
                settings.emulator.fast_boot_disc && !args.bios_boot,
            );
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus);
            eprintln!("[cli] mounted ccd-backed disc {}", game_path.display());
            bus
        }
        other => {
            return Err(format!("unsupported file extension: .{other}"));
        }
    };

    if args.hold_forward || args.hold_run {
        let mut buttons = ButtonState::NONE;
        if args.hold_run {
            buttons.press(button::CIRCLE);
        }
        bus.set_port1_buttons(buttons);
        if args.hold_forward {
            bus.set_port1_sticks(0x80, 0x80, 0x80, 0x00);
        }
    }
    let mut tape_cursor = 0usize;
    if let Some(samples) = tape_samples.as_ref() {
        samples[tape_cursor].apply_to_bus(&mut bus);
    }
    let mut profile_summary = args
        .dump_guest_profile
        .then(telemetry::GuestTelemetrySummary::default);
    let mut observed_guest_frames = bus.telemetry.frames_seen();
    let mut visual_hash_log = DisplayHashLog::new(
        args.visual_hash_log.as_deref(),
        args.visual_hash_interval,
        "visual",
    )?;
    let mut observed_visual_frames = bus
        .telemetry
        .counter_total(telemetry::counter::VISUAL_FRAMES);
    let mut guest_hash_log = DisplayHashLog::new(
        args.guest_hash_log.as_deref(),
        args.guest_hash_interval,
        "guest",
    )?;
    let mut observed_guest_hash_frames = observed_guest_frames;

    // Step the CPU. Report early on opcode errors -- they're usually
    // "we hit an unimplemented instruction" and worth surfacing.
    let mut stopped_at: Option<u64> = None;
    let mut audio_cycle_accum = 0u64;
    let gte_profile_before = cpu.cop2().profile_snapshot();
    for i in 0..args.steps {
        let cycles_before = bus.cycles();
        if let Err(e) = cpu.step(&mut bus) {
            eprintln!("[cli] step {i} failed: {e:?}");
            stopped_at = Some(i);
            break;
        }
        audio_cycle_accum =
            audio_cycle_accum.saturating_add(bus.cycles().saturating_sub(cycles_before));
        let sample_count = (audio_cycle_accum / SAMPLE_CYCLES) as usize;
        audio_cycle_accum %= SAMPLE_CYCLES;
        if sample_count > 0 {
            bus.run_spu_samples(sample_count);
            let _ = bus.spu.drain_audio();
        }
        let current_guest_frames = bus.telemetry.frames_seen();
        if let Some(samples) = tape_samples.as_ref() {
            if current_guest_frames > 0 {
                let desired_cursor = (current_guest_frames - 1) as usize;
                let desired_cursor = desired_cursor.min(samples.len().saturating_sub(1));
                if desired_cursor != tape_cursor {
                    tape_cursor = desired_cursor;
                    samples[tape_cursor].apply_to_bus(&mut bus);
                }
            }
        }
        if current_guest_frames != observed_guest_frames {
            if let Some(summary) = profile_summary.as_mut() {
                let events = bus.telemetry.drain_events();
                summary.add_events(&events);
            }
            observed_guest_frames = current_guest_frames;
        }
        let current_visual_frames = bus
            .telemetry
            .counter_total(telemetry::counter::VISUAL_FRAMES);
        while observed_guest_hash_frames < current_guest_frames {
            observed_guest_hash_frames += 1;
            guest_hash_log.record(
                observed_guest_hash_frames,
                current_guest_frames,
                current_visual_frames,
                cpu.tick(),
                bus.cycles(),
                &bus,
            )?;
        }
        while observed_visual_frames < current_visual_frames {
            observed_visual_frames += 1;
            visual_hash_log.record(
                observed_visual_frames,
                current_guest_frames,
                current_visual_frames,
                cpu.tick(),
                bus.cycles(),
                &bus,
            )?;
        }
        if let Some(target) = args.guest_visual_frames {
            if target > 0
                && bus
                    .telemetry
                    .counter_total(telemetry::counter::VISUAL_FRAMES)
                    >= target
            {
                stopped_at = Some(i + 1);
                break;
            }
        }
        if let Some(target) = guest_frame_limit {
            if target > 0 && bus.telemetry.frames_seen() >= target {
                stopped_at = Some(i + 1);
                break;
            }
        }
    }
    visual_hash_log.flush()?;
    guest_hash_log.flush()?;

    println!(
        "tick={}  cycles={}  pc=0x{:08x}{}",
        cpu.tick(),
        bus.cycles(),
        cpu.pc(),
        match stopped_at {
            Some(i) => format!("  stopped-at={i}"),
            None => String::new(),
        }
    );
    let gte_profile_after = cpu.cop2().profile_snapshot();

    if args.dump_hash {
        let mut h = 0xCBF2_9CE4_8422_2325u64;
        for &w in bus.gpu.vram.words() {
            for b in w.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0100_0000_01B3);
            }
        }
        let (dh, dw, dhi, _) = bus.gpu.display_hash();
        println!("vram_fnv1a_64=0x{h:016x}");
        println!("display_fnv1a_64=0x{dh:016x}  w={dw}  h={dhi}");
    }

    if args.dump_guest_profile {
        let counter_totals = bus.telemetry.counter_totals();
        let counter_max_values = bus.telemetry.counter_max_values();
        let counter_latest_values = bus.telemetry.counter_latest_values();
        let mut summary = profile_summary.unwrap_or_default();
        let events = bus.telemetry.drain_events();
        summary.add_events(&events);
        summary.counters = counter_totals;
        summary.counter_max_values = counter_max_values;
        summary.counter_latest_values = counter_latest_values;
        print_guest_profile(&summary);
        print_gte_profile(
            &gte_profile_before,
            &gte_profile_after,
            summary.frames.max(1),
        );
    }

    if let Some(path) = args.dump_vram {
        dump_vram_ppm(&bus, &path)?;
        eprintln!("[cli] VRAM → {}", path.display());
    }

    if let Some(path) = args.dump_hw {
        let used_24bpp_fallback = dump_hw_ppm(&bus, &path)?;
        if used_24bpp_fallback {
            eprintln!(
                "[cli] HW renderer → {} (24bpp display fallback)",
                path.display()
            );
        } else {
            eprintln!(
                "[cli] HW renderer → {} ({} cmd_log entries replayed)",
                path.display(),
                bus.gpu.cmd_log.len()
            );
        }
    }

    Ok(())
}

fn cmd_build_editor_playtest_disc() -> Result<(), String> {
    let disc_path = build_embedded_playtest_disc()?;
    println!("{}", disc_path.display());
    Ok(())
}

fn cmd_build_project_disc(args: BuildProjectDiscArgs) -> Result<(), String> {
    let (project_root, project_file) = resolve_project_arg(&args.project);
    let project_root = std::fs::canonicalize(&project_root)
        .map_err(|e| format!("project root {}: {e}", project_root.display()))?;
    let project_file = std::fs::canonicalize(&project_file)
        .map_err(|e| format!("project file {}: {e}", project_file.display()))?;
    let project = ProjectDocument::load_from_path(&project_file)
        .map_err(|e| format!("load {}: {e}", project_file.display()))?;

    let repo_root = cli_repo_root();
    run_make(
        &repo_root,
        "cook-playtest",
        &[format!("PROJECT={}", project_file.display())],
    )?;
    run_make(&repo_root, "build-editor-playtest", &[])?;

    let source_cue = build_embedded_playtest_disc()?;
    let dest_cue = project_root
        .join("baked")
        .join(format!("{}.cue", psxed_project::project_file_stem(&project.name)));
    let bytes = copy_project_disc(&source_cue, &dest_cue)?;
    eprintln!("[cli] project disc -> {} ({} KiB)", dest_cue.display(), bytes / 1024);
    println!("{}", dest_cue.display());
    Ok(())
}

fn run_make(repo_root: &Path, target: &str, extra_args: &[String]) -> Result<(), String> {
    let status = std::process::Command::new("make")
        .arg(target)
        .args(extra_args)
        .current_dir(repo_root)
        .status()
        .map_err(|e| format!("spawn make {target}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("make {target} failed: {status}"))
    }
}

fn cli_repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

fn attach_headless_playtest_pad(bus: &mut Bus) {
    bus.attach_digital_pad_port1();
    let _ = bus.force_port1_analog_mode();
}

struct DisplayHashLog {
    writer: Option<BufWriter<std::fs::File>>,
    interval: u64,
    checkpoint_kind: &'static str,
}

impl DisplayHashLog {
    fn new(
        path: Option<&Path>,
        interval: u64,
        checkpoint_kind: &'static str,
    ) -> Result<Self, String> {
        let Some(path) = path else {
            return Ok(Self {
                writer: None,
                interval: interval.max(1),
                checkpoint_kind,
            });
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let file =
            std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "checkpoint_kind,checkpoint_frame,guest_frame,visual_frame,cpu_tick,bus_cycles,display_hash,width,height,byte_len"
        )
        .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(Self {
            writer: Some(writer),
            interval: interval.max(1),
            checkpoint_kind,
        })
    }

    fn record(
        &mut self,
        checkpoint_frame: u64,
        guest_frame: u64,
        visual_frame: u64,
        cpu_tick: u64,
        bus_cycles: u64,
        bus: &Bus,
    ) -> Result<(), String> {
        let Some(writer) = self.writer.as_mut() else {
            return Ok(());
        };
        if checkpoint_frame % self.interval != 0 {
            return Ok(());
        }
        let (hash, width, height, byte_len) = bus.gpu.display_hash();
        writeln!(
            writer,
            "{},{checkpoint_frame},{guest_frame},{visual_frame},{cpu_tick},{bus_cycles},0x{hash:016x},{width},{height},{byte_len}",
            self.checkpoint_kind
        )
        .map_err(|e| format!("write visual hash log: {e}"))
    }

    fn flush(&mut self) -> Result<(), String> {
        if let Some(writer) = self.writer.as_mut() {
            writer
                .flush()
                .map_err(|e| format!("flush visual hash log: {e}"))?;
        }
        Ok(())
    }
}

fn cmd_dump_editor_preview(args: DumpEditorPreviewArgs) -> Result<(), String> {
    let (project_root, project_file) = resolve_project_arg(&args.project);
    let project = ProjectDocument::load_from_path(&project_file)
        .map_err(|e| format!("load {}: {e}", project_file.display()))?;

    let camera = ViewportCameraState {
        mode: ViewportCameraMode::Orbit,
        yaw_q12: args.yaw,
        pitch_q12: args.pitch,
        radius: args.radius,
        target: [args.target_x, args.target_y, args.target_z],
        position: [0, 0, 0],
    };

    let mut textures = crate::editor_textures::EditorTextures::new();
    textures.refresh(&project, &project_root);
    textures.refresh_models(&project, &project_root);
    let mut assets = crate::editor_assets::EditorAssets::new();
    assets.refresh(&project, &project_root);

    let empty_hidden: HashSet<NodeId> = HashSet::new();
    let frame = crate::editor_preview::build_phase1_frame(
        &project,
        camera,
        true,
        true,
        true,
        !args.no_grid,
        true,
        true,
        &empty_hidden,
        None,
        NodeId::ROOT,
        None,
        None,
        &[],
        &[],
        None,
        &[],
        None,
        &[],
        None,
        &textures,
        &assets,
    );

    let (device, queue) = headless_wgpu_device()?;
    let mut hw = psx_gpu_render::HwRenderer::new_headless(device, queue);
    let _ = hw.set_internal_scale(2, None);
    hw.render_frame(&Gpu::new(), &frame.cmd_log, textures.vram_words());

    let scale = hw.internal_scale();
    let (w, h, rgba) = hw.read_subrect_rgba8(0, 0, 320 * scale, 240 * scale);
    write_rgb_ppm_from_rgba(&args.out, w, h, &rgba)?;
    eprintln!("[cli] editor preview -> {}", args.out.display());
    Ok(())
}

fn resolve_project_arg(path: &Path) -> (PathBuf, PathBuf) {
    let path = if path.is_absolute() || path.exists() {
        path.to_path_buf()
    } else {
        let repo_path = cli_repo_root().join(path);
        if repo_path.exists() {
            repo_path
        } else {
            path.to_path_buf()
        }
    };
    if path.is_dir() {
        (path.clone(), path.join("project.ron"))
    } else {
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        (root, path)
    }
}

fn print_gte_profile(before: &GteProfileSnapshot, after: &GteProfileSnapshot, frames: u64) {
    let ops = after.ops.saturating_sub(before.ops);
    let cycles = after
        .estimated_cycles
        .saturating_sub(before.estimated_cycles);
    let frames = frames.max(1);
    println!("gte_profile:");
    println!("  ops={}  per_frame={:.0}", ops, ops as f64 / frames as f64);
    println!(
        "  estimated_cycles={}  per_frame={:.0}",
        cycles,
        cycles as f64 / frames as f64
    );
    println!("  opcodes:");
    for opcode in 0..after.opcode_counts.len() {
        let count = after.opcode_counts[opcode].saturating_sub(before.opcode_counts[opcode]);
        if count == 0 {
            continue;
        }
        println!(
            "    0x{opcode:02x} {:<6} count={:<10} per_frame={:.0}",
            gte_opcode_name(opcode as u8),
            count,
            count as f64 / frames as f64
        );
    }
}

fn gte_opcode_name(opcode: u8) -> &'static str {
    match opcode {
        0x01 => "RTPS",
        0x06 => "NCLIP",
        0x0c => "OP",
        0x10 => "DPCS",
        0x11 => "INTPL",
        0x12 => "MVMVA",
        0x13 => "NCDS",
        0x14 => "CDP",
        0x16 => "NCDT",
        0x1b => "NCCS",
        0x1c => "CC",
        0x1e => "NCS",
        0x20 => "NCT",
        0x28 => "SQR",
        0x29 => "DCPL",
        0x2a => "DPCT",
        0x2d => "AVSZ3",
        0x2e => "AVSZ4",
        0x30 => "RTPT",
        0x3d => "GPF",
        0x3e => "GPL",
        0x3f => "NCCT",
        _ => "UNKNOWN",
    }
}

fn print_guest_profile(summary: &telemetry::GuestTelemetrySummary) {
    if !summary.has_data() {
        println!("guest_profile=empty");
        return;
    }

    let frames = summary.frames.max(1) as f32;
    println!("guest_profile_frames={}", summary.frames);
    println!("guest_profile_frame_meaning=frame_begin_markers");
    print_guest_pacing_profile(summary);
    print_guest_render_breakdown(summary);
    println!("guest_profile_tasks:");
    for id in 0..telemetry::TASK_COUNT {
        let cycles = summary.task_cycles[id];
        if cycles == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_hit={:.0} max_hit={} hits={}",
            telemetry::task_name(id as u16),
            cycles,
            cycles as f32 / (summary.task_hits[id].max(1) as f32),
            summary.task_max_cycles[id],
            summary.task_hits[id],
        );
    }
    println!("guest_profile_stages:");
    for id in 1..telemetry::STAGE_COUNT {
        let cycles = summary.stage_cycles[id];
        if cycles == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_frame={:.0} per_hit={:.0} max_hit={} hits={}",
            telemetry::stage_name(id as u16),
            cycles,
            cycles as f32 / frames,
            cycles as f32 / (summary.stage_hits[id].max(1) as f32),
            summary.stage_max_cycles[id],
            summary.stage_hits[id],
        );
    }
    println!("guest_profile_counters:");
    for id in 1..telemetry::COUNTER_COUNT {
        let value = summary.counters[id];
        if value == 0 {
            continue;
        }
        println!(
            "  {:<18} total={:<10} per_frame={:.0} latest={}",
            telemetry::counter_name(id as u16),
            value,
            value as f32 / frames,
            summary.counter_latest_values[id],
        );
    }
}

fn print_guest_render_breakdown(summary: &telemetry::GuestTelemetrySummary) {
    let render_cycles = summary.stage_cycles[telemetry::stage::RENDER as usize];
    if render_cycles == 0 {
        println!("guest_profile_render_breakdown=not_emitted");
        return;
    }

    let render_hits = summary.stage_hits[telemetry::stage::RENDER as usize].max(1);
    let mut accounted = 0u64;
    println!("guest_profile_render_breakdown:");
    for &(stage_id, label) in GUEST_RENDER_BREAKDOWN_STAGES {
        let cycles = summary.stage_cycles[stage_id as usize];
        if cycles == 0 {
            continue;
        }
        accounted = accounted.saturating_add(cycles);
        println!(
            "  {:<18} pct={:>5.1} per_render={:.0} cycles={}",
            label,
            percent_u64(cycles, render_cycles),
            cycles as f64 / render_hits as f64,
            cycles,
        );
    }

    let other = render_cycles.saturating_sub(accounted);
    if other > render_cycles / 200 {
        println!(
            "  {:<18} pct={:>5.1} per_render={:.0} cycles={}",
            "other",
            percent_u64(other, render_cycles),
            other as f64 / render_hits as f64,
            other,
        );
    }
}

fn percent_u64(part: u64, total: u64) -> f64 {
    (part as f64) * 100.0 / total.max(1) as f64
}

fn print_guest_pacing_profile(summary: &telemetry::GuestTelemetrySummary) {
    let pacing_ids = [
        telemetry::counter::SIM_TICKS,
        telemetry::counter::VISUAL_FRAMES,
        telemetry::counter::VISUAL_SKIPPED_VBLANKS,
        telemetry::counter::VISUAL_DEADLINE_MISSES,
        telemetry::counter::VISUAL_INTERVAL_VBLANKS,
        telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS,
    ];
    let has_pacing = pacing_ids
        .iter()
        .any(|&id| counter_total(summary, id) > 0 || counter_max_value(summary, id) > 0);
    if !has_pacing {
        println!("guest_profile_pacing=not_emitted");
        return;
    }

    let sim_ticks = counter_total(summary, telemetry::counter::SIM_TICKS);
    let visual_frames = counter_total(summary, telemetry::counter::VISUAL_FRAMES);
    let skipped = counter_total(summary, telemetry::counter::VISUAL_SKIPPED_VBLANKS);
    let misses = counter_total(summary, telemetry::counter::VISUAL_DEADLINE_MISSES);
    let interval_total = counter_total(summary, telemetry::counter::VISUAL_INTERVAL_VBLANKS);
    let max_lateness = counter_max_value(summary, telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS);
    let interval = if summary.frames > 0 && interval_total > 0 {
        Some(interval_total as f64 / summary.frames as f64)
    } else {
        None
    };
    let update_per_sim = div_u64(
        summary.stage_cycles[telemetry::stage::UPDATE as usize],
        sim_ticks,
    );
    let render_per_visual = div_u64(
        summary.stage_cycles[telemetry::stage::RENDER as usize],
        visual_frames,
    );
    let visual_budget = interval.map(|vblanks| vblanks * NTSC_CPU_CYCLES_PER_VBLANK as f64);

    println!("guest_profile_pacing:");
    println!("  sim_ticks={}", fmt_known_u64(sim_ticks));
    println!("  visual_frames={}", fmt_known_u64(visual_frames));
    println!("  visual_skipped_vblanks={}", skipped);
    println!("  visual_deadline_misses={}", misses);
    println!("  visual_interval_vblanks={}", fmt_optional_f64(interval));
    println!("  visual_max_lateness_vblanks={}", max_lateness);
    println!(
        "  update_cycles_per_sim_tick={}",
        fmt_optional_f64(update_per_sim)
    );
    println!(
        "  render_cycles_per_visual_frame={}",
        fmt_optional_f64(render_per_visual)
    );
    println!(
        "  visual_budget_cycles={}  vblanks={}  cycles_per_vblank={}",
        fmt_optional_f64(visual_budget),
        fmt_optional_f64_2(interval),
        NTSC_CPU_CYCLES_PER_VBLANK
    );
    println!(
        "  visual_budget_status={}",
        visual_budget_status(render_per_visual, visual_budget)
    );
    println!(
        "  cadence_status={}",
        cadence_status(interval, misses, max_lateness)
    );
}

fn counter_total(summary: &telemetry::GuestTelemetrySummary, id: u16) -> u64 {
    summary
        .counters
        .get(id as usize)
        .copied()
        .unwrap_or_default()
}

fn counter_max_value(summary: &telemetry::GuestTelemetrySummary, id: u16) -> u32 {
    summary
        .counter_max_values
        .get(id as usize)
        .copied()
        .unwrap_or_default()
}

fn div_u64(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn fmt_known_u64(value: u64) -> String {
    if value == 0 {
        "unknown".to_string()
    } else {
        value.to_string()
    }
}

fn fmt_optional_f64(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.0}"),
        None => "unknown".to_string(),
    }
}

fn fmt_optional_f64_2(value: Option<f64>) -> String {
    match value {
        Some(value) => format!("{value:.2}"),
        None => "unknown".to_string(),
    }
}

fn visual_budget_status(
    render_per_visual: Option<f64>,
    visual_budget: Option<f64>,
) -> &'static str {
    match (render_per_visual, visual_budget) {
        (Some(cycles), Some(budget)) if cycles <= budget => "pass",
        (Some(_), Some(_)) => "fail",
        _ => "unknown",
    }
}

fn cadence_status(interval: Option<f64>, misses: u64, max_lateness: u32) -> &'static str {
    match interval {
        Some(_) if misses == 0 && max_lateness == 0 => "steady",
        Some(_) => "missed_or_late",
        None => "unknown",
    }
}

fn maybe_fast_boot_disc(
    bus: &mut Bus,
    cpu: &mut Cpu,
    disc: &Disc,
    path: &std::path::Path,
    enabled: bool,
) {
    if !enabled {
        return;
    }
    if let Err(e) = warm_bios_for_disc_fast_boot(bus, cpu, DISC_FAST_BOOT_WARMUP_STEPS) {
        eprintln!(
            "[cli] BIOS warmup failed for {} ({e:?}); leaving BIOS boot fallback in place",
            path.display()
        );
        return;
    }
    match fast_boot_disc_with_hle(bus, cpu, disc, false) {
        Ok(info) => eprintln!(
            "[cli] warm-fast-booted {} via {} entry=0x{:08x} load=0x{:08x} payload={}B",
            path.display(),
            info.boot_path,
            info.initial_pc,
            info.load_addr,
            info.payload_len
        ),
        Err(e) => eprintln!(
            "[cli] fast boot unavailable for {} ({e:?}); falling back to BIOS boot",
            path.display()
        ),
    }
}

fn fmt_empty(s: &str) -> String {
    if s.is_empty() {
        "(unset)".into()
    } else {
        s.to_string()
    }
}

fn kind_label(k: GameKind) -> &'static str {
    match k {
        GameKind::DiscBin => "disc-bin",
        GameKind::DiscIso => "disc-iso",
        GameKind::DiscCue => "disc-cue",
        GameKind::DiscCcd => "disc-ccd",
        GameKind::Exe => "homebrew",
        GameKind::Unknown => "unknown",
    }
}

fn region_label(e: &LibraryEntry) -> &'static str {
    use psoxide_settings::library::Region;
    match e.region {
        Region::NtscU => "NTSC-U",
        Region::Pal => "PAL",
        Region::NtscJ => "NTSC-J",
        Region::Unknown => "unknown",
    }
}

fn dump_hw_ppm(bus: &Bus, path: &std::path::Path) -> Result<bool, String> {
    let display = bus.gpu.display_area();
    if display.bpp24 {
        let (rgba, w, h) = bus.gpu.display_rgba8();
        write_rgb_ppm_from_rgba(path, w, h, &rgba)?;
        return Ok(true);
    }

    let (device, queue) = headless_wgpu_device()?;

    let mut hw = psx_gpu_render::HwRenderer::new_headless(device, queue);
    let initial_vram =
        vec![0u16; (psx_gpu_render::VRAM_WIDTH * psx_gpu_render::VRAM_HEIGHT) as usize];
    hw.render_frame(&bus.gpu, &bus.gpu.cmd_log, &initial_vram);

    let s = hw.internal_scale();
    let (w, h, rgba) = hw.read_subrect_rgba8(
        display.x as u32 * s,
        display.y as u32 * s,
        display.width as u32 * s,
        display.height as u32 * s,
    );
    write_rgb_ppm_from_rgba(path, w, h, &rgba)?;
    Ok(false)
}

fn headless_wgpu_device() -> Result<(wgpu::Device, wgpu::Queue), String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
        backends: wgpu::Backends::PRIMARY,
        ..Default::default()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or_else(|| "no compatible wgpu adapter".to_string())?;
    pollster::block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("psoxide-hw-dump-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        },
        None,
    ))
    .map_err(|e| format!("request device: {e:?}"))
}

fn write_rgb_ppm_from_rgba(
    path: &std::path::Path,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> Result<(), String> {
    use std::io::Write;
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    writeln!(f, "P6\n{w} {h}\n255").map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for chunk in rgba.chunks_exact(4) {
        rgb.push(chunk[0]);
        rgb.push(chunk[1]);
        rgb.push(chunk[2]);
    }
    f.write_all(&rgb).map_err(|e| e.to_string())?;
    Ok(())
}

fn dump_vram_ppm(bus: &Bus, path: &std::path::Path) -> Result<(), String> {
    use std::io::Write;
    let w = emulator_core::VRAM_WIDTH;
    let h = emulator_core::VRAM_HEIGHT;
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    writeln!(f, "P6\n{w} {h}\n255").map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for &pix in bus.gpu.vram.words() {
        let r5 = (pix & 0x1F) as u8;
        let g5 = ((pix >> 5) & 0x1F) as u8;
        let b5 = ((pix >> 10) & 0x1F) as u8;
        rgb.push((r5 << 3) | (r5 >> 2));
        rgb.push((g5 << 3) | (g5 >> 2));
        rgb.push((b5 << 3) | (b5 >> 2));
    }
    f.write_all(&rgb).map_err(|e| e.to_string())?;
    Ok(())
}
