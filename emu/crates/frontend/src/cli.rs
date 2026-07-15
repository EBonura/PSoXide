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

#[cfg(feature = "editor")]
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};
use emulator_core::{
    button, fast_boot_disc_with_hle, spu::SAMPLE_CYCLES, telemetry, warm_bios_for_disc_fast_boot,
    Bus, ButtonState, Cpu, DISC_FAST_BOOT_WARMUP_STEPS,
};
// `Gpu` is only constructed for the editor 3D preview dump.
#[cfg(feature = "editor")]
use emulator_core::Gpu;
use psoxide_settings::{
    library::{GameKind, LibraryEntry},
    ConfigPaths, Library, Settings,
};
use psoxide_validation::{
    format_hash, ActualHashes, PixelHash, ValidationArtifact, ValidationRunner, ValidationSuite,
};
use psx_iso::{Disc, Exe, TrackType};
#[cfg(feature = "editor")]
use psxed_project::{NodeId, ProjectDocument};
#[cfg(feature = "editor")]
use psxed_ui::{ViewportCameraMode, ViewportCameraState};

use crate::app::{bus_from_configured_bios, fast_boot_embedded_playtest_disc};
#[cfg(feature = "editor")]
use crate::playtest_disc::{build_embedded_playtest_disc, copy_project_disc};
use crate::playtest_input::read_input_tape;

mod headless_log;
mod iso_inspect;
mod profile_report;

use headless_log::{CounterLog, DisplayHashLog, GuestProfileLog};
use iso_inspect::{contains_bytes, iso_root_entries, iso_volume_id};
use profile_report::{print_gte_profile, print_guest_profile};

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

    /// Run the shadow compute-shader rasterizer alongside the CPU
    /// rasterizer: per frame the frontend drains the CPU's `cmd_log`
    /// and replays each GP0 packet through the GPU compute path. This
    /// is the A/B instrument for aligning the hardware renderer with
    /// the silicon-verified CPU rasterizer (the CPU path is the
    /// reference). Press F12 in the GUI to switch which output is
    /// displayed.
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
    #[cfg(feature = "editor")]
    BuildEditorPlaytestDisc,
    /// Cook, build, and export a project CUE/BIN disc without opening the GUI.
    #[cfg(feature = "editor")]
    BuildProjectDisc(BuildProjectDiscArgs),
    /// Validate an authored CUE/BIN image before burning it to CD-R.
    PreburnCheck(PreburnCheckArgs),
    /// Render an editor 3D preview screenshot without opening the GUI.
    #[cfg(feature = "editor")]
    DumpEditorPreview(DumpEditorPreviewArgs),
    /// Run exact-hash validation checkpoints from a manifest.
    Validate(ValidateArgs),
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
#[derive(Debug, Clone, Args)]
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
    /// Press pad-1 button masks on the headless route clock. Format:
    /// `<mask>@<tick>+<frames>`, comma-separated, e.g.
    /// `0x4000@45+12,0x4000@80+16`.
    #[arg(long)]
    pub pad_pulses: Option<String>,
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
    /// Print guest debug-log lines (the editor's Debug Viz channel) to
    /// stderr as they arrive. Requires a guest built with the
    /// `emulator-telemetry` feature; silent otherwise.
    #[arg(long)]
    pub guest_debug_log: bool,
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
    /// Write a per-guest-frame CSV of frametime (bus-cycle delta) alongside the
    /// portal/streaming masks and camera position. Lets a streaming/visibility
    /// change be measured frame-by-frame and a drawn-but-not-visible room be
    /// pinned to an exact camera position over a recorded sweep.
    #[arg(long)]
    pub counter_log: Option<PathBuf>,
    /// Write a per-completed-guest-frame CSV of guest telemetry stages and
    /// counters. Use this with a recorded input tape to identify which systems
    /// stack together on deadline-miss spikes.
    #[arg(long)]
    pub profile_log: Option<PathBuf>,
    /// Optional path to dump the final VRAM as a raw PPM image.
    /// Lets you eyeball the boot state without firing up the GUI.
    #[arg(long)]
    pub dump_vram: Option<PathBuf>,
    /// Optional path to dump the final 2 MiB main RAM as a raw binary
    /// blob, for offline diffing of guest state across two runs.
    #[arg(long)]
    pub dump_ram: Option<PathBuf>,
    /// Optional path to dump the HW renderer's output as a PPM. Spins
    /// up a headless wgpu device, replays the cumulative `cmd_log`
    /// through the same pipeline the live GUI uses, and writes the
    /// result. Use this to regression-test the HW pipeline without
    /// a window or screen-capture permission.
    #[arg(long)]
    pub dump_hw: Option<PathBuf>,
    /// Optional path to dump the CPU rasterizer's DISPLAY image (the
    /// `display_rgba8` view: the display sub-rect of software VRAM,
    /// 24bpp-aware) as a PPM. Pair with `--dump-hw` at the same step
    /// count for an apples-to-apples backend comparison.
    #[arg(long)]
    pub dump_display: Option<PathBuf>,
    /// Optional path to dump the SPU's mixed stereo output as a 16-bit
    /// 44.1 kHz WAV, for A/B comparison against a reference emulator.
    #[arg(long)]
    pub dump_audio: Option<PathBuf>,
    /// Print a guest-runtime telemetry summary captured out-of-band.
    #[arg(long)]
    pub dump_guest_profile: bool,
    /// Hold the left analog stick fully forward during the headless run.
    #[arg(long)]
    pub hold_forward: bool,
    /// Hold the game run button during the headless run.
    #[arg(long)]
    pub hold_run: bool,
    /// Enable GPU wireframe render mode (edges only) for this run, mirroring the
    /// toolbar toggle. Pair with `--dump-hw` for a single-frame edge render.
    #[arg(long)]
    pub wireframe: bool,
    /// Sample-time texture filter for `--dump-hw`: none|xbr.
    #[arg(long, default_value = "none")]
    pub texture_filter: String,
}

/// Arguments for `build-project-disc`.
#[cfg(feature = "editor")]
#[derive(Debug, Args)]
pub struct BuildProjectDiscArgs {
    /// Project directory containing `project.ron`, or a direct path to a project file.
    #[arg(long, default_value = "editor/projects/default")]
    pub project: PathBuf,
}

/// Arguments for `preburn-check`.
#[derive(Debug, Args)]
pub struct PreburnCheckArgs {
    /// Project CUE path to validate.
    #[arg(long)]
    pub cue: PathBuf,
    /// Built PSX EXE to scan for forbidden diagnostic strings.
    #[arg(long)]
    pub exe: Option<PathBuf>,
    /// Expected ISO9660 volume ID.
    #[arg(long)]
    pub volume: Option<String>,
    /// Root ISO file that must exist, e.g. `SYSTEM.CNF;1`.
    #[arg(long)]
    pub require_file: Vec<String>,
    /// Require at least one CD-DA audio track in the CUE.
    #[arg(long)]
    pub require_audio_track: bool,
    /// Fail if this string appears in the built EXE.
    #[arg(long)]
    pub forbid_exe_string: Vec<String>,
}

/// Arguments for `dump-editor-preview`.
#[cfg(feature = "editor")]
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
    /// Active floor to render (0 = ground). Diagnostic for stacked-floor rooms.
    #[arg(long, default_value_t = 0)]
    pub active_floor: usize,
}

/// Arguments for `validate`.
#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// RON validation suite manifest.
    #[arg(long, default_value = "validation/suite.ron")]
    pub manifest: PathBuf,
    /// Runner to execute. Currently `psoxide`; manifest room is reserved for
    /// `redux` and `duckstation`.
    #[arg(long, default_value = "psoxide")]
    pub runner: ValidationRunner,
    /// Optional target-name filter.
    #[arg(long)]
    pub target: Option<String>,
    /// Optional checkpoint-name filter.
    #[arg(long)]
    pub checkpoint: Option<String>,
    /// Run each selected checkpoint N times against the same baseline. Useful
    /// for catching nondeterministic hashes without recooking the target.
    #[arg(long, default_value_t = 1)]
    pub repeat: u32,
    /// Directory where failed validation runs write display/VRAM images and
    /// metadata.
    #[arg(long, default_value = "validation/artifacts")]
    pub artifact_dir: PathBuf,
    /// Write observed hashes back into the manifest instead of failing on
    /// missing or changed baselines.
    #[arg(long)]
    pub bless: bool,
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
        #[cfg(feature = "editor")]
        Command::BuildEditorPlaytestDisc => cmd_build_editor_playtest_disc(),
        #[cfg(feature = "editor")]
        Command::BuildProjectDisc(args) => cmd_build_project_disc(args),
        Command::PreburnCheck(args) => cmd_preburn_check(args),
        #[cfg(feature = "editor")]
        Command::DumpEditorPreview(args) => cmd_dump_editor_preview(args),
        Command::Validate(args) => cmd_validate(&paths, args),
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
        println!("(library is empty - run `scan` first)");
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
    let _ = run_headless_launch(paths, args, true)?;
    Ok(())
}

#[derive(Debug, Clone)]
struct HeadlessLaunchResult {
    tick: u64,
    cycles: u64,
    pc: u32,
    stopped_at: Option<u64>,
    vram_hash: u64,
    display_hash: u64,
    display_width: u32,
    display_height: u32,
    display_byte_len: u64,
    guest_frames: u64,
    visual_frames: u64,
    display_rgba: Vec<u8>,
    display_rgba_width: u32,
    display_rgba_height: u32,
    vram_words: Vec<u16>,
}

fn run_headless_launch(
    paths: &ConfigPaths,
    args: LaunchArgs,
    emit_summary: bool,
) -> Result<HeadlessLaunchResult, String> {
    let settings = Settings::load(&paths.settings_file()).unwrap_or_default();
    let pad_pulses = args
        .pad_pulses
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(parse_pad_pulses)
        .transpose()?
        .unwrap_or_default();
    let has_pad_pulses = !pad_pulses.is_empty();
    if args.input_tape.is_some() && (args.hold_forward || args.hold_run) {
        return Err(
            "--input-tape cannot be combined with --hold-forward or --hold-run".to_string(),
        );
    }
    if args.input_tape.is_some() && has_pad_pulses {
        return Err("--input-tape cannot be combined with --pad-pulses".to_string());
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
            if emit_summary {
                eprintln!(
                    "[cli] loaded input tape {} ({} frames)",
                    path.display(),
                    samples.len()
                );
            }
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
            if emit_summary {
                eprintln!(
                    "[cli] side-loaded {} - entry=0x{:08x} payload={}B",
                    game_path.display(),
                    exe.initial_pc,
                    exe.payload.len()
                );
            }
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
            if emit_summary {
                eprintln!("[cli] mounted disc {}", game_path.display());
            }
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
            if emit_summary {
                eprintln!("[cli] mounted cue-backed disc {}", game_path.display());
            }
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
            if emit_summary {
                eprintln!("[cli] mounted ccd-backed disc {}", game_path.display());
            }
            bus
        }
        other => {
            return Err(format!("unsupported file extension: .{other}"));
        }
    };

    if args.wireframe {
        bus.gpu.wireframe_enabled = true;
    }

    let mut held_button_mask = 0u16;
    if args.hold_forward || args.hold_run {
        if args.hold_run {
            held_button_mask |= button::CIRCLE;
        }
        bus.set_port1_buttons(ButtonState::from_bits(held_button_mask));
        if args.hold_forward {
            bus.set_port1_sticks(0x80, 0x80, 0x80, 0x00);
        }
    }
    // Headless route clock. The editor records/replays exactly one tape sample
    // per `app::step_one_frame` tick -- a `vblank_period` cycle budget (capped
    // at `run_steps_per_frame` instructions). It does NOT key scripted input to
    // `frames_seen` (guest FrameBegin events), which stall during loading while
    // the editor keeps ticking. Mirror that tick here so a headless replay or
    // `--pad-pulses` route reproduces the editor frame-for-frame; keying off
    // `frames_seen` desyncs the whole run after the load.
    const ROUTE_RUN_STEPS_PER_FRAME: u64 = 1_000_000; // matches AppState default
    let route_vblank_period = bus.vblank_period().max(1);
    let mut route_tick_deadline = bus.cycles().saturating_add(route_vblank_period);
    let mut route_tick_steps = 0u64;
    let mut route_ticks = 0u64;
    let mut tape_cursor = 0usize;
    if let Some(samples) = tape_samples.as_ref() {
        samples[tape_cursor].apply_to_bus(&mut bus);
    }
    let mut current_pulsed_button_mask = None;
    if has_pad_pulses {
        sync_pad_pulses(
            &mut bus,
            held_button_mask,
            &pad_pulses,
            route_ticks,
            &mut current_pulsed_button_mask,
        );
    }
    let collect_profile_events = args.dump_guest_profile || args.profile_log.is_some();
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
    let mut counter_log = CounterLog::new(args.counter_log.as_deref())?;
    let mut observed_counter_frames = observed_guest_frames;
    let mut profile_log = GuestProfileLog::new(args.profile_log.as_deref())?;

    // Step the CPU. Report early on opcode errors -- they're usually
    // "we hit an unimplemented instruction" and worth surfacing.
    let mut stopped_at: Option<u64> = None;
    let mut audio_cycle_accum = 0u64;
    let mut audio_capture: Vec<(i16, i16)> = Vec::new();
    let gte_profile_before = cpu.cop2().profile_snapshot();
    for i in 0..args.steps {
        let cycles_before = bus.cycles();
        if let Err(e) = cpu.step(&mut bus) {
            eprintln!("[cli] step {i} failed: {e:?}");
            eprintln!(
                "[cli] regs: ra=0x{:08x} sp=0x{:08x} gp=0x{:08x} fp=0x{:08x} a0=0x{:08x} a1=0x{:08x} v0=0x{:08x} v1=0x{:08x} t9=0x{:08x}",
                cpu.gpr(31), cpu.gpr(29), cpu.gpr(28), cpu.gpr(30),
                cpu.gpr(4), cpu.gpr(5), cpu.gpr(2), cpu.gpr(3), cpu.gpr(25)
            );
            stopped_at = Some(i);
            break;
        }
        audio_cycle_accum =
            audio_cycle_accum.saturating_add(bus.cycles().saturating_sub(cycles_before));
        let sample_count = (audio_cycle_accum / SAMPLE_CYCLES) as usize;
        audio_cycle_accum %= SAMPLE_CYCLES;
        if sample_count > 0 {
            bus.run_spu_samples(sample_count);
            let drained = bus.spu.drain_audio();
            if args.dump_audio.is_some() {
                audio_capture.extend(drained);
            }
        }
        let current_guest_frames = bus.telemetry.frames_seen();
        if args.guest_debug_log {
            for line in bus.telemetry.drain_debug_logs() {
                eprintln!("[guest f{} c{}] {}", line.frame, line.cycles, line.text);
            }
        }
        // Advance on the editor's tick clock (see setup above), not on
        // `frames_seen`: one sample/pulse window per vblank-period cycle
        // budget, capped at the same instruction budget `step_one_frame` uses.
        route_tick_steps += 1;
        if bus.cycles() >= route_tick_deadline || route_tick_steps >= ROUTE_RUN_STEPS_PER_FRAME {
            route_tick_deadline = bus.cycles().saturating_add(route_vblank_period);
            route_tick_steps = 0;
            route_ticks += 1;
            if let Some(samples) = tape_samples.as_ref() {
                let cursor = (route_ticks as usize).min(samples.len().saturating_sub(1));
                if cursor != tape_cursor {
                    tape_cursor = cursor;
                    samples[tape_cursor].apply_to_bus(&mut bus);
                }
            }
            if has_pad_pulses {
                sync_pad_pulses(
                    &mut bus,
                    held_button_mask,
                    &pad_pulses,
                    route_ticks,
                    &mut current_pulsed_button_mask,
                );
            }
        }
        if current_guest_frames != observed_guest_frames {
            if collect_profile_events {
                let events = bus.telemetry.drain_events();
                if let Some(summary) = profile_summary.as_mut() {
                    summary.add_events(&events);
                }
                profile_log.add_events(&events, cpu.tick(), bus.cycles())?;
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
        while observed_counter_frames < current_guest_frames {
            observed_counter_frames += 1;
            counter_log.record(observed_counter_frames, cpu.tick(), bus.cycles(), &bus)?;
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
            // When replaying a tape the natural clock is tape ticks, not
            // `frames_seen`; otherwise `--guest-frames N` stops at the wrong
            // moment now that the tape advances on the vblank tick.
            let reached = if tape_samples.is_some() {
                route_ticks >= target
            } else {
                bus.telemetry.frames_seen() >= target
            };
            if target > 0 && reached {
                stopped_at = Some(i + 1);
                break;
            }
        }
    }
    if collect_profile_events {
        let events = bus.telemetry.drain_events();
        if let Some(summary) = profile_summary.as_mut() {
            summary.add_events(&events);
        }
        profile_log.add_events(&events, cpu.tick(), bus.cycles())?;
    }
    profile_log.finish(cpu.tick(), bus.cycles())?;
    visual_hash_log.flush()?;
    guest_hash_log.flush()?;
    counter_log.flush()?;
    profile_log.flush()?;

    if emit_summary {
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
        if std::env::var_os("PSOXIDE_TRACE_HLE_BIOS").is_some() {
            eprintln!(
                "[hle-bios] sr={:08x} istat={:03x} imask={:03x} irq-high-steps={} irq-taken={}",
                cpu.cop0()[12],
                bus.irq().stat(),
                bus.irq().mask(),
                cpu.irq_line_high_steps(),
                cpu.should_take_interrupt_steps()
            );
        }
    }
    let gte_profile_after = cpu.cop2().profile_snapshot();

    if args.dump_hash {
        let h = hash_vram(&bus);
        let (dh, dw, dhi, _) = bus.gpu.display_hash();
        println!("vram_fnv1a_64=0x{h:016x}");
        println!("display_fnv1a_64=0x{dh:016x}  w={dw}  h={dhi}");
    }

    if args.dump_guest_profile {
        let counter_totals = bus.telemetry.counter_totals();
        let counter_max_values = bus.telemetry.counter_max_values();
        let counter_latest_values = bus.telemetry.counter_latest_values();
        let mut summary = profile_summary.unwrap_or_default();
        summary.counters = counter_totals;
        summary.counter_max_values = counter_max_values;
        summary.counter_latest_values = counter_latest_values;
        print_guest_profile(&summary);
        print_gte_profile(&gte_profile_before, &gte_profile_after, &summary);
    }

    if let Some(path) = args.dump_vram {
        dump_vram_ppm(&bus, &path)?;
        if emit_summary {
            eprintln!("[cli] VRAM → {}", path.display());
        }
    }

    if let Some(path) = args.dump_audio.as_ref() {
        write_wav_s16_stereo(path, 44_100, &audio_capture)?;
        if emit_summary {
            eprintln!(
                "[cli] audio → {} ({} stereo samples, {:.2}s @ 44.1kHz)",
                path.display(),
                audio_capture.len(),
                audio_capture.len() as f64 / 44_100.0
            );
        }
    }

    if let Some(path) = args.dump_hw {
        let fallback = dump_hw_ppm(&bus, &path, parse_texture_filter(&args.texture_filter))?;
        if emit_summary {
            if let Some(reason) = fallback {
                eprintln!("[cli] HW renderer → {} ({reason})", path.display());
            } else {
                eprintln!(
                    "[cli] HW renderer → {} ({} cmd_log entries replayed)",
                    path.display(),
                    bus.gpu.cmd_log.len()
                );
            }
            // Display + cmd_log state at dump time: the first thing to read
            // when a dump comes out black (wrong display area? bpp24? did
            // any draw/upload reach the GPU at all?).
            let da = bus.gpu.display_area();
            let (mut draws, mut fills, mut uploads) = (0u64, 0u64, 0u64);
            for e in bus.gpu.cmd_log.iter() {
                match e.opcode {
                    0x20..=0x7F => draws += 1,
                    0x02 => fills += 1,
                    0xA0..=0xBF => uploads += 1,
                    _ => {}
                }
            }
            eprintln!(
                "[cli] display ({},{}) {}x{} bpp={} | cmd_log draws={} fills={} uploads={}",
                da.x,
                da.y,
                da.width,
                da.height,
                if da.bpp24 { 24 } else { 15 },
                draws,
                fills,
                uploads
            );
            // Every distinct GP1 08h display-mode word the game has set --
            // bit 4 = 24bpp. Answers "did the game EVER request 24bpp?".
            let modes: Vec<String> = bus
                .gpu
                .display_mode_history()
                .map(|m| format!("{m:06X}{}", if m & 0x10 != 0 { "(24bpp)" } else { "" }))
                .collect();
            eprintln!("[cli] GP1 08h modes seen: {}", modes.join(" "));
            // Recent display-start moves (GP1 05h), decoded to (x,y) -- the
            // double-buffer flip trail. Shows where the display pointed.
            let starts: Vec<String> = bus
                .gpu
                .gp1_write_history()
                .iter()
                .filter(|w| (**w >> 24) & 0x3F == 0x05)
                .map(|w| format!("({},{})", w & 0x3FF, (w >> 10) & 0x1FF))
                .collect();
            let tail: Vec<&String> = starts.iter().rev().take(24).collect();
            eprintln!(
                "[cli] GP1 05h display-start trail (last {} of {}): {:?}",
                tail.len(),
                starts.len(),
                tail.iter().rev().map(|s| s.as_str()).collect::<Vec<_>>()
            );
            // Optional raw cmd_log dump (index,opcode,word0) for offline
            // analysis of draw parameters over time (e.g. fade tints).
            if let Ok(p) = std::env::var("PSOXIDE_DUMP_CMDLOG") {
                let mut out = String::new();
                for e in bus.gpu.cmd_log.iter() {
                    let w = |i: usize| e.fifo.get(i).copied().unwrap_or(0);
                    out.push_str(&format!(
                        "{},{:#04X},{:#010X},{:#010X},{:#010X}\n",
                        e.index,
                        e.opcode,
                        w(0),
                        w(1),
                        w(2)
                    ));
                }
                let _ = std::fs::write(&p, out);
                eprintln!("[cli] cmd_log csv → {p}");
            }
        }
    }

    if let Some(path) = args.dump_display {
        let (rgba, w, h) = bus.gpu.display_rgba8();
        write_rgb_ppm_from_rgba(&path, w, h, &rgba)?;
        if emit_summary {
            eprintln!("[cli] CPU display → {} ({w}x{h})", path.display());
        }
    }

    if let Some(path) = args.dump_ram {
        std::fs::write(&path, bus.ram()).map_err(|e| format!("write ram dump: {e}"))?;
        if emit_summary {
            eprintln!(
                "[cli] main RAM → {} ({} bytes)",
                path.display(),
                bus.ram().len()
            );
        }
    }

    let (display_hash, display_width, display_height, display_byte_len) = bus.gpu.display_hash();
    let (display_rgba, display_rgba_width, display_rgba_height) = bus.gpu.display_rgba8();
    Ok(HeadlessLaunchResult {
        tick: cpu.tick(),
        cycles: bus.cycles(),
        pc: cpu.pc(),
        stopped_at,
        vram_hash: hash_vram(&bus),
        display_hash,
        display_width,
        display_height,
        display_byte_len: display_byte_len as u64,
        guest_frames: bus.telemetry.frames_seen(),
        visual_frames: bus
            .telemetry
            .counter_total(telemetry::counter::VISUAL_FRAMES),
        display_rgba,
        display_rgba_width,
        display_rgba_height,
        vram_words: bus.gpu.vram.words().to_vec(),
    })
}

/// Write 16-bit stereo PCM as a canonical 44-byte-header WAV. No crate
/// dependency -- the SPU output is already interleaved `(left, right)`.
fn write_wav_s16_stereo(
    path: &std::path::Path,
    sample_rate: u32,
    samples: &[(i16, i16)],
) -> Result<(), String> {
    let data_len = (samples.len() as u32).saturating_mul(4); // 2 ch * 2 bytes
    let mut out: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // format = PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // channels
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for (l, r) in samples {
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&r.to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("write wav {}: {e}", path.display()))
}

#[cfg(feature = "editor")]
fn cmd_build_editor_playtest_disc() -> Result<(), String> {
    let disc_path =
        build_embedded_playtest_disc(crate::playtest_disc::DEFAULT_EMBEDDED_PLAYTEST_VOLUME_ID)?;
    println!("{}", disc_path.display());
    Ok(())
}

#[cfg(feature = "editor")]
fn cmd_build_project_disc(args: BuildProjectDiscArgs) -> Result<(), String> {
    let dest_cue = build_project_disc_path(&args.project)?;
    println!("{}", dest_cue.display());
    Ok(())
}

fn cmd_preburn_check(args: PreburnCheckArgs) -> Result<(), String> {
    let repo_root = cli_repo_root();
    let cue_path = resolve_cli_path(&repo_root, &args.cue);
    let disc = psoxide_settings::library::load_disc_from_cue(&cue_path)
        .map_err(|error| format!("load {}: {error}", cue_path.display()))?;

    let volume_id = iso_volume_id(&disc)?;
    if let Some(expected) = args.volume.as_deref() {
        if volume_id != expected {
            return Err(format!(
                "volume ID mismatch: expected {expected}, got {volume_id}"
            ));
        }
    }

    let track1 = disc
        .track(1)
        .ok_or_else(|| "disc has no track 1".to_string())?;
    if track1.track_type != TrackType::Data {
        return Err("track 1 is not a data track".to_string());
    }
    if args.require_audio_track {
        let has_audio_track = (1..=disc.last_track_number().unwrap_or(0))
            .filter_map(|number| disc.track(number))
            .any(|track| track.track_type == TrackType::Audio);
        if !has_audio_track {
            return Err("disc has no CD-DA audio track".to_string());
        }
    }

    let root_entries = iso_root_entries(&disc)?;
    let root_names: Vec<&str> = root_entries
        .iter()
        .filter(|entry| !entry.is_dir())
        .map(|entry| entry.identifier.as_str())
        .collect();
    for required in &args.require_file {
        let required = required.to_ascii_uppercase();
        if !root_names.iter().any(|name| *name == required) {
            return Err(format!("missing root file: {required}"));
        }
    }

    let boot = psx_iso::load_boot_exe_from_disc(&disc)
        .map_err(|error| format!("boot EXE lookup failed: {error:?}"))?;
    if boot.boot_path != "PSX.EXE;1" {
        return Err(format!(
            "SYSTEM.CNF boots {}, expected PSX.EXE;1",
            boot.boot_path
        ));
    }

    if let Some(exe_path) = args.exe.as_deref() {
        let exe_path = resolve_cli_path(&repo_root, exe_path);
        let exe = std::fs::read(&exe_path).map_err(|e| format!("{}: {e}", exe_path.display()))?;
        for forbidden in &args.forbid_exe_string {
            if contains_bytes(&exe, forbidden.as_bytes()) {
                return Err(format!(
                    "forbidden diagnostic string present in EXE: {forbidden:?}"
                ));
            }
        }
    }

    println!("preburn structural check ok");
    println!("  cue:       {}", cue_path.display());
    println!("  sectors:   {}", disc.sector_count());
    println!("  volume:    {volume_id}");
    println!(
        "  tracks:    {}",
        (1..=disc.last_track_number().unwrap_or(0))
            .filter_map(|number| disc.track(number))
            .map(|track| format!(
                "{:02}:{}",
                track.number,
                match track.track_type {
                    TrackType::Data => "DATA",
                    TrackType::Audio => "AUDIO",
                }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("  boot:      {}", boot.boot_path);
    println!("  root:      {}", root_names.join(", "));
    Ok(())
}

#[cfg(feature = "editor")]
fn build_project_disc_path(project_path: &Path) -> Result<PathBuf, String> {
    let (project_root, project_file) = resolve_project_arg(project_path);
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

    let volume_id = crate::playtest_disc::project_disc_volume_id(&project.name);
    let source_cue = build_embedded_playtest_disc(&volume_id)?;
    let dest_cue = project_root.join("baked").join(format!(
        "{}.cue",
        psxed_project::project_file_stem(&project.name)
    ));
    let bytes = copy_project_disc(&source_cue, &dest_cue)?;
    eprintln!(
        "[cli] project disc -> {} ({} KiB)",
        dest_cue.display(),
        bytes / 1024
    );
    Ok(dest_cue)
}

fn cmd_validate(paths: &ConfigPaths, args: ValidateArgs) -> Result<(), String> {
    if args.repeat == 0 {
        return Err("--repeat must be at least 1".to_string());
    }
    if args.bless && args.repeat != 1 {
        return Err("--bless cannot be combined with --repeat > 1".to_string());
    }

    let repo_root = cli_repo_root();
    let manifest_path = resolve_cli_path(&repo_root, &args.manifest);
    let manifest_dir = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let artifact_dir = resolve_cli_path(&repo_root, &args.artifact_dir);
    let mut suite = ValidationSuite::load(&manifest_path).map_err(|e| e.to_string())?;

    let mut selected_checkpoints = 0usize;
    let mut selected_runs = 0usize;
    let mut failed = 0usize;
    let mut blessed = false;
    for target in &mut suite.targets {
        if !matches_filter(&args.target, &target.name) {
            continue;
        }
        let has_selected_checkpoint = target.checkpoints.iter().any(|checkpoint| {
            matches_filter(&args.checkpoint, &checkpoint.name) && checkpoint.runner == args.runner
        });
        if !has_selected_checkpoint {
            continue;
        }
        let target_name = target.name.clone();
        let artifact = resolve_validation_artifact(&repo_root, &manifest_dir, &target.artifact)?;
        for checkpoint in &mut target.checkpoints {
            if !matches_filter(&args.checkpoint, &checkpoint.name)
                || checkpoint.runner != args.runner
            {
                continue;
            }
            selected_checkpoints += 1;
            if args.runner != ValidationRunner::Psoxide {
                return Err(format!(
                    "validation runner `{}` is present in the manifest but is not wired yet",
                    args.runner
                ));
            }
            let launch_args =
                validation_launch_args(&repo_root, &manifest_dir, &artifact, checkpoint);
            for repeat_index in 1..=args.repeat {
                selected_runs += 1;
                let result = run_headless_launch(paths, launch_args.clone(), false)?;
                let actual = ActualHashes {
                    display: PixelHash::from_u64(
                        result.display_hash,
                        result.display_width,
                        result.display_height,
                        result.display_byte_len,
                    ),
                    vram: format_hash(result.vram_hash),
                };
                let label =
                    validation_run_label(&target_name, &checkpoint.name, repeat_index, args.repeat);

                if args.bless {
                    checkpoint.expected.bless(&actual);
                    blessed = true;
                    println!(
                        "BLESS {label} display={} vram={} tick={} frames={}/{}",
                        actual.display.hash,
                        actual.vram,
                        result.tick,
                        result.guest_frames,
                        result.visual_frames
                    );
                    continue;
                }

                let report = checkpoint.expected.compare(&actual);
                if report.passed() {
                    println!(
                        "PASS  {label} display={} vram={} tick={} frames={}/{}",
                        actual.display.hash,
                        actual.vram,
                        result.tick,
                        result.guest_frames,
                        result.visual_frames
                    );
                } else {
                    failed += 1;
                    let artifact_path = write_validation_failure_artifacts(
                        &artifact_dir,
                        &target_name,
                        &checkpoint.name,
                        repeat_index,
                        args.repeat,
                        &actual,
                        &result,
                    )?;
                    println!(
                        "FAIL  {label} display={} vram={} tick={} cycles={} pc=0x{:08x}{} frames={}/{} artifacts={}",
                        actual.display.hash,
                        actual.vram,
                        result.tick,
                        result.cycles,
                        result.pc,
                        match result.stopped_at {
                            Some(step) => format!(" stopped-at={step}"),
                            None => String::new(),
                        },
                        result.guest_frames,
                        result.visual_frames,
                        artifact_path.display()
                    );
                    for mismatch in report.mismatches {
                        println!("      - {mismatch}");
                    }
                }
            }
        }
    }

    if selected_checkpoints == 0 {
        return Err(format!(
            "validation selected no checkpoints in {}",
            manifest_path.display()
        ));
    }
    if blessed {
        suite
            .save_pretty(&manifest_path)
            .map_err(|e| e.to_string())?;
        println!("updated {}", manifest_path.display());
    }
    if failed > 0 {
        Err(format!(
            "{failed}/{selected_runs} validation run(s) failed across {selected_checkpoints} checkpoint(s)"
        ))
    } else {
        println!(
            "{selected_runs} validation run(s) passed across {selected_checkpoints} checkpoint(s)"
        );
        Ok(())
    }
}

fn validation_run_label(target: &str, checkpoint: &str, repeat_index: u32, repeat: u32) -> String {
    if repeat > 1 {
        format!("{target}:{checkpoint}#{repeat_index}/{repeat}")
    } else {
        format!("{target}:{checkpoint}")
    }
}

fn write_validation_failure_artifacts(
    artifact_root: &Path,
    target: &str,
    checkpoint: &str,
    repeat_index: u32,
    repeat: u32,
    actual: &ActualHashes,
    result: &HeadlessLaunchResult,
) -> Result<PathBuf, String> {
    let mut dir_name = format!(
        "{}__{}",
        sanitize_artifact_segment(target),
        sanitize_artifact_segment(checkpoint)
    );
    if repeat > 1 {
        dir_name.push_str(&format!("__run_{repeat_index:02}_of_{repeat:02}"));
    }
    let dir = artifact_root.join(dir_name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

    write_rgb_ppm_from_rgba(
        &dir.join("display.ppm"),
        result.display_rgba_width,
        result.display_rgba_height,
        &result.display_rgba,
    )?;
    write_vram_words_ppm(&dir.join("vram.ppm"), &result.vram_words)?;

    let mut metadata = std::fs::File::create(dir.join("metadata.txt"))
        .map_err(|e| format!("create metadata for {}: {e}", dir.display()))?;
    writeln!(metadata, "target={target}").map_err(|e| e.to_string())?;
    writeln!(metadata, "checkpoint={checkpoint}").map_err(|e| e.to_string())?;
    writeln!(metadata, "repeat_index={repeat_index}").map_err(|e| e.to_string())?;
    writeln!(metadata, "repeat_total={repeat}").map_err(|e| e.to_string())?;
    writeln!(metadata, "display_hash={}", actual.display.hash).map_err(|e| e.to_string())?;
    writeln!(metadata, "display_width={}", actual.display.width).map_err(|e| e.to_string())?;
    writeln!(metadata, "display_height={}", actual.display.height).map_err(|e| e.to_string())?;
    writeln!(metadata, "display_byte_len={}", actual.display.byte_len)
        .map_err(|e| e.to_string())?;
    writeln!(metadata, "vram_hash={}", actual.vram).map_err(|e| e.to_string())?;
    writeln!(metadata, "tick={}", result.tick).map_err(|e| e.to_string())?;
    writeln!(metadata, "cycles={}", result.cycles).map_err(|e| e.to_string())?;
    writeln!(metadata, "pc=0x{:08x}", result.pc).map_err(|e| e.to_string())?;
    if let Some(stopped_at) = result.stopped_at {
        writeln!(metadata, "stopped_at={stopped_at}").map_err(|e| e.to_string())?;
    }
    writeln!(metadata, "guest_frames={}", result.guest_frames).map_err(|e| e.to_string())?;
    writeln!(metadata, "visual_frames={}", result.visual_frames).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn sanitize_artifact_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

#[derive(Debug, Clone)]
struct ResolvedValidationArtifact {
    path: PathBuf,
    embedded_playtest: bool,
    bios_boot: bool,
}

fn resolve_validation_artifact(
    repo_root: &Path,
    manifest_dir: &Path,
    artifact: &ValidationArtifact,
) -> Result<ResolvedValidationArtifact, String> {
    let (path, embedded_playtest, bios_boot) = match artifact {
        ValidationArtifact::Project { project } => {
            // Project artifacts are cooked + built through the editor's
            // disc-build pipeline, which is absent in emulator-only builds.
            #[cfg(feature = "editor")]
            {
                let project = resolve_manifest_path(repo_root, manifest_dir, project);
                (build_project_disc_path(&project)?, true, false)
            }
            #[cfg(not(feature = "editor"))]
            {
                let _ = project;
                return Err("validation Project artifacts require the `editor` feature".to_string());
            }
        }
        ValidationArtifact::Disc {
            path,
            embedded_playtest,
            bios_boot,
        } => (
            resolve_manifest_path(repo_root, manifest_dir, path),
            *embedded_playtest,
            *bios_boot,
        ),
        ValidationArtifact::Example { path } | ValidationArtifact::Commercial { path } => (
            resolve_manifest_path(repo_root, manifest_dir, path),
            false,
            false,
        ),
    };
    Ok(ResolvedValidationArtifact {
        path,
        embedded_playtest,
        bios_boot,
    })
}

fn validation_launch_args(
    repo_root: &Path,
    manifest_dir: &Path,
    artifact: &ResolvedValidationArtifact,
    checkpoint: &psoxide_validation::ValidationCheckpoint,
) -> LaunchArgs {
    let input_tape = checkpoint
        .input_tape
        .as_ref()
        .map(|path| resolve_manifest_path(repo_root, manifest_dir, path));
    LaunchArgs {
        path: Some(artifact.path.clone()),
        game_id: None,
        steps: checkpoint.stop.steps,
        guest_frames: checkpoint.stop.guest_frames,
        guest_visual_frames: checkpoint.stop.guest_visual_frames,
        input_tape,
        pad_pulses: checkpoint.pad_pulses.clone(),
        embedded_playtest: artifact.embedded_playtest,
        bios_boot: artifact.bios_boot,
        dump_hash: false,
        guest_debug_log: false,
        visual_hash_log: None,
        visual_hash_interval: 1,
        guest_hash_log: None,
        guest_hash_interval: 60,
        counter_log: None,
        profile_log: None,
        dump_vram: None,
        dump_ram: None,
        dump_hw: None,
        dump_display: None,
        dump_audio: None,
        dump_guest_profile: false,
        hold_forward: checkpoint.hold_forward,
        hold_run: checkpoint.hold_run,
        wireframe: false,
        texture_filter: "none".to_string(),
    }
}

fn matches_filter(filter: &Option<String>, value: &str) -> bool {
    filter.as_ref().map_or(true, |filter| filter == value)
}

fn resolve_cli_path(repo_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() || path.exists() {
        path.to_path_buf()
    } else {
        repo_root.join(path)
    }
}

fn resolve_manifest_path(repo_root: &Path, manifest_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let local = manifest_dir.join(path);
    if local.exists() {
        local
    } else {
        repo_root.join(path)
    }
}

#[cfg(feature = "editor")]
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct PadPulse {
    mask: u16,
    start_tick: u64,
    frames: u64,
}

fn parse_pad_pulses(text: &str) -> Result<Vec<PadPulse>, String> {
    let mut pulses = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        pulses.push(parse_pad_pulse(entry)?);
    }
    Ok(pulses)
}

fn parse_pad_pulse(text: &str) -> Result<PadPulse, String> {
    let (mask_text, rest) = text
        .split_once('@')
        .ok_or_else(|| format!("invalid pad pulse `{text}`: expected <mask>@<tick>+<frames>"))?;
    let mask =
        parse_u16_mask(mask_text).ok_or_else(|| format!("invalid pad pulse mask `{mask_text}`"))?;
    let (start_text, frames_text) = match rest.split_once('+') {
        Some((start, frames)) => (start.trim(), frames.trim()),
        None => (rest.trim(), "1"),
    };
    let start_tick = start_text
        .parse::<u64>()
        .map_err(|_| format!("invalid pad pulse tick `{start_text}`"))?;
    let frames = frames_text
        .parse::<u64>()
        .map_err(|_| format!("invalid pad pulse frame count `{frames_text}`"))?;
    if frames == 0 {
        return Err(format!(
            "invalid pad pulse `{text}`: frame count must be > 0"
        ));
    }
    Ok(PadPulse {
        mask,
        start_tick,
        frames,
    })
}

fn parse_u16_mask(text: &str) -> Option<u16> {
    let s = text.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u16>().ok()
    }
}

fn active_pad_pulse_mask(pulses: &[PadPulse], route_tick: u64) -> u16 {
    let mut mask = 0;
    for pulse in pulses {
        let end_tick = pulse.start_tick.saturating_add(pulse.frames);
        if route_tick >= pulse.start_tick && route_tick < end_tick {
            mask |= pulse.mask;
        }
    }
    mask
}

fn sync_pad_pulses(
    bus: &mut Bus,
    held_button_mask: u16,
    pulses: &[PadPulse],
    route_tick: u64,
    current_mask: &mut Option<u16>,
) {
    let next_mask = held_button_mask | active_pad_pulse_mask(pulses, route_tick);
    if current_mask.is_some_and(|mask| mask == next_mask) {
        return;
    }
    bus.set_port1_buttons(ButtonState::from_bits(next_mask));
    *current_mask = Some(next_mask);
}

fn hash_vram(bus: &Bus) -> u64 {
    let mut h = psx_hw::hash::Fnv1a64::new();
    for &word in bus.gpu.vram.words() {
        h.update(&word.to_le_bytes());
    }
    h.finish()
}

#[cfg(feature = "editor")]
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
        // Active room defaults to the first room; pass the requested
        // active floor so stacked-floor rooms can be inspected per floor.
        project
            .active_scene()
            .nodes()
            .iter()
            .find(|n| matches!(n.kind, psxed_project::NodeKind::Room { .. }))
            .map(|n| n.id),
        args.active_floor,
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

#[cfg(feature = "editor")]
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

fn counter_total(summary: &telemetry::GuestTelemetrySummary, id: u16) -> u64 {
    summary
        .counters
        .get(id as usize)
        .copied()
        .unwrap_or_default()
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

fn parse_texture_filter(s: &str) -> u32 {
    match s.to_ascii_lowercase().as_str() {
        "xbr" => 3,
        _ => 0,
    }
}

fn dump_hw_ppm(
    bus: &Bus,
    path: &std::path::Path,
    texture_filter: u32,
) -> Result<Option<&'static str>, String> {
    let display = bus.gpu.display_area();
    let has_screen_offset =
        bus.gpu.horizontal_display_offset_px() != 0 || bus.gpu.vertical_display_offset_px() != 0;
    if display.bpp24 || has_screen_offset {
        let (rgba, w, h) = bus.gpu.display_rgba8();
        write_rgb_ppm_from_rgba(path, w, h, &rgba)?;
        return Ok(Some(if display.bpp24 {
            "24bpp display fallback"
        } else {
            "screen-offset display fallback"
        }));
    }

    let (device, queue) = headless_wgpu_device()?;

    let mut hw = psx_gpu_render::HwRenderer::new_headless(device, queue);
    hw.set_texture_filter(texture_filter);
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
    Ok(None)
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
    write_vram_words_ppm(path, bus.gpu.vram.words())
}

fn write_vram_words_ppm(path: &std::path::Path, words: &[u16]) -> Result<(), String> {
    let w = emulator_core::VRAM_WIDTH;
    let h = emulator_core::VRAM_HEIGHT;
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    writeln!(f, "P6\n{w} {h}\n255").map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for &pix in words {
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
