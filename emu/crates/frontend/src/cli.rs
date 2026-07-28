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

use std::collections::BTreeMap;
#[cfg(feature = "editor")]
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use emulator_core::{
    button, fast_boot_disc_with_hle, telemetry, warm_bios_for_disc_fast_boot, Bus, ButtonState,
    Cpu, EmulatorState, DISC_FAST_BOOT_WARMUP_STEPS,
};
// `Gpu` is only constructed for the editor 3D preview dump.
#[cfg(feature = "editor")]
use emulator_core::Gpu;
use psoxide_settings::{
    library::{GameKind, LibraryEntry},
    savestate::SaveStateV1,
    ConfigPaths, Library, Settings,
};
use psoxide_validation::{
    format_hash, ActualHashes, PixelHash, ValidationArtifact, ValidationRunner, ValidationSuite,
};
use psx_iso::{Disc, Exe, TrackType};
#[cfg(feature = "editor")]
use psxed_project::{NodeId, ProjectDocument};
#[cfg(feature = "editor")]
use psxed_ui::{
    EditorPlaytestStatus, EditorViewport3dPresentation, EditorWorkspace, ViewportCameraMode,
    ViewportCameraState,
};

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

    /// Boot the GUI directly into the native editor workspace, bypassing the
    /// emulator menu. Intended for deterministic editor development and
    /// screenshot validation.
    #[cfg(feature = "editor")]
    #[arg(long)]
    pub editor: bool,

    /// Project directory to open when `--editor` is set. The directory must
    /// contain a valid `project.ron`.
    #[cfg(feature = "editor")]
    #[arg(long, value_name = "DIR", requires = "editor")]
    pub editor_project: Option<PathBuf>,

    /// Editor workspace to show immediately when `--editor` is set.
    #[cfg(feature = "editor")]
    #[arg(long, value_enum, value_name = "VIEW", requires = "editor")]
    pub editor_view: Option<EditorViewArg>,

    /// Resource name (or numeric resource id) to focus when booting the
    /// Animation workspace.
    #[cfg(feature = "editor")]
    #[arg(long, value_name = "NAME_OR_ID", requires = "editor")]
    pub editor_resource: Option<String>,

    /// Headless subcommand. Omit to launch the GUI.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Scriptable native-editor destinations for deterministic GUI startup.
#[cfg(feature = "editor")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EditorViewArg {
    #[value(name = "3d")]
    ThreeD,
    #[value(name = "2d")]
    TwoD,
    Animation,
    Material,
}

#[cfg(feature = "editor")]
impl EditorViewArg {
    pub const fn project_view(self) -> psxed_project::EditorWorkspaceView {
        match self {
            Self::ThreeD => psxed_project::EditorWorkspaceView::Room,
            Self::TwoD => psxed_project::EditorWorkspaceView::Ui,
            Self::Animation => psxed_project::EditorWorkspaceView::Animation,
            Self::Material => psxed_project::EditorWorkspaceView::Material,
        }
    }
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
    /// Render the complete editor UI to PNG without opening a native window.
    #[cfg(feature = "editor")]
    DumpEditorUi(DumpEditorUiArgs),
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
    /// Restore this save state after mounting the selected game. This is useful
    /// for extracting a precise gameplay checkpoint through the headless
    /// profiler without navigating there again in the GUI.
    #[arg(long, value_name = "PATH")]
    pub savestate: Option<PathBuf>,
    /// Override the configured BIOS for this headless launch.
    #[arg(long)]
    pub bios: Option<PathBuf>,
    /// Mount a disc alongside a side-loaded executable without booting from
    /// that disc. This is useful for hardware probes and homebrew that use
    /// the HLE BIOS entry path but still exercise the CD-ROM controller.
    #[arg(long)]
    pub disc: Option<PathBuf>,
    /// Load port 1's memory card from this 128 KiB `.mcd` file and persist any
    /// writes back at the end of the headless run. A missing file starts as a
    /// freshly formatted card. This makes cold-boot save tests reproducible.
    #[arg(long, value_name = "PATH")]
    pub memcard: Option<PathBuf>,
    /// Load port 2's memory card from this 128 KiB `.mcd` file and persist any
    /// writes back at the end of the headless run. This complements
    /// `--memcard` for dual-card, slot-2 fallback, and card-copy tests.
    #[arg(long, value_name = "PATH")]
    pub memcard2: Option<PathBuf>,
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
    /// Wait this many emulator route ticks before applying frame zero from
    /// `--input-tape`. Recordings started after the console was already
    /// running need this offset when replayed from a cold boot.
    #[arg(long, default_value_t = 0)]
    pub input_tape_delay_ticks: u64,
    /// Transcribe the replayed `--input-tape` onto the guest's own input clock
    /// and write it here as a poll-bound `PXITAPE2`. Each completed port-1 poll
    /// records the sample the guest actually read, so the transcript reproduces
    /// this exact playthrough while replaying identically on builds of
    /// differing speed. Use it to convert a legacy video-frame tape into a
    /// benchmark that cannot diverge.
    #[arg(long)]
    pub input_tape_transcribe: Option<PathBuf>,
    /// Press pad-1 button masks on the headless route clock. Format:
    /// `<mask>@<tick>+<frames>`, comma-separated, e.g.
    /// `0x4000@45+12,0x4000@80+16`.
    #[arg(long)]
    pub pad_pulses: Option<String>,
    /// Keep port 1 in digital-pad mode instead of forcing the default
    /// DualShock-compatible analog mode. This reproduces captures made with
    /// an original digital controller, whose poll ID is 0x41.
    #[arg(long)]
    pub digital_pad: bool,
    /// Treat an authored disc as an embedded editor Play disc and boot it
    /// through the same no-BIOS HLE path used by the editor viewport.
    #[arg(long)]
    pub embedded_playtest: bool,
    /// Force the real BIOS disc boot path instead of direct
    /// SYSTEM.CNF fast boot.
    #[arg(long)]
    pub bios_boot: bool,
    /// Override how many real-BIOS instructions run before warm disc fast
    /// boot. Longer warmups retain later BIOS-initialised peripheral state.
    #[arg(long)]
    pub bios_warmup_steps: Option<u64>,
    /// Apply the late PAL PSone SCPH-9902 memory-controller profile after
    /// BIOS warmup. Useful with an earlier PAL BIOS used as a substitute ROM.
    #[arg(long)]
    pub scph_9902: bool,
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
    /// Write one emulator-owned CSV row per headless route tick. Unlike guest
    /// telemetry, this works with shipping discs and records CPU/cycle cost plus
    /// display-buffer flips, which makes rendered-frame cadence measurable from
    /// an input-tape replay.
    #[arg(long)]
    pub route_log: Option<PathBuf>,
    /// Write an emulator-owned GP0 command census per route tick. Command
    /// capture is drained after every tick, so long input-tape replays can
    /// measure draw composition without retaining the whole command stream.
    #[arg(long)]
    pub gpu_frame_stats_log: Option<PathBuf>,
    /// Capture the software display at fixed route-tick checkpoints. These
    /// screenshots are emulator-owned and do not alter guest RAM or timing.
    #[arg(long)]
    pub route_screenshot_dir: Option<PathBuf>,
    /// Route ticks between screenshots written by `--route-screenshot-dir`.
    #[arg(long, default_value_t = 3_000)]
    pub route_screenshot_interval: u64,
    /// Sample the guest program counter entirely from the emulator and write
    /// aggregate address counts. This profiles an untouched shipping binary:
    /// no guest telemetry code, RAM, or timing hooks are required.
    #[arg(long)]
    pub pc_sample_log: Option<PathBuf>,
    /// Sample the guest program counter together with register $ra and the
    /// words at `$sp + 20` and `$sp + 36`, then write aggregate callsite
    /// counts. The latter recovers the caller through the standard 16-byte
    /// compiler-builtins inner frame plus 20-byte wrapper save slot, attributing
    /// memcpy/memset interiors without guest instrumentation.
    #[arg(long)]
    pub pc_sample_callsite_log: Option<PathBuf>,
    /// Write out-of-band PC samples grouped into route-tick windows. This
    /// identifies which guest code dominates individual slow gameplay spans
    /// instead of averaging menu, loading and gameplay into one profile.
    #[arg(long)]
    pub pc_sample_window_log: Option<PathBuf>,
    /// Route ticks per bucket in `--pc-sample-window-log`.
    #[arg(long, default_value_t = 300)]
    pub pc_sample_window_ticks: u64,
    /// Retired guest instructions between program-counter samples.
    #[arg(long, default_value_t = 16_384)]
    pub pc_sample_instructions: u64,
    /// Optional path to dump the final VRAM as a raw PPM image.
    /// Lets you eyeball the boot state without firing up the GUI.
    #[arg(long)]
    pub dump_vram: Option<PathBuf>,
    /// Optional path to dump the final 2 MiB main RAM as a raw binary
    /// blob, for offline diffing of guest state across two runs.
    #[arg(long)]
    pub dump_ram: Option<PathBuf>,
    /// Optional path to dump the final 512 KiB SPU RAM as little-endian
    /// halfwords, for transfer and BIOS-initialisation diagnostics.
    #[arg(long)]
    pub dump_spu_ram: Option<PathBuf>,
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
    /// Optional path to dump every guest CD-ROM command as CSV. This is
    /// recorded entirely by the emulator, so shipping guests can be traced
    /// without linking emulator-only telemetry into PS1 RAM.
    #[arg(long)]
    pub cd_command_log: Option<PathBuf>,
    /// Print a guest-runtime telemetry summary captured out-of-band.
    #[arg(long)]
    pub dump_guest_profile: bool,
    /// Hold the left analog stick fully forward during the headless run.
    #[arg(long)]
    pub hold_forward: bool,
    /// Hold the game run button during the headless run.
    #[arg(long)]
    pub hold_run: bool,
    /// Press buttons at scheduled route ticks, so a headless run can clear a
    /// menu and reach gameplay.
    ///
    /// `--hold-forward` only holds the stick, and every project now boots to a
    /// menu, which left no headless route into gameplay at all and so no way to
    /// measure a render change. Recorded tapes do not fill the gap: they are
    /// keyed to the pad-poll clock of the run that recorded them and desync.
    ///
    /// Spec is `tick:button[:hold]`, comma separated, ticks counted from the
    /// start of the route. `hold` defaults to 4 ticks, which is long enough for
    /// the guest to poll the pad at least once. Buttons: cross, circle, square,
    /// triangle, start, select, up, down, left, right, l1, r1, l2, r2.
    ///
    /// Combines with `--hold-forward`: held input stays applied, and a scheduled
    /// press is added on top for its duration.
    #[arg(long)]
    pub press: Option<String>,
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

/// Arguments for `dump-editor-ui`.
#[cfg(feature = "editor")]
#[derive(Debug, Args)]
pub struct DumpEditorUiArgs {
    /// Project directory containing `project.ron`.
    #[arg(long, default_value = "editor/projects/default")]
    pub project: PathBuf,
    /// Top-level editor workspace to render.
    #[arg(long, value_enum, default_value = "3d")]
    pub view: EditorViewArg,
    /// Resource name or numeric id to focus in Animation Studio or Material Lab.
    #[arg(long, value_name = "NAME_OR_ID")]
    pub resource: Option<String>,
    /// Output PNG path.
    #[arg(long)]
    pub out: PathBuf,
    /// Offscreen framebuffer width in physical pixels.
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    /// Offscreen framebuffer height in physical pixels.
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    /// Inject the `.` frame-selected shortcut before the captured frame.
    #[arg(long)]
    pub frame_selected: bool,
    /// Render the embedded Play viewport with the named Room Topology debug
    /// view. Accepted values: rooms, cells, portals, streaming.
    #[arg(long, value_name = "VIEW")]
    pub debug_map_view: Option<String>,
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
        #[cfg(feature = "editor")]
        Command::DumpEditorUi(args) => cmd_dump_editor_ui(args),
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
    let mut settings = Settings::load(&paths.settings_file()).unwrap_or_default();
    if let Some(bios) = args.bios.as_ref() {
        settings.paths.bios = bios.to_string_lossy().into_owned();
    }
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
    if args.input_tape.is_none() && args.input_tape_delay_ticks != 0 {
        return Err("--input-tape-delay-ticks requires --input-tape".to_string());
    }
    if args.route_screenshot_dir.is_some() && args.route_screenshot_interval == 0 {
        return Err("--route-screenshot-interval must be greater than zero".to_string());
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
    let mut tape_poll_bound = false;
    let mut tape_start_poll = 0u64;
    let tape_samples = match args.input_tape.as_deref() {
        Some(path) => {
            let tape = read_input_tape(path)?;
            if tape.samples.is_empty() {
                return Err(format!("input tape has no frames: {}", path.display()));
            }
            tape_poll_bound = tape.clock == emulator_core::input_tape::TapeClock::PadPoll;
            tape_start_poll = tape.start_poll;
            if emit_summary {
                if tape_poll_bound {
                    eprintln!(
                        "[cli] loaded poll-bound input tape {} ({} polls, starting at poll {})",
                        path.display(),
                        tape.samples.len(),
                        tape_start_poll
                    );
                } else {
                    eprintln!(
                        "[cli] loaded input tape {} ({} frames)",
                        path.display(),
                        tape.samples.len()
                    );
                }
            }
            Some(tape.samples)
        }
        None => None,
    };
    let guest_frame_limit = args.guest_frames.or_else(|| {
        tape_samples
            .as_ref()
            .map(|samples| (samples.len() as u64).saturating_add(args.input_tape_delay_ticks))
    });
    let capture_gpu_commands = args.dump_hw.is_some() || args.gpu_frame_stats_log.is_some();

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
            if capture_gpu_commands {
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
            attach_headless_playtest_pad(&mut bus, args.digital_pad);
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
            if capture_gpu_commands {
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
                    args.bios_warmup_steps
                        .unwrap_or(DISC_FAST_BOOT_WARMUP_STEPS),
                );
            }
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus, args.digital_pad);
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
            if capture_gpu_commands {
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
                    args.bios_warmup_steps
                        .unwrap_or(DISC_FAST_BOOT_WARMUP_STEPS),
                );
            }
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus, args.digital_pad);
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
            if capture_gpu_commands {
                bus.gpu.enable_cmd_log();
            }
            let disc = psoxide_settings::library::load_disc_from_ccd(&game_path)?;
            maybe_fast_boot_disc(
                &mut bus,
                &mut cpu,
                &disc,
                &game_path,
                settings.emulator.fast_boot_disc && !args.bios_boot,
                args.bios_warmup_steps
                    .unwrap_or(DISC_FAST_BOOT_WARMUP_STEPS),
            );
            bus.cdrom.insert_disc(Some(disc));
            attach_headless_playtest_pad(&mut bus, args.digital_pad);
            if emit_summary {
                eprintln!("[cli] mounted ccd-backed disc {}", game_path.display());
            }
            bus
        }
        other => {
            return Err(format!("unsupported file extension: .{other}"));
        }
    };

    if args.scph_9902 {
        bus.apply_scph_9902_profile();
    }

    if let Some(path) = args.memcard.as_ref() {
        let bytes = if path.exists() {
            std::fs::read(path)
                .map_err(|error| format!("read memory card {}: {error}", path.display()))?
        } else {
            Vec::new()
        };
        bus.attach_memcard_port1(bytes);
        if emit_summary {
            eprintln!("[cli] port-1 memory card → {}", path.display());
        }
    }

    if let Some(path) = args.memcard2.as_ref() {
        let bytes = if path.exists() {
            std::fs::read(path)
                .map_err(|error| format!("read memory card {}: {error}", path.display()))?
        } else {
            Vec::new()
        };
        bus.attach_memcard_port2(bytes);
        if emit_summary {
            eprintln!("[cli] port-2 memory card → {}", path.display());
        }
    }

    if let Some(disc_path) = args.disc.as_ref() {
        if ext != "exe" {
            return Err("--disc can only be combined with an .exe launch path".to_string());
        }
        let disc = load_headless_disc(disc_path)?;
        bus.cdrom.insert_disc(Some(disc));
        if emit_summary {
            eprintln!("[cli] mounted auxiliary disc {}", disc_path.display());
        }
    }

    if let Some(path) = args.savestate.as_ref() {
        let loaded = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || SaveStateV1::<EmulatorState>::read_from(path))
                .map_err(|e| format!("spawn save-state loader: {e}"))?
                .join()
                .map_err(|_| "save-state loader panicked".to_string())?
                .map_err(|e| e.to_string())
        })?;
        let header = loaded.header;
        let mut payload = loaded.payload;
        payload.bus.restore_excluded_from(&mut bus);
        cpu = payload.cpu;
        bus = payload.bus;
        // A GUI save can capture the pad while a movement key is held. The
        // headless runner has no window event loop to release that key, so use
        // the same fresh neutral controller a normal headless launch starts
        // with before applying any explicit tape/pulse input below.
        attach_headless_playtest_pad(&mut bus, args.digital_pad);
        bus.set_port1_buttons(ButtonState::default());
        bus.set_port1_sticks(0x80, 0x80, 0x80, 0x80);
        if capture_gpu_commands {
            bus.gpu.enable_cmd_log();
        }
        if emit_summary {
            eprintln!(
                "[cli] restored save state {} (game={} tick={})",
                path.display(),
                header.game_id,
                header.cpu_tick
            );
        }
    }

    if args.cd_command_log.is_some() {
        bus.cdrom.enable_command_log(65_536);
    }
    if std::env::var_os("PSOXIDE_DUMP_DMA_LL").is_some() {
        bus.set_gpu_linked_list_log_enabled(true);
    }

    if args.wireframe {
        bus.gpu.wireframe_enabled = true;
    }

    let scripted_presses = match args.press.as_deref() {
        Some(spec) => parse_press_script(spec)?,
        None => Vec::new(),
    };
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
    let mut tape_started = false;
    // Transcription state: the pad sample that was live during the route tick
    // just finished, and one entry per poll the guest completed while it was.
    let mut transcript: Vec<emulator_core::input_tape::PadSample> = Vec::new();
    let mut transcript_live = emulator_core::input_tape::PadSample::from_buttons(0);
    let mut transcript_polls = 0u64;
    // Set once a poll-bound tape has delivered its final sample; the run ends
    // there rather than at a wall-clock tick count.
    let mut tape_exhausted = false;
    if let Some(path) = args.route_screenshot_dir.as_ref() {
        std::fs::create_dir_all(path).map_err(|e| format!("mkdir {}: {e}", path.display()))?;
    }
    let mut route_log = match args.route_log.as_ref() {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let file = std::fs::File::create(path)
                .map_err(|e| format!("create route log {}: {e}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            writeln!(
                writer,
                "route_tick,tape_frame,cpu_tick,bus_cycles,cpu_tick_delta,bus_cycle_delta,display_x,display_y,display_width,display_height,display_start_changed,port1_polls,icache_refill_events_delta,icache_refill_words_delta,icache_refill_stall_cycles_delta"
            )
            .map_err(|e| format!("write route log {}: {e}", path.display()))?;
            let area = bus.gpu.display_area();
            writeln!(
                writer,
                "0,0,{},{},0,0,{},{},{},{},0,{},0,0,0",
                cpu.tick(),
                bus.cycles(),
                area.x,
                area.y,
                area.width,
                area.height,
                bus.port1_completed_polls(),
            )
            .map_err(|e| format!("write route log {}: {e}", path.display()))?;
            Some(writer)
        }
        None => None,
    };
    let mut gpu_frame_stats_log = match args.gpu_frame_stats_log.as_ref() {
        Some(path) => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let file = std::fs::File::create(path)
                .map_err(|e| format!("create GPU stats log {}: {e}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            writeln!(
                writer,
                "route_tick,tape_frame,display_start_changed,frame_draw_words,frame_draw_hash,run_draw_words,run_draw_hash,commands,draws,fills,textured_tris,textured_quads,textured_rects,sky_quads,texture_windows,texture_window_zero,texture_window_changes,texture_window_redundant,gpu_cycles,dma_gpu_cycles,fill_cycles,textured_tri_cycles,textured_quad_cycles,textured_rect_cycles,other_cycles"
            )
            .map_err(|e| format!("write GPU stats log {}: {e}", path.display()))?;
            Some(writer)
        }
        None => None,
    };
    let initial_display_area = bus.gpu.display_area();
    let mut route_last_display_start = (initial_display_area.x, initial_display_area.y);
    let mut gpu_stats_last_display_start = route_last_display_start;
    let mut gpu_prev_timing = bus.gpu.gp0_timing_histogram();
    let mut gpu_prev_dma_timing = bus.gpu.gp0_dma_timing_histogram();
    let mut gpu_prev_texture_window = None;
    let mut gpu_frame_draw_words = 0u64;
    let mut gpu_frame_draw_hash = 0xcbf2_9ce4_8422_2325u64;
    let mut gpu_run_draw_words = 0u64;
    let mut gpu_run_draw_hash = 0xcbf2_9ce4_8422_2325u64;
    let mut route_last_cpu_tick = cpu.tick();
    let mut route_last_bus_cycles = bus.cycles();
    let mut route_last_icache_profile = cpu.instruction_cache_profile();
    if args.input_tape_delay_ticks == 0 && !tape_poll_bound {
        if let Some(samples) = tape_samples.as_ref() {
            samples[tape_cursor].apply_to_bus(&mut bus);
            tape_started = true;
        }
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
    let mut pc_samples = args.pc_sample_log.as_ref().map(|_| BTreeMap::new());
    let mut pc_callsite_samples = args
        .pc_sample_callsite_log
        .as_ref()
        .map(|_| BTreeMap::new());
    let mut pc_window_samples = args.pc_sample_window_log.as_ref().map(|_| BTreeMap::new());
    let pc_sample_interval = args.pc_sample_instructions.max(1);
    let pc_sample_window_ticks = args.pc_sample_window_ticks.max(1);
    let mut next_pc_sample = 0u64;

    // Step the CPU. Report early on opcode errors -- they're usually
    // "we hit an unimplemented instruction" and worth surfacing.
    let mut stopped_at: Option<u64> = None;
    let mut audio_capture: Vec<(i16, i16)> = Vec::new();
    let gte_profile_before = cpu.cop2().profile_snapshot();
    for i in 0..args.steps {
        if i >= next_pc_sample {
            if let Some(samples) = pc_samples.as_mut() {
                *samples.entry(cpu.pc()).or_insert(0u64) += 1;
            }
            if let Some(samples) = pc_callsite_samples.as_mut() {
                let stack_offset = (cpu.gpr(29) as usize & 0x1f_ffff).saturating_add(20);
                let stack_return_address = bus
                    .ram()
                    .get(stack_offset..stack_offset.saturating_add(4))
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
                    .unwrap_or_default();
                let inner_stack_offset = (cpu.gpr(29) as usize & 0x1f_ffff).saturating_add(36);
                let inner_stack_return_address = bus
                    .ram()
                    .get(inner_stack_offset..inner_stack_offset.saturating_add(4))
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
                    .unwrap_or_default();
                *samples
                    .entry((
                        cpu.pc(),
                        cpu.gpr(31),
                        stack_return_address,
                        inner_stack_return_address,
                    ))
                    .or_insert(0u64) += 1;
            }
            if let Some(samples) = pc_window_samples.as_mut() {
                let window_start = route_ticks / pc_sample_window_ticks * pc_sample_window_ticks;
                *samples.entry((window_start, cpu.pc())).or_insert(0u64) += 1;
            }
            next_pc_sample = next_pc_sample.saturating_add(pc_sample_interval);
        }
        // A poll-bound tape must land sample N on poll N. When the guest
        // catches up on missed simulation ticks it polls two or three times
        // inside a single route tick, so re-checking only on the route clock
        // would feed the burst a stale sample and desynchronise the route.
        // Consecutive polls are a whole simulation tick apart, so this coarse
        // stride is exact and stays off the per-instruction path.
        if tape_poll_bound && i & 0x3F == 0 {
            if let Some(samples) = tape_samples.as_ref() {
                let polls = bus.port1_completed_polls();
                if polls >= tape_start_poll {
                    let index = polls.saturating_sub(tape_start_poll);
                    if index >= samples.len() as u64 {
                        tape_exhausted = true;
                    }
                    let cursor = index.min(samples.len().saturating_sub(1) as u64) as usize;
                    if !tape_started || cursor != tape_cursor {
                        tape_started = true;
                        tape_cursor = cursor;
                        samples[tape_cursor].apply_to_bus(&mut bus);
                    }
                }
            }
        }
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
        if bus.run_spu_to_current_cycle() != 0 {
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
            if !scripted_presses.is_empty() {
                // Re-derive the whole mask each tick rather than tracking
                // press/release edges: overlapping presses then just OR
                // together, and a missed release cannot leave a button stuck
                // down for the rest of the run.
                let mut mask = held_button_mask;
                for press in &scripted_presses {
                    if route_ticks >= press.tick && route_ticks < press.tick + press.hold {
                        mask |= press.mask;
                    }
                }
                bus.set_port1_buttons(ButtonState::from_bits(mask));
            }
            if args.input_tape_transcribe.is_some() {
                // Attribute polls to the sample that was live while they ran,
                // before this tick installs a new one. The bus sample is
                // constant across a route tick and a poll interval spans
                // several ticks, so this is exact.
                let polls = bus.port1_completed_polls();
                for _ in 0..polls.saturating_sub(transcript_polls) {
                    transcript.push(transcript_live);
                }
                transcript_polls = polls;
            }
            if let Some(samples) = tape_samples.as_ref() {
                if tape_poll_bound {
                    // Index by the guest's own input clock. A route tick is far
                    // shorter than a pad-poll interval, so the sample for poll N
                    // is always in place before the guest reads it, whatever the
                    // frame rate. That is what makes the route identical between
                    // builds of different speed.
                    let polls = bus.port1_completed_polls();
                    if polls >= tape_start_poll {
                        let index = polls.saturating_sub(tape_start_poll);
                        if index >= samples.len() as u64 {
                            tape_exhausted = true;
                        }
                        let cursor = index.min(samples.len().saturating_sub(1) as u64) as usize;
                        if !tape_started || cursor != tape_cursor {
                            tape_started = true;
                            tape_cursor = cursor;
                            samples[tape_cursor].apply_to_bus(&mut bus);
                        }
                        transcript_live = samples[tape_cursor];
                    }
                } else if route_ticks >= args.input_tape_delay_ticks {
                    let cursor = route_ticks
                        .saturating_sub(args.input_tape_delay_ticks)
                        .min(samples.len().saturating_sub(1) as u64)
                        as usize;
                    if !tape_started || cursor != tape_cursor {
                        tape_started = true;
                        tape_cursor = cursor;
                        samples[tape_cursor].apply_to_bus(&mut bus);
                    }
                    transcript_live = samples[tape_cursor];
                }
            }
            if let Some(dir) = args.route_screenshot_dir.as_ref() {
                if route_ticks % args.route_screenshot_interval == 0 {
                    let path = dir.join(format!("tick-{route_ticks:06}.ppm"));
                    let (rgba, width, height) = bus.gpu.display_rgba8();
                    write_rgb_ppm_from_rgba(&path, width, height, &rgba)?;
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
            if let Some(writer) = route_log.as_mut() {
                let area = bus.gpu.display_area();
                let display_start = (area.x, area.y);
                let display_start_changed = u8::from(display_start != route_last_display_start);
                let cpu_tick = cpu.tick();
                let bus_cycles = bus.cycles();
                let icache_profile = cpu.instruction_cache_profile();
                writeln!(
                    writer,
                    "{route_ticks},{tape_cursor},{cpu_tick},{bus_cycles},{},{},{},{},{},{},{display_start_changed},{},{},{},{}",
                    cpu_tick.saturating_sub(route_last_cpu_tick),
                    bus_cycles.saturating_sub(route_last_bus_cycles),
                    area.x,
                    area.y,
                    area.width,
                    area.height,
                    bus.port1_completed_polls(),
                    icache_profile
                        .refill_events
                        .saturating_sub(route_last_icache_profile.refill_events),
                    icache_profile
                        .refill_words
                        .saturating_sub(route_last_icache_profile.refill_words),
                    icache_profile
                        .refill_stall_cycles
                        .saturating_sub(route_last_icache_profile.refill_stall_cycles),
                )
                .map_err(|e| e.to_string())?;
                route_last_display_start = display_start;
                route_last_cpu_tick = cpu_tick;
                route_last_bus_cycles = bus_cycles;
                route_last_icache_profile = icache_profile;
            }
            if let Some(writer) = gpu_frame_stats_log.as_mut() {
                let commands = bus.gpu.drain_completed_cmd_log();
                let timing = bus.gpu.gp0_timing_histogram();
                let dma_timing = bus.gpu.gp0_dma_timing_histogram();
                let mut timing_delta = [0u64; 256];
                let mut dma_timing_delta = [0u64; 256];
                for opcode in 0..256 {
                    timing_delta[opcode] = timing[opcode].saturating_sub(gpu_prev_timing[opcode]);
                    dma_timing_delta[opcode] =
                        dma_timing[opcode].saturating_sub(gpu_prev_dma_timing[opcode]);
                }
                gpu_prev_timing = timing;
                gpu_prev_dma_timing = dma_timing;
                let mut draws = 0u64;
                let mut fills = 0u64;
                let mut textured_tris = 0u64;
                let mut textured_quads = 0u64;
                let mut textured_rects = 0u64;
                let mut sky_quads = 0u64;
                let mut texture_windows = 0u64;
                let mut texture_window_zero = 0u64;
                let mut texture_window_changes = 0u64;
                let mut texture_window_redundant = 0u64;
                for command in &commands {
                    // Hash only commands that can affect the displayed framebuffer.
                    // Texture uploads are intentionally excluded: their transfer
                    // timing can move across a display flip without changing a draw.
                    if matches!(command.opcode, 0x02 | 0x20..=0x7F | 0xE1..=0xE6) {
                        for word in core::iter::once(command.fifo.len() as u32)
                            .chain(command.fifo.iter().copied())
                        {
                            for byte in word.to_le_bytes() {
                                gpu_frame_draw_hash ^= u64::from(byte);
                                gpu_frame_draw_hash =
                                    gpu_frame_draw_hash.wrapping_mul(0x0000_0100_0000_01b3);
                                gpu_run_draw_hash ^= u64::from(byte);
                                gpu_run_draw_hash =
                                    gpu_run_draw_hash.wrapping_mul(0x0000_0100_0000_01b3);
                            }
                            gpu_frame_draw_words = gpu_frame_draw_words.saturating_add(1);
                            gpu_run_draw_words = gpu_run_draw_words.saturating_add(1);
                        }
                    }
                    if matches!(command.opcode, 0x20..=0x7F) {
                        draws += 1;
                    }
                    match command.opcode {
                        0x02 => fills += 1,
                        0x24..=0x27 | 0x34..=0x37 => textured_tris += 1,
                        0x2C..=0x2F | 0x3C..=0x3F => {
                            textured_quads += 1;
                            if command.fifo.len() >= 9
                                && command.fifo[1] == 0
                                && command.fifo[3] == 319
                                && command.fifo[5] == (239u32 << 16)
                                && command.fifo[7] == ((239u32 << 16) | 319)
                            {
                                sky_quads += 1;
                            }
                        }
                        0x64..=0x7F => textured_rects += 1,
                        0xE2 => {
                            texture_windows += 1;
                            let value = command.fifo[0] & 0x000F_FFFF;
                            texture_window_zero += u64::from(value == 0);
                            if gpu_prev_texture_window == Some(value) {
                                texture_window_redundant += 1;
                            } else {
                                texture_window_changes += 1;
                                gpu_prev_texture_window = Some(value);
                            }
                        }
                        _ => {}
                    }
                }
                let area = bus.gpu.display_area();
                let display_start = (area.x, area.y);
                let display_start_changed = u8::from(display_start != gpu_stats_last_display_start);
                let sum_cycles = |ranges: &[(usize, usize)]| -> u64 {
                    ranges
                        .iter()
                        .map(|&(first, last)| timing_delta[first..=last].iter().sum::<u64>())
                        .sum()
                };
                let gpu_cycles: u64 = timing_delta.iter().sum();
                let dma_gpu_cycles: u64 = dma_timing_delta.iter().sum();
                let fill_cycles = timing_delta[0x02];
                let textured_tri_cycles = sum_cycles(&[(0x24, 0x27), (0x34, 0x37)]);
                let textured_quad_cycles = sum_cycles(&[(0x2C, 0x2F), (0x3C, 0x3F)]);
                let textured_rect_cycles = sum_cycles(&[(0x64, 0x7F)]);
                let classified_cycles = fill_cycles
                    .saturating_add(textured_tri_cycles)
                    .saturating_add(textured_quad_cycles)
                    .saturating_add(textured_rect_cycles);
                let other_cycles = gpu_cycles.saturating_sub(classified_cycles);
                writeln!(
                    writer,
                    "{route_ticks},{tape_cursor},{display_start_changed},{gpu_frame_draw_words},0x{gpu_frame_draw_hash:016x},{gpu_run_draw_words},0x{gpu_run_draw_hash:016x},{},{draws},{fills},{textured_tris},{textured_quads},{textured_rects},{sky_quads},{texture_windows},{texture_window_zero},{texture_window_changes},{texture_window_redundant},{gpu_cycles},{dma_gpu_cycles},{fill_cycles},{textured_tri_cycles},{textured_quad_cycles},{textured_rect_cycles},{other_cycles}",
                    commands.len(),
                )
                .map_err(|e| e.to_string())?;
                gpu_stats_last_display_start = display_start;
                if display_start_changed != 0 {
                    gpu_frame_draw_words = 0;
                    gpu_frame_draw_hash = 0xcbf2_9ce4_8422_2325;
                }
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
        if tape_exhausted {
            stopped_at = Some(i + 1);
            break;
        }
        if let Some(target) = guest_frame_limit {
            // When replaying a tape the natural clock is tape ticks, not
            // `frames_seen`; otherwise `--guest-frames N` stops at the wrong
            // moment now that the tape advances on the vblank tick.
            let reached = if tape_poll_bound {
                false
            } else if tape_samples.is_some() {
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
    if let Some(path) = args.input_tape_transcribe.as_ref() {
        for _ in 0..bus.port1_completed_polls().saturating_sub(transcript_polls) {
            transcript.push(transcript_live);
        }
        emulator_core::input_tape::write_tape_poll_bound(path, &transcript, 0)?;
        if emit_summary {
            eprintln!(
                "[cli] transcribed {} polls to {}",
                transcript.len(),
                path.display()
            );
        }
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
        // A headless route that never leaves a menu looks identical whether the
        // guest ignored the input or never read the pad at all, so report the
        // poll count: zero means scripted presses and tapes alike were written
        // to a port nobody is reading.
        println!(
            "route-ticks={route_ticks}  port1-polls={}",
            bus.port1_completed_polls()
        );
        // A guest that services its CD interrupt too late loses sectors and is
        // told nothing about it, so it shows up as a stall or corrupt asset
        // far from the cause. Report it whenever it happens.
        let dropped = bus.cdrom_dropped_sectors();
        if dropped > 0 {
            let (first, last) = bus.cdrom_dropped_lba_range();
            println!(
                "cd-sectors-dropped={dropped}  lba {first}..{last}  (guest read the disc too slowly)"
            );
        }
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

    if let Some(path) = args.cd_command_log.as_ref() {
        write_cd_command_log(path, bus.cdrom.command_log())?;
        if emit_summary {
            eprintln!(
                "[cli] CD command log → {} ({} commands)",
                path.display(),
                bus.cdrom.command_log().len()
            );
        }
    }
    if let Some(path) = std::env::var_os("PSOXIDE_DUMP_DMA_LL") {
        let mut out = String::from("transfer,address,header\n");
        for &(transfer, address, header) in bus.gpu_linked_list_log() {
            out.push_str(&format!("{transfer},{address:#08X},{header:#010X}\n"));
        }
        std::fs::write(&path, out)
            .map_err(|e| format!("write GPU linked-list log {}: {e}", path.to_string_lossy()))?;
        if emit_summary {
            eprintln!(
                "[cli] GPU linked-list log → {} ({} nodes)",
                path.to_string_lossy(),
                bus.gpu_linked_list_log().len()
            );
        }
    }

    if let Some(writer) = route_log.as_mut() {
        writer.flush().map_err(|e| e.to_string())?;
    }
    if let (Some(path), Some(samples)) = (args.pc_sample_log.as_ref(), pc_samples.as_ref()) {
        write_pc_sample_log(path, samples)?;
        if emit_summary {
            let total = samples.values().copied().sum::<u64>();
            eprintln!(
                "[cli] PC samples → {} ({} samples, {} addresses)",
                path.display(),
                total,
                samples.len()
            );
        }
    }
    if let (Some(path), Some(samples)) = (
        args.pc_sample_callsite_log.as_ref(),
        pc_callsite_samples.as_ref(),
    ) {
        write_pc_callsite_sample_log(path, samples)?;
        if emit_summary {
            let total = samples.values().copied().sum::<u64>();
            eprintln!(
                "[cli] PC callsite samples → {} ({} samples, {} pairs)",
                path.display(),
                total,
                samples.len()
            );
        }
    }
    if let (Some(path), Some(samples)) = (
        args.pc_sample_window_log.as_ref(),
        pc_window_samples.as_ref(),
    ) {
        write_pc_window_sample_log(path, samples)?;
        if emit_summary {
            let total = samples.values().copied().sum::<u64>();
            eprintln!(
                "[cli] windowed PC samples → {} ({} samples, {} buckets)",
                path.display(),
                total,
                samples.len()
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

    if let Some(path) = args.dump_spu_ram {
        let mut bytes = Vec::with_capacity(bus.spu.ram_halfwords().len() * 2);
        for &halfword in bus.spu.ram_halfwords() {
            bytes.extend_from_slice(&halfword.to_le_bytes());
        }
        std::fs::write(&path, bytes)
            .map_err(|error| format!("write SPU RAM {}: {error}", path.display()))?;
        if emit_summary {
            eprintln!("[cli] SPU RAM → {}", path.display());
        }
    }

    if let Some(path) = args.memcard.as_ref() {
        if let Some(bytes) = bus.memcard_port1_snapshot() {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("create memory-card directory {}: {error}", parent.display())
                })?;
            }
            std::fs::write(path, &bytes)
                .map_err(|error| format!("write memory card {}: {error}", path.display()))?;
            if emit_summary {
                eprintln!(
                    "[cli] persisted port-1 memory card → {} ({} bytes)",
                    path.display(),
                    bytes.len()
                );
            }
        }
    }

    if let Some(path) = args.memcard2.as_ref() {
        if let Some(bytes) = bus.memcard_port2_snapshot() {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("create memory-card directory {}: {error}", parent.display())
                })?;
            }
            std::fs::write(path, &bytes)
                .map_err(|error| format!("write memory card {}: {error}", path.display()))?;
            if emit_summary {
                eprintln!(
                    "[cli] persisted port-2 memory card → {} ({} bytes)",
                    path.display(),
                    bytes.len()
                );
            }
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

/// Write a stable command-level CD-ROM trace without requiring guest-side
/// telemetry. Parameters are kept in issue order and encoded as space-separated
/// uppercase bytes so the CSV stays directly diffable across emulator runs.
fn write_cd_command_log(
    path: &std::path::Path,
    entries: &[emulator_core::cdrom::CdRomCommandLogEntry],
) -> Result<(), String> {
    let mut out = String::from("cycle,command,param_len,params\n");
    for entry in entries {
        let params = entry.params[..entry.param_len as usize]
            .iter()
            .map(|byte| format!("{byte:02X}"))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "{},0x{:02X},{},{}\n",
            entry.cycle, entry.command, entry.param_len, params
        ));
    }
    std::fs::write(path, out)
        .map_err(|error| format!("write CD command log {}: {error}", path.display()))
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
        savestate: None,
        bios: None,
        disc: None,
        memcard: None,
        memcard2: None,
        steps: checkpoint.stop.steps,
        guest_frames: checkpoint.stop.guest_frames,
        guest_visual_frames: checkpoint.stop.guest_visual_frames,
        input_tape,
        input_tape_delay_ticks: 0,
        input_tape_transcribe: None,
        pad_pulses: checkpoint.pad_pulses.clone(),
        digital_pad: false,
        embedded_playtest: artifact.embedded_playtest,
        bios_boot: artifact.bios_boot,
        bios_warmup_steps: None,
        scph_9902: false,
        dump_hash: false,
        guest_debug_log: false,
        visual_hash_log: None,
        visual_hash_interval: 1,
        guest_hash_log: None,
        guest_hash_interval: 60,
        counter_log: None,
        profile_log: None,
        route_log: None,
        gpu_frame_stats_log: None,
        route_screenshot_dir: None,
        route_screenshot_interval: 3_000,
        pc_sample_log: None,
        pc_sample_callsite_log: None,
        pc_sample_window_log: None,
        pc_sample_window_ticks: 300,
        pc_sample_instructions: 16_384,
        dump_vram: None,
        dump_ram: None,
        dump_spu_ram: None,
        dump_hw: None,
        dump_display: None,
        dump_audio: None,
        cd_command_log: None,
        dump_guest_profile: false,
        hold_forward: checkpoint.hold_forward,
        press: None,
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

fn attach_headless_playtest_pad(bus: &mut Bus, digital_only: bool) {
    if digital_only {
        bus.attach_original_digital_pad_port1();
    } else {
        bus.attach_digital_pad_port1();
        let _ = bus.force_port1_analog_mode();
    }
}

fn load_headless_disc(path: &Path) -> Result<Disc, String> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "bin" | "iso" => std::fs::read(path)
            .map(Disc::from_bin)
            .map_err(|error| error.to_string()),
        "cue" => psoxide_settings::library::load_disc_from_cue(path),
        "ccd" => psoxide_settings::library::load_disc_from_ccd(path),
        other => Err(format!("unsupported auxiliary disc extension: .{other}")),
    }
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
fn cmd_dump_editor_ui(args: DumpEditorUiArgs) -> Result<(), String> {
    if args.width == 0 || args.height == 0 {
        return Err("headless editor dimensions must be non-zero".to_string());
    }
    if args.width > 8192 || args.height > 8192 {
        return Err("headless editor dimensions must not exceed 8192 px".to_string());
    }

    let (project_root, _) = resolve_project_arg(&args.project);
    let mut editor = EditorWorkspace::open_directory(&project_root)
        .map_err(|error| format!("open editor project at {}: {error}", project_root.display()))?;
    let workspace_view = args.view.project_view();
    editor.show_workspace(workspace_view);

    if let Some(selector) = args.resource.as_deref() {
        if !matches!(
            args.view,
            EditorViewArg::Animation | EditorViewArg::Material
        ) {
            return Err("--resource requires --view animation or --view material".to_string());
        }
        let numeric_id = selector.parse::<u64>().ok();
        let resource_id = editor
            .project()
            .resources
            .iter()
            .find(|resource| {
                numeric_id.is_some_and(|id| resource.id.raw() == id) || resource.name == selector
            })
            .or_else(|| {
                editor
                    .project()
                    .resources
                    .iter()
                    .find(|resource| resource.name.eq_ignore_ascii_case(selector))
            })
            .map(|resource| resource.id)
            .ok_or_else(|| format!("editor resource {selector:?} was not found"))?;
        let focused = match args.view {
            EditorViewArg::Animation => {
                editor.open_animation_viewer_for_resource(resource_id)
                    && editor.animation_viewer_resource_is_focused(resource_id)
            }
            EditorViewArg::Material => editor.focus_material_resource(resource_id),
            EditorViewArg::ThreeD | EditorViewArg::TwoD => false,
        };
        if !focused {
            return Err(format!(
                "editor resource {selector:?} could not be focused in {:?}",
                args.view
            ));
        }
    }

    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    let viewport_texture = ctx.load_texture(
        "headless-editor-viewport",
        egui::ColorImage::new([640, 480], egui::Color32::from_rgb(8, 10, 14)),
        egui::TextureOptions::NEAREST,
    );
    let (viewport, play_status) = if let Some(view) = args.debug_map_view.as_deref() {
        if !editor.set_play_debug_map_view(view) {
            return Err(format!(
                "unknown --debug-map-view {view:?}; expected rooms, cells, portals, or streaming"
            ));
        }
        let topology = psxed_project::playtest::build_debug_topology(editor.project());
        let room_count = topology
            .cells
            .iter()
            .map(|cell| cell.runtime_room_index + 1)
            .max()
            .unwrap_or_default();
        let room_mask = if room_count >= u64::BITS as usize {
            u64::MAX
        } else if room_count == 0 {
            0
        } else {
            (1u64 << room_count) - 1
        };
        let portal_count = topology.portals.len().min(u64::BITS as usize);
        let portal_mask = if portal_count == u64::BITS as usize {
            u64::MAX
        } else if portal_count == 0 {
            0
        } else {
            (1u64 << portal_count) - 1
        };
        let metrics = psxed_ui::EditorPlaytestMetrics {
            chunk_visible: room_count as u32,
            chunk_loaded: room_count as u32,
            stream_slot_limit: room_count.max(1) as u32,
            portal_visible_rooms: room_count as u32,
            chunk_loaded_mask: room_mask,
            chunk_active_mask: room_mask,
            chunk_drawn_mask: room_mask,
            portal_visible_mask: room_mask,
            portal_tested_mask: room_mask,
            portal_accepted_mask: room_mask,
            portal_tested_portal_mask: portal_mask,
            portal_accepted_portal_mask: portal_mask,
            player_map_valid: room_count > 0,
            player_room_index: 0,
            portal_current_room_index: 0,
            ..Default::default()
        };
        (
            EditorViewport3dPresentation::play(
                viewport_texture.id(),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                psxed_ui::EditorPlaytestTapeStatus::default(),
                Some(metrics),
                false,
            ),
            EditorPlaytestStatus::Running {
                input_captured: false,
            },
        )
    } else {
        (
            EditorViewport3dPresentation::edit(viewport_texture.id(), Vec::new()),
            EditorPlaytestStatus::Idle,
        )
    };

    // Prime one complete frame before injecting input. This mirrors the native
    // app's first layout pass and ensures fonts/resource textures and widget
    // focus state all exist before the captured interaction frame.
    let first = ctx.run(
        headless_editor_input(args.width, args.height, 0.0, Vec::new()),
        |ctx| editor.draw(ctx, viewport.clone(), play_status),
    );
    let mut textures_delta = first.textures_delta;

    let events = if args.frame_selected {
        vec![egui::Event::Key {
            key: egui::Key::Period,
            physical_key: Some(egui::Key::Period),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    } else {
        Vec::new()
    };
    let captured = ctx.run(
        headless_editor_input(args.width, args.height, 1.0 / 60.0, events),
        |ctx| editor.draw(ctx, viewport.clone(), play_status),
    );
    textures_delta.append(captured.textures_delta);
    let paint_jobs = ctx.tessellate(captured.shapes, captured.pixels_per_point);

    render_headless_editor_png(
        &paint_jobs,
        textures_delta,
        args.width,
        args.height,
        captured.pixels_per_point,
        &args.out,
    )?;
    println!(
        "headless editor ui: view={:?} resource={} status={:?} out={}",
        workspace_view,
        args.resource.as_deref().unwrap_or("(none)"),
        editor.status_text(),
        args.out.display()
    );
    Ok(())
}

#[cfg(feature = "editor")]
fn headless_editor_input(
    width: u32,
    height: u32,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::RawInput {
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32));
    let mut input = egui::RawInput {
        screen_rect: Some(rect),
        time: Some(time),
        events,
        focused: true,
        ..Default::default()
    };
    if let Some(viewport) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
        viewport.native_pixels_per_point = Some(1.0);
        viewport.inner_rect = Some(rect);
        viewport.outer_rect = Some(rect);
        viewport.focused = Some(true);
        viewport.fullscreen = Some(true);
    }
    input
}

#[cfg(feature = "editor")]
fn render_headless_editor_png(
    paint_jobs: &[egui::ClippedPrimitive],
    textures_delta: egui::TexturesDelta,
    width: u32,
    height: u32,
    pixels_per_point: f32,
    out: &Path,
) -> Result<(), String> {
    let (device, queue) = headless_wgpu_device()?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("psoxide-headless-editor-ui"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);
    for (id, delta) in &textures_delta.set {
        renderer.update_texture(&device, &queue, *id, delta);
    }

    let screen = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [width, height],
        pixels_per_point,
    };
    let unpadded_bytes_per_row = width * 4;
    let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(alignment) * alignment;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("psoxide-headless-editor-ui-readback"),
        size: (padded_bytes_per_row * height) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("psoxide-headless-editor-ui-encoder"),
    });
    renderer.update_buffers(&device, &queue, &mut encoder, paint_jobs, &screen);
    {
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("psoxide-headless-editor-ui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.04,
                            g: 0.04,
                            b: 0.06,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            })
            .forget_lifetime();
        renderer.render(&mut pass, paint_jobs, &screen);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::Maintain::Wait);
    receiver
        .recv()
        .map_err(|_| "headless editor readback callback dropped".to_string())?
        .map_err(|error| format!("headless editor readback: {error:?}"))?;
    let mapped = slice.get_mapped_range();
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height {
        let start = (row * padded_bytes_per_row) as usize;
        let end = start + unpadded_bytes_per_row as usize;
        rgba.extend_from_slice(&mapped[start..end]);
    }
    drop(mapped);
    readback.unmap();

    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    image::save_buffer_with_format(
        out,
        &rgba,
        width,
        height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .map_err(|error| format!("write {}: {error}", out.display()))?;
    Ok(())
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
    warmup_steps: u64,
) {
    if !enabled {
        return;
    }
    // Warm the firmware with the tray closed and the target disc already
    // present. Retail BIOS startup configures the CD path and uploads its
    // shell audio banks before Exec; warming an empty drive loses that
    // observable peripheral state even if the executable is loaded later.
    bus.cdrom.insert_disc(Some(disc.clone()));
    if let Err(e) = warm_bios_for_disc_fast_boot(bus, cpu, warmup_steps) {
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

fn write_pc_sample_log(path: &Path, samples: &BTreeMap<u32, u64>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let file = std::fs::File::create(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    writeln!(writer, "pc,samples,percent").map_err(|error| error.to_string())?;
    let total = samples.values().copied().sum::<u64>();
    let mut ranked = samples
        .iter()
        .map(|(&pc, &count)| (pc, count))
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|(pc_a, count_a), (pc_b, count_b)| {
        count_b.cmp(count_a).then_with(|| pc_a.cmp(pc_b))
    });
    for (pc, count) in ranked {
        let percent = if total == 0 {
            0.0
        } else {
            count as f64 * 100.0 / total as f64
        };
        writeln!(writer, "0x{pc:08x},{count},{percent:.6}").map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn write_pc_callsite_sample_log(
    path: &Path,
    samples: &BTreeMap<(u32, u32, u32, u32), u64>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let file = std::fs::File::create(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    writeln!(
        writer,
        "pc,return_address,stack_return_address,inner_stack_return_address,samples,percent"
    )
    .map_err(|error| error.to_string())?;
    let total = samples.values().copied().sum::<u64>();
    let mut ranked = samples
        .iter()
        .map(
            |(&(pc, return_address, stack_return_address, inner_stack_return_address), &count)| {
                (
                    pc,
                    return_address,
                    stack_return_address,
                    inner_stack_return_address,
                    count,
                )
            },
        )
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(
        |(pc_a, return_a, stack_a, inner_a, count_a),
         (pc_b, return_b, stack_b, inner_b, count_b)| {
            count_b
                .cmp(count_a)
                .then_with(|| pc_a.cmp(pc_b))
                .then_with(|| return_a.cmp(return_b))
                .then_with(|| stack_a.cmp(stack_b))
                .then_with(|| inner_a.cmp(inner_b))
        },
    );
    for (pc, return_address, stack_return_address, inner_stack_return_address, count) in ranked {
        let percent = if total == 0 {
            0.0
        } else {
            count as f64 * 100.0 / total as f64
        };
        writeln!(
            writer,
            "0x{pc:08x},0x{return_address:08x},0x{stack_return_address:08x},0x{inner_stack_return_address:08x},{count},{percent:.6}"
        )
        .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn write_pc_window_sample_log(
    path: &Path,
    samples: &BTreeMap<(u64, u32), u64>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let file = std::fs::File::create(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    writeln!(writer, "window_start_tick,pc,samples,percent_window")
        .map_err(|error| error.to_string())?;
    let mut window_totals = BTreeMap::<u64, u64>::new();
    for (&(window_start, _), &count) in samples {
        *window_totals.entry(window_start).or_insert(0) += count;
    }
    for (&(window_start, pc), &count) in samples {
        let window_total = window_totals.get(&window_start).copied().unwrap_or(0);
        let percent = if window_total == 0 {
            0.0
        } else {
            count as f64 * 100.0 / window_total as f64
        };
        writeln!(writer, "{window_start},0x{pc:08x},{count},{percent:.6}")
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
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

/// One scheduled button press from `--press`.
struct ScriptedPress {
    /// Route tick the press starts on.
    tick: u64,
    /// How many route ticks to hold it.
    hold: u64,
    /// Pad button bits.
    mask: u16,
}

/// Parse a `--press` spec: `tick:button[:hold]`, comma separated.
fn parse_press_script(spec: &str) -> Result<Vec<ScriptedPress>, String> {
    let mut out = Vec::new();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        let mut parts = entry.split(':');
        let (Some(tick), Some(name)) = (parts.next(), parts.next()) else {
            return Err(format!("--press '{entry}': expected tick:button[:hold]"));
        };
        let tick: u64 = tick
            .trim()
            .parse()
            .map_err(|_| format!("--press '{entry}': '{tick}' is not a tick number"))?;
        let mask = press_button_mask(name.trim())
            .ok_or_else(|| format!("--press '{entry}': unknown button '{name}'"))?;
        // A press shorter than a pad-poll interval can fall entirely between
        // two polls and never be seen, so the default is several ticks.
        let hold = match parts.next() {
            Some(hold) => hold
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("--press '{entry}': '{hold}' is not a hold length"))?,
            None => 4,
        };
        if hold == 0 {
            return Err(format!("--press '{entry}': hold must be at least 1 tick"));
        }
        if parts.next().is_some() {
            return Err(format!("--press '{entry}': expected tick:button[:hold]"));
        }
        out.push(ScriptedPress { tick, hold, mask });
    }
    if out.is_empty() {
        return Err("--press: no presses in spec".to_string());
    }
    Ok(out)
}

/// Map a `--press` button name to its pad bits.
fn press_button_mask(name: &str) -> Option<u16> {
    use emulator_core::pad::button;
    Some(match name.to_ascii_lowercase().as_str() {
        "cross" | "x" => button::CROSS,
        "circle" | "o" => button::CIRCLE,
        "square" => button::SQUARE,
        "triangle" => button::TRIANGLE,
        "start" => button::START,
        "select" => button::SELECT,
        "up" => button::UP,
        "down" => button::DOWN,
        "left" => button::LEFT,
        "right" => button::RIGHT,
        "l1" => button::L1,
        "r1" => button::R1,
        "l2" => button::L2,
        "r2" => button::R2,
        _ => return None,
    })
}

#[cfg(test)]
mod press_script_tests {
    use super::*;

    #[test]
    fn launch_parses_independent_memory_cards_for_both_ports() {
        let cli = Cli::try_parse_from([
            "frontend",
            "launch",
            "--path",
            "game.exe",
            "--memcard",
            "slot-1.mcd",
            "--memcard2",
            "slot-2.mcd",
        ])
        .expect("dual-card launch arguments");

        let Some(Command::Launch(args)) = cli.command else {
            panic!("expected launch command");
        };
        assert_eq!(args.memcard, Some(PathBuf::from("slot-1.mcd")));
        assert_eq!(args.memcard2, Some(PathBuf::from("slot-2.mcd")));
    }

    #[test]
    fn parses_ticks_buttons_and_optional_hold() {
        let script = parse_press_script("30:cross, 60:start:12").expect("valid spec");
        assert_eq!(script.len(), 2);
        assert_eq!(script[0].tick, 30);
        assert_eq!(script[0].hold, 4, "default hold spans several pad polls");
        assert_eq!(script[0].mask, emulator_core::pad::button::CROSS);
        assert_eq!(script[1].tick, 60);
        assert_eq!(script[1].hold, 12);
        assert_eq!(script[1].mask, emulator_core::pad::button::START);
    }

    #[test]
    fn rejects_specs_that_would_silently_do_nothing() {
        // A typo'd button, a zero hold and an empty spec all mean the run
        // quietly never leaves the menu, which is the failure this flag exists
        // to remove. Fail loudly instead.
        assert!(parse_press_script("30:corss").is_err());
        assert!(parse_press_script("30:cross:0").is_err());
        assert!(parse_press_script("").is_err());
        assert!(parse_press_script("cross").is_err());
    }

    #[test]
    fn callsite_sample_csv_keeps_both_stack_return_addresses() {
        let path =
            std::env::temp_dir().join(format!("psoxide-pc-callsites-{}.csv", std::process::id()));
        let mut samples = BTreeMap::new();
        samples.insert((0x800b_5608, 0x800b_622c, 0x800b_622c, 0x8009_4ce8), 3);
        samples.insert((0x800b_560c, 0x800b_622c, 0x800b_622c, 0x8007_e5ec), 1);
        write_pc_callsite_sample_log(&path, &samples).expect("write callsite CSV");
        let csv = std::fs::read_to_string(&path).expect("read callsite CSV");
        let _ = std::fs::remove_file(&path);
        assert!(csv.starts_with(
            "pc,return_address,stack_return_address,inner_stack_return_address,samples,percent\n"
        ));
        assert!(csv.contains("0x800b5608,0x800b622c,0x800b622c,0x80094ce8,3,75.000000"));
        assert!(csv.contains("0x800b560c,0x800b622c,0x800b622c,0x8007e5ec,1,25.000000"));
    }
}
