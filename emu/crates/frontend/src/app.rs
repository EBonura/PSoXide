//! Top-level application state and UI orchestration.
//!
//! Owns the emulator state (currently just a `Cpu` + `Bus` -- VRAM will
//! join once the GPU subsystem lands) and drives the per-frame UI build.

use std::collections::{BTreeSet, VecDeque};
#[cfg(feature = "editor")]
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use emulator_core::{
    fast_boot_disc_with_hle, warm_bios_for_disc_fast_boot, Bus, Cpu, EmulatorState,
    EmulatorStateRef, DISC_FAST_BOOT_WARMUP_STEPS,
};
use psoxide_settings::library::{GameKind, Region};
#[cfg(not(target_arch = "wasm32"))]
use psoxide_settings::savestate::peek_header;
use psoxide_settings::savestate::SaveStateV1;
use psoxide_settings::{ConfigPaths, Library, LibraryEntry, Settings};
use psx_iso::{Disc, Exe, SECTOR_BYTES};
use psx_trace::InstructionRecord;
#[cfg(feature = "editor")]
use psxed_project::EditorWorkspaceView;
#[cfg(feature = "editor")]
use psxed_ui::{EditorPlaytestStatus, EditorWorkspace};

use crate::burn::{validate_burn_target_path, BurnState};
#[cfg(feature = "editor")]
use crate::embedded_playtest::EmbeddedPlaytestState;
#[cfg(feature = "editor")]
use crate::playtest_disc::{
    build_embedded_playtest_disc, build_log_failure_detail, copy_project_disc,
    editor_playtest_build_log_path, project_baked_disc_path, project_build_menu_metadata,
    project_disc_volume_id, DEFAULT_EMBEDDED_PLAYTEST_VOLUME_ID,
};
use crate::playtest_input::{PlaytestInputEvent, PlaytestInputTape, Port1PadSample};
use crate::ui;
use crate::ui::hud::HudState;
use crate::ui::memory::MemoryView;
use crate::ui::menu::{LibraryItem as MenuLibraryItem, MenuState, PadBindTarget, SaveStateRow};
use crate::{paths_equivalent, repo_root_dir};

/// Ring-buffer capacity for the execution-history panel. 16 rows is
/// the "what just ran" context window -- enough to spot a tight loop
/// or trace a branch without the history section taking over the
/// registers side panel vertically.
pub const EXEC_HISTORY_CAP: usize = 16;

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        !matches!(value.as_str(), "" | "0" | "false" | "FALSE" | "off" | "OFF")
    })
}

/// Panels that can be shown/hidden via the Menu. The Menu *is* the
/// library browser (Games / Examples columns), so we don't have
/// a separate "library" panel -- it's integrated into the shell
/// the PSX way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanelVisibility {
    /// Unified emulator diagnostics sidebar.
    pub debug_sidebar: bool,
    /// CPU registers + exec history section.
    pub registers: bool,
    /// Memory / disassembly viewer section.
    pub memory: bool,
    /// VRAM viewer section.
    pub vram: bool,
    /// Frame-profiler section.
    pub profiler: bool,
}

impl PanelVisibility {
    /// Startup visibility: everything collapsed and the sidebar hidden,
    /// unless `PSOXIDE_DEBUG_SIDEBAR=1` (dev hook: open the sidebar with
    /// every section expanded, e.g. for layout work / screenshots).
    pub fn startup() -> Self {
        let dev_open = std::env::var("PSOXIDE_DEBUG_SIDEBAR")
            .map(|v| v == "1")
            .unwrap_or(false);
        Self {
            debug_sidebar: dev_open,
            registers: dev_open,
            memory: dev_open,
            vram: dev_open,
            profiler: dev_open,
        }
    }
}

/// Hardware-renderer internal scale mode. Both modes use the same
/// renderer; Native forces scale 1, Window chooses a larger scale
/// from the framebuffer panel size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScaleMode {
    /// Internal scale chosen from the available framebuffer area.
    #[default]
    Window,
    /// Internal scale 1, presented in the same framebuffer area.
    Native,
}

/// Sample-time texture filter, cycled from the toolbar. Maps to the
/// `u_texfilter` uniform the fragment shader reads. Seam-free filtering isn't
/// possible on PSX's packed VRAM (matches DuckStation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureFilter {
    /// PSX-native point sampling.
    #[default]
    None,
    /// Edge-directed xBR (best on 3D; soft/odd on 2D/tiled backgrounds).
    Xbr,
}

impl TextureFilter {
    /// Cycle to the next mode (wraps).
    pub fn next(self) -> Self {
        match self {
            TextureFilter::None => TextureFilter::Xbr,
            TextureFilter::Xbr => TextureFilter::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TextureFilter::None => "None",
            TextureFilter::Xbr => "xBR",
        }
    }

    /// `u_texfilter.x` value the shader branches on.
    pub fn mode(self) -> u32 {
        match self {
            TextureFilter::None => 0,
            TextureFilter::Xbr => 3,
        }
    }
}

/// Active host workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workspace {
    /// Emulator/debugger workspace.
    Emulator,
    /// Mouse/keyboard editor workspace.
    #[cfg(feature = "editor")]
    Editor,
}

impl Workspace {
    /// True when editor panels own the central UI. Always false without the
    /// editor feature (there is no editor workspace to switch into).
    pub const fn is_editor(self) -> bool {
        #[cfg(feature = "editor")]
        {
            matches!(self, Self::Editor)
        }
        #[cfg(not(feature = "editor"))]
        {
            false
        }
    }
}

/// Work to perform after the shared editor-playtest MIPS build exits.
#[cfg(feature = "editor")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum EditorBuildCompletion {
    /// Wrap the built runtime into a CUE/BIN disc and load it into the
    /// embedded editor viewport.
    RunEmbedded { volume_id: String },
    /// Copy the built CUE/BIN disc into the active project's baked output folder.
    ExportProject {
        dest_path: PathBuf,
        volume_id: String,
    },
}

/// Top-level app state. Owns the emulator state directly -- no Arc/Mutex,
/// single-threaded, UI reads state in-place per frame.
/// Payloads baked into the web build. They appear in the menu and boot via
/// the no-BIOS HLE path. Full disc images are not baked here -- they stream
/// on demand instead (`web_stream`), because `include_bytes!` puts the image
/// inside the wasm every visitor downloads up front.
pub mod bundled {
    /// What a baked-in payload is, which decides both how it boots and which
    /// menu column it lands in.
    #[derive(Copy, Clone, PartialEq, Eq)]
    pub enum BundledKind {
        /// A full disc image, booted through the no-BIOS HLE disc path. Games.
        #[allow(dead_code)]
        DiscBin,
        /// A raw PSX-EXE, side-loaded. Examples and tests; roughly a tenth the
        /// size of the equivalent disc, which is what makes bundling a set of
        /// them affordable in the wasm payload.
        Exe,
    }

    /// One baked-in payload.
    pub struct BundledDisc {
        /// Menu launch id; the `bundled:` prefix is how launch routing spots it.
        pub id: &'static str,
        /// Menu title.
        pub title: &'static str,
        /// Menu subtitle.
        pub subtitle: &'static str,
        /// Disc image or PSX-EXE.
        pub kind: BundledKind,
        /// Raw payload bytes.
        pub bytes: &'static [u8],
    }

    /// The baked-in payloads in menu order. The first auto-boots on load.
    ///
    /// Games hold shipped homebrew; examples hold the SDK/engine samples and
    /// tests. They are separate columns in the menu, so a sample never shows up
    /// beside a real game.
    pub static DISCS: &[BundledDisc] = &[
        BundledDisc {
            id: "bundled:game-breakout",
            title: "game-breakout",
            subtitle: "sample game",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/game-breakout.exe"),
        },
        BundledDisc {
            id: "bundled:game-invaders",
            title: "game-invaders",
            subtitle: "sample game",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/game-invaders.exe"),
        },
        BundledDisc {
            id: "bundled:game-magikaaaaaarp-pong",
            title: "game-magikaaaaaarp-pong",
            subtitle: "sample game",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/game-magikaaaaaarp-pong.exe"),
        },
        BundledDisc {
            id: "bundled:game-pong",
            title: "game-pong",
            subtitle: "sample game",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/game-pong.exe"),
        },
        BundledDisc {
            id: "bundled:hello-audio",
            title: "hello-audio",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-audio.exe"),
        },
        BundledDisc {
            id: "bundled:hello-cdda",
            title: "hello-cdda",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-cdda.exe"),
        },
        BundledDisc {
            id: "bundled:hello-engine",
            title: "hello-engine",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-engine.exe"),
        },
        BundledDisc {
            id: "bundled:hello-gte",
            title: "hello-gte",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-gte.exe"),
        },
        BundledDisc {
            id: "bundled:hello-input",
            title: "hello-input",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-input.exe"),
        },
        BundledDisc {
            id: "bundled:hello-memcard",
            title: "hello-memcard",
            subtitle: "hardware diagnostic",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-memcard.exe"),
        },
        BundledDisc {
            id: "bundled:hello-ot",
            title: "hello-ot",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-ot.exe"),
        },
        BundledDisc {
            id: "bundled:hello-pack",
            title: "hello-pack",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-pack.exe"),
        },
        BundledDisc {
            id: "bundled:hello-tex",
            title: "hello-tex",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-tex.exe"),
        },
        BundledDisc {
            id: "bundled:hello-tri",
            title: "hello-tri",
            subtitle: "sample",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/hello-tri.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-3d",
            title: "showcase-3d",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-3d.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-fog",
            title: "showcase-fog",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-fog.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-lights",
            title: "showcase-lights",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-lights.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-model",
            title: "showcase-model",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-model.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-particles",
            title: "showcase-particles",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-particles.exe"),
        },
        BundledDisc {
            id: "bundled:showcase-text",
            title: "showcase-text",
            subtitle: "engine showcase",
            kind: BundledKind::Exe,
            bytes: include_bytes!("../assets/examples/showcase-text.exe"),
        },
    ];

    /// Look up a baked-in disc by its menu launch id.
    pub fn find(id: &str) -> Option<&'static BundledDisc> {
        DISCS.iter().find(|d| d.id == id)
    }
}

/// Analog-stick drive for the freelook camera (see the toolbar EYE toggle
/// and the L3+R3 pad chord). Filled each frame from the merged host sticks
/// (keyboard-emulated + real gamepad); applied per frame. Left stick moves,
/// right stick looks, matching the standard twin-stick freecam.
#[derive(Copy, Clone, Default)]
pub struct FreelookInput {
    /// Left stick, −1.0..=1.0 per axis. x = strafe, y = dolly (up = forward).
    pub left: (f32, f32),
    /// Right stick, −1.0..=1.0 per axis. x = yaw, y = pitch (up = look up).
    pub right: (f32, f32),
    /// Held while R2 is down -- move and look faster.
    pub boost: bool,
}

#[derive(Clone, Debug)]
struct PendingInputProfileCapture {
    tape_path: PathBuf,
    phase: &'static str,
}

pub struct AppState {
    /// Active host workspace.
    pub workspace: Workspace,
    /// Embedded editor workspace. Kept alive while hidden so editor
    /// state survives a quick trip back to the Menu/emulator.
    #[cfg(feature = "editor")]
    pub editor: EditorWorkspace,
    /// In-process playtest launched from the editor viewport.
    #[cfg(feature = "editor")]
    pub embedded_playtest: EmbeddedPlaytestState,
    /// Video-frame-exact port-1 recording/replay shared by ordinary emulator
    /// sessions, headless runs and embedded editor playtests.
    playtest_input_tape: PlaytestInputTape,
    /// Whole-run profiler capture waiting for the current host sample to be
    /// recorded before it is persisted beside its input tape.
    pending_input_profile_capture: Option<PendingInputProfileCapture>,
    /// Editor project directory observed at the last
    /// [`AppState::sync_embedded_playtest_with_editor_project`]
    /// call. When the editor's current project_dir diverges, the
    /// embedded playtest belongs to a different project and gets
    /// stopped so the viewport doesn't keep showing stale output.
    #[cfg(feature = "editor")]
    editor_project_dir_seen: PathBuf,
    /// Deferred action attached to the currently running editor build.
    #[cfg(feature = "editor")]
    editor_build_completion: Option<EditorBuildCompletion>,
    /// Byte position already mirrored from the active compiler log into the
    /// editor's bottom Console.
    #[cfg(feature = "editor")]
    editor_build_log_offset: u64,
    /// Incomplete final compiler line retained until its newline arrives.
    #[cfg(feature = "editor")]
    editor_build_log_pending: Vec<u8>,
    /// Background `make examples` job launched from the Examples menu.
    examples_build_child: Option<Child>,
    /// CD burning submenu state and burner hotplug watcher.
    pub(crate) burn: BurnState,
    pub panels: PanelVisibility,
    /// Last rendered debug-sidebar width, used as the slide-in animation
    /// target so a resized sidebar animates to its own width.
    pub sidebar_width: f32,
    /// Framebuffer mode -- shared HW renderer at native scale vs
    /// window-fitted high resolution. Toggled via the debug toolbar.
    pub scale_mode: ScaleMode,
    /// Sample-time texture filter, cycled from the toolbar.
    pub texture_filter: TextureFilter,
    /// When true the top toolbar is slid up out of view, leaving only a
    /// small floating restore tab at the top-right.
    pub toolbar_hidden: bool,
    /// Physical pixel size used by the central framebuffer on the
    /// previous UI frame. The renderer uses this as its internal
    /// resolution budget; one-frame latency is fine because it only
    /// changes when resizing/toggling scale mode.
    pub framebuffer_present_size_px: (u32, u32),
    pub cpu: Cpu,
    /// Debug freelook camera pose pushed to the CPU each frame (off
    /// unless the toolbar EYE toggle is on).
    pub freelook: emulator_core::FreelookState,
    /// Latest twin-stick freelook drive, refreshed each frame from the
    /// merged host sticks (only consumed while `freelook.enabled`).
    pub freelook_input: FreelookInput,
    /// Optional because we let the frontend run without a BIOS for UI
    /// development. If absent, register panels show the reset-state CPU
    /// but no instruction stepping is possible. Unused until the step
    /// button lands alongside the Menu.
    pub bus: Option<Bus>,
    /// Incremented whenever CPU-owned VRAM is replaced or mutated outside
    /// normal GP0 command replay. The shell uses this to rebuild the
    /// persistent hardware-renderer target from the CPU truth before
    /// replaying the next command log.
    pub gpu_resync_generation: u64,
    pub menu: MenuState,
    pub hud: HudState,
    /// Rolling frame-time breakdown, visible from the profiler toolbar button.
    pub profiler: ui::profiler::FrameProfiler,
    pub memory_view: MemoryView,
    /// When true, the shell advances emulation on each redraw. Toggled
    /// via the Menu's Run/Pause item.
    pub running: bool,
    /// Safety cap for one frontend frame. The run loop targets PSX
    /// master-clock cycles, not this many instructions, but the cap
    /// prevents a broken guest from spinning forever in one redraw.
    pub run_steps_per_frame: u32,
    /// Rolling window of the last [`EXEC_HISTORY_CAP`] retired
    /// instructions, newest at the back. Driven by both single-step
    /// and continuous-run paths.
    pub exec_history: VecDeque<InstructionRecord>,
    /// PC addresses at which the run loop pauses. Toggled from the
    /// memory viewer; displayed in the register panel.
    pub breakpoints: BTreeSet<u32>,
    /// Snapshot of `cpu.gprs()` at some point the user chose (via the
    /// register panel's "Snapshot" button). The panel highlights GPRs
    /// whose current value differs from the snapshot. Reset clears
    /// this along with the rest of the emulator state.
    pub gpr_snapshot: Option<[u32; 32]>,
    /// Persisted user preferences (BIOS path, library root, input
    /// mappings, video tweaks). Read at startup, re-saved when Menu
    /// settings actions commit changes. The frontend mutates this
    /// directly; the filesystem is written via
    /// [`AppState::save_settings`].
    pub settings: Settings,
    /// Cached library scan results. Populated from
    /// `<config>/library.ron` at startup, refreshed by
    /// [`AppState::rescan_library`] (triggered from the Menu's
    /// Games / Examples "Refresh library" row).
    pub library: Library,
    /// Resolved on-disk paths (settings.ron, library.ron, per-game
    /// subtree). Set once from the platform default or a
    /// `--config-dir` override and never mutated afterwards.
    pub paths: ConfigPaths,
    /// What the BIOS was asked to boot at the last launch. `None`
    /// = no game loaded yet (initial state on first run, also after
    /// "Reset" with no last-loaded game).
    pub current_game: Option<LibraryEntry>,
    /// Short-lived status line -- shows "Launched <title>",
    /// "Scan complete: 54 games", etc. Displayed beneath the
    /// library panel; cleared after a few frames.
    pub status_message: Option<(String, f32)>,
    /// Host-audio gain controlled from the toolbar. `1.0` is unity.
    pub audio_volume: f32,
    /// Toolbar mute latch. Kept separate from `audio_volume` so
    /// unmuting restores the prior level.
    pub audio_muted: bool,
    /// Native save-state thumbnails waiting for the shell to capture the
    /// hardware-renderer display. AppState owns save files, but only the shell
    /// has access to the wgpu texture that was actually presented.
    #[cfg(not(target_arch = "wasm32"))]
    pending_savestate_thumbnails: Vec<PathBuf>,
    /// Web build only: the BIOS image uploaded this session. The browser has no
    /// filesystem, so a real-BIOS retail disc boot reads its BIOS from here
    /// instead of `settings.paths.bios`. `None` until the user loads one.
    #[cfg(target_arch = "wasm32")]
    bios_bytes: Option<Vec<u8>>,
    /// Web build: games found by the last folder scan, as `(id, title,
    /// subtitle)`. Injected into the Games menu category; launching one reads
    /// its file bytes on demand.
    #[cfg(target_arch = "wasm32")]
    web_games: Vec<(String, String, String)>,
    /// Metadata for the current game's single persistent browser quick-save.
    /// The payload itself stays in IndexedDB and is only read for F7/load.
    #[cfg(target_arch = "wasm32")]
    web_quick_save: Option<(String, u64, u64)>,
    /// Streamed CD-DA tracks waiting to be copied into the mounted disc, as
    /// `(track, pcm, bytes_copied)`. Copied a slice per frame: one 40 MB
    /// memcpy in a single frame underruns the CD audio, and the launcher's
    /// stall watchdog answers an underrun by restarting the song.
    #[cfg(target_arch = "wasm32")]
    web_track_patches: Vec<(u8, Vec<u8>, usize)>,
    /// Web build: how the current game was booted, holding what a cold reboot
    /// needs. Input recording and replay reboot through this so tapes align
    /// with poll 0 of a fresh machine.
    #[cfg(target_arch = "wasm32")]
    web_boot: Option<WebBoot>,
    /// [`emulator_core::game_image_hash`] of the current game image, both
    /// targets. Recorded into browser tape CSVs; compared when a replay
    /// loads so a changed build gets flagged to the user. `None` when the
    /// image bytes were never in hand (e.g. a bundled example boot).
    current_game_hash: Option<u64>,
}

/// How the web build booted the current game (see `AppState::web_boot`).
#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
enum WebBoot {
    /// Homebrew PS-EXE; small enough to keep the bytes for reboot.
    Exe(Vec<u8>),
    /// Uploaded raw disc booted through the uploaded real BIOS. The disc
    /// itself is taken back out of the outgoing bus at reboot time.
    DiscBios,
    /// Streamed disc booted through HLE fast boot (no BIOS), same reboot
    /// path as `DiscBios`.
    DiscHle,
}

impl Default for AppState {
    fn default() -> Self {
        Self::with_config_dir(None)
    }
}

impl AppState {
    /// Build app state honouring an optional `--config-dir`
    /// override. `None` means "use the platform default" (the
    /// normal user path); `Some(p)` means "use this directory"
    /// (testing / portable installs).
    pub fn with_config_dir(override_dir: Option<PathBuf>) -> Self {
        // Resolve the config directory up-front. In production this
        // lives under ~/Library/Application Support/PSoXide
        // (macOS) etc; if the OS won't give us one we degrade to a
        // tempdir-rooted view so the app still runs -- just without
        // persistence.
        let paths = match override_dir {
            Some(p) => ConfigPaths::rooted(p),
            #[cfg(not(target_arch = "wasm32"))]
            None => ConfigPaths::platform_default().unwrap_or_else(|e| {
                eprintln!("[frontend] no platform config dir ({e}); persistence disabled");
                ConfigPaths::rooted(std::env::temp_dir().join("PSoXide-ephemeral"))
            }),
            // wasm has no filesystem: root at a virtual path and skip the
            // directories/temp_dir resolution (both panic on wasm). Persistence
            // is in-memory only; the fs reads below degrade to defaults.
            #[cfg(target_arch = "wasm32")]
            None => ConfigPaths::rooted(PathBuf::from("/psoxide")),
        };
        let _ = paths.ensure_dir(paths.root());

        // Legacy file-based workspace: surface once, then ignore.
        // The new model is project = directory under
        // editor/projects/. No automated migration; a stale
        // workspace.ron is just a starter snapshot.
        #[cfg(feature = "editor")]
        {
            let legacy_workspace = paths.editor_dir().join("workspace.ron");
            if legacy_workspace.is_file() {
                eprintln!(
                    "[frontend] legacy editor/workspace.ron at {} ignored - projects now live under editor/projects/",
                    legacy_workspace.display()
                );
            }
        }

        let settings = match Settings::load(&paths.settings_file()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[frontend] settings load: {e}; using defaults");
                Settings::default()
            }
        };

        #[cfg(feature = "editor")]
        let editor = {
            let preferred_dir = settings
                .editor
                .last_project_dir
                .clone()
                .unwrap_or_else(psxed_project::new_project_template_dir);
            EditorWorkspace::open_directory(&preferred_dir)
                .or_else(|first_err| {
                    eprintln!(
                        "[frontend] open editor project at {} failed: {first_err}; falling back to BSP starter",
                        preferred_dir.display()
                    );
                    EditorWorkspace::open_directory(psxed_project::new_project_template_dir())
                })
                .unwrap_or_else(|err| {
                    panic!("open BSP starter project: {err}");
                })
        };
        let library = Library::load_or_empty(&paths.library_file());

        // Legacy env-var side-load path: if PSOXIDE_EXE or
        // PSOXIDE_DISC is set, honour it so existing developer
        // workflows keep working. The library UI is the forward path
        // for everyone else; authored builds should normally launch
        // through CUE/BIN discs.
        let mut cpu = Cpu::new();
        let bus = load_initial_bus(&settings, &mut cpu);
        let autorun = bus.is_some() && env_flag("PSOXIDE_AUTORUN");

        let initial_gpu_resync_generation = if bus.is_some() { 1 } else { 0 };
        #[cfg(feature = "editor")]
        let editor_project_dir_seen = editor.project_dir().to_path_buf();
        let mut out = Self {
            workspace: Workspace::Emulator,
            #[cfg(feature = "editor")]
            editor,
            #[cfg(feature = "editor")]
            embedded_playtest: EmbeddedPlaytestState::default(),
            playtest_input_tape: PlaytestInputTape::default(),
            pending_input_profile_capture: None,
            #[cfg(feature = "editor")]
            editor_project_dir_seen,
            #[cfg(feature = "editor")]
            editor_build_completion: None,
            #[cfg(feature = "editor")]
            editor_build_log_offset: 0,
            #[cfg(feature = "editor")]
            editor_build_log_pending: Vec::new(),
            examples_build_child: None,
            burn: BurnState::default(),
            panels: PanelVisibility::startup(),
            sidebar_width: 430.0,
            scale_mode: ScaleMode::default(),
            texture_filter: TextureFilter::default(),
            toolbar_hidden: false,
            framebuffer_present_size_px: (320, 240),
            cpu,
            freelook: emulator_core::FreelookState::default(),
            freelook_input: FreelookInput::default(),
            bus,
            gpu_resync_generation: initial_gpu_resync_generation,
            menu: MenuState::with_running(autorun),
            hud: HudState::default(),
            profiler: ui::profiler::FrameProfiler::default(),
            memory_view: MemoryView::default(),
            running: autorun,
            run_steps_per_frame: 1_000_000,
            exec_history: VecDeque::with_capacity(EXEC_HISTORY_CAP),
            breakpoints: BTreeSet::new(),
            gpr_snapshot: None,
            settings,
            library,
            paths,
            current_game: None,
            status_message: None,
            audio_volume: 1.0,
            audio_muted: false,
            #[cfg(not(target_arch = "wasm32"))]
            pending_savestate_thumbnails: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            bios_bytes: None,
            #[cfg(target_arch = "wasm32")]
            web_games: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            web_quick_save: None,
            #[cfg(target_arch = "wasm32")]
            web_track_patches: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            web_boot: None,
            current_game_hash: None,
        };
        // Startup auto-rescan: always run when a developer-facing build dir
        // exists so stale `library.ron` entries (e.g. cargo
        // `deps/<name>-<hash>.exe` intermediates picked up by an
        // earlier version of the scanner before the deps/ filter
        // landed) get purged. `scan_roots` is mtime-cached for
        // already-seen files, so the cost is bounded by
        // "number of files that changed since last scan" -- cheap
        // on every boot.
        //
        // Scoped to "SDK/project dirs exist" so an end-user install
        // without local builds doesn't pay the cost every startup.
        let sdk_exists = out
            .resolve_sdk_examples_dir()
            .is_some_and(|sdk_dir| sdk_dir.exists());
        let projects_exist = out
            .resolve_editor_projects_dir()
            .is_some_and(|projects_dir| projects_dir.exists());
        if sdk_exists || projects_exist {
            if let Err(e) = out.rescan_library() {
                eprintln!("[frontend] startup auto-rescan skipped: {e}");
            }
        }
        // Seed the Menu's Games + Examples columns from the (now
        // possibly-rescanned) library so the user sees entries
        // immediately instead of a "No games found" placeholder.
        out.refresh_menu_library();
        out.menu
            .sync_fast_boot_label(out.settings.emulator.fast_boot_disc);
        out.menu
            .set_menu_opacity(out.settings.video.menu_opacity_pct);
        #[cfg(feature = "editor")]
        out.menu.sync_editor_label(out.workspace.is_editor());
        out.sync_menu_settings_paths();
        out.sync_menu_controls();
        // Dev/preview hook in the PSOXIDE_AUTORUN tradition: open the
        // controls panel straight away, so visual tweaks to the pad
        // drawing can be screenshotted without clicking through the UI.
        #[cfg(not(target_arch = "wasm32"))]
        if env_flag("PSOXIDE_OPEN_CONTROLS") {
            out.menu.open_controls();
        }
        // Web: look (async) for a previously-saved BIOS/folder so the menu can
        // offer a one-click reconnect.
        #[cfg(target_arch = "wasm32")]
        crate::web_files::check_saved();
        // Both builds start on the open menu (bundled discs like Celeste are
        // launchable from the Games/Examples categories), rather than
        // auto-booting into a game.
        out
    }
}

impl AppState {
    /// Append guest-runtime debug output captured from the telemetry port.
    /// Without the editor there is no Play debug terminal to receive it, so
    /// the lines are dropped.
    pub fn append_guest_debug_logs(
        &mut self,
        logs: Vec<emulator_core::telemetry::GuestDebugLogLine>,
    ) {
        #[cfg(feature = "editor")]
        self.editor.append_play_debug_terminal_lines(
            logs.into_iter()
                .map(|line| format!("[f{} c{}] {}", line.frame, line.cycles, line.text)),
        );
        #[cfg(not(feature = "editor"))]
        let _ = logs;
    }

    /// Rebuild the emulator state around `entry`. Same flow the
    /// headless `launch` CLI runs: mount the disc or legacy EXE,
    /// plug a pad into port 1, and use a real BIOS only for retail
    /// disc paths. On success the emulator is paused at the reset
    /// vector (or the legacy EXE entry point); the user clicks Run
    /// to start stepping.
    /// Boot a homebrew disc image from raw bytes via the no-BIOS HLE path (the
    /// same one embedded Play uses for PSoXide-authored discs). Used by the web
    /// build to auto-boot a bundled disc, and later a user-supplied one.
    pub fn boot_disc_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() < SECTOR_BYTES {
            return Err("disc image too small to be valid".to_string());
        }
        self.boot_disc(Disc::from_bin(bytes))
    }

    /// Boot an already-modelled disc via the same no-BIOS HLE path. Split out
    /// of [`Self::boot_disc_bytes`] so the web build's streamed CUE+BIN discs
    /// (multi-track, CD-DA) reach the identical boot sequence.
    pub fn boot_disc(&mut self, disc: Disc) -> Result<(), String> {
        let mut bus = Bus::new_without_bios();
        let mut cpu = Cpu::new();
        fast_boot_disc_with_hle(&mut bus, &mut cpu, &disc, true)
            .map_err(|e| format!("boot disc: {e:?}"))?;
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();
        self.bus = Some(bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        self.cpu = cpu;
        self.running = true;
        self.menu.open = false;
        self.menu.sync_run_label(true);
        Ok(())
    }

    pub fn launch_entry(&mut self, entry: &LibraryEntry) -> Result<(), String> {
        // One tape belongs to one bootable disc identity. Finish the outgoing
        // recording before replacing Bus/current_game so a file can never
        // silently contain frames from two executables.
        self.stop_input_recording_if_active();

        // Flush the outgoing game's memcard before we discard its
        // Bus state. Silently log on failure -- we'd rather launch
        // the new game than refuse because of a stale save.
        if let Err(e) = self.flush_memcard_port1() {
            eprintln!("[frontend] memcard flush before launch: {e}");
        }
        let mut cpu = Cpu::new();
        let mut boot_mode = "EXE";
        // Image hash for input-tape change detection, computed where the
        // bytes are already in hand so no path re-reads the file.
        let game_hash;

        let bus = match entry.kind {
            GameKind::Exe => {
                let mut bus = Bus::new_without_bios();
                let bytes = std::fs::read(&entry.path)
                    .map_err(|e| format!("{}: {e}", entry.path.display()))?;
                game_hash = Some(emulator_core::game_image_hash(&bytes));
                let exe = Exe::parse(&bytes).map_err(|e| format!("parse EXE: {e:?}"))?;
                bus.load_exe_payload(exe.load_addr, &exe.payload);
                bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
                cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());
                // HLE BIOS is effectively mandatory for side-loaded
                // EXEs: the kernel's syscall tables (A0 / B0 / C0)
                // + cold-init state aren't populated when we jump
                // straight to the EXE entry instead of the reset vector.
                // A zero-filled synthetic BIOS is enough because CPU
                // execution starts in the homebrew payload and BIOS
                // table calls are intercepted by HLE dispatch.
                bus.enable_hle_bios();
                bus.attach_digital_pad_port1();
                if let Some(disc) = load_sidecar_disc_for_exe(&entry.path)? {
                    bus.cdrom.insert_disc(Some(disc));
                }
                bus
            }
            GameKind::DiscBin | GameKind::DiscIso => {
                let mut bus = bus_from_configured_bios(&self.settings)?;
                let bytes = std::fs::read(&entry.path)
                    .map_err(|e| format!("{}: {e}", entry.path.display()))?;
                if bytes.len() < SECTOR_BYTES {
                    return Err(format!(
                        "{} is too small to be a valid disc image",
                        entry.path.display()
                    ));
                }
                game_hash = Some(emulator_core::game_image_hash(&bytes));
                let disc = Disc::from_bin(bytes);
                boot_mode = maybe_fast_boot_disc(
                    &mut bus,
                    &mut cpu,
                    &disc,
                    entry,
                    self.settings.emulator.fast_boot_disc,
                );
                bus.cdrom.insert_disc(Some(disc));
                bus.attach_digital_pad_port1();
                // Load + attach the per-game memory card on port 1.
                // File lives under `<config>/games/<id>/memcard-1.mcd`;
                // first launch of any game gets a fresh 128 KiB blank.
                self.paths
                    .ensure_game_tree(&entry.id)
                    .map_err(|e| e.to_string())?;
                let mc_path = self.paths.memcard_file(&entry.id, 1);
                let mc_bytes = std::fs::read(&mc_path).unwrap_or_default();
                bus.attach_memcard_port1(mc_bytes);
                bus
            }
            GameKind::DiscCue | GameKind::DiscCcd => {
                let mut bus = bus_from_configured_bios(&self.settings)?;
                let disc = match entry.kind {
                    GameKind::DiscCue => psoxide_settings::library::load_disc_from_cue(&entry.path),
                    GameKind::DiscCcd => psoxide_settings::library::load_disc_from_ccd(&entry.path),
                    _ => unreachable!(),
                }?;
                game_hash = Some(disc_image_hash(&disc));
                boot_mode = maybe_fast_boot_disc(
                    &mut bus,
                    &mut cpu,
                    &disc,
                    entry,
                    self.settings.emulator.fast_boot_disc,
                );
                bus.cdrom.insert_disc(Some(disc));
                bus.attach_digital_pad_port1();
                self.paths
                    .ensure_game_tree(&entry.id)
                    .map_err(|e| e.to_string())?;
                let mc_path = self.paths.memcard_file(&entry.id, 1);
                let mc_bytes = std::fs::read(&mc_path).unwrap_or_default();
                bus.attach_memcard_port1(mc_bytes);
                bus
            }
            GameKind::Unknown => {
                return Err(format!(
                    "unsupported game kind for {}",
                    entry.path.display()
                ));
            }
        };

        // Swap everything at once -- no half-loaded state. Start in
        // the running state so the user sees the game boot
        // immediately when they hit Enter in the Menu -- matches a real
        // PS1 where selecting a disc and pressing X fires it right up.
        // The Menu's caller (`apply_menu_action::LaunchGame`) closes
        // the overlay so the game is actually visible.
        self.bus = Some(bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        self.cpu = cpu;
        self.running = true;
        self.workspace = Workspace::Emulator;
        self.exec_history.clear();
        self.gpr_snapshot = None;
        self.current_game = Some(entry.clone());
        self.current_game_hash = game_hash;
        self.refresh_save_state_menu_rows();
        self.menu.sync_run_label(true);
        #[cfg(feature = "editor")]
        self.menu.sync_editor_label(false);
        self.status_message = Some((
            format!("Launched: {} ({boot_mode})", entry.title),
            STATUS_MESSAGE_TTL_SECS,
        ));
        Ok(())
    }

    /// Push a new native save state for the running game to
    /// `<config>/games/<id>/savestates/slot{N}.psx`, where `N` is one
    /// past whatever the highest existing slot is (0 if this is the
    /// first save). Saves are a history, not fixed named slots -- this
    /// never overwrites a previous save; use [`AppState::load_state`]
    /// to pick a specific past one or [`AppState::load_latest_state`]
    /// for "undo to my last save." No-op (with a status toast) if no
    /// game is currently running -- there's nothing meaningful to key
    /// the save off of. The web build instead replaces one per-game
    /// quick-save in origin-scoped IndexedDB.
    #[cfg(target_arch = "wasm32")]
    pub fn save_state(&mut self) {
        let Some(game_id) = self.current_game.as_ref().map(|game| game.id.clone()) else {
            self.status_message_set("Save state: no game running");
            return;
        };
        let Some(bus) = self.bus.as_ref() else {
            self.status_message_set("Save state: no game running");
            return;
        };
        let tick = self.cpu.tick();
        let created_at = unix_now_secs();
        let state = SaveStateV1::new_at(
            EmulatorStateRef {
                cpu: &self.cpu,
                bus,
            },
            game_id.clone(),
            tick,
            created_at,
        );
        let bytes = match state.to_bytes() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.status_message_set(format!("Save state failed: {error}"));
                return;
            }
        };
        crate::web_files::save_quick_state(game_id, bytes, created_at, tick);
        self.status_message_set("Saving browser quick-save...");
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_state(&mut self) {
        let Some(game_id) = self.current_game.as_ref().map(|g| g.id.clone()) else {
            self.status_message_set("Save state: no game running");
            return;
        };
        if self.bus.is_none() {
            self.status_message_set("Save state: no game running");
            return;
        }
        if let Err(e) = self.paths.ensure_game_tree(&game_id) {
            self.status_message_set(format!("Save state failed: {e}"));
            return;
        }
        let existing = self.paths.list_savestate_slots(&game_id);
        let Some(slot) = existing.last().map_or(Some(0u8), |&max| max.checked_add(1)) else {
            self.status_message_set(
                "Save state failed: 256 saves for this game already, delete some first".to_string(),
            );
            return;
        };
        let tick = self.cpu.tick();
        let cpu = &self.cpu;
        let bus = self.bus.as_ref().expect("checked above");
        let path = self.paths.savestate_file(&game_id, slot);
        let thumb_path = self.paths.savestate_thumbnail_file(&game_id, slot);
        // The derive-generated (de)serialize walk over the emulator's
        // nested state graph (Bus -> Gpu/Spu/CdRom/... -> further
        // nested structs) adds enough stack depth that, stacked on top
        // of wherever winit/egui/wgpu's own call chain happens to be
        // when a key is pressed, it can exceed Windows' 1 MiB default
        // main-thread stack (confirmed: the same encode/decode works
        // fine standalone, only overflows when invoked from inside the
        // real event-handling call stack). Run it on a dedicated thread
        // with a generous stack instead of chasing the exact
        // contributing frames.
        let result = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || {
                    let snapshot = EmulatorStateRef { cpu, bus };
                    let state = SaveStateV1::new(snapshot, game_id.clone(), tick);
                    state.write_to(&path)
                })
                .expect("spawn save-state thread")
                .join()
        });
        match result {
            Ok(Ok(())) => {
                // A fresh save always becomes the new quick-load target --
                // "undo to my last save" should mean *this* save until the
                // user explicitly pins something else.
                let _ = self.paths.write_top_slot(&game_id, slot);
                // Defer readback until the UI render returns. The shell then
                // captures the persistent HW target the user actually saw,
                // rather than the independently rasterized CPU VRAM mirror.
                self.pending_savestate_thumbnails.push(thumb_path);
                self.status_message_set(format!("Saved slot {slot}"));
                self.refresh_save_state_menu_rows();
            }
            Ok(Err(e)) => self.status_message_set(format!("Save state failed: {e}")),
            Err(_) => self.status_message_set("Save state failed: internal error".to_string()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn take_pending_savestate_thumbnails(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.pending_savestate_thumbnails)
    }

    /// Load whichever save is "on top" -- the pinned quick-load target
    /// (see [`ConfigPaths::read_top_slot`]) if one is set and still
    /// exists, otherwise the most recent (highest slot number) save --
    /// "undo to my last save." Status-toasts "No save states yet"
    /// rather than erroring if none exist. `start_paused` is forwarded
    /// to [`AppState::load_state`].
    #[cfg(target_arch = "wasm32")]
    pub fn load_latest_state(&mut self, start_paused: bool) {
        let Some(game_id) = self.current_game.as_ref().map(|game| game.id.clone()) else {
            self.status_message_set("Load state: no game running");
            return;
        };
        crate::web_files::load_quick_state(game_id, start_paused);
        self.status_message_set("Loading browser quick-save...");
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_latest_state(&mut self, start_paused: bool) {
        let Some(game_id) = self.current_game.as_ref().map(|g| g.id.clone()) else {
            self.status_message_set("Load state: no game running");
            return;
        };
        let slot = self
            .paths
            .read_top_slot(&game_id)
            .or_else(|| self.paths.list_savestate_slots(&game_id).last().copied());
        match slot {
            Some(slot) => self.load_state(slot, start_paused),
            None => self.status_message_set("No save states yet"),
        }
    }

    /// Pin `slot` as the save history's "top" -- what
    /// [`AppState::load_latest_state`] (and F7) target next -- without
    /// touching slot numbering or any other save's position in the
    /// chronological list. No-op (status toast) if the slot doesn't
    /// exist, e.g. it was deleted out from under the menu.
    #[cfg(target_arch = "wasm32")]
    pub fn pin_save_state_as_top(&mut self, _slot: u8) {
        self.status_message_set("The browser quick-save is already the F7 target");
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn pin_save_state_as_top(&mut self, slot: u8) {
        let Some(game_id) = self.current_game.as_ref().map(|g| g.id.clone()) else {
            return;
        };
        if !self.paths.list_savestate_slots(&game_id).contains(&slot) {
            self.status_message_set(format!("Pin failed: slot {slot} no longer exists"));
            return;
        }
        match self.paths.write_top_slot(&game_id, slot) {
            Ok(()) => {
                self.status_message_set(format!("Slot {slot} pinned as quick-load target"));
                self.refresh_save_state_menu_rows();
            }
            Err(e) => self.status_message_set(format!("Pin failed: {e}")),
        }
    }

    /// Load `<config>/games/<id>/savestates/slot{slot}.psx` back into
    /// the running emulator.
    ///
    /// The save file deliberately omits the BIOS image and the
    /// mounted disc (see [`Bus::restore_excluded_from`]) to keep save
    /// files small, so this re-runs the normal boot path for the
    /// current game first -- purely to obtain a correctly-configured
    /// donor `Bus` (right BIOS, right disc, right memory card) -- and
    /// then splices the restored CPU/Bus register state on top of it.
    ///
    /// `start_paused` leaves the emulator paused on the restored frame
    /// instead of immediately resuming -- the sensible default when a
    /// human just asked to jump back in time and probably wants to
    /// look around before continuing.
    #[cfg(target_arch = "wasm32")]
    pub fn load_state(&mut self, _slot: u8, start_paused: bool) {
        self.load_latest_state(start_paused);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_state(&mut self, slot: u8, start_paused: bool) {
        let Some(game) = self.current_game.clone() else {
            self.status_message_set("Load state: no game running");
            return;
        };
        let path = self.paths.savestate_file(&game.id, slot);
        // See the comment in `save_state`: decoding the full state
        // graph back into owned `Cpu`/`Bus` values overflows Windows'
        // default 1 MiB main-thread stack when invoked from deep
        // inside the real winit/egui/wgpu event-handling call chain
        // (reproduced: the identical on-disk save decodes fine
        // standalone, only crashes from inside the running GUI). Same
        // fix: run it on a dedicated large-stack thread.
        let load_result = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(64 * 1024 * 1024)
                .spawn_scoped(scope, || SaveStateV1::<EmulatorState>::read_from(&path))
                .expect("spawn save-state thread")
                .join()
        });
        let loaded = match load_result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                self.status_message_set(format!("Load state failed: {e}"));
                return;
            }
            Err(_) => {
                self.status_message_set("Load state failed: internal error".to_string());
                return;
            }
        };
        if loaded.header.game_id != game.id {
            self.status_message_set("Load state failed: save is from a different game".to_string());
            return;
        }
        if let Err(e) = self.launch_entry(&game) {
            self.status_message_set(format!("Load state failed: could not remount game: {e}"));
            return;
        }
        let mut payload = loaded.payload;
        if let Some(fresh_bus) = self.bus.as_mut() {
            payload.bus.restore_excluded_from(fresh_bus);
        }
        let mc_bytes = std::fs::read(self.paths.memcard_file(&game.id, 1)).unwrap_or_default();
        payload.bus.attach_memcard_port1(mc_bytes);
        self.cpu = payload.cpu;
        self.bus = Some(payload.bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        if start_paused {
            self.running = false;
            self.menu.sync_run_label(false);
        }
        self.status_message_set(format!("Loaded slot {slot}"));
        self.refresh_save_state_menu_rows();
    }

    /// Rebuild the System menu's save-state rows from whatever's
    /// actually on disk for the running game. Call after anything
    /// that could change the set of saves (a save, a load, launching
    /// a different game) -- cheap (one directory listing plus a
    /// header-only peek per file, no full-state decode) so there's no
    /// need to cache this across frames.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn refresh_save_state_menu_rows(&mut self) {
        let rows = match self.current_game.as_ref().map(|g| g.id.clone()) {
            Some(game_id) => self.build_save_state_rows(&game_id),
            None => Vec::new(),
        };
        self.menu.sync_save_states(self.running, &rows);
    }

    #[cfg(target_arch = "wasm32")]
    fn refresh_save_state_menu_rows(&mut self) {
        let rows = self
            .web_quick_save
            .as_ref()
            .filter(|(game_id, _, _)| {
                self.current_game
                    .as_ref()
                    .is_some_and(|game| game.id == *game_id)
            })
            .map(|(_, created_at, cpu_tick)| {
                vec![SaveStateRow {
                    slot: 0,
                    label: format!("{} -- tick {}", format_relative_time(*created_at), cpu_tick),
                    thumbnail_path: None,
                    is_top: true,
                }]
            })
            .unwrap_or_default();
        self.menu.sync_save_states(self.running, &rows);
    }

    /// Newest-first `SaveStateRow`s for `game_id`, each labeled with a
    /// relative save time and the CPU tick it was taken at. Slots
    /// whose header can't be read (corrupt/truncated file) are
    /// silently dropped from the list rather than breaking the whole
    /// menu -- the file still exists on disk if someone wants to poke
    /// at it manually.
    #[cfg(not(target_arch = "wasm32"))]
    fn build_save_state_rows(&self, game_id: &str) -> Vec<SaveStateRow> {
        let mut slots = self.paths.list_savestate_slots(game_id);
        slots.reverse();
        // Fall back to "highest slot" for the top marker exactly the
        // same way `load_latest_state` does, so the row the panel
        // highlights as "Top (F7)" always agrees with what F7 will
        // actually load.
        let top = self
            .paths
            .read_top_slot(game_id)
            .or_else(|| slots.first().copied());
        slots
            .into_iter()
            .filter_map(|slot| {
                let path = self.paths.savestate_file(game_id, slot);
                let header = peek_header(&path).ok()?;
                let thumbnail = self.paths.savestate_thumbnail_file(game_id, slot);
                Some(SaveStateRow {
                    slot,
                    label: format!(
                        "{} -- tick {}",
                        format_relative_time(header.created_at),
                        header.cpu_tick
                    ),
                    thumbnail_path: thumbnail.exists().then_some(thumbnail),
                    is_top: Some(slot) == top,
                })
            })
            .collect()
    }

    /// Convenience: look up an entry by menu launch token and launch
    /// it. Most tokens are stable library IDs; project builds use
    /// path-qualified tokens because authored PSoXide discs can still
    /// share a PSX volume ID.
    pub fn launch_by_id(&mut self, id: &str) -> Result<(), String> {
        // Baked-in payloads boot via the no-BIOS HLE path, not the library.
        // Both targets: the download has no source tree, so its Examples come
        // from here too.
        if let Some(disc) = bundled::find(id) {
            let kind = match disc.kind {
                bundled::BundledKind::DiscBin => {
                    self.boot_disc_bytes(disc.bytes.to_vec())?;
                    GameKind::DiscBin
                }
                bundled::BundledKind::Exe => {
                    self.boot_exe_bytes(disc.bytes.to_vec())?;
                    GameKind::Exe
                }
            };
            // Web-only bookkeeping for the "now playing" header; the native
            // build tracks the current entry through the library instead.
            #[cfg(target_arch = "wasm32")]
            self.set_web_current_game(
                disc.id.to_string(),
                disc.title.to_string(),
                kind,
                disc.bytes.len() as u64,
            );
            #[cfg(not(target_arch = "wasm32"))]
            let _ = kind;
            return Ok(());
        }
        // Web streamed discs: kick off the fetch; boot happens on a later
        // frame via `poll_web_uploads` once the bytes are in.
        #[cfg(target_arch = "wasm32")]
        if let Some(disc) = crate::web_stream::find(id) {
            crate::web_stream::start(disc);
            self.status_message_set(format!("Downloading {}...", disc.title));
            return Ok(());
        }
        // Web folder-scanned games: read the file's bytes asynchronously, then
        // boot on a later frame via `poll_web_uploads`.
        #[cfg(target_arch = "wasm32")]
        if id.starts_with("web:") {
            crate::web_files::read_game(id);
            self.status_message_set("Loading game...");
            return Ok(());
        }
        let Some(entry) = library_entry_for_launch_id(&self.library.entries, id).cloned() else {
            return Err(format!("no library entry with id={id}"));
        };
        self.launch_entry(&entry)
    }

    /// Give a browser-loaded game the same stable identity native library
    /// entries provide. Save states use this id as their IndexedDB key.
    #[cfg(target_arch = "wasm32")]
    fn set_web_current_game(&mut self, id: String, title: String, kind: GameKind, size: u64) {
        self.current_game = Some(LibraryEntry {
            path: PathBuf::from(&id),
            id: id.clone(),
            kind,
            title,
            region: Region::Unknown,
            size,
            mtime: 0,
            diagnostic: None,
        });
        self.web_quick_save = None;
        self.refresh_save_state_menu_rows();
        crate::web_files::inspect_quick_state(id);
    }

    /// Open the burn settings submenu for a launchable library entry.
    pub fn open_burn_menu_by_id(&mut self, id: &str) -> Result<(), String> {
        let Some(entry) = library_entry_for_launch_id(&self.library.entries, id).cloned() else {
            return Err(format!("no library entry with id={id}"));
        };
        if !self.entry_can_open_burn_menu(&entry) {
            return Err(
                "disc burning is only available for built examples and projects".to_string(),
            );
        }
        validate_burn_target_path(&entry.path)?;
        self.burn.open_for(&entry);
        match self.burn.scan_now() {
            Ok(Some(notice)) => self.status_message_set(notice),
            Ok(None) => {}
            Err(error) => self.status_message_set(format!("Burner scan failed: {error}")),
        }
        Ok(())
    }

    fn entry_can_open_burn_menu(&self, entry: &LibraryEntry) -> bool {
        if entry.kind != GameKind::DiscCue {
            return false;
        }
        let in_examples = self
            .resolve_sdk_examples_dir()
            .filter(|root| root.exists())
            .is_some_and(|root| path_is_under(&entry.path, &root));
        let in_projects = self
            .resolve_editor_projects_dir()
            .filter(|root| root.exists())
            .is_some_and(|root| path_is_under(&entry.path, &root));
        in_examples || in_projects
    }

    /// Poll CD burner hotplug in the same lightweight style as controller notices.
    pub fn poll_burner_hotplug(&mut self) {
        match self.burn.tick() {
            Ok(Some(notice)) => self.status_message_set(notice),
            Ok(None) => {}
            Err(error) => {
                self.burn.status = format!("Burner scan failed: {error}");
            }
        }
    }

    /// Walk the configured library root(s) and update the cache.
    /// Scans roots in one pass:
    ///
    /// 1. `settings.paths.game_library` -- user's retail-disc folder.
    /// 2. `settings.paths.sdk_examples` (or auto-detected
    ///    `build/examples/mipsel-sony-psx/release/` under the repo
    ///    root) -- `.cue` homebrew discs built by `make examples`.
    /// 3. Auto-detected `editor/projects/` under the repo root --
    ///    project-baked disc images surfaced in the Projects category.
    ///
    /// Either can be missing without erroring. If neither yields
    /// entries, the Menu's columns show the "No … found" placeholder
    /// instead of blowing up.
    ///
    /// Also refreshes the Menu's Games + Examples columns so the
    /// newly-scanned entries appear immediately.
    pub fn rescan_library(&mut self) -> Result<usize, String> {
        let game_library = self.settings.paths.game_library.trim();
        let game_root = if game_library.is_empty() {
            None
        } else {
            Some(PathBuf::from(game_library))
        };
        let sdk_root = self.resolve_sdk_examples_dir();
        let projects_root = self.resolve_editor_projects_dir();

        // No roots → still not an error; the UI shows empty columns.
        // Matches the "fresh clone, user hasn't set a library yet"
        // state rather than punishing it with a dialog.
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(g) = game_root.clone() {
            if g.exists() {
                roots.push(g);
            } else {
                return Err(format!("Library root does not exist: {}", g.display()));
            }
        }
        if let Some(s) = sdk_root.clone() {
            // sdk_root from auto-detect may not exist (e.g. on an
            // end-user install that never built the examples); that
            // doesn't deserve an error. `scan_roots` silently skips
            // missing roots for exactly this reason.
            roots.push(s);
        }
        if let Some(p) = projects_root.clone() {
            roots.push(p);
        }

        let root_refs: Vec<&std::path::Path> = roots.iter().map(|p| p.as_path()).collect();
        let changed = self
            .library
            .scan_roots(&root_refs)
            .map_err(|e| format!("scan failed: {e}"))?;
        self.library
            .save(&self.paths.library_file())
            .map_err(|e| format!("save library.ron: {e}"))?;
        self.refresh_menu_library();
        let sdk_hint = match &sdk_root {
            Some(p) if p.exists() => format!(" (SDK: {})", p.display()),
            _ => String::new(),
        };
        self.status_message = Some((
            format!(
                "Scan complete: {} entries{sdk_hint}",
                self.library.entries.len()
            ),
            STATUS_MESSAGE_TTL_SECS,
        ));
        Ok(changed)
    }

    /// Build the public SDK/engine examples in the background so the
    /// Examples menu can populate a fresh clone without blocking UI
    /// frames. Completion is handled by [`Self::poll_examples_build`].
    pub fn start_examples_build(&mut self) {
        if self.finish_completed_examples_build() {
            return;
        }
        if self.examples_build_child.is_some() {
            self.status_message_set("Examples build still running");
            return;
        }

        let workspace_root = repo_root_dir();
        let mut command = Command::new("make");
        command
            .arg("examples")
            .current_dir(&workspace_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match command.spawn() {
            Ok(child) => {
                self.examples_build_child = Some(child);
                self.status_message_set("Building public examples");
            }
            Err(error) => {
                let message = format!("Build examples failed to start: {error}");
                eprintln!("[frontend] {message}");
                self.status_message_set(message);
            }
        }
    }

    /// Poll a background examples build. On success, rescan the
    /// library so the newly-created CUE/BIN discs appear immediately.
    pub fn poll_examples_build(&mut self) {
        self.finish_completed_examples_build();
    }

    fn finish_completed_examples_build(&mut self) -> bool {
        let Some(child) = self.examples_build_child.as_mut() else {
            return false;
        };
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return false,
            Err(error) => {
                self.examples_build_child = None;
                let message = format!("Examples build poll failed: {error}");
                eprintln!("[frontend] {message}");
                self.status_message_set(message);
                return true;
            }
        };

        self.examples_build_child = None;
        self.finish_examples_build(status);
        true
    }

    fn finish_examples_build(&mut self, status: ExitStatus) {
        if !status.success() {
            let message = format!("Examples build failed: {status}");
            eprintln!("[frontend] {message}; run `make examples` for full logs");
            self.status_message_set(message);
            return;
        }

        match self.rescan_library() {
            Ok(_) => self.status_message_set("Examples built and library refreshed"),
            Err(error) => {
                let message = format!("Examples built; refresh failed: {error}");
                eprintln!("[frontend] {message}");
                self.status_message_set(message);
            }
        }
    }

    pub fn stop_examples_build(&mut self) {
        if let Some(mut child) = self.examples_build_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Resolve where to look for SDK-built example `.exe`s. Honours
    /// the explicit `settings.paths.sdk_examples` if the user set
    /// one; otherwise walks up from the frontend crate's source
    /// directory (`CARGO_MANIFEST_DIR`) to the repo root and joins
    /// the canonical build-output path. Returns `None` when the
    /// resolver can't place the repo root -- in which case scanning
    /// proceeds with only the game-library root.
    fn resolve_sdk_examples_dir(&self) -> Option<PathBuf> {
        if !self.settings.paths.sdk_examples.is_empty() {
            return Some(PathBuf::from(&self.settings.paths.sdk_examples));
        }
        // `emu/crates/frontend/` → four `..`s land at the repo root.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent()?.parent()?.parent()?;
        let candidate = repo_root.join("build/examples/mipsel-sony-psx/release");
        Some(candidate)
    }

    /// Resolve the editor projects root used for launchable project
    /// builds. The editor owns the project folders; the frontend only
    /// scans them for disc-image outputs so baked builds can be launched
    /// without opening the editor first.
    fn resolve_editor_projects_dir(&self) -> Option<PathBuf> {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.parent()?.parent()?.parent()?;
        Some(repo_root.join("editor/projects"))
    }

    /// Project the current library into the Menu's Games + Examples +
    /// Projects columns. Three passes:
    ///
    /// 1. Walk every CUE entry and parse it to find its primary
    ///    (data-track) BIN. Build a map
    ///    `absolute_bin_path → (cue_title, cue_id)` so each BIN
    ///    the CUE owns shows up with the CUE's friendly filename
    ///    as its title (e.g. the disc's own name instead of
    ///    the raw PVD ID "SCUS-94900"), and under the CUE's stable
    ///    game ID so savestates key off the disc identity rather
    ///    than the BIN byte hash alone.
    /// 2. Walk every entry. SDK examples and project builds launch
    ///    from their CUEs; their EXE/BIN siblings are intermediates.
    ///    Retail BIN entries still use a CUE title/id when one owns
    ///    them, and retail CUE entries remain hidden from Games.
    /// 3. Alphabetise each column.
    ///
    /// Result: the title shows once, under its friendly
    /// title, and clicking it launches the BIN.
    pub fn refresh_menu_library(&mut self) {
        use std::collections::{HashMap, HashSet};

        let sdk_examples_root = self.resolve_sdk_examples_dir().filter(|p| p.exists());

        // Pass 1: map "BIN path" → (CUE-derived title, CUE id).
        let mut cue_owns_bin: HashMap<PathBuf, (String, String)> = HashMap::new();
        for e in &self.library.entries {
            if e.kind != GameKind::DiscCue {
                continue;
            }
            if let Some(bin) = psoxide_settings::library::primary_bin_from_cue(&e.path) {
                cue_owns_bin.insert(bin, (e.title.clone(), e.id.clone()));
            }
        }

        // Pass 2: project menu entries, applying dedup + title overrides.
        let mut games: Vec<MenuLibraryItem> = Vec::new();
        let mut examples: Vec<MenuLibraryItem> = Vec::new();
        let mut projects: Vec<MenuLibraryItem> = Vec::new();
        let mut cue_already_listed: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let project_root = self.resolve_editor_projects_dir().filter(|p| p.exists());

        for e in &self.library.entries {
            let label = e
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("<unknown>");
            let is_sdk_example = sdk_examples_root
                .as_ref()
                .is_some_and(|root| path_is_under(&e.path, root));

            // Audio tracks: any "(Track N)" filename where N != 1.
            // Multi-track CUE rips leave each audio track as a
            // standalone BIN; none of those boot, so hide them.
            if label.contains("(Track ")
                && !label.contains("(Track 01)")
                && !label.contains("(Track 1)")
            {
                continue;
            }
            if is_sdk_example {
                match e.kind {
                    GameKind::DiscCue => examples.push(MenuLibraryItem {
                        id: path_launch_id(&e.path),
                        title: e.title.clone(),
                        subtitle: format_subtitle(e),
                        burnable: true,
                        launchable: true,
                    }),
                    GameKind::DiscBin | GameKind::DiscIso | GameKind::DiscCcd | GameKind::Exe => {
                        continue;
                    }
                    GameKind::Unknown => {}
                }
                continue;
            }

            match e.kind {
                _ if is_internal_example_artifact(&e.path) => continue,
                GameKind::DiscCue
                    if project_root
                        .as_ref()
                        .is_some_and(|root| path_is_under(&e.path, root)) =>
                {
                    // Project-baked discs are surfaced from their project.ron
                    // metadata, which lives in the editor crates. Without the
                    // editor feature there is no project metadata to read, so
                    // project CUEs are skipped.
                    #[cfg(feature = "editor")]
                    {
                        let root = project_root.as_ref().expect("checked above");
                        if let Some(metadata) = project_build_menu_metadata(&e.path, root) {
                            if !metadata.current {
                                continue;
                            }
                            projects.push(MenuLibraryItem {
                                id: project_build_launch_id(&e.path),
                                title: metadata.title,
                                subtitle: metadata.subtitle,
                                burnable: true,
                                launchable: true,
                            });
                        }
                    }
                }
                // Retail CUEs are not shown directly -- their BIN is.
                // CCDs are shown directly because their `.img`
                // sidecar is not a separate library entry.
                GameKind::DiscCue => continue,
                GameKind::DiscBin | GameKind::DiscIso | GameKind::DiscCcd => {
                    if project_root
                        .as_ref()
                        .is_some_and(|root| path_is_under(&e.path, root))
                    {
                        continue;
                    }
                    // If a CUE owns this BIN, use the CUE's
                    // friendly title + stable ID. Also dedup: the
                    // *first* BIN of a CUE wins; subsequent BINs
                    // (multi-disc sets not yet modelled) are
                    // hidden to keep the list clean.
                    let (title, id) = if let Some((cue_title, cue_id)) = cue_owns_bin.get(&e.path) {
                        if !cue_already_listed.insert(cue_id.clone()) {
                            continue;
                        }
                        (cue_title.clone(), cue_id.clone())
                    } else {
                        (e.title.clone(), e.id.clone())
                    };
                    games.push(MenuLibraryItem {
                        id,
                        title,
                        subtitle: format_subtitle(e),
                        burnable: false,
                        launchable: true,
                    });
                }
                GameKind::Exe => {
                    if project_root
                        .as_ref()
                        .is_some_and(|root| path_is_under(&e.path, root))
                    {
                        continue;
                    } else {
                        examples.push(MenuLibraryItem {
                            id: e.id.clone(),
                            title: e.title.clone(),
                            subtitle: format_subtitle(e),
                            burnable: false,
                            launchable: true,
                        });
                    }
                }
                GameKind::Unknown => {}
            }
        }

        let built_examples: HashSet<String> = examples
            .iter()
            .map(|entry| example_key(&entry.title))
            .collect();
        examples.extend(public_example_source_items(&built_examples));

        merge_baked_examples(&mut examples, &built_examples);

        // Pass 3: stable alphabetical order per column.
        games.sort_by_key(|a| a.title.to_lowercase());
        examples.sort_by_key(|a| a.title.to_lowercase());
        projects.sort_by_key(|a| a.title.to_lowercase());
        // Web build: surface the baked-in payloads. A disc image is a shipped
        // game and belongs under Games; a baked EXE is an SDK/engine sample and
        // belongs under Examples, next to the ones a source tree would list.
        // Baked disc images are shipped games; the EXEs were folded into
        // Examples above, for both targets.
        #[cfg(target_arch = "wasm32")]
        for disc in bundled::DISCS {
            if disc.kind != bundled::BundledKind::DiscBin {
                continue;
            }
            games.push(MenuLibraryItem {
                id: disc.id.to_string(),
                title: disc.title.to_string(),
                subtitle: disc.subtitle.to_string(),
                burnable: false,
                launchable: true,
            });
        }
        // Streamed discs sit beside the baked ones; the subtitle says the
        // download out loud so the click is informed.
        #[cfg(target_arch = "wasm32")]
        for disc in crate::web_stream::DISCS {
            games.push(MenuLibraryItem {
                id: disc.id.to_string(),
                title: disc.title.to_string(),
                subtitle: disc.subtitle.to_string(),
                burnable: false,
                launchable: true,
            });
        }
        #[cfg(target_arch = "wasm32")]
        for (id, title, subtitle) in &self.web_games {
            games.push(MenuLibraryItem {
                id: id.clone(),
                title: title.clone(),
                subtitle: subtitle.clone(),
                burnable: false,
                launchable: true,
            });
        }
        self.menu.set_library(&games, &examples, &projects);
    }

    /// Persist the current `Settings` to `settings.ron`. Called
    /// when a settings-panel control commits a change.
    pub fn save_settings(&self) -> Result<(), String> {
        self.settings
            .save(&self.paths.settings_file())
            .map_err(|e| format!("save settings.ron: {e}"))
    }

    /// True when the user game-library path is blank.
    pub fn games_path_missing(&self) -> bool {
        self.settings.paths.game_library.trim().is_empty()
    }

    /// Warning banner to show at the top of the Menu, if any.
    pub fn menu_setup_warning(&self) -> Option<&'static str> {
        // Web: a gentle onboarding hint until the user loads their own content
        // (the bundled discs play with no setup). Native: prompt to point the
        // library at a folder when none is configured yet.
        #[cfg(target_arch = "wasm32")]
        {
            let (has_bios, has_games) = (self.bios_bytes.is_some(), !self.web_games.is_empty());
            if has_bios && has_games {
                return None;
            }
            // A saved BIOS/folder from a previous visit that the browser won't
            // re-open without a user gesture: offer one-click Reconnect.
            if crate::web_files::saved_available() {
                return Some(
                    "Saved BIOS / games found - your browser needs one click: Reconnect in Settings",
                );
            }
            // Otherwise prompt for whatever's missing (bundled discs still play
            // without either).
            return match (has_bios, has_games) {
                (true, true) => None,
                (false, false) => {
                    Some("Load a BIOS and a games folder you legally own from Settings")
                }
                (false, true) => Some("Load a BIOS you legally own from Settings"),
                (true, false) => Some("Load a games folder you legally own from Settings"),
            };
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.games_path_missing() {
            Some("Set a games folder in Settings to list your disc collection")
        } else {
            None
        }
    }

    /// Move Menu selection to Settings and ensure the overlay is open.
    pub fn select_settings_category(&mut self) {
        self.menu.open = true;
        self.menu.select_category("Settings");
    }

    /// Choose and persist a BIOS image from the Menu Settings column.
    /// `rfd` (native file dialog) has no wasm backend, so the web build gets a
    /// stub below; disc/BIOS loading on the web is a later phase.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn choose_bios_path(&mut self) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Choose PlayStation BIOS")
            .add_filter("PlayStation BIOS", &["bin", "rom"]);
        if let Some(dir) = path_parent_or_self(self.settings.paths.bios.trim()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.pick_file() else {
            return;
        };
        self.settings.paths.bios = path.to_string_lossy().into_owned();
        match self.save_settings() {
            Ok(()) => {
                self.sync_menu_settings_paths();
                self.status_message_set(format!("BIOS path saved: {}", path_label(&path)));
            }
            Err(e) => {
                eprintln!("[frontend] {e}");
                self.status_message_set(e);
            }
        }
    }

    /// Web: pick a BIOS (persistent File System Access picker where available,
    /// else a one-shot `<input>`). Bytes land in `bios_bytes` via
    /// `poll_web_uploads` on a later frame; the chosen location is remembered.
    #[cfg(target_arch = "wasm32")]
    pub fn choose_bios_path(&mut self) {
        crate::web_files::pick_bios();
    }

    /// Web: reconnect the previously-saved BIOS + games folder (one click to
    /// re-grant; the browser won't re-read remembered files without a gesture).
    #[cfg(target_arch = "wasm32")]
    pub fn reconnect_web_files(&mut self) {
        crate::web_files::reconnect();
        self.status_message_set("Reconnecting saved BIOS / games...");
    }

    /// Pick a recorded input tape (CSV or `.pxtape`) and replay it against a
    /// fresh cold boot of the current game. Native opens a blocking file
    /// dialog and loads immediately; the web build opens the browser picker
    /// and the upload lands in `poll_web_uploads` a frame later.
    pub fn pick_input_replay(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            if self.web_boot.is_none() {
                self.status_message_set("Launch a game before loading an input replay");
                return;
            }
            crate::web_files::pick_tape();
            self.status_message_set("Pick an input recording (.csv / .pxtape)...");
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(entry) = self.current_game.clone() else {
                self.status_message_set("Launch a game before loading an input replay");
                return;
            };
            if self.playtest_input_tape.is_recording() {
                self.status_message_set("Stop input recording before loading a replay");
                return;
            }
            let mut dialog = rfd::FileDialog::new()
                .set_title("Load input replay")
                .add_filter("Input tapes", &["csv", "pxtape"]);
            // Start where F8 recordings for this game land.
            if let Some(dir) = self
                .paths
                .latest_input_tape_file(&entry.id)
                .parent()
                .filter(|dir| dir.is_dir())
            {
                dialog = dialog.set_directory(dir);
            }
            let Some(path) = dialog.pick_file() else {
                return;
            };
            self.load_native_replay(&entry, &path);
        }
    }

    /// Native: parse a tape file, relaunch the current game from disk so the
    /// tape's poll clock aligns with poll 0 of a cold boot, and start
    /// replaying. Advisory only on a game-hash mismatch: a changed build may
    /// still replay fine.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_native_replay(&mut self, entry: &LibraryEntry, path: &Path) {
        let tape = match crate::playtest_input::read_input_tape(path) {
            Ok(tape) => tape,
            Err(error) => {
                self.status_message_set(format!("Replay load failed: {error}"));
                return;
            }
        };
        let hash_note = match (tape.game_hash, self.current_game_hash) {
            (Some(tape_hash), Some(game_hash)) if tape_hash != game_hash => {
                "; note: the game changed since this tape was recorded, replay may diverge"
            }
            _ => "",
        };
        if let Err(error) = self.launch_entry(entry) {
            self.status_message_set(format!("Replay relaunch failed: {error}"));
            return;
        }
        match self.playtest_input_tape.start_replay_from_tape(tape) {
            Ok(frames) => {
                self.menu.open = false;
                self.status_message_set(format!(
                    "Game relaunched; replaying {frames} frames{hash_note}"
                ));
            }
            Err(error) => self.status_message_set(format!("Replay failed: {error}")),
        }
    }

    /// Web: parse an uploaded input tape, reboot the current game so the tape
    /// aligns with a cold boot, and start replaying it. Advisory only on a
    /// game-hash mismatch: a changed build may still replay fine.
    #[cfg(target_arch = "wasm32")]
    fn load_web_replay(&mut self, name: &str, bytes: &[u8]) {
        if self.playtest_input_tape.is_recording() {
            self.status_message_set("Stop input recording before loading a replay");
            return;
        }
        let tape = match emulator_core::tape_from_bytes(bytes) {
            Ok(tape) => tape,
            Err(error) => {
                self.status_message_set(format!("{name}: {error}"));
                return;
            }
        };
        let hash_note = match (tape.game_hash, self.current_game_hash) {
            (Some(tape_hash), Some(game_hash)) if tape_hash != game_hash => {
                "; note: the game changed since this tape was recorded, replay may diverge"
            }
            _ => "",
        };
        if let Err(error) = self.reboot_current_web_game() {
            self.status_message_set(format!("Replay unavailable: {error}"));
            return;
        }
        match self.playtest_input_tape.start_replay_from_tape(tape) {
            Ok(frames) => {
                self.running = true;
                self.menu.open = false;
                self.menu.sync_run_label(true);
                self.status_message_set(format!(
                    "Game rebooted; replaying {frames} frames{hash_note}"
                ));
            }
            Err(error) => self.status_message_set(format!("{name}: {error}")),
        }
    }

    /// Web: cold-boot the current game again from what was kept at launch.
    /// Recording and replay both start from this reboot so a tape's poll
    /// clock counts from poll 0 of a deterministic fresh machine.
    #[cfg(target_arch = "wasm32")]
    fn reboot_current_web_game(&mut self) -> Result<(), String> {
        let boot = self
            .web_boot
            .clone()
            .ok_or_else(|| "launch a game first".to_string())?;
        // The boot helpers clear `current_game` (they serve first launches);
        // a reboot keeps the game's identity, so save and restore it.
        let saved_game = self.current_game.take();
        let result = match boot {
            WebBoot::Exe(bytes) => self.boot_exe_bytes(bytes),
            WebBoot::DiscBios | WebBoot::DiscHle => {
                let disc = self
                    .bus
                    .as_mut()
                    .and_then(|bus| bus.cdrom.take_disc())
                    .ok_or_else(|| "no disc image to reboot".to_string());
                match (disc, boot) {
                    (Err(error), _) => Err(error),
                    (Ok(disc), WebBoot::DiscBios) => self.boot_disc_with_bios(disc),
                    (Ok(disc), _) => self.boot_disc(disc),
                }
            }
        };
        self.current_game = saved_game;
        result
    }

    /// Web: drain any BIOS / game files the user picked and apply them. Called
    /// once per frame from the shell (uploads complete asynchronously).
    #[cfg(target_arch = "wasm32")]
    pub fn poll_web_uploads(&mut self) {
        self.poll_streamed_disc();
        // A folder scan finished: rebuild the Games list and jump to it.
        if let Some(scanned) = crate::web_files::take_scanned() {
            let n = scanned.len();
            self.web_games = scanned;
            self.refresh_menu_library();
            self.menu.select_category("Games");
            self.status_message_set(format!("Found {n} game(s) in folder"));
        }
        for loaded in crate::web_files::drain() {
            match loaded.kind {
                crate::web_files::Upload::Bios => {
                    let kib = loaded.bytes.len() / 1024;
                    self.bios_bytes = Some(loaded.bytes);
                    self.sync_menu_settings_paths();
                    self.status_message_set(format!("BIOS loaded: {} ({kib} KiB)", loaded.name));
                }
                crate::web_files::Upload::Game => {
                    // One tape belongs to one bootable disc identity (the
                    // native launch path holds the same rule): download the
                    // outgoing recording before replacing the machine.
                    self.stop_input_recording_if_active();
                    let size = loaded.bytes.len() as u64;
                    let kind = if loaded.bytes.starts_with(b"PS-X EXE") {
                        GameKind::Exe
                    } else {
                        GameKind::DiscBin
                    };
                    let game_hash = emulator_core::game_image_hash(&loaded.bytes);
                    let game_id = loaded
                        .game_id
                        .unwrap_or_else(|| format!("web:{}", loaded.name));
                    let title = Path::new(&loaded.name)
                        .file_stem()
                        .and_then(|stem| stem.to_str())
                        .unwrap_or(&loaded.name)
                        .to_string();
                    // PS-EXE homebrew boots via HLE (no BIOS); anything else is
                    // treated as a raw disc image and needs the real BIOS.
                    let (result, boot) = if kind == GameKind::Exe {
                        let reboot_bytes = loaded.bytes.clone();
                        (
                            self.boot_exe_bytes(loaded.bytes),
                            WebBoot::Exe(reboot_bytes),
                        )
                    } else {
                        (
                            self.boot_disc_bytes_with_bios(loaded.bytes),
                            WebBoot::DiscBios,
                        )
                    };
                    match result {
                        Ok(()) => {
                            self.web_boot = Some(boot);
                            self.current_game_hash = Some(game_hash);
                            self.set_web_current_game(game_id, title, kind, size);
                            self.status_message_set(format!("Launched: {}", loaded.name));
                        }
                        Err(e) => self.status_message_set(e),
                    }
                }
                crate::web_files::Upload::Tape => {
                    self.load_web_replay(&loaded.name, &loaded.bytes);
                }
            }
        }
        for event in crate::web_files::drain_quick_states() {
            self.apply_web_quick_state_event(event);
        }
    }

    /// Advance the streamed disc: progress lines while the boot payload
    /// assembles, then model-and-boot on the data track plus first song, then
    /// patch the remaining CD-DA tracks into the mounted disc as their
    /// downloads decode. Placeholder tracks are zero-filled, which the drive
    /// plays as silence until the real sectors land -- geometry is complete
    /// from the first frame, so nothing the CD state machine sees is ever
    /// impossible on real hardware.
    #[cfg(target_arch = "wasm32")]
    fn poll_streamed_disc(&mut self) {
        use crate::web_stream::{BgEvent, BootStatus};
        match crate::web_stream::poll_boot() {
            BootStatus::Idle => {}
            BootStatus::Progress(line) => self.status_message_set(line),
            BootStatus::Ready {
                disc,
                cue,
                data,
                first_pcm,
                first_number,
                layout,
            } => {
                let total = data.len() + layout.iter().map(|(_, n)| n).sum::<usize>();
                let mut data = Some(data);
                let mut first = Some(first_pcm);
                let result = psoxide_settings::library::disc_from_cue_pieces(&cue, |n| {
                    if n == 1 {
                        data.take()
                            .ok_or_else(|| "data piece taken twice".to_string())
                    } else if n == first_number {
                        first
                            .take()
                            .ok_or_else(|| "first track taken twice".to_string())
                    } else {
                        layout
                            .iter()
                            .find(|(num, _)| *num == n)
                            .map(|(_, bytes)| vec![0u8; *bytes])
                            .ok_or_else(|| format!("track {n} not in the manifest"))
                    }
                })
                .and_then(|modelled| self.boot_disc(modelled));
                match result {
                    Ok(()) => {
                        self.set_web_current_game(
                            disc.id.to_string(),
                            disc.title.to_string(),
                            GameKind::DiscBin,
                            total as u64,
                        );
                        self.status_message_set(format!("Launched: {}", disc.title));
                    }
                    Err(e) => self.status_message_set(format!("{}: {e}", disc.title)),
                }
            }
            BootStatus::Failed(message) => self.status_message_set(message),
        }

        for BgEvent::TrackReady(number, pcm) in crate::web_stream::poll_background() {
            self.web_track_patches.push((number, pcm, 0));
        }
        // Copy pending tracks into the disc a slice at a time. The audible
        // consequence of a track landing must be music, not a hitch.
        const PATCH_BYTES_PER_FRAME: usize = 2 * 1024 * 1024;
        let mut landed = false;
        if let Some((number, pcm, copied)) = self.web_track_patches.first_mut() {
            let target = self
                .bus
                .as_mut()
                .and_then(|bus| bus.cdrom.disc_mut())
                .and_then(|disc| disc.track_bytes_mut(*number));
            match target {
                Some(buf) if buf.len() == pcm.len() => {
                    let end = (*copied + PATCH_BYTES_PER_FRAME).min(pcm.len());
                    buf[*copied..end].copy_from_slice(&pcm[*copied..end]);
                    *copied = end;
                    if *copied == pcm.len() {
                        landed = true;
                        self.web_track_patches.remove(0);
                    }
                }
                _ => {
                    // The user booted something else while the track was in
                    // flight, or sizes disagree; the download is dropped and
                    // that track stays silent.
                    eprintln!("[web] track {number}: no matching disc to patch");
                    self.web_track_patches.remove(0);
                }
            }
        }
        let all_done = landed && self.web_track_patches.is_empty();
        match crate::web_stream::progress_line() {
            Some(line) => self.status_message_set(line),
            // The frame the last track lands, the line disappears; say so
            // once instead of leaving a stale percentage on screen.
            None if all_done => self.status_message_set("All music tracks loaded"),
            None => {}
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn apply_web_quick_state_event(&mut self, event: crate::web_files::QuickStateEvent) {
        use crate::web_files::QuickStateEvent;

        match event {
            QuickStateEvent::Saved {
                game_id,
                created_at,
                cpu_tick,
                result,
            } => match result {
                Ok(()) => {
                    if self
                        .current_game
                        .as_ref()
                        .is_some_and(|game| game.id == game_id)
                    {
                        self.web_quick_save = Some((game_id, created_at, cpu_tick));
                        self.refresh_save_state_menu_rows();
                        self.status_message_set("Browser quick-save stored");
                    }
                }
                Err(error) => self.status_message_set(format!("Save state failed: {error}")),
            },
            QuickStateEvent::Inspected { game_id, result } => match result {
                Ok(Some((created_at, cpu_tick))) => {
                    if self
                        .current_game
                        .as_ref()
                        .is_some_and(|game| game.id == game_id)
                    {
                        self.web_quick_save = Some((game_id, created_at, cpu_tick));
                        self.refresh_save_state_menu_rows();
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.status_message_set(format!("Could not inspect browser save: {error}"));
                }
            },
            QuickStateEvent::Loaded {
                game_id,
                start_paused,
                result,
            } => match result {
                Ok(Some(bytes)) => self.restore_web_quick_state(&game_id, &bytes, start_paused),
                Ok(None) => self.status_message_set("No browser quick-save yet"),
                Err(error) => self.status_message_set(format!("Load state failed: {error}")),
            },
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn restore_web_quick_state(&mut self, game_id: &str, bytes: &[u8], start_paused: bool) {
        let Some(current_game) = self.current_game.as_ref() else {
            self.status_message_set("Load state: no game running");
            return;
        };
        if current_game.id != game_id {
            self.status_message_set("Load state cancelled: a different game is now running");
            return;
        }
        let loaded = match SaveStateV1::<EmulatorState>::from_bytes(bytes) {
            Ok(state) => state,
            Err(error) => {
                self.status_message_set(format!("Load state failed: {error}"));
                return;
            }
        };
        if loaded.header.game_id != game_id {
            self.status_message_set("Load state failed: save is from a different game");
            return;
        }
        let Some(donor_bus) = self.bus.as_mut() else {
            self.status_message_set("Load state: no game running");
            return;
        };
        let mut payload = loaded.payload;
        payload.bus.restore_excluded_from(donor_bus);
        self.cpu = payload.cpu;
        self.bus = Some(payload.bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        self.running = !start_paused;
        self.menu.sync_run_label(self.running);
        self.web_quick_save = Some((
            game_id.to_string(),
            loaded.header.created_at,
            loaded.header.cpu_tick,
        ));
        self.refresh_save_state_menu_rows();
        self.status_message_set("Loaded browser quick-save");
    }

    /// Web: boot a raw `.bin` disc image through the uploaded real BIOS,
    /// mirroring the native `GameKind::DiscBin` path (minus the filesystem
    /// memcard -- the web build uses a blank in-memory card).
    #[cfg(target_arch = "wasm32")]
    fn boot_disc_bytes_with_bios(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() < SECTOR_BYTES {
            return Err("disc image too small to be valid".to_string());
        }
        self.boot_disc_with_bios(Disc::from_bin(bytes))
    }

    /// Web: boot an already-modelled disc through the uploaded real BIOS.
    /// Split from the bytes wrapper so a replay reboot can reuse the disc
    /// taken back out of the outgoing bus instead of keeping a second copy.
    #[cfg(target_arch = "wasm32")]
    fn boot_disc_with_bios(&mut self, disc: Disc) -> Result<(), String> {
        let Some(bios) = self.bios_bytes.clone() else {
            return Err("Load a BIOS first (Settings -> Load BIOS file)".to_string());
        };
        let mut bus = Bus::new(bios).map_err(|e| format!("BIOS rejected: {e}"))?;
        let mut cpu = Cpu::new();
        maybe_fast_boot_disc_path(
            &mut bus,
            &mut cpu,
            &disc,
            std::path::Path::new("uploaded.bin"),
            self.settings.emulator.fast_boot_disc,
        );
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();
        bus.attach_memcard_port1(Vec::new());
        self.swap_in_booted(bus, cpu);
        Ok(())
    }

    /// Side-load a homebrew PS-EXE from bytes via the no-BIOS HLE path,
    /// mirroring the native `GameKind::Exe` branch.
    ///
    /// Both targets: a downloaded build boots its baked-in examples through
    /// here, having no file on disk to point the library at.
    fn boot_exe_bytes(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        let exe = Exe::parse(&bytes).map_err(|e| format!("parse EXE: {e:?}"))?;
        let mut bus = Bus::new_without_bios();
        let mut cpu = Cpu::new();
        bus.load_exe_payload(exe.load_addr, &exe.payload);
        bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
        cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());
        bus.enable_hle_bios();
        bus.attach_digital_pad_port1();
        self.swap_in_booted(bus, cpu);
        Ok(())
    }

    /// Common tail for a from-bytes boot: swap in the new machine, start
    /// running, and close the menu so the game is visible. Nothing here is
    /// web-specific; the native build reaches it through the baked-in examples.
    fn swap_in_booted(&mut self, bus: Bus, cpu: Cpu) {
        self.bus = Some(bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        self.cpu = cpu;
        self.running = true;
        self.current_game = None;
        self.exec_history.clear();
        self.gpr_snapshot = None;
        self.menu.open = false;
        self.menu.sync_run_label(true);
    }

    /// Choose and persist the games folder from the Menu Settings column.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn choose_games_path(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Choose games folder");
        if let Some(dir) = path_parent_or_self(self.settings.paths.game_library.trim()) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog.pick_folder() else {
            return;
        };
        self.settings.paths.game_library = path.to_string_lossy().into_owned();
        match self.save_settings() {
            Ok(()) => {
                self.sync_menu_settings_paths();
                match self.rescan_library() {
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[frontend] rescan after games path change failed: {e}");
                        self.status_message_set(format!("Games path saved; rescan failed: {e}"));
                    }
                }
            }
            Err(e) => {
                eprintln!("[frontend] {e}");
                self.status_message_set(e);
            }
        }
    }

    /// Web: pick a games *folder* (persistent File System Access picker where
    /// available, else `<input webkitdirectory>`). Scanned recursively for
    /// .bin/.exe; results land via `poll_web_uploads`. The folder is remembered.
    #[cfg(target_arch = "wasm32")]
    pub fn choose_games_path(&mut self) {
        crate::web_files::pick_games();
    }

    /// Refresh the Settings Menu row values from persisted path state.
    pub fn sync_menu_settings_paths(&mut self) {
        self.menu
            .sync_settings_paths(self.bios_path_label(), self.games_path_label());
    }

    /// Current display label for every rebindable port-1 target, for
    /// the controls panel.
    fn controls_labels(&self) -> Vec<(PadBindTarget, String)> {
        PadBindTarget::ALL
            .into_iter()
            .map(|t| (t, binding_for_target(&self.settings.input.port1, t).label()))
            .collect()
    }

    /// Push the current binding labels into the Menu's controls panel.
    pub fn sync_menu_controls(&mut self) {
        let labels = self.controls_labels();
        self.menu.sync_controls(labels);
    }

    /// Bind `target` to `binding`, stealing it from any other target
    /// that currently uses the same key -- one physical key silently
    /// driving two pad inputs is never what the user meant. Persists
    /// immediately and refreshes the panel labels; key events read the
    /// bindings live, so the change applies to the running game on the
    /// very next press.
    pub fn apply_rebind(&mut self, target: PadBindTarget, binding: psoxide_settings::InputBinding) {
        for other in PadBindTarget::ALL {
            if other != target && *binding_for_target(&self.settings.input.port1, other) == binding
            {
                *binding_for_target_mut(&mut self.settings.input.port1, other) =
                    psoxide_settings::InputBinding::Unbound;
            }
        }
        *binding_for_target_mut(&mut self.settings.input.port1, target) = binding.clone();
        if let Err(e) = self.save_settings() {
            eprintln!("[frontend] settings save after rebind: {e}");
        }
        self.sync_menu_controls();
        self.status_message_set(format!("{} bound to {}", target.label(), binding.label()));
    }

    /// Restore every port-1 binding to the built-in defaults.
    pub fn reset_controls(&mut self) {
        self.settings.input.port1 = psoxide_settings::settings::PortBindings::default();
        if let Err(e) = self.save_settings() {
            eprintln!("[frontend] settings save after controls reset: {e}");
        }
        self.sync_menu_controls();
        self.status_message_set("Controls reset to defaults".to_string());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn bios_path_label(&self) -> String {
        let configured = self.settings.paths.bios.trim();
        if !configured.is_empty() {
            return path_label(PathBuf::from(configured));
        }
        if let Some(env) = std::env::var_os("PSOXIDE_BIOS") {
            return format!("env: {}", path_label(PathBuf::from(env)));
        }
        "Missing".into()
    }

    /// Web: the BIOS is held in memory (uploaded), not a path.
    #[cfg(target_arch = "wasm32")]
    fn bios_path_label(&self) -> String {
        if self.bios_bytes.is_some() {
            "Loaded".into()
        } else {
            "None".into()
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn games_path_label(&self) -> String {
        let configured = self.settings.paths.game_library.trim();
        if configured.is_empty() {
            "Missing".into()
        } else {
            path_label(PathBuf::from(configured))
        }
    }

    /// Web: games are picked per-file, so there is no folder path to show.
    #[cfg(target_arch = "wasm32")]
    fn games_path_label(&self) -> String {
        String::new()
    }

    /// Persist the embedded editor project if it has unsaved edits,
    /// and remember which project directory is active so the next
    /// launch reopens it.
    #[cfg(feature = "editor")]
    pub fn save_editor_project(&mut self) -> Result<bool, String> {
        let saved = self
            .editor
            .save_if_dirty()
            .map_err(|e| format!("save editor project: {e}"))?;
        let current = Some(self.editor.project_dir().to_path_buf());
        if self.settings.editor.last_project_dir != current {
            self.settings.editor.last_project_dir = current;
            if let Err(e) = self.save_settings() {
                eprintln!("[frontend] {e}");
            }
        }
        Ok(saved)
    }

    /// Flip the disc fast-boot preference, keep the Menu label in
    /// sync, and persist immediately so the next launch uses the
    /// requested path even if the app exits abruptly.
    pub fn toggle_fast_boot_disc(&mut self) {
        let enabled = !self.settings.emulator.fast_boot_disc;
        self.settings.emulator.fast_boot_disc = enabled;
        self.menu.sync_fast_boot_label(enabled);

        let msg = if enabled {
            "Fast boot enabled: PS logo skipped on disc launch"
        } else {
            "Fast boot disabled: BIOS logo shown on disc launch"
        };

        match self.save_settings() {
            Ok(()) => self.status_message_set(msg),
            Err(e) => {
                eprintln!("[frontend] {e}");
                self.status_message_set(format!("{msg} (settings save failed)"));
            }
        }
    }

    /// Cycle the menu backdrop opacity through a few presets, keep the Menu
    /// in sync, and persist immediately.
    pub fn cycle_menu_opacity(&mut self) {
        const PRESETS: [u8; 5] = [50, 65, 80, 90, 100];
        let current = self.settings.video.menu_opacity_pct;
        let next = PRESETS
            .iter()
            .copied()
            .find(|&p| p > current)
            .unwrap_or(PRESETS[0]);
        self.settings.video.menu_opacity_pct = next;
        self.menu.set_menu_opacity(next);

        let msg = format!("Menu opacity: {next}%");
        match self.save_settings() {
            Ok(()) => self.status_message_set(msg),
            Err(e) => {
                eprintln!("[frontend] {e}");
                self.status_message_set(format!("{msg} (settings save failed)"));
            }
        }
    }
}

/// Editor-workspace orchestration: entering/leaving the editor, the embedded
/// Play state machine, project builds, and input-tape recording/replay. The
/// whole surface drops out without the `editor` feature.
#[cfg(feature = "editor")]
const DEFAULT_EMBEDDED_PLAYTEST_FEATURES: &str = "cd-stream-bench";

#[cfg(feature = "editor")]
impl AppState {
    /// Configure a deterministic native-editor launch before the event loop
    /// starts. This is the implementation behind the frontend's `--editor`
    /// development flags; it never depends on menu selection or synthetic
    /// keyboard input.
    pub fn open_editor_startup(
        &mut self,
        project_dir: Option<PathBuf>,
        view: Option<EditorWorkspaceView>,
        resource_selector: Option<&str>,
    ) -> Result<(), String> {
        if let Some(project_dir) = project_dir {
            let project_dir = if project_dir.is_absolute() {
                project_dir
            } else {
                repo_root_dir().join(project_dir)
            };
            self.editor = EditorWorkspace::open_directory(&project_dir).map_err(|error| {
                format!("open editor project at {}: {error}", project_dir.display())
            })?;
            self.editor_project_dir_seen = self.editor.project_dir().to_path_buf();
        }

        if let Some(view) = view {
            self.editor.show_workspace(view);
            if self.editor.active_workspace_view() != view {
                return Err(format!("editor failed to select startup view {view:?}"));
            }
        }

        if let Some(selector) = resource_selector {
            if view != Some(EditorWorkspaceView::Animation) {
                return Err("--editor-resource requires --editor-view animation".to_string());
            }
            let numeric_id = selector.parse::<u64>().ok();
            let resource_id = self
                .editor
                .project()
                .resources
                .iter()
                .find(|resource| {
                    numeric_id.is_some_and(|id| resource.id.raw() == id)
                        || resource.name == selector
                })
                .or_else(|| {
                    self.editor
                        .project()
                        .resources
                        .iter()
                        .find(|resource| resource.name.eq_ignore_ascii_case(selector))
                })
                .map(|resource| resource.id)
                .ok_or_else(|| format!("editor resource {selector:?} was not found"))?;
            if !self.editor.open_animation_viewer_for_resource(resource_id) {
                return Err(format!(
                    "editor resource {selector:?} cannot open in Animation Studio"
                ));
            }
            if !self
                .editor
                .animation_viewer_resource_is_focused(resource_id)
            {
                return Err(format!(
                    "Animation Studio failed to focus editor resource {selector:?}"
                ));
            }
        }

        self.open_editor_workspace();
        self.menu.open = false;
        self.status_message_set("Editor opened from deterministic startup flags");
        Ok(())
    }

    /// Select the BSP workspace's orthographic viewport for the native
    /// `--editor-view top` startup route.
    pub fn show_editor_room_orthographic(&mut self) {
        self.editor.show_room_orthographic();
    }

    /// Enter the embedded editor workspace.
    pub fn open_editor_workspace(&mut self) {
        self.running = false;
        self.workspace = Workspace::Editor;
        self.menu.sync_run_label(false);
        self.menu.sync_editor_label(true);
        self.status_message_set("Editor workspace open");
    }

    /// Return from the editor workspace to the emulator view.
    pub fn close_editor_workspace(&mut self) {
        self.stop_embedded_playtest();
        let save_result = self.save_editor_project();
        self.workspace = Workspace::Emulator;
        self.menu.sync_editor_label(false);
        match save_result {
            Ok(true) => self.status_message_set("Returned to emulator workspace (editor saved)"),
            Ok(false) => self.status_message_set("Returned to emulator workspace"),
            Err(e) => {
                eprintln!("[frontend] {e}");
                self.status_message_set("Returned to emulator workspace (editor save failed)");
            }
        }
    }

    /// Toggle the embedded editor workspace.
    pub fn toggle_editor_workspace(&mut self) {
        if self.workspace.is_editor() {
            self.close_editor_workspace();
        } else {
            self.open_editor_workspace();
        }
    }

    /// Editor-facing status mirror for the embedded play controls.
    pub fn editor_playtest_status(&self) -> EditorPlaytestStatus {
        self.embedded_playtest.editor_status()
    }

    /// Editor-facing input-tape summary for the play viewport overlay.
    pub fn editor_playtest_input_tape_status(&self) -> psxed_ui::EditorPlaytestTapeStatus {
        self.playtest_input_tape.editor_status()
    }

    /// Resolve the persistent input tape for the current editor project.
    fn editor_playtest_input_tape_path(&self) -> PathBuf {
        let stem = self
            .editor
            .project_dir()
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| psxed_project::project_file_stem(&self.editor.project().name));
        self.paths
            .editor_dir()
            .join("playtest_tapes")
            .join(format!("{stem}.pxtape"))
    }

    /// True when the editor viewport is currently the live game.
    pub fn embedded_playtest_running(&self) -> bool {
        self.embedded_playtest.is_running()
    }

    /// True when keyboard/gamepad input should be routed to the
    /// embedded game even though the editor workspace is visible.
    pub fn embedded_playtest_input_captured(&self) -> bool {
        self.embedded_playtest.input_captured()
    }

    /// Cook the active project while preserving the cooker's stderr output
    /// and mirroring the same progress lines into the editor Console.
    fn cook_editor_playtest_to_disk(&mut self) -> Result<String, String> {
        let (result, lines) =
            psxed_project::playtest::capture_cook_output(|| self.editor.cook_playtest_to_disk());
        self.editor.append_console_lines(lines);
        result
    }

    /// Build and run the active editor project: cook assets, spawn
    /// the existing MIPS build target, wrap the EXE into a bootable
    /// disc image, then launch that disc. The build is asynchronous;
    /// call [`Self::poll_embedded_playtest_build`] once per frame to
    /// load the resulting disc when it exits successfully.
    pub fn start_embedded_playtest(&mut self) {
        self.stop_embedded_playtest();
        self.editor.set_status("Play: cooking assets...");
        self.editor
            .append_console_lines(["[cook] Play: cooking assets..."]);
        if let Err(error) = self.save_editor_project() {
            let message = format!("Embedded Play failed: {error}");
            self.editor.append_console_lines([message]);
            self.editor
                .set_status("Embedded Play failed while saving — see Console");
            self.embedded_playtest.fail();
            return;
        }
        let cook_status = match self.cook_editor_playtest_to_disk() {
            Ok(status) => status,
            Err(error) => {
                let message = format!("Embedded Play failed while cooking assets: {error}");
                self.editor.append_console_lines([message]);
                self.editor
                    .set_status("Embedded Play cook failed — see Console");
                self.embedded_playtest.fail();
                return;
            }
        };
        self.editor
            .append_console_lines([format!("[cook] {cook_status}")]);
        self.editor.set_status("Play: compiling PS1 runtime...");

        let volume_id = project_disc_volume_id(&self.editor.project().name);
        if let Err(error) =
            self.spawn_editor_playtest_build(EditorBuildCompletion::RunEmbedded { volume_id })
        {
            let message = format!("Embedded Play build failed: {error}");
            self.editor.append_console_lines([message]);
            self.editor
                .set_status("Embedded Play build failed — see Console");
            self.embedded_playtest.fail();
        }
    }

    /// Build the active project by cooking assets, compiling the runtime,
    /// and exporting a CUE/BIN disc into the project folder so Projects
    /// can launch it without opening the editor.
    pub fn build_current_project_for_launcher(&mut self) {
        self.stop_embedded_playtest();
        self.editor
            .set_status("Building project: cooking assets...");
        self.editor
            .append_console_lines(["[cook] Project build: cooking assets..."]);
        if let Err(error) = self.save_editor_project() {
            let message = format!("Project build failed: {error}");
            self.editor.append_console_lines([message]);
            self.editor
                .set_status("Project build failed while saving — see Console");
            self.embedded_playtest.fail();
            return;
        }
        let dest_path =
            project_baked_disc_path(self.editor.project_dir(), &self.editor.project().name);
        let volume_id = project_disc_volume_id(&self.editor.project().name);
        let cook_status = match self.cook_editor_playtest_to_disk() {
            Ok(status) => status,
            Err(error) => {
                let message = format!("Project build failed while cooking assets: {error}");
                self.editor.append_console_lines([message]);
                self.editor
                    .set_status("Project build cook failed — see Console");
                self.embedded_playtest.fail();
                return;
            }
        };
        self.editor
            .append_console_lines([format!("[cook] {cook_status}")]);
        self.editor.set_status("Building project PS1 runtime...");

        if let Err(error) = self.spawn_editor_playtest_build(EditorBuildCompletion::ExportProject {
            dest_path,
            volume_id,
        }) {
            let message = format!("Project build failed: {error}");
            self.editor.append_console_lines([message]);
            self.editor.set_status("Project build failed — see Console");
            self.embedded_playtest.fail();
        }
    }

    fn begin_editor_build_log(&mut self, label: &str, log_path: &Path) {
        self.editor_build_log_offset = 0;
        self.editor_build_log_pending.clear();
        self.editor
            .append_console_lines([format!("[build] {label} started · {}", log_path.display())]);
    }

    fn poll_editor_build_log(&mut self, flush_partial: bool) {
        if self.editor_build_completion.is_none() {
            return;
        }
        let log_path = editor_playtest_build_log_path();
        let Ok(mut file) = std::fs::File::open(&log_path) else {
            return;
        };
        let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
            return;
        };
        if length < self.editor_build_log_offset {
            self.editor_build_log_offset = 0;
            self.editor_build_log_pending.clear();
        }
        if file
            .seek(SeekFrom::Start(self.editor_build_log_offset))
            .is_err()
        {
            return;
        }
        let mut bytes = Vec::new();
        if file.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.editor_build_log_offset = self
            .editor_build_log_offset
            .saturating_add(bytes.len() as u64);
        self.editor_build_log_pending.extend_from_slice(&bytes);

        let complete_len = if flush_partial {
            self.editor_build_log_pending.len()
        } else {
            self.editor_build_log_pending
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |newline| newline + 1)
        };
        if complete_len == 0 {
            return;
        }
        let complete: Vec<u8> = self
            .editor_build_log_pending
            .drain(..complete_len)
            .collect();
        let mut lines = Vec::new();
        for bytes in complete.split(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(bytes);
            let line = line.trim_end_matches('\r');
            if !line.is_empty() {
                lines.push(line.to_string());
            }
        }
        if !lines.is_empty() {
            self.editor.append_console_lines(lines);
        }
    }

    fn spawn_editor_playtest_build(
        &mut self,
        completion: EditorBuildCompletion,
    ) -> Result<(), String> {
        let workspace_root = repo_root_dir();
        // Capture the build's stdout+stderr to a log file rather than
        // discarding it, so a compile failure surfaces the actual error
        // (not just "exit status: 2"). Both streams go to the same file so
        // the log reads in source order.
        let log_path = editor_playtest_build_log_path();
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let log_file = std::fs::File::create(&log_path)
            .map_err(|error| format!("create build log {}: {error}", log_path.display()))?;
        let log_stderr = log_file
            .try_clone()
            .map_err(|error| format!("clone build log handle: {error}"))?;
        let build_label = match &completion {
            EditorBuildCompletion::ExportProject { .. } => "Project build",
            EditorBuildCompletion::RunEmbedded { .. } => "Embedded Play build",
        };
        self.begin_editor_build_log(build_label, &log_path);
        let mut command = Command::new("make");
        command
            .arg("build-editor-playtest")
            .current_dir(&workspace_root)
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_stderr));
        // Keep ordinary embedded Play on the hardware-equivalent guest. The
        // current v0.4 content has only a few KiB of static-RAM headroom and
        // emulator telemetry moves the BSS start by 16 KiB, so silently adding
        // it makes Play fail at link time even though the export build fits.
        // Profiling remains opt-in by launching the editor with an explicit
        // EDITOR_PLAYTEST_FEATURES value that includes emulator-telemetry.
        if matches!(completion, EditorBuildCompletion::RunEmbedded { .. })
            && std::env::var_os("EDITOR_PLAYTEST_FEATURES").is_none()
            && std::env::var_os("EDITOR_PLAYTEST_CARGO_FEATURE_FLAGS").is_none()
        {
            command.env(
                "EDITOR_PLAYTEST_FEATURES",
                DEFAULT_EMBEDDED_PLAYTEST_FEATURES,
            );
        }
        let child = command
            .spawn()
            .map_err(|error| format!("spawn make: {error}"))?;
        self.editor_build_completion = Some(completion);
        self.embedded_playtest.start_building(child);
        Ok(())
    }

    /// Poll the background build child, then either launch the wrapped
    /// CUE/BIN playtest disc or export that disc as a project build.
    pub fn poll_embedded_playtest_build(&mut self) {
        self.poll_editor_build_log(false);
        let wait_result = {
            let Some(child) = self.embedded_playtest.building_child_mut() else {
                return;
            };
            child.try_wait()
        };
        let status = match wait_result {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(error) => {
                let message = format!("{} poll failed: {error}", self.editor_build_label());
                self.poll_editor_build_log(true);
                self.editor.append_console_lines([message]);
                self.editor.set_status("Build process failed — see Console");
                self.editor_build_completion = None;
                self.embedded_playtest.fail();
                return;
            }
        };
        self.poll_editor_build_log(true);

        if !status.success() {
            // Surface the real compiler error from the captured build log,
            // not just the bare exit status, plus where to read the full log.
            let label = self.editor_build_label();
            let log_path = editor_playtest_build_log_path();
            let detail = build_log_failure_detail(&log_path);
            let message = format!("{label} failed ({status}). {detail}");
            self.editor.append_console_lines([message]);
            self.editor
                .set_status(format!("{label} failed — see Console"));
            self.editor_build_completion = None;
            self.embedded_playtest.fail();
            return;
        }
        self.editor
            .append_console_lines(["[build] PS1 runtime compilation complete"]);

        let completion = self.editor_build_completion.take().unwrap_or_else(|| {
            EditorBuildCompletion::RunEmbedded {
                volume_id: DEFAULT_EMBEDDED_PLAYTEST_VOLUME_ID.to_string(),
            }
        });
        match completion {
            EditorBuildCompletion::RunEmbedded { volume_id } => {
                self.editor
                    .set_status("Embedded Play build complete; creating disc image...");
                match self.load_embedded_playtest_disc(&volume_id) {
                    Ok(()) => {
                        self.embedded_playtest.start_running(true);
                        self.running = true;
                        self.menu.open = false;
                        self.menu.sync_run_label(true);
                        self.editor
                            .set_status("Embedded Play running in the 3D viewport");
                        self.status_message_set("Embedded Play running");
                    }
                    Err(error) => {
                        let message = format!("Embedded Play load failed: {error}");
                        self.editor.append_console_lines([message]);
                        self.editor
                            .set_status("Embedded Play load failed — see Console");
                        self.embedded_playtest.fail();
                    }
                }
            }
            EditorBuildCompletion::ExportProject {
                dest_path,
                volume_id,
            } => match self.export_project_build(dest_path, &volume_id) {
                Ok(message) => {
                    self.embedded_playtest.stop();
                    self.editor.append_console_lines([message.clone()]);
                    self.editor.set_status("Project build complete");
                    self.status_message_set(message);
                }
                Err(error) => {
                    let message = format!("Project build export failed: {error}");
                    self.editor.append_console_lines([message]);
                    self.editor
                        .set_status("Project build export failed — see Console");
                    self.embedded_playtest.fail();
                }
            },
        }
    }

    fn editor_build_label(&self) -> &'static str {
        match self.editor_build_completion.as_ref() {
            Some(EditorBuildCompletion::ExportProject { .. }) => "Project build",
            _ => "Embedded Play build",
        }
    }

    /// Stop embedded play mode and return the editor viewport to the
    /// authored 3D preview.
    pub fn stop_embedded_playtest(&mut self) {
        let tape_path = self.editor_playtest_input_tape_path();
        #[cfg(not(target_arch = "wasm32"))]
        let capture_phase = if self.playtest_input_tape.is_recording() {
            Some("record")
        } else if self.playtest_input_tape.is_replaying() {
            Some("replay")
        } else {
            None
        };
        let stop_result = if self.playtest_input_tape.is_recording() {
            self.finish_input_recording(&tape_path).map(Some)
        } else {
            self.playtest_input_tape.stop_replay();
            Ok(None)
        };
        if let Err(error) = stop_result {
            eprintln!("[frontend] stop input tape: {error}");
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(phase) = capture_phase {
                self.queue_input_profile_capture(&tape_path, phase);
            }
        }
        if let Some(mut child) = self.embedded_playtest.take_build_child() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.editor_build_completion = None;
        self.embedded_playtest.stop();
        self.running = false;
        self.menu.sync_run_label(false);
    }

    /// Reconcile the embedded playtest with the editor's current
    /// project directory. Called once per frame after the editor UI
    /// runs so that switching project from the editor menu
    /// implicitly stops a play session that belongs to the previous
    /// project, instead of letting the viewport keep rendering it
    /// against the wrong assets.
    pub fn sync_embedded_playtest_with_editor_project(&mut self) {
        let current = self.editor.project_dir();
        if current == self.editor_project_dir_seen {
            return;
        }
        self.editor_project_dir_seen = current.to_path_buf();
        let status = self.editor_playtest_status();
        if status == EditorPlaytestStatus::Idle {
            return;
        }
        let was_active = status.is_active();
        self.stop_embedded_playtest();
        if was_active {
            self.status_message_set("Embedded play stopped: project changed");
        }
    }

    /// Capture input for the embedded game and resume emulation.
    pub fn capture_embedded_playtest_input(&mut self) {
        if self.embedded_playtest.capture_input() {
            self.running = true;
            self.menu.open = false;
            self.menu.sync_run_label(true);
            self.editor.set_status("Embedded Play input captured");
        }
    }

    /// Release input capture from the embedded game and pause it.
    pub fn release_embedded_playtest_input(&mut self) {
        if self.embedded_playtest.release_input() {
            self.running = false;
            self.menu.open = true;
            self.menu.sync_run_label(false);
            self.editor
                .set_status("Embedded Play paused; click viewport to resume");
        }
    }

    fn start_embedded_playtest_input_recording(&mut self) {
        if !self.embedded_playtest_running() {
            self.editor
                .set_status("Start Embedded Play before recording input");
            return;
        }
        let start_poll = self
            .bus
            .as_ref()
            .map(|bus| bus.port1_completed_polls())
            .unwrap_or(0);
        self.playtest_input_tape.start_recording(start_poll);
        #[cfg(not(target_arch = "wasm32"))]
        self.begin_input_profile_capture();
        let _ = self.embedded_playtest.capture_input();
        self.running = true;
        self.menu.open = false;
        self.menu.sync_run_label(true);
        let message = "Input recording started";
        self.editor.set_status(message);
        self.status_message_set(message);
    }

    fn stop_embedded_playtest_input_recording(&mut self) {
        let path = self.editor_playtest_input_tape_path();
        let result = self.finish_input_recording(&path);
        #[cfg(not(target_arch = "wasm32"))]
        self.queue_input_profile_capture(&path, "record");
        match result {
            Ok(frames) => {
                #[cfg(target_arch = "wasm32")]
                let message = format!("Input recording downloaded: {frames} frames (CSV)");
                #[cfg(not(target_arch = "wasm32"))]
                let message = format!("Input recording saved: {frames} frames");
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
            Err(error) => {
                let message = format!("Input recording save failed: {error}");
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
        }
    }

    fn start_embedded_playtest_input_replay(&mut self) {
        if !self.embedded_playtest_running() {
            self.editor
                .set_status("Start Embedded Play before replaying input");
            return;
        }
        if self.playtest_input_tape.is_recording() {
            self.editor
                .set_status("Stop input recording before replaying it");
            return;
        }
        let path = self.editor_playtest_input_tape_path();
        match self.playtest_input_tape.start_replay(&path) {
            Ok(frames) => {
                #[cfg(not(target_arch = "wasm32"))]
                self.begin_input_profile_capture();
                let _ = self.embedded_playtest.capture_input();
                self.running = true;
                self.menu.open = false;
                self.menu.sync_run_label(true);
                let message = format!("Input replay started: {frames} frames");
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
            Err(error) => {
                let message = format!("Input replay unavailable: {error}");
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
        }
    }

    fn stop_embedded_playtest_input_replay(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        let path = self.editor_playtest_input_tape_path();
        self.playtest_input_tape.stop_replay();
        #[cfg(not(target_arch = "wasm32"))]
        self.queue_input_profile_capture(&path, "replay");
        let message = "Input replay stopped";
        self.editor.set_status(message);
        self.status_message_set(message);
    }

    fn embedded_playtest_profiler_history_path(&self) -> PathBuf {
        self.editor
            .project_dir()
            .join("logs")
            .join("play_profiler_history.csv")
    }

    fn dump_embedded_playtest_profiler_history(&mut self) {
        let sample_count = self.profiler.history_len();
        if sample_count == 0 {
            let message = "Profiler history is empty";
            self.editor.set_status(message);
            self.status_message_set(message);
            return;
        }

        let path = self.embedded_playtest_profiler_history_path();
        let write_result = (|| -> Result<(), String> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&path, self.profiler.history_csv())
                .map_err(|error| format!("{}: {error}", path.display()))?;
            Ok(())
        })();

        match write_result {
            Ok(()) => {
                let message = format!(
                    "Profiler history saved: {sample_count} frames -> {}",
                    path.display()
                );
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
            Err(error) => {
                let message = format!("Profiler history save failed: {error}");
                self.editor.set_status(message.clone());
                self.status_message_set(message);
            }
        }
    }

    /// Handle one request emitted by the editor UI.
    pub fn handle_editor_playtest_request(&mut self, request: psxed_ui::EditorPlaytestRequest) {
        match request {
            psxed_ui::EditorPlaytestRequest::Play | psxed_ui::EditorPlaytestRequest::Rebuild => {
                self.start_embedded_playtest();
            }
            psxed_ui::EditorPlaytestRequest::BuildProject => {
                self.build_current_project_for_launcher();
            }
            psxed_ui::EditorPlaytestRequest::Stop => {
                self.stop_embedded_playtest();
                self.editor
                    .set_status("Embedded Play stopped; returned to edit preview");
            }
            psxed_ui::EditorPlaytestRequest::CaptureInput => {
                self.capture_embedded_playtest_input();
            }
            psxed_ui::EditorPlaytestRequest::StartInputRecording => {
                self.start_embedded_playtest_input_recording();
            }
            psxed_ui::EditorPlaytestRequest::StopInputRecording => {
                self.stop_embedded_playtest_input_recording();
            }
            psxed_ui::EditorPlaytestRequest::StartInputReplay => {
                self.start_embedded_playtest_input_replay();
            }
            psxed_ui::EditorPlaytestRequest::StopInputReplay => {
                self.stop_embedded_playtest_input_replay();
            }
            psxed_ui::EditorPlaytestRequest::DumpProfilerHistory => {
                self.dump_embedded_playtest_profiler_history();
            }
            psxed_ui::EditorPlaytestRequest::SetWireframe { enabled } => {
                if let Some(bus) = self.bus.as_mut() {
                    bus.gpu.wireframe_enabled = enabled;
                    let message = if enabled {
                        "Embedded Play wireframe enabled"
                    } else {
                        "Embedded Play wireframe disabled"
                    };
                    self.editor.set_status(message);
                    self.status_message_set(message);
                }
            }
        }
    }

    fn load_embedded_playtest_disc(&mut self, volume_id: &str) -> Result<(), String> {
        let mut bus = Bus::new_without_bios();
        let mut cpu = Cpu::new();

        let disc_path = build_embedded_playtest_disc(volume_id)?;
        let disc = load_authored_disc(&disc_path)?;
        // Embedded Play is PSoXide-authored homebrew: no user BIOS is
        // required. The runtime fast-boots with HLE BIOS dispatch, while
        // still mounting a real disc image so CD streaming exercises the
        // same path as exported project builds.
        fast_boot_embedded_playtest_disc(&mut bus, &mut cpu, &disc, &disc_path);
        bus.cdrom.insert_disc(Some(disc));
        bus.attach_digital_pad_port1();

        self.bus = Some(bus);
        self.gpu_resync_generation = self.gpu_resync_generation.wrapping_add(1);
        self.cpu = cpu;
        self.exec_history.clear();
        self.gpr_snapshot = None;
        self.current_game = None;
        Ok(())
    }

    fn export_project_build(
        &mut self,
        dest_path: PathBuf,
        volume_id: &str,
    ) -> Result<String, String> {
        let source_path = build_embedded_playtest_disc(volume_id)?;
        let build_bytes = copy_project_disc(&source_path, &dest_path)?;

        let rescan_error = self.rescan_library().err();
        let display_path = dest_path
            .canonicalize()
            .unwrap_or_else(|_| dest_path.clone());
        let mut message = format!(
            "Project disc exported -> {} ({} KiB)",
            display_path.display(),
            build_bytes / 1024
        );
        if let Some(error) = rescan_error {
            message.push_str(&format!("; launcher rescan failed: {error}"));
        }
        Ok(message)
    }
}

impl AppState {
    fn active_input_tape_path(&self) -> Result<PathBuf, String> {
        #[cfg(feature = "editor")]
        if self.workspace.is_editor() && self.embedded_playtest_running() {
            return Ok(self.editor_playtest_input_tape_path());
        }
        let game = self
            .current_game
            .as_ref()
            .ok_or_else(|| "launch a game before recording input".to_string())?;
        Ok(self.paths.latest_input_tape_file(&game.id))
    }

    fn input_profile_capture_path(tape_path: &Path, phase: &str) -> PathBuf {
        let stem = tape_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("input");
        tape_path.with_file_name(format!("{stem}.{phase}.profile.csv"))
    }

    fn begin_input_profile_capture(&mut self) {
        // A completed replay can end at the input boundary immediately before
        // another UI action. Persist it before replacing the bounded aggregate.
        self.flush_pending_input_profile_capture();
        self.profiler.begin_capture();
    }

    fn finish_input_recording(&mut self, path: &Path) -> Result<usize, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.playtest_input_tape.stop_recording(path)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (frames, csv) = self
                .playtest_input_tape
                .stop_recording_csv(self.current_game_hash);
            let default_stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty() && *stem != "latest")
                .unwrap_or("input");
            let filename_stem = self
                .current_game
                .as_ref()
                .map(|game| format!("psoxide-input-{}", game.id))
                .unwrap_or_else(|| format!("psoxide-input-{default_stem}"));
            crate::web_files::download_input_csv(&filename_stem, &csv)?;
            Ok(frames)
        }
    }

    fn queue_input_profile_capture(&mut self, tape_path: &Path, phase: &'static str) {
        if self.profiler.capture_active() {
            self.pending_input_profile_capture = Some(PendingInputProfileCapture {
                tape_path: tape_path.to_path_buf(),
                phase,
            });
        }
    }

    /// Persist a queued whole-run recording/replay profile after the current
    /// frontend sample has been folded into the profiler.
    pub fn flush_pending_input_profile_capture(&mut self) {
        let Some(pending) = self.pending_input_profile_capture.take() else {
            return;
        };
        let Some(report) = self.profiler.finish_capture() else {
            return;
        };
        let sample_count = report.sample_count();
        let path = Self::input_profile_capture_path(&pending.tape_path, pending.phase);
        let write_result = (|| -> Result<(), String> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| format!("{}: {error}", parent.display()))?;
            }
            std::fs::write(&path, report.csv())
                .map_err(|error| format!("{}: {error}", path.display()))
        })();

        match write_result {
            Ok(()) => {
                let message = format!(
                    "Whole-run profile saved: {sample_count} samples -> {}",
                    path.display()
                );
                #[cfg(feature = "editor")]
                if self.workspace.is_editor() {
                    self.editor.set_status(message.clone());
                }
                self.status_message_set(message);
            }
            Err(error) => {
                let message = format!("Whole-run profile save failed: {error}");
                #[cfg(feature = "editor")]
                if self.workspace.is_editor() {
                    self.editor.set_status(message.clone());
                }
                self.status_message_set(message);
            }
        }
    }

    /// Apply recording/replay at the port-1 boundary of one emulated video
    /// frame. The headless `--input-tape` runner consumes the same PXITAPE1
    /// samples at this exact clock boundary.
    pub fn input_sample_for_frame(&mut self, live_sample: Port1PadSample) -> Port1PadSample {
        let (sample, event) = self.playtest_input_tape.sample_for_frame(live_sample);
        if let Some(PlaytestInputEvent::ReplayFinished { frames }) = event {
            #[cfg(not(target_arch = "wasm32"))]
            if let Ok(tape_path) = self.active_input_tape_path() {
                self.queue_input_profile_capture(&tape_path, "replay");
            }
            let message = format!("Input replay finished: {frames} frames");
            #[cfg(feature = "editor")]
            if self.workspace.is_editor() {
                self.editor.set_status(message.clone());
            }
            self.status_message_set(message);
        }
        sample
    }

    /// Advance the input tape by the pad polls the guest completed during the
    /// frame that just ran. Call once per stepped frame, after the step.
    pub fn input_note_polls(&mut self, live_sample: Port1PadSample, polls: u64) {
        self.playtest_input_tape.note_polls(live_sample, polls);
    }

    /// `(recording, frames)` for the persistent on-screen REC indicator.
    pub fn input_recording_status(&self) -> (bool, usize) {
        (
            self.playtest_input_tape.is_recording(),
            self.playtest_input_tape.frame_count(),
        )
    }

    /// Start a fresh recording, or stop and persist the active one. On the
    /// web the start side reboots the game first (cold-boot tape, poll 0)
    /// and the stop side downloads the tape as a CSV.
    pub fn toggle_input_recording(&mut self) {
        if self.playtest_input_tape.is_recording() {
            self.stop_input_recording_if_active();
            return;
        }
        let path = match self.active_input_tape_path() {
            Ok(path) => path,
            Err(error) => {
                self.status_message_set(format!("Input recording unavailable: {error}"));
                return;
            }
        };
        #[cfg(target_arch = "wasm32")]
        let _ = &path;
        // Web recordings are cold-boot tapes: reboot the game first so the
        // tape's poll clock counts from 0 and a later replay upload can
        // reproduce the whole run from the same fresh machine.
        #[cfg(target_arch = "wasm32")]
        if let Err(error) = self.reboot_current_web_game() {
            self.status_message_set(format!("Input recording unavailable: {error}"));
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(parent) = path.parent() {
            if let Err(error) = self.paths.ensure_dir(parent) {
                self.status_message_set(format!("Input recording unavailable: {error}"));
                return;
            }
        }
        let start_poll = self
            .bus
            .as_ref()
            .map(|bus| bus.port1_completed_polls())
            .unwrap_or(0);
        self.playtest_input_tape.start_recording(start_poll);
        #[cfg(not(target_arch = "wasm32"))]
        self.begin_input_profile_capture();
        self.running = true;
        self.menu.open = false;
        self.menu.sync_run_label(true);
        self.menu.sync_input_recording_label(true);
        #[cfg(target_arch = "wasm32")]
        self.status_message_set("Game rebooted; recording input from boot (F8 to download CSV)");
        #[cfg(not(target_arch = "wasm32"))]
        self.status_message_set(format!(
            "Input recording started (F8 to stop): {}",
            path.display()
        ));
    }

    /// Persist an active recording (native: tape file; web: CSV download).
    /// Safe in every exit path; a no-op while idle or replaying.
    pub fn stop_input_recording_if_active(&mut self) {
        if !self.playtest_input_tape.is_recording() {
            return;
        }
        let path = match self.active_input_tape_path() {
            Ok(path) => path,
            Err(error) => {
                self.status_message_set(format!("Input recording save failed: {error}"));
                return;
            }
        };
        let result = self.finish_input_recording(&path);
        #[cfg(not(target_arch = "wasm32"))]
        self.queue_input_profile_capture(&path, "record");
        self.menu.sync_input_recording_label(false);
        match result {
            Ok(frames) => {
                #[cfg(target_arch = "wasm32")]
                self.status_message_set(format!(
                    "Input recording downloaded: {frames} frames (CSV)"
                ));
                #[cfg(not(target_arch = "wasm32"))]
                self.status_message_set(format!(
                    "Input recording saved: {frames} frames → {}",
                    path.display()
                ));
            }
            Err(error) => {
                self.status_message_set(format!("Input recording save failed: {error}"));
            }
        }
    }

    /// Flush any dirty memory-card state on port 1 back to its
    /// `<config>/games/<id>/memcard-1.mcd` file. A no-op when no
    /// card is attached or when no writes have landed since load.
    /// Called from the shell's exit path and periodically during
    /// run so a hard crash doesn't lose save progress.
    pub fn flush_memcard_port1(&mut self) -> Result<(), String> {
        let Some(game) = self.current_game.as_ref().map(|g| g.id.clone()) else {
            return Ok(()); // no game loaded → nothing to persist
        };
        let Some(bus) = self.bus.as_mut() else {
            return Ok(());
        };
        if let Some(bytes) = bus.memcard_port1_snapshot() {
            let path = self.paths.memcard_file(&game, 1);
            self.paths
                .ensure_game_tree(&game)
                .map_err(|e| e.to_string())?;
            std::fs::write(&path, &bytes)
                .map_err(|e| format!("save memcard {}: {e}", path.display()))?;
            eprintln!(
                "[frontend] persisted port-1 memcard → {} ({} bytes)",
                path.display(),
                bytes.len()
            );
        }
        Ok(())
    }

    /// Decay the short-lived status message. Called once per frame
    /// with the frame's dt.
    pub fn tick_status(&mut self, dt: f32) {
        if let Some((_, ref mut ttl)) = self.status_message {
            *ttl -= dt;
            if *ttl <= 0.0 {
                self.status_message = None;
            }
        }
    }

    /// Show `msg` in the status toast for the standard TTL. Used
    /// by action handlers to surface success / failure from the
    /// Menu without allocating a whole notification subsystem.
    pub fn status_message_set(&mut self, msg: impl Into<String>) {
        self.status_message = Some((msg.into(), STATUS_MESSAGE_TTL_SECS));
    }

    /// Current output gain after the mute latch is applied.
    pub fn effective_audio_volume(&self) -> f32 {
        if self.audio_muted {
            0.0
        } else {
            self.audio_volume.clamp(0.0, 1.5)
        }
    }
}

/// Seconds a status toast stays visible.
const STATUS_MESSAGE_TTL_SECS: f32 = 3.5;

/// Format the right-aligned subtitle the Menu shows next to a
/// game's title. Keeps everything in one place so the Games and
/// Examples columns stay visually consistent.
fn format_subtitle(e: &LibraryEntry) -> String {
    let region = match e.region {
        Region::NtscU => "NTSC-U",
        Region::Pal => "PAL",
        Region::NtscJ => "NTSC-J",
        Region::Unknown => "",
    };
    let size = display_size_bytes(e);
    let size_mib = size / (1024 * 1024);
    match (region.is_empty(), e.kind) {
        (false, GameKind::DiscBin | GameKind::DiscIso | GameKind::DiscCue | GameKind::DiscCcd) => {
            format!("{region} · {size_mib} MiB")
        }
        (true, GameKind::DiscBin | GameKind::DiscIso | GameKind::DiscCue | GameKind::DiscCcd) => {
            format!("{size_mib} MiB")
        }
        (_, GameKind::Exe) => {
            if size < 1024 {
                format!("{size} B")
            } else if size < 1024 * 1024 {
                format!("{} KiB", size / 1024)
            } else {
                format!("{size_mib} MiB")
            }
        }
        _ => String::new(),
    }
}

/// Format a Unix timestamp (seconds) as a short "N ago" string
/// relative to now, for save-state menu row labels. No date/time
/// crate needed for this -- it's one coarse bucket, not a calendar.
/// Treats anything under 2 seconds as "just now" rather than showing
/// "0s ago" or "1s ago" (also absorbs small clock skew).
fn format_relative_time(unix_secs: u64) -> String {
    let now = unix_now_secs();
    let elapsed = now.saturating_sub(unix_secs);
    if elapsed < 2 {
        "just now".to_string()
    } else if elapsed < 60 {
        format!("{elapsed}s ago")
    } else if elapsed < 3600 {
        format!("{}m ago", elapsed / 60)
    } else if elapsed < 86_400 {
        format!("{}h ago", elapsed / 3600)
    } else {
        format!("{}d ago", elapsed / 86_400)
    }
}

#[cfg(target_arch = "wasm32")]
fn unix_now_secs() -> u64 {
    (js_sys::Date::now() / 1_000.0) as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn display_size_bytes(e: &LibraryEntry) -> u64 {
    if e.kind == GameKind::DiscCue {
        if let Some(bin) = psoxide_settings::library::primary_bin_from_cue(&e.path) {
            if let Ok(metadata) = std::fs::metadata(bin) {
                return metadata.len();
            }
        }
    }
    e.size
}

const PATH_LAUNCH_ID_PREFIX: &str = "path:";
const PROJECT_LAUNCH_ID_PREFIX: &str = "project-path:";

fn path_launch_id(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    format!("{PATH_LAUNCH_ID_PREFIX}{}", canonical.to_string_lossy())
}

fn project_build_launch_id(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    format!("{PROJECT_LAUNCH_ID_PREFIX}{}", canonical.to_string_lossy())
}

fn library_entry_for_launch_id<'a>(
    entries: &'a [LibraryEntry],
    launch_id: &str,
) -> Option<&'a LibraryEntry> {
    if let Some(path) = launch_id.strip_prefix(PATH_LAUNCH_ID_PREFIX) {
        let path = Path::new(path);
        return entries
            .iter()
            .find(|entry| paths_equivalent(&entry.path, path));
    }
    if let Some(path) = launch_id.strip_prefix(PROJECT_LAUNCH_ID_PREFIX) {
        let path = Path::new(path);
        return entries
            .iter()
            .find(|entry| paths_equivalent(&entry.path, path));
    }
    entries.iter().find(|entry| entry.id == launch_id)
}

fn is_internal_example_artifact(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(ext) = path.extension().and_then(|n| n.to_str()) else {
        return false;
    };
    if !stem.starts_with("editor-playtest")
        || !matches!(
            ext.to_ascii_lowercase().as_str(),
            "exe" | "bin" | "cue" | "iso"
        )
    {
        return false;
    }

    let mut parts = path.components().rev().filter_map(|component| {
        let std::path::Component::Normal(part) = component else {
            return None;
        };
        part.to_str()
    });
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next()
        ),
        (
            Some(_file),
            Some("release"),
            Some("mipsel-sony-psx"),
            Some("examples"),
            Some("build")
        )
    )
}

fn path_is_under(path: &Path, root: &Path) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => path.starts_with(root),
    }
}

fn public_example_source_items(
    built_examples: &std::collections::HashSet<String>,
) -> Vec<MenuLibraryItem> {
    let mut items = Vec::new();
    let root = repo_root_dir();
    for examples_root in [root.join("sdk/examples"), root.join("engine/examples")] {
        let Ok(entries) = std::fs::read_dir(&examples_root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join("Cargo.toml").is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == "editor-playtest" || built_examples.contains(&example_key(name)) {
                continue;
            }
            items.push(MenuLibraryItem {
                id: format!("example-source:{name}"),
                title: name.to_string(),
                subtitle: "not built".to_string(),
                burnable: false,
                launchable: false,
            });
        }
    }
    items
}

/// Fold the baked-in examples into a scanned Examples column.
///
/// Three cases, and the middle one is what shipped broken: a source checkout
/// lists every known example, including ones it has not compiled, as a "not
/// built" placeholder. Those placeholders are added AFTER `built` is collected,
/// so keying only on `built` let a baked copy sit next to the placeholder for
/// the same example and the row appeared twice. The web build has no source
/// tree, hence no placeholders, which is why it looked fine there.
///
/// - A real local build wins; the baked copy is dropped, so a checkout runs
///   what it just compiled rather than a stale bundled binary.
/// - A "not built" placeholder is REPLACED by the baked copy, which is
///   genuinely runnable.
/// - Otherwise the baked copy is appended.
fn merge_baked_examples(
    examples: &mut Vec<MenuLibraryItem>,
    built: &std::collections::HashSet<String>,
) {
    for baked in bundled::DISCS {
        if baked.kind != bundled::BundledKind::Exe {
            continue;
        }
        let key = example_key(baked.title);
        if built.contains(&key) {
            continue;
        }
        let item = MenuLibraryItem {
            id: baked.id.to_string(),
            title: baked.title.to_string(),
            subtitle: baked.subtitle.to_string(),
            burnable: false,
            launchable: true,
        };
        match examples
            .iter_mut()
            .find(|entry| example_key(&entry.title) == key)
        {
            Some(placeholder) => *placeholder = item,
            None => examples.push(item),
        }
    }
}

fn example_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// Pick the BIOS path the launcher should read, honouring
/// precedence: explicit settings field > env var. Centralised so
/// every normal frontend caller agrees and no local path leaks into
/// app defaults.
pub(crate) fn resolve_bios_path(settings: &Settings) -> Result<PathBuf, String> {
    let configured = settings.paths.bios.trim();
    if !configured.is_empty() {
        Ok(PathBuf::from(configured))
    } else if let Ok(p) = std::env::var("PSOXIDE_BIOS") {
        Ok(PathBuf::from(p))
    } else {
        Err("BIOS path is not configured. Open Settings and choose a BIOS image, or export PSOXIDE_BIOS.".to_string())
    }
}

pub(crate) fn bus_from_configured_bios(settings: &Settings) -> Result<Bus, String> {
    let bios_path = resolve_bios_path(settings)?;
    let bios =
        std::fs::read(&bios_path).map_err(|e| format!("BIOS {}: {e}", bios_path.display()))?;
    Bus::new(bios).map_err(|e| format!("BIOS rejected: {e}"))
}

// Only the native file-dialog helpers (`choose_*_path`) use this to seed the
// dialog's starting directory; the web build has no native dialog.
#[cfg(not(target_arch = "wasm32"))]
fn path_parent_or_self(value: &str) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    let path = PathBuf::from(value);
    if path.is_dir() {
        Some(path)
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

// Only the native file-path UI uses this; the web build shows "Loaded"/"None".
#[cfg(not(target_arch = "wasm32"))]
fn path_label(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// Record a retired instruction into the ring buffer, evicting the
/// oldest entry when capacity is reached.
///
/// Free-function rather than a method so callers can borrow `AppState`
/// fields disjointly: `state.bus`, `state.cpu`, and
/// `state.exec_history` often need to be held mutably at once inside
/// the step loop, which a `&mut self` method would block.
pub fn push_history(history: &mut VecDeque<InstructionRecord>, record: InstructionRecord) {
    if history.len() >= EXEC_HISTORY_CAP {
        history.pop_front();
    }
    history.push_back(record);
}

/// Guest-side work performed while advancing one video frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepFrameReport {
    /// Cycle budget for the target video frame.
    pub target_cycles: u64,
    /// Bus cycles actually advanced.
    pub cycles: u64,
    /// CPU instructions retired.
    pub instructions: u64,
    /// VBlank IRQ raises observed while stepping.
    pub vblanks: u64,
    /// True when the safety instruction cap stopped the frame early.
    pub hit_step_cap: bool,
}

/// Retire enough instructions to cover one PSX video frame's worth of
/// master-clock cycles. Any execution error auto-pauses, reopens the
/// Menu, and surfaces the stopped state via the register panel. Hitting
/// a breakpoint does the same. Split out here (rather than living in
/// the shell loop) so both the shell's per-frame run path and the
/// toolbar's "advance one frame" button can invoke the same logic.
/// Per-frame freelook integration: accumulate held-key deltas into the camera
/// pose. ponytail: rates and signs are consts, easy to tune or flip if a
/// direction feels inverted on real content (translation is in GTE view units).
fn integrate_freelook(fl: &mut emulator_core::FreelookState, input: &FreelookInput) {
    const LOOK: f32 = 0.03; // radians per frame at full deflection
    const MOVE: f32 = 24.0; // view units per frame at full deflection
    const DEADZONE: f32 = 0.15; // ignore stick drift / resting jitter

    // Radial deadzone + rescale so the usable range still spans 0..1.
    let apply = |v: f32| -> f32 {
        let a = v.abs();
        if a <= DEADZONE {
            0.0
        } else {
            ((a - DEADZONE) / (1.0 - DEADZONE)).min(1.0) * v.signum()
        }
    };
    let (lx, ly) = (apply(input.left.0), apply(input.left.1));
    let (rx, ry) = (apply(input.right.0), apply(input.right.1));

    let boost = if input.boost { 4.0 } else { 1.0 };
    let (look, mv) = (LOOK * boost, MOVE * boost);

    // Right stick looks: right = yaw right, up = pitch up. Signs match the
    // old key scheme (yaw_right did -=, pitch_up did +=).
    fl.yaw -= look * rx;
    fl.pitch += look * ry;
    // Left stick moves in the rotated (look-relative) view frame: up = dolly
    // forward (tz -=), right = strafe right (tx -=).
    fl.tz -= mv * ly;
    fl.tx -= mv * lx;
}

/// Camera state sent to the projection hook for this frame.
///
/// `FreelookState::enabled` doubles as the frontend's "pad controls the
/// camera" flag. Once those controls are released, a non-identity camera
/// delta must still be projected so gameplay continues from the framing the
/// user chose. Only an explicit reset removes that delta.
fn freelook_for_projection(mut fl: emulator_core::FreelookState) -> emulator_core::FreelookState {
    let has_camera_delta =
        fl.yaw != 0.0 || fl.pitch != 0.0 || fl.tx != 0.0 || fl.ty != 0.0 || fl.tz != 0.0;
    fl.enabled |= has_camera_delta;
    fl
}

#[cfg(test)]
mod freelook_projection_tests {
    use super::freelook_for_projection;
    use emulator_core::FreelookState;

    #[test]
    fn moved_camera_stays_projected_after_controls_are_released() {
        let moved = FreelookState {
            enabled: false,
            yaw: 0.25,
            tx: 64.0,
            ..Default::default()
        };

        let projected = freelook_for_projection(moved);

        assert!(projected.enabled);
        assert_eq!(projected.yaw, moved.yaw);
        assert_eq!(projected.tx, moved.tx);
    }

    #[test]
    fn reset_camera_stops_projecting_after_controls_are_released() {
        assert!(!freelook_for_projection(FreelookState::default()).enabled);
    }
}

pub fn step_one_frame(state: &mut AppState) -> StepFrameReport {
    let max_steps = state.run_steps_per_frame.max(1);
    // Freelook: integrate held keys into the camera pose, then push it to the
    // GTE hook for this frame (a no-op while the toggle is off).
    if state.freelook.enabled {
        integrate_freelook(&mut state.freelook, &state.freelook_input);
    }
    state
        .cpu
        .set_freelook(freelook_for_projection(state.freelook));
    let Some(bus) = state.bus.as_mut() else {
        state.running = false;
        state.menu.sync_run_label(false);
        return StepFrameReport::default();
    };

    // Only fill `exec_history` while the register section can be
    // inspected; otherwise the 404-byte `InstructionRecord` per step
    // is pure overhead.
    let trace = state.panels.debug_sidebar && state.panels.registers;
    // Hash-set lookups aren't free at ~250K steps/frame; skip the
    // per-instruction breakpoint probe entirely in the common
    // no-breakpoints case.
    let check_breakpoints = !state.breakpoints.is_empty();
    let cycles_before = bus.cycles();
    let tick_before = state.cpu.tick();
    let vblank_before = bus.irq().raise_counts()[0];
    let frame_budget = bus.vblank_period().max(1);
    let target_cycles = cycles_before.saturating_add(frame_budget);
    let mut steps_run = 0;
    for _ in 0..max_steps {
        if bus.cycles() >= target_cycles {
            break;
        }
        // Breakpoint check happens BEFORE stepping so the paused PC
        // is the BP address itself -- the instruction at that PC has
        // not yet executed.
        if check_breakpoints && state.breakpoints.contains(&state.cpu.pc()) {
            state.running = false;
            state.menu.sync_run_label(false);
            state.menu.open = true;
            break;
        }
        steps_run += 1;

        let result = if trace {
            match state.cpu.step_traced(bus) {
                Ok(record) => {
                    push_history(&mut state.exec_history, record);
                    Ok(())
                }
                Err(e) => Err(e),
            }
        } else {
            state.cpu.step(bus)
        };
        if result.is_err() {
            state.running = false;
            state.menu.sync_run_label(false);
            state.menu.open = true;
            break;
        }
    }

    let cycles_after = bus.cycles();
    let vblank_after = bus.irq().raise_counts()[0];
    StepFrameReport {
        target_cycles: frame_budget,
        cycles: cycles_after.saturating_sub(cycles_before),
        instructions: state.cpu.tick().saturating_sub(tick_before),
        vblanks: vblank_after.saturating_sub(vblank_before),
        hit_step_cap: steps_run >= max_steps && cycles_after < target_cycles && state.running,
    }
}

/// Fast-boot an embedded editor playtest disc through the same no-BIOS path
/// used by the in-editor Play viewport. Reached via the editor Play path and
/// the headless CLI; both are compiled out on wasm, so it is dead there.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn fast_boot_embedded_playtest_disc(
    bus: &mut Bus,
    cpu: &mut Cpu,
    disc: &Disc,
    path: &Path,
) {
    match fast_boot_disc_with_hle(bus, cpu, disc, true) {
        Ok(info) => {
            eprintln!(
                "[frontend] embedded Play disc fast-booted {} via {} entry=0x{:08x} payload={}B",
                path.display(),
                info.boot_path,
                info.initial_pc,
                info.payload_len
            );
        }
        Err(e) => {
            eprintln!(
                "[frontend] embedded Play disc fast boot unavailable for {} ({e:?}); falling back to BIOS boot",
                path.display()
            );
        }
    }
}

/// Input-tape change-detection hash for a modelled disc: every track's raw
/// bytes in order, hashed as one stream. A single-track bin matches
/// [`emulator_core::game_image_hash`] of the raw file.
fn disc_image_hash(disc: &Disc) -> u64 {
    emulator_core::game_image_hash_parts(
        (0u8..=99).filter_map(|number| disc.track(number).map(|track| track.bytes.as_slice())),
    )
}

fn maybe_fast_boot_disc(
    bus: &mut Bus,
    cpu: &mut Cpu,
    disc: &Disc,
    entry: &LibraryEntry,
    enabled: bool,
) -> &'static str {
    maybe_fast_boot_disc_path(bus, cpu, disc, &entry.path, enabled)
}

/// The settings field a rebind target reads from. Kept as a pair of
/// free functions (rather than one `&mut` accessor used for reads too)
/// so label-building can borrow the settings immutably.
fn binding_for_target(
    b: &psoxide_settings::settings::PortBindings,
    target: PadBindTarget,
) -> &psoxide_settings::InputBinding {
    match target {
        PadBindTarget::Up => &b.up,
        PadBindTarget::Down => &b.down,
        PadBindTarget::Left => &b.left,
        PadBindTarget::Right => &b.right,
        PadBindTarget::Cross => &b.cross,
        PadBindTarget::Circle => &b.circle,
        PadBindTarget::Square => &b.square,
        PadBindTarget::Triangle => &b.triangle,
        PadBindTarget::L1 => &b.l1,
        PadBindTarget::L2 => &b.l2,
        PadBindTarget::R1 => &b.r1,
        PadBindTarget::R2 => &b.r2,
        PadBindTarget::Start => &b.start,
        PadBindTarget::Select => &b.select,
        PadBindTarget::L3 => &b.l3,
        PadBindTarget::R3 => &b.r3,
        PadBindTarget::Analog => &b.analog,
        PadBindTarget::LStickUp => &b.left_stick.up,
        PadBindTarget::LStickDown => &b.left_stick.down,
        PadBindTarget::LStickLeft => &b.left_stick.left,
        PadBindTarget::LStickRight => &b.left_stick.right,
        PadBindTarget::RStickUp => &b.right_stick.up,
        PadBindTarget::RStickDown => &b.right_stick.down,
        PadBindTarget::RStickLeft => &b.right_stick.left,
        PadBindTarget::RStickRight => &b.right_stick.right,
    }
}

/// Mutable twin of [`binding_for_target`], for rebind writes.
fn binding_for_target_mut(
    b: &mut psoxide_settings::settings::PortBindings,
    target: PadBindTarget,
) -> &mut psoxide_settings::InputBinding {
    match target {
        PadBindTarget::Up => &mut b.up,
        PadBindTarget::Down => &mut b.down,
        PadBindTarget::Left => &mut b.left,
        PadBindTarget::Right => &mut b.right,
        PadBindTarget::Cross => &mut b.cross,
        PadBindTarget::Circle => &mut b.circle,
        PadBindTarget::Square => &mut b.square,
        PadBindTarget::Triangle => &mut b.triangle,
        PadBindTarget::L1 => &mut b.l1,
        PadBindTarget::L2 => &mut b.l2,
        PadBindTarget::R1 => &mut b.r1,
        PadBindTarget::R2 => &mut b.r2,
        PadBindTarget::Start => &mut b.start,
        PadBindTarget::Select => &mut b.select,
        PadBindTarget::L3 => &mut b.l3,
        PadBindTarget::R3 => &mut b.r3,
        PadBindTarget::Analog => &mut b.analog,
        PadBindTarget::LStickUp => &mut b.left_stick.up,
        PadBindTarget::LStickDown => &mut b.left_stick.down,
        PadBindTarget::LStickLeft => &mut b.left_stick.left,
        PadBindTarget::LStickRight => &mut b.left_stick.right,
        PadBindTarget::RStickUp => &mut b.right_stick.up,
        PadBindTarget::RStickDown => &mut b.right_stick.down,
        PadBindTarget::RStickLeft => &mut b.right_stick.left,
        PadBindTarget::RStickRight => &mut b.right_stick.right,
    }
}

fn maybe_fast_boot_disc_path(
    bus: &mut Bus,
    cpu: &mut Cpu,
    disc: &Disc,
    path: &Path,
    enabled: bool,
) -> &'static str {
    if !enabled {
        return "BIOS boot";
    }
    if let Err(e) = warm_bios_for_disc_fast_boot(bus, cpu, DISC_FAST_BOOT_WARMUP_STEPS) {
        eprintln!(
            "[frontend] BIOS warmup failed for {} ({e:?}); falling back to BIOS boot",
            path.display()
        );
        return "BIOS boot";
    }
    match fast_boot_disc_with_hle(bus, cpu, disc, false) {
        Ok(info) => {
            eprintln!(
                "[frontend] warm-fast-booted {} via {} entry=0x{:08x} load=0x{:08x} payload={}B",
                path.display(),
                info.boot_path,
                info.initial_pc,
                info.load_addr,
                info.payload_len
            );
            "fast boot"
        }
        Err(e) => {
            eprintln!(
                "[frontend] fast boot unavailable for {} ({e:?}); falling back to BIOS boot",
                path.display()
            );
            "BIOS boot"
        }
    }
}

fn load_initial_bus(settings: &Settings, cpu: &mut Cpu) -> Option<Bus> {
    if let Some((exe, exe_path)) = load_exe() {
        let mut bus = Bus::new_without_bios();
        bus.load_exe_payload(exe.load_addr, &exe.payload);
        bus.clear_exe_bss(exe.bss_addr, exe.bss_size);
        cpu.seed_from_exe(exe.initial_pc, exe.initial_gp, exe.initial_sp());
        bus.enable_hle_bios();
        bus.attach_digital_pad_port1();
        if let Some(disc) = load_disc() {
            bus.cdrom.insert_disc(Some(disc));
        } else {
            match load_sidecar_disc_for_exe(&exe_path) {
                Ok(Some(disc)) => bus.cdrom.insert_disc(Some(disc)),
                Ok(None) => {}
                Err(e) => eprintln!("[frontend] sidecar disc load failed: {e}"),
            }
        }
        eprintln!(
            "[frontend] side-loaded EXE: entry=0x{:08x} payload={}B (hle-bios + pad1)",
            exe.initial_pc,
            exe.payload.len()
        );
        return Some(bus);
    }
    load_bus(settings)
}

fn load_bus(settings: &Settings) -> Option<Bus> {
    let path = match resolve_bios_path(settings) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("[frontend] {e}");
            return None;
        }
    };
    let mut bus = match std::fs::read(&path) {
        Ok(bytes) => match Bus::new(bytes) {
            Ok(bus) => bus,
            Err(e) => {
                eprintln!("[frontend] BIOS at {} rejected: {e}", path.display());
                return None;
            }
        },
        Err(e) => {
            eprintln!("[frontend] no BIOS at {}: {e}", path.display());
            return None;
        }
    };

    // Optional disc. Absence is not an error -- BIOS boots fine without
    // one and just sits on the "insert disc" screen. Presence wires the
    // bytes into the CD-ROM controller's tray so `CdlGetID` / `CdlReadN`
    // return real data once the BIOS/game asks.
    if let Some(disc) = load_disc() {
        bus.cdrom.insert_disc(Some(disc));
    }

    Some(bus)
}

/// Read `PSOXIDE_EXE` → PSX-EXE file → parsed `Exe`. Logs and returns
/// `None` on any trouble so a misconfigured path doesn't wedge boot.
fn load_exe() -> Option<(Exe, PathBuf)> {
    let var = std::env::var("PSOXIDE_EXE").ok()?;
    let path = PathBuf::from(&var);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[frontend] PSOXIDE_EXE={} unreadable: {e}", path.display());
            return None;
        }
    };
    match Exe::parse(&bytes) {
        Ok(exe) => Some((exe, path)),
        Err(e) => {
            eprintln!("[frontend] PSOXIDE_EXE={} malformed: {e:?}", path.display());
            None
        }
    }
}

/// Read `PSOXIDE_DISC` → disc image → `Disc`. Accepts raw BIN/ISO and
/// CUE-backed multitrack images. Logs and returns `None` on any trouble
/// so a misconfigured path doesn't wedge the frontend.
fn load_disc() -> Option<Disc> {
    let var = std::env::var("PSOXIDE_DISC").ok()?;
    let path = PathBuf::from(&var);
    match load_authored_disc(&path) {
        Ok(disc) => {
            eprintln!(
                "[frontend] mounted disc {} ({} sectors)",
                path.display(),
                disc.sector_count()
            );
            Some(disc)
        }
        Err(error) => {
            eprintln!(
                "[frontend] PSOXIDE_DISC={} unreadable: {error}",
                path.display()
            );
            None
        }
    }
}

fn load_authored_disc(path: &Path) -> Result<Disc, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "cue" {
        psoxide_settings::library::load_disc_from_cue(path).map_err(|error| error.to_string())
    } else {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if bytes.len() < SECTOR_BYTES {
            return Err(format!(
                "too small ({} bytes, need at least {SECTOR_BYTES})",
                bytes.len()
            ));
        }
        Ok(Disc::from_bin(bytes))
    }
}

fn load_sidecar_disc_for_exe(exe_path: &Path) -> Result<Option<Disc>, String> {
    let cue_path = exe_path.with_extension("cue");
    if cue_path.is_file() {
        let disc = psoxide_settings::library::load_disc_from_cue(&cue_path)
            .map_err(|e| format!("{}: {e}", cue_path.display()))?;
        eprintln!(
            "[frontend] mounted sidecar CUE {} ({} sectors)",
            cue_path.display(),
            disc.sector_count()
        );
        return Ok(Some(disc));
    }

    let ccd_path = exe_path.with_extension("ccd");
    if ccd_path.is_file() {
        let disc = psoxide_settings::library::load_disc_from_ccd(&ccd_path)
            .map_err(|e| format!("{}: {e}", ccd_path.display()))?;
        eprintln!(
            "[frontend] mounted sidecar CCD {} ({} sectors)",
            ccd_path.display(),
            disc.sector_count()
        );
        return Ok(Some(disc));
    }

    Ok(None)
}

/// Build all panels/overlays for one frame. Called from `gfx::Graphics::render`
/// inside the egui context. `dt` drives Menu animations.
pub fn build_ui(
    ctx: &egui::Context,
    state: &mut AppState,
    input_router: &mut crate::input::InputRouter,
    vram_tex: egui::TextureId,
    display_tex: egui::TextureId,
    #[cfg(feature = "editor")] editor_viewport: psxed_ui::EditorViewport3dPresentation,
    display_uv: egui::Rect,
    dt: f32,
) {
    ui::draw_layout(
        ctx,
        state,
        input_router,
        vram_tex,
        display_tex,
        #[cfg(feature = "editor")]
        editor_viewport,
        display_uv,
        dt,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_run_profile_is_written_next_to_its_input_tape() {
        let root = frontend_test_temp_dir("input-high-water");
        let tape_path = root.join("routes").join("cortex.pxtape");
        let mut state = AppState::with_config_dir(Some(root.clone()));
        state.begin_input_profile_capture();

        let mut sample = ui::profiler::FrameProfileSample {
            total_ms: 17.5,
            emu_ms: 12.0,
            ..ui::profiler::FrameProfileSample::default()
        };
        sample.guest.counter_latest_values
            [emulator_core::telemetry::counter::TRI_PRIMITIVES as usize] = 1080;
        sample.guest.counter_latest_values
            [emulator_core::telemetry::counter::TRI_PRIMITIVE_REMAINING as usize] = 456;
        sample.guest.counter_max_values
            [emulator_core::telemetry::counter::TRI_PRIMITIVES as usize] = 1080.0;
        sample.guest.counters[emulator_core::telemetry::counter::TRI_PRIMITIVES as usize] = 1080.0;
        state.profiler.record(sample);
        state.queue_input_profile_capture(&tape_path, "record");
        state.flush_pending_input_profile_capture();

        let profile_path = root.join("routes").join("cortex.record.profile.csv");
        let csv = std::fs::read_to_string(&profile_path).unwrap();
        assert!(
            csv.starts_with("kind,id,name,total,hits,average,max,latest,capacity,peak_percent\n")
        );
        assert!(csv.contains("counter,1,tri prims,1080"));
        assert!(csv.contains(",1536,70.31"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "editor")]
    #[test]
    fn editor_wireframe_request_updates_the_live_gpu() {
        let root = frontend_test_temp_dir("editor-wireframe");
        let mut state = AppState::with_config_dir(Some(root.clone()));
        state.bus = Some(Bus::new_without_bios());

        state.handle_editor_playtest_request(psxed_ui::EditorPlaytestRequest::SetWireframe {
            enabled: true,
        });
        assert!(state.bus.as_ref().unwrap().gpu.wireframe_enabled);

        state.handle_editor_playtest_request(psxed_ui::EditorPlaytestRequest::SetWireframe {
            enabled: false,
        });
        assert!(!state.bus.as_ref().unwrap().gpu.wireframe_enabled);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn internal_editor_playtest_artifacts_are_hidden_from_menu() {
        assert!(is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/editor-playtest.exe"
        )));
        assert!(is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/editor-playtest.bin"
        )));
        assert!(is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/editor-playtest.cue"
        )));
        assert!(is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/editor-playtest.iso"
        )));
        assert!(is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/editor-playtest-cortex-ignition-v1-profile.bin"
        )));
        assert!(!is_internal_example_artifact(Path::new(
            "build/examples/mipsel-sony-psx/release/hello-cdda.exe"
        )));
        assert!(!is_internal_example_artifact(Path::new(
            "/games/editor-playtest.bin"
        )));
    }

    #[test]
    fn public_example_placeholders_are_discovered_from_source_dirs() {
        let built = std::collections::HashSet::from([example_key("hello-tri")]);
        let examples = public_example_source_items(&built);
        assert!(examples
            .iter()
            .any(|entry| entry.title == "game-pong" && !entry.launchable));
        assert!(examples
            .iter()
            .all(|entry| entry.title != "editor-playtest"));
        assert!(examples.iter().all(|entry| entry.title != "hello-tri"));
    }

    #[test]
    fn project_build_launch_ids_resolve_by_path_when_disc_ids_collide() {
        let root = frontend_test_temp_dir("project-build-launch-ids");
        let a_path = root.join("demo4").join("baked").join("demo4.cue");
        let b_path = root
            .join("cortex_ignition_v1")
            .join("baked")
            .join("cortex_ignition_v1.cue");
        std::fs::create_dir_all(a_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b_path.parent().unwrap()).unwrap();
        std::fs::write(&a_path, b"disc a").unwrap();
        std::fs::write(&b_path, b"disc b").unwrap();

        let entries = vec![
            LibraryEntry {
                id: "same-disc-id".to_string(),
                path: a_path.clone(),
                kind: GameKind::DiscCue,
                title: "PSOXIDE".to_string(),
                region: Region::Unknown,
                size: 6,
                mtime: 0,
                diagnostic: None,
            },
            LibraryEntry {
                id: "same-disc-id".to_string(),
                path: b_path.clone(),
                kind: GameKind::DiscCue,
                title: "PSOXIDE".to_string(),
                region: Region::Unknown,
                size: 6,
                mtime: 0,
                diagnostic: None,
            },
        ];

        assert_eq!(
            library_entry_for_launch_id(&entries, "same-disc-id")
                .unwrap()
                .path,
            a_path
        );
        assert_eq!(
            library_entry_for_launch_id(&entries, &project_build_launch_id(&b_path))
                .unwrap()
                .path,
            b_path
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn path_launch_ids_resolve_duplicate_disc_ids_by_path() {
        let root = frontend_test_temp_dir("example-launch-ids");
        let a_path = root.join("hello-tex.cue");
        let b_path = root.join("hello-tri.cue");
        std::fs::write(&a_path, b"disc a").unwrap();
        std::fs::write(&b_path, b"disc b").unwrap();

        let entries = vec![
            LibraryEntry {
                id: "same-disc-id".to_string(),
                path: a_path.clone(),
                kind: GameKind::DiscCue,
                title: "hello-tex".to_string(),
                region: Region::Unknown,
                size: 6,
                mtime: 0,
                diagnostic: None,
            },
            LibraryEntry {
                id: "same-disc-id".to_string(),
                path: b_path.clone(),
                kind: GameKind::DiscCue,
                title: "hello-tri".to_string(),
                region: Region::Unknown,
                size: 6,
                mtime: 0,
                diagnostic: None,
            },
        ];

        assert_eq!(
            library_entry_for_launch_id(&entries, "same-disc-id")
                .unwrap()
                .path,
            a_path
        );
        assert_eq!(
            library_entry_for_launch_id(&entries, &path_launch_id(&b_path))
                .unwrap()
                .path,
            b_path
        );

        let _ = std::fs::remove_dir_all(root);
    }

    fn frontend_test_temp_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "psoxide-frontend-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(test)]
mod baked_example_merge_tests {
    use super::{bundled, example_key, merge_baked_examples, MenuLibraryItem};

    fn item(title: &str, subtitle: &str, launchable: bool) -> MenuLibraryItem {
        MenuLibraryItem {
            id: format!("scanned:{title}"),
            title: title.to_string(),
            subtitle: subtitle.to_string(),
            burnable: false,
            launchable,
        }
    }

    fn a_baked_example() -> &'static bundled::BundledDisc {
        bundled::DISCS
            .iter()
            .find(|d| d.kind == bundled::BundledKind::Exe)
            .expect("at least one baked example ships")
    }

    /// The shipped bug: a source checkout lists uncompiled examples as "not
    /// built", and the baked copy was appended beside one instead of replacing
    /// it, so the row showed twice.
    #[test]
    fn a_not_built_placeholder_is_replaced_not_duplicated() {
        let baked = a_baked_example();
        let mut examples = vec![item(baked.title, "not built", false)];
        merge_baked_examples(&mut examples, &Default::default());

        let matching: Vec<_> = examples
            .iter()
            .filter(|e| example_key(&e.title) == example_key(baked.title))
            .collect();
        assert_eq!(matching.len(), 1, "one row per example, got {examples:?}");
        assert!(matching[0].launchable, "the surviving row must be runnable");
        assert_eq!(matching[0].id, baked.id, "the baked copy should have won");
    }

    /// A real local build beats the bundled one, so a checkout runs what it
    /// just compiled.
    #[test]
    fn a_local_build_wins_over_the_baked_copy() {
        let baked = a_baked_example();
        let mut examples = vec![item(baked.title, "EXE · 118 KiB", true)];
        let built = std::iter::once(example_key(baked.title)).collect();
        merge_baked_examples(&mut examples, &built);

        let matching: Vec<_> = examples
            .iter()
            .filter(|e| example_key(&e.title) == example_key(baked.title))
            .collect();
        assert_eq!(matching.len(), 1);
        assert!(
            matching[0].id.starts_with("scanned:"),
            "the scanned build should have been kept"
        );
    }

    /// With nothing scanned at all -- the web build, and any download -- every
    /// baked example is appended exactly once.
    #[test]
    fn an_empty_column_gets_each_baked_example_once() {
        let mut examples = Vec::new();
        merge_baked_examples(&mut examples, &Default::default());

        let expected = bundled::DISCS
            .iter()
            .filter(|d| d.kind == bundled::BundledKind::Exe)
            .count();
        assert_eq!(examples.len(), expected);
        let mut keys: Vec<_> = examples.iter().map(|e| example_key(&e.title)).collect();
        keys.sort();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before, "no example may appear twice");
    }
}

// The streamed delivery's decode invariant: claxon must reproduce the flac
// CLI's bytes exactly, decoding whole blocks with the reader recreated
// between budget slices the way the web build does. Dormant without the env
// vars; point them at any track piece to re-verify (this caught a real bug:
// a sample iterator dropped mid-block silently discards the block's tail).
#[cfg(all(test, not(target_arch = "wasm32")))]
mod claxon_parity {
    #[test]
    fn claxon_matches_the_flac_cli() {
        let Ok(flac_path) = std::env::var("PROBE_FLAC") else {
            return;
        };
        let Ok(raw_path) = std::env::var("PROBE_RAW") else {
            return;
        };
        let flac_bytes = std::fs::read(&flac_path).unwrap();
        let reference = std::fs::read(&raw_path).unwrap();
        // Decode the way the web build does: whole blocks, with the block
        // reader recreated between budget slices, which must resume cleanly.
        let mut reader = claxon::FlacReader::new(std::io::Cursor::new(flac_bytes)).unwrap();
        let mut out = Vec::with_capacity(reference.len());
        let mut buffer = Vec::new();
        'outer: loop {
            let mut frames = reader.blocks();
            for _ in 0..8 {
                match frames.read_next_or_eof(core::mem::take(&mut buffer)) {
                    Ok(Some(block)) => {
                        for i in 0..block.duration() {
                            out.extend_from_slice(&(block.sample(0, i) as i16).to_le_bytes());
                            out.extend_from_slice(&(block.sample(1, i) as i16).to_le_bytes());
                        }
                        buffer = block.into_buffer();
                    }
                    Ok(None) | Err(_) => break 'outer,
                }
            }
        }
        eprintln!(
            "PROBE: claxon {} bytes, reference {}",
            out.len(),
            reference.len()
        );
        let diff = out.iter().zip(reference.iter()).position(|(a, b)| a != b);
        eprintln!("PROBE: first differing byte {diff:?}");
        assert_eq!(out.len(), reference.len());
        assert_eq!(diff, None);
    }
}
