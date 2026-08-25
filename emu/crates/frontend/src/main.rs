// SPDX-License-Identifier: GPL-2.0-or-later
//! PSoXide desktop frontend.
//!
//! Modular layout:
//! - `theme`   -- fonts, colors, framed-section helpers.
//! - `icons`   -- Lucide codepoint constants.
//! - `gfx`     -- winit window + wgpu surface + egui-wgpu plumbing.
//! - `app`     -- top-level state, UI orchestration entry point.
//! - `ui/*`    -- individual panels (central, registers, vram, menu, hud).

#![warn(missing_docs)]

mod app;
mod app_icon;
mod audio;
mod burn;
// Browser file upload (BIOS + game). wasm-only: the native build uses rfd.
#[cfg(target_arch = "wasm32")]
mod web_files;
// Same-origin streamed discs (the demo disc). wasm-only: native has a library.
#[cfg(target_arch = "wasm32")]
mod web_stream;
// The headless CLI (`scan`/`list`/`launch`/...) is a native developer tool:
// it reads argv, the filesystem, and spins up its own offscreen wgpu device.
// None of that applies in the browser, so it is compiled out on wasm and the
// web entry point goes straight to the GUI.
#[cfg(not(target_arch = "wasm32"))]
mod cli;
mod disasm;
#[cfg(feature = "editor")]
mod editor_assets;
#[cfg(feature = "editor")]
mod editor_preview;
#[cfg(feature = "editor")]
mod editor_textures;
#[cfg(feature = "editor")]
mod embedded_playtest;
mod gfx;
mod icons;
mod input;
#[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
mod mcp;
#[cfg(feature = "editor")]
mod playtest_disc;
mod playtest_input;
mod theme;
mod ui;

use std::path::{Path, PathBuf};
use std::sync::Arc;
// `web_time::Instant` is a drop-in for `std::time::Instant`: on native it
// re-exports the std type, and on wasm it reads the browser performance clock
// instead of std's clock, which panics on wasm32-unknown-unknown.
use web_time::{Duration, Instant};

#[cfg(not(target_arch = "wasm32"))]
use clap::Parser;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::app::AppState;
#[cfg(not(target_arch = "wasm32"))]
use crate::cli::Cli;
use crate::gfx::Graphics;
use crate::playtest_input::Port1PadSample;
use crate::ui::profiler::FrameProfileSample;
use crate::ui::{menu::MenuInput, MenuOutcome};

use emulator_core::button;
// `counter` / `task` telemetry IDs feed the editor's Play metrics overlay only.
#[cfg(feature = "editor")]
use emulator_core::telemetry::{counter, task};
use psoxide_settings::settings::{InputBinding, PortBindings, StickBindings};

/// Default window size when not running fullscreen. Chosen big
/// enough to show the Menu + a framebuffer comfortably on a
/// standard laptop display.
const INITIAL_WIDTH: u32 = 1600;
const INITIAL_HEIGHT: u32 = 1000;
/// Keep the toolbar usable: full debug controls + boot toggle +
/// volume slider + transport buttons need roughly 700 logical px on
/// Retina displays, and the initial window is already larger.
const MIN_WIDTH: u32 = 1400;
const MIN_HEIGHT: u32 = 700;
/// PSX CPU clock used to convert the active machine's exact VBlank period to
/// wall time. NTSC is about 59.29 Hz here, not 60 Hz; forcing 60 Hz steadily
/// overproduces SPU samples until the host queue has to discard them.
const PSX_MASTER_CLOCK_HZ: f32 = 33_868_800.0;
const FALLBACK_FRAME_DT: f32 = 1.0 / 60.0;
/// Used only by the editor's Play metrics overlay (cycles -> milliseconds).
#[cfg(feature = "editor")]
const PSX_CYCLES_PER_MS: f32 = 33_868_800.0 / 1000.0;
/// Don't try to catch up an arbitrarily long stall in one redraw;
/// cap the burst so a debugger stop or window drag doesn't spend
/// seconds chewing through delayed emu frames.
const MAX_CATCHUP_FRAMES: u32 = 4;

fn guest_frame_dt(vblank_period: Option<u64>) -> f32 {
    vblank_period
        .filter(|period| *period != 0)
        .map(|period| period as f32 / PSX_MASTER_CLOCK_HZ)
        .unwrap_or(FALLBACK_FRAME_DT)
}
/// Paused redraw cadence while interaction is plausible: a pad was
/// recently used, the Menu overlay is up, or the editor workspace is
/// front. Matches the run cadence so pad-driven menu navigation feels
/// identical paused or running.
#[cfg(not(target_arch = "wasm32"))]
const ACTIVE_TICK_DT: f32 = 1.0 / 60.0;
/// Paused redraw cadence when fully idle. Redraws still happen (each
/// one drains queued MCP tool calls and polls gilrs for pad hot-plug /
/// first input) but at a rate whose UI cost rounds to nothing.
#[cfg(not(target_arch = "wasm32"))]
const IDLE_TICK_DT: f32 = 1.0 / 15.0;
/// How long after the last gamepad edge the paused shell stays at the
/// active tick. Pad input arrives by polling, not as window events, so
/// it cannot wake the loop itself; this window keeps controller menu
/// navigation at the active cadence between presses.
#[cfg(not(target_arch = "wasm32"))]
const PAD_ACTIVITY_WINDOW_SECS: f32 = 1.5;

fn elapsed_ms(start: Instant) -> f32 {
    start.elapsed().as_secs_f32() * 1000.0
}

// Path helpers shared by `app` and the editor-gated `playtest_disc` module,
// kept private at the crate root so both sibling modules can reach them.
fn repo_root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
}

fn paths_equivalent(path: &Path, other: &Path) -> bool {
    match (path.canonicalize(), other.canonicalize()) {
        (Ok(path), Ok(other)) => path == other,
        _ => path == other,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    // Argument parsing first -- if a subcommand is present, we
    // dispatch through the headless CLI and never open a window.
    // Clap's derive API panics with a nicely-formatted message on
    // bad arguments, which is exactly what a CLI user expects.
    let cli = Cli::parse();
    if cli.command.is_some() {
        if let Err(e) = cli::run(cli) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // `--config-dir` also applies to the GUI path -- lets testers
    // point the app at a scratch directory without touching their
    // real settings. The GUI defaults to borderless-fullscreen;
    // `--windowed` opts back into a regular floating window for
    // development next to a terminal / docs.
    let config_dir = cli.config_dir;
    let fullscreen = !cli.windowed;
    let gpu_compute = cli.gpu_compute;

    #[cfg(feature = "editor")]
    let editor_startup = cli.editor.then_some((
        cli.editor_project,
        cli.editor_view.map(cli::EditorViewArg::project_view),
        cli.editor_view
            .is_some_and(cli::EditorViewArg::is_room_orthographic),
        cli.editor_resource,
    ));

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = Shell::new(config_dir, fullscreen, gpu_compute);
    #[cfg(feature = "editor")]
    if let Some((project_dir, view, room_orthographic, resource)) = editor_startup {
        if let Err(error) = app
            .state
            .open_editor_startup(project_dir, view, resource.as_deref())
        {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
        if room_orthographic {
            app.state.show_editor_room_orthographic();
        }
    }
    event_loop.run_app(&mut app).expect("event loop");
}

/// Browser entry point. Trunk builds this bin and calls `main`, so no explicit
/// `#[wasm_bindgen(start)]` is needed. There is no argv, filesystem, or headless
/// CLI on the web: the canvas is attached in `resumed` (see the wasm branch of
/// `Shell::resumed`) and the app boots straight into the GUI. winit's
/// `spawn_app` drives the event loop without returning, which is required on
/// the web because the loop is owned by the browser, not by Rust.
#[cfg(target_arch = "wasm32")]
fn main() {
    use winit::platform::web::EventLoopExtWebSys;

    // Forward Rust panics to the devtools console instead of an opaque
    // `RuntimeError: unreachable`, so a panic during init is debuggable.
    console_error_panic_hook::set_once();

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    // No `--config-dir`/`--windowed`/`--gpu-compute` on the web: windowed
    // (a canvas), no shadow compute backend, platform-default config (which
    // degrades to in-memory since the browser has no config directory).
    let app = Shell::new(None, false, false);
    event_loop.spawn_app(app);
}

/// What a L3+R3 press resolved to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FreelookChordAction {
    /// Released quickly: switch between freecam and the game's own controls,
    /// leaving the camera exactly where it is.
    Toggle,
    /// Held past the threshold: put the camera back at the game's viewpoint,
    /// without changing which mode is active.
    Reset,
}

/// Tap-versus-hold state for the L3+R3 chord.
///
/// Kept apart from the event loop because the interesting part is a four-way
/// transition with one easy mistake in it: the release that ends a HOLD must
/// not also register as a TAP, or every reset would silently flip the mode too.
#[derive(Default)]
struct FreelookChord {
    down: bool,
    hold_fired: bool,
}

impl FreelookChord {
    /// Advance one frame. `hold_reached` is whether the current press has been
    /// down at least as long as the hold threshold; the caller owns the clock.
    fn update(&mut self, chord: bool, hold_reached: bool) -> Option<FreelookChordAction> {
        let action = match (self.down, chord) {
            // Still held: fire the reset once, the moment the threshold passes,
            // so it is felt with the buttons still down rather than on release.
            (true, true) if hold_reached && !self.hold_fired => {
                self.hold_fired = true;
                Some(FreelookChordAction::Reset)
            }
            // Released without ever reaching the threshold: a tap.
            (true, false) if !self.hold_fired => Some(FreelookChordAction::Toggle),
            _ => None,
        };
        if !chord {
            self.hold_fired = false;
        }
        self.down = chord;
        action
    }
}

struct Shell {
    graphics: Option<Graphics>,
    state: AppState,
    pending_input: MenuInput,
    last_frame: Instant,
    /// Every piece of pad state the shell derives from keyboard events
    /// (button mask, emulated sticks, SOCD recency). One struct so the
    /// paths that must invalidate it wholesale -- focus loss, a rebind
    /// capture consuming events, Reset Controls -- clear one thing and
    /// can't miss a field.
    host_input: HostKeyboardInput,
    /// Tap-versus-hold tracking for the L3+R3 freecam chord.
    freelook_chord: FreelookChord,
    /// When the current L3+R3 press began. `None` while the chord is up.
    freelook_chord_down_at: Option<Instant>,
    /// Whether to open the window in borderless-fullscreen mode.
    /// Decision is made at startup via CLI flag and then captured
    /// here; changing it at runtime would need a window recreation.
    fullscreen: bool,
    /// Host audio output. `None` when no device is available
    /// (headless CI, devices that can't open a stereo stream).
    /// Emulation keeps running regardless -- silence is fine.
    audio: Option<audio::AudioOut>,
    /// Host input router - tracks every connected gamepad, assigns devices
    /// to PS1 ports, detects the Select+Start menu chord, and logs hotplug
    /// events. Always constructible; failed native gamepad initialisation
    /// leaves the keyboard route working.
    input: input::InputRouter,
    /// Last machine and routing generations applied to the emulated SIO ports.
    controller_layout_stamp: Option<(u64, u64)>,
    /// Wall-clock debt waiting to be converted into emulated
    /// "frames". Without this, the current `ControlFlow::Poll`
    /// shell runs the guest as fast as redraws can arrive, which
    /// massively overfills the audio queue and produces crackle
    /// from dropped samples.
    emu_frame_accum: f32,
    /// Phase C -- when `Some`, the experimental compute-shader
    /// rasterizer is shadowing the CPU rasterizer: each frame the
    /// CPU's `cmd_log` is drained and replayed onto the GPU compute
    /// path, and the display reads from the GPU's VRAM.
    compute_backend: Option<psx_gpu_render::ComputeBackend>,
    /// Whether to display the GPU compute output instead of the CPU
    /// VRAM. Toggled at runtime by F12. Independent of whether the
    /// compute backend is active -- when off, GPU still runs (so it
    /// stays in sync) but the user sees CPU output.
    display_gpu_compute: bool,
    /// Last CPU-VRAM generation that has been copied into the persistent
    /// hardware-renderer target.
    hw_seen_gpu_resync_generation: u64,
    /// Previous frame's scanout mode. Returning from 24bpp video to
    /// 15bpp gameplay needs a target rebuild because the visible panel
    /// was using the CPU-decoded fallback while 24bpp was active.
    hw_last_display_bpp24: bool,
    /// When the gamepad last produced input (buttons, sticks, or the
    /// analog toggle). Drives the paused redraw scheduler's active
    /// window; see [`PAD_ACTIVITY_WINDOW_SECS`].
    last_pad_activity: Instant,
    /// Stamp of the machine state (bus cycles, GPU resync generation,
    /// wireframe flag) at the last VRAM-derived sync, or `None` when no
    /// bus existed. While the stamp is unchanged the guest cannot have
    /// touched VRAM, so the per-redraw VRAM snapshot, RGBA debug-view
    /// conversion, and full-VRAM texture re-uploads are all skipped --
    /// on a 120 Hz host they were burning over half a core with the
    /// emulator sitting paused.
    vram_synced_stamp: Option<(u64, u64, bool)>,
    /// Live MCP debug server bridge. `None` if the server failed to start
    /// (e.g. port already bound). Drained each redraw against the emulator.
    #[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
    mcp: Option<mcp::McpBridge>,
    /// Deferred GPU init landing pad for the web build. wgpu's adapter/device
    /// request is async and the browser main thread cannot block on it, so
    /// `resumed` kicks the init off `spawn_local` and the finished `Graphics`
    /// is dropped in here; the shell installs it on the next tick. Empty until
    /// init completes. `Rc<RefCell<..>>` (not a channel) because wgpu types are
    /// `!Send` on single-threaded wasm.
    #[cfg(target_arch = "wasm32")]
    graphics_init: std::rc::Rc<std::cell::RefCell<Option<Graphics>>>,
}

impl Default for Shell {
    fn default() -> Self {
        Self::new(None, false, false)
    }
}

impl Shell {
    fn new(config_dir: Option<std::path::PathBuf>, fullscreen: bool, gpu_compute: bool) -> Self {
        // A downloaded build has no projects directory yet. Copy the bundled
        // sample in once, before anything reads the project list. Failure is
        // non-fatal: starting with no sample beats refusing to start.
        // Seeding a sample *project* is an editor concern, so this needs the
        // editor feature as well as a native target. Without the second gate a
        // `--no-default-features` build -- the emulator-only configuration this
        // crate documents as supported, and what hl-psx's regression harness
        // uses -- failed to compile: psxed-project is an optional dependency.
        #[cfg(all(not(target_arch = "wasm32"), feature = "editor"))]
        match psxed_project::ensure_projects_seeded() {
            Ok(true) => eprintln!(
                "[projects] seeded sample project into {}",
                psxed_project::projects_dir().display()
            ),
            Ok(false) => {}
            Err(error) => eprintln!("[projects] could not seed sample project: {error}"),
        }
        let audio = audio::AudioOut::open();
        if let Some(a) = audio.as_ref() {
            eprintln!("[audio] opened host stream @ {} Hz", a.host_sample_rate());
        } else {
            eprintln!("[audio] no host output device available - running silent");
        }
        let input = input::InputRouter::new();
        if input.is_connected() {
            eprintln!(
                "[input] already-connected pads: {}",
                input.connected_names()
            );
        } else {
            eprintln!("[input] no pads connected at startup - watching for hot-plug");
        }
        // The compute backend gets its own headless wgpu device.
        // We *could* share the main `Graphics` device for zero-copy
        // VRAM-to-display, but that needs `Arc<Device>` plumbing
        // throughout `Graphics` -- bigger refactor for a marginal
        // perf win in an opt-in shadow path. Per-frame VRAM bounces
        // through CPU memory, which costs ~1 MiB read + 1 MiB write
        // and is invisible next to the rasterizer cost.
        let compute_backend = if gpu_compute {
            eprintln!("[gpu-compute] enabling shadow compute rasterizer");
            Some(psx_gpu_render::ComputeBackend::new_headless())
        } else {
            None
        };
        Self {
            graphics: None,
            state: AppState::with_config_dir(config_dir),
            pending_input: MenuInput::default(),
            last_frame: Instant::now(),
            host_input: HostKeyboardInput::default(),
            freelook_chord: FreelookChord::default(),
            freelook_chord_down_at: None,
            fullscreen,
            audio,
            input,
            controller_layout_stamp: None,
            emu_frame_accum: 0.0,
            compute_backend,
            display_gpu_compute: gpu_compute,
            hw_seen_gpu_resync_generation: 0,
            hw_last_display_bpp24: false,
            last_pad_activity: Instant::now(),
            vram_synced_stamp: None,
            #[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
            mcp: mcp::start(),
            #[cfg(target_arch = "wasm32")]
            graphics_init: std::rc::Rc::new(std::cell::RefCell::new(None)),
        }
    }
}

/// Map a winit physical key to a PSX digital-pad bitmask using the
/// persisted port-1 bindings. Returns `None` for keys that aren't
/// bound.
fn key_to_pad_button(physical: &PhysicalKey, bindings: &PortBindings) -> Option<u16> {
    [
        (button::UP, &bindings.up),
        (button::DOWN, &bindings.down),
        (button::LEFT, &bindings.left),
        (button::RIGHT, &bindings.right),
        (button::CROSS, &bindings.cross),
        (button::CIRCLE, &bindings.circle),
        (button::SQUARE, &bindings.square),
        (button::TRIANGLE, &bindings.triangle),
        (button::L1, &bindings.l1),
        (button::R1, &bindings.r1),
        (button::L2, &bindings.l2),
        (button::R2, &bindings.r2),
        (button::START, &bindings.start),
        (button::SELECT, &bindings.select),
        (button::R3, &bindings.r3),
        (button::L3, &bindings.l3),
    ]
    .into_iter()
    .find_map(|(mask, binding)| binding_matches_key(binding, physical).then_some(mask))
}

/// `true` when the key should act as the DualShock Analog button.
fn key_is_analog_button(physical: &PhysicalKey, bindings: &PortBindings) -> bool {
    binding_matches_key(&bindings.analog, physical)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KeyboardStickState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl KeyboardStickState {
    fn update_key(
        &mut self,
        physical: &PhysicalKey,
        state: ElementState,
        bindings: &StickBindings,
    ) -> bool {
        let pressed = state == ElementState::Pressed;
        let mut matched = false;
        if binding_matches_key(&bindings.up, physical) {
            self.up = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.down, physical) {
            self.down = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.left, physical) {
            self.left = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.right, physical) {
            self.right = pressed;
            matched = true;
        }
        matched
    }

    fn vector(self) -> (f32, f32) {
        (
            keyboard_axis(self.left, self.right),
            keyboard_axis(self.down, self.up),
        )
    }
}

fn keyboard_axis(negative: bool, positive: bool) -> f32 {
    match (negative, positive) {
        (true, false) => -1.0,
        (false, true) => 1.0,
        _ => 0.0,
    }
}

fn merge_sticks(gamepad: (f32, f32), keyboard: (f32, f32)) -> (f32, f32) {
    (
        merge_axis(gamepad.0, keyboard.0),
        merge_axis(gamepad.1, keyboard.1),
    )
}

fn merge_axis(gamepad: f32, keyboard: f32) -> f32 {
    if keyboard != 0.0 {
        keyboard
    } else {
        gamepad
    }
}

/// Strip simultaneous opposing cardinal directions from the pad mask,
/// keeping only the most recently pressed side of each pair (see
/// [`HostKeyboardInput`]). The real d-pad's rocker makes the
/// combination impossible, so the guest must never see it; with no
/// recorded recency (or a stale one no longer held) the pair resolves
/// to neutral, which is what the rocker does mid-travel.
fn socd_resolve(mask: u16, last_horiz: u16, last_vert: u16) -> u16 {
    let mut out = mask;
    const HORIZ: u16 = button::LEFT | button::RIGHT;
    const VERT: u16 = button::UP | button::DOWN;
    if out & HORIZ == HORIZ {
        out = (out & !HORIZ) | (last_horiz & HORIZ);
    }
    if out & VERT == VERT {
        out = (out & !VERT) | (last_vert & VERT);
    }
    out
}

/// Fold a set of rising-edge presses into the SOCD recency trackers.
/// Shared by the keyboard press path (one button bit per event) and
/// the gamepad path (a whole poll's edges at once) so both devices
/// compete on equal terms -- a newer gamepad direction must beat an
/// older keyboard one and vice versa. Opposite edges arriving in the
/// same gamepad poll carry no ordering information, so the pair's
/// recency resets and [`socd_resolve`] yields neutral.
fn update_socd_recency(last_horiz: &mut u16, last_vert: &mut u16, pressed: u16) {
    const HORIZ: u16 = button::LEFT | button::RIGHT;
    const VERT: u16 = button::UP | button::DOWN;
    match pressed & HORIZ {
        button::LEFT => *last_horiz = button::LEFT,
        button::RIGHT => *last_horiz = button::RIGHT,
        HORIZ => *last_horiz = 0,
        _ => {}
    }
    match pressed & VERT {
        button::UP => *last_vert = button::UP,
        button::DOWN => *last_vert = button::DOWN,
        VERT => *last_vert = 0,
        _ => {}
    }
}

/// Every piece of pad state derived from host keyboard events: the
/// port-1 button mask, both keyboard-emulated sticks, and the SOCD
/// recency trackers.
///
/// Grouped so the paths that must throw the whole cache away clear one
/// struct and can't miss a field. That matters because this state is
/// only correct while every key release is observed and the bindings
/// that produced it are still live; three paths break that assumption:
///
/// - **focus loss** -- the OS doesn't reliably deliver `Released` for
///   keys still held during Alt-Tab;
/// - **a rebind capture consuming events** -- capture owns the
///   keyboard outright, including releases whose normal processing
///   would have cleared held bits;
/// - **Reset Controls** -- bits set under the old bindings can never
///   be cleared by releases matched against the new ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HostKeyboardInput {
    /// Live port-1 pad mask. Key press/release events toggle bits
    /// here; the shell merges it with the gamepad mask each frame.
    pad1_mask: u16,
    /// Keyboard-emulated left analog stick state.
    left_stick: KeyboardStickState,
    /// Keyboard-emulated right analog stick state.
    right_stick: KeyboardStickState,
    /// Most recently pressed button of the LEFT/RIGHT pair, for SOCD
    /// (simultaneous opposing cardinal directions) resolution: when
    /// both are held the guest sees only the most recent press, and
    /// releasing it re-exposes the still-held opposite. Last-input
    /// priority matches what a player expects from tapping the other
    /// direction without letting go first.
    socd_last_horiz: u16,
    /// Same, for the UP/DOWN pair.
    socd_last_vert: u16,
}

impl HostKeyboardInput {
    /// Drop everything: mask, sticks, and SOCD recency. The pad
    /// simply reports "nothing held" until fresh key events arrive.
    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Apply one keyboard press/release to the cached pad state:
    /// button mask (with SOCD recency on presses) and both emulated
    /// sticks.
    fn apply_key_event(
        &mut self,
        physical: &PhysicalKey,
        state: ElementState,
        bindings: &PortBindings,
    ) {
        if let Some(mask) = key_to_pad_button(physical, bindings) {
            match state {
                ElementState::Pressed => {
                    self.pad1_mask |= mask;
                    update_socd_recency(&mut self.socd_last_horiz, &mut self.socd_last_vert, mask);
                }
                ElementState::Released => self.pad1_mask &= !mask,
            }
        }
        self.left_stick
            .update_key(physical, state, &bindings.left_stick);
        self.right_stick
            .update_key(physical, state, &bindings.right_stick);
    }

    /// Fold the gamepad's rising-edge presses from this poll into the
    /// SOCD recency, so a newer pad direction beats an older keyboard
    /// one. Call before [`Self::resolved_mask`] each frame.
    fn note_gamepad_edges(&mut self, pressed_mask: u16) {
        update_socd_recency(
            &mut self.socd_last_horiz,
            &mut self.socd_last_vert,
            pressed_mask,
        );
    }

    /// The SOCD-clean guest mask: keyboard bits merged with the
    /// gamepad's, opposing cardinals resolved by recency.
    fn resolved_mask(&self, gamepad_mask: u16) -> u16 {
        socd_resolve(
            self.pad1_mask | gamepad_mask,
            self.socd_last_horiz,
            self.socd_last_vert,
        )
    }
}

/// Match a persisted binding against the *physical* key that changed,
/// rather than the logical `Key` winit derives from the current keyboard
/// layout.
///
/// This used to match on `Key` (the layout-translated character/name).
/// Windows re-runs its dead-key/AltGr translation on every keystroke, and
/// on layouts that put a dead key near the default control cluster (ABNT2,
/// International-US, ...) a still-held key can flip from `Key::Character`
/// to `Key::Dead`/`Unidentified` the instant a *second* key goes down --
/// which read as "holding a second button drops the first bind", exactly
/// the multi-key symptom this was chasing. A physical key's `KeyCode`
/// comes straight from the hardware scancode (or the DOM `code` on web),
/// so it never depends on modifier state, layout, or what other keys are
/// currently held.
fn binding_matches_key(binding: &InputBinding, physical: &PhysicalKey) -> bool {
    let PhysicalKey::Code(code) = physical else {
        return false;
    };
    match binding {
        InputBinding::Unbound => false,
        InputBinding::Character(expected) => char_to_keycode(*expected) == Some(*code),
        InputBinding::Named(expected) => named_key_codes(expected).contains(code),
    }
}

/// Physical-position mapping for a persisted `InputBinding::Character`.
/// Bindings are stored as the character a US-QWERTY layout produces at a
/// given position (the default set: q/e/z/x/c/s/r/1/3/i/j/k/l), so this is
/// a fixed position table rather than a live layout query -- which is what
/// keeps a binding stable regardless of the keyboard layout actually
/// active at runtime.
fn char_to_keycode(c: char) -> Option<KeyCode> {
    Some(match c.to_ascii_lowercase() {
        'a' => KeyCode::KeyA,
        'b' => KeyCode::KeyB,
        'c' => KeyCode::KeyC,
        'd' => KeyCode::KeyD,
        'e' => KeyCode::KeyE,
        'f' => KeyCode::KeyF,
        'g' => KeyCode::KeyG,
        'h' => KeyCode::KeyH,
        'i' => KeyCode::KeyI,
        'j' => KeyCode::KeyJ,
        'k' => KeyCode::KeyK,
        'l' => KeyCode::KeyL,
        'm' => KeyCode::KeyM,
        'n' => KeyCode::KeyN,
        'o' => KeyCode::KeyO,
        'p' => KeyCode::KeyP,
        'q' => KeyCode::KeyQ,
        'r' => KeyCode::KeyR,
        's' => KeyCode::KeyS,
        't' => KeyCode::KeyT,
        'u' => KeyCode::KeyU,
        'v' => KeyCode::KeyV,
        'w' => KeyCode::KeyW,
        'x' => KeyCode::KeyX,
        'y' => KeyCode::KeyY,
        'z' => KeyCode::KeyZ,
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        '9' => KeyCode::Digit9,
        _ => return None,
    })
}

/// Physical keycode(s) a persisted `InputBinding::Named` label matches.
/// Covers the label set the old logical-key lookup accepted plus every
/// label [`keycode_to_binding`] can produce for the controls panel's
/// rebind capture (function keys, numpad -- prime real estate for
/// dodging keyboard-matrix ghosting). "Shift" matches either physical
/// shift key since the binding format predates left/right being
/// distinguished.
fn named_key_codes(name: &str) -> &'static [KeyCode] {
    match name.to_ascii_lowercase().as_str() {
        "arrowup" => &[KeyCode::ArrowUp],
        "arrowdown" => &[KeyCode::ArrowDown],
        "arrowleft" => &[KeyCode::ArrowLeft],
        "arrowright" => &[KeyCode::ArrowRight],
        "enter" => &[KeyCode::Enter],
        "backspace" => &[KeyCode::Backspace],
        "shift" => &[KeyCode::ShiftLeft, KeyCode::ShiftRight],
        "space" => &[KeyCode::Space],
        "tab" => &[KeyCode::Tab],
        "escape" => &[KeyCode::Escape],
        "f1" => &[KeyCode::F1],
        "f2" => &[KeyCode::F2],
        "f3" => &[KeyCode::F3],
        "f4" => &[KeyCode::F4],
        "f5" => &[KeyCode::F5],
        "f6" => &[KeyCode::F6],
        "f7" => &[KeyCode::F7],
        "f8" => &[KeyCode::F8],
        "f9" => &[KeyCode::F9],
        "f10" => &[KeyCode::F10],
        "f11" => &[KeyCode::F11],
        "f12" => &[KeyCode::F12],
        "numpad0" => &[KeyCode::Numpad0],
        "numpad1" => &[KeyCode::Numpad1],
        "numpad2" => &[KeyCode::Numpad2],
        "numpad3" => &[KeyCode::Numpad3],
        "numpad4" => &[KeyCode::Numpad4],
        "numpad5" => &[KeyCode::Numpad5],
        "numpad6" => &[KeyCode::Numpad6],
        "numpad7" => &[KeyCode::Numpad7],
        "numpad8" => &[KeyCode::Numpad8],
        "numpad9" => &[KeyCode::Numpad9],
        "numpadadd" => &[KeyCode::NumpadAdd],
        "numpadsubtract" => &[KeyCode::NumpadSubtract],
        "numpadmultiply" => &[KeyCode::NumpadMultiply],
        "numpaddivide" => &[KeyCode::NumpadDivide],
        "numpaddecimal" => &[KeyCode::NumpadDecimal],
        "numpadenter" => &[KeyCode::NumpadEnter],
        "controlleft" => &[KeyCode::ControlLeft],
        "controlright" => &[KeyCode::ControlRight],
        "altleft" => &[KeyCode::AltLeft],
        "altright" => &[KeyCode::AltRight],
        "semicolon" => &[KeyCode::Semicolon],
        "comma" => &[KeyCode::Comma],
        "period" => &[KeyCode::Period],
        "slash" => &[KeyCode::Slash],
        "quote" => &[KeyCode::Quote],
        "bracketleft" => &[KeyCode::BracketLeft],
        "bracketright" => &[KeyCode::BracketRight],
        "backslash" => &[KeyCode::Backslash],
        "minus" => &[KeyCode::Minus],
        "equal" => &[KeyCode::Equal],
        "backquote" => &[KeyCode::Backquote],
        "intlro" => &[KeyCode::IntlRo],
        "intlbackslash" => &[KeyCode::IntlBackslash],
        _ => &[],
    }
}

/// The persistable binding a captured physical key becomes -- the
/// reverse of [`binding_matches_key`]'s lookup, used by the controls
/// panel's press-a-key rebind flow. Letters and digits round-trip
/// through the existing `Character` form; everything else gets a
/// `Named` label that [`named_key_codes`] recognises. `None` means the
/// key is not bindable: Escape stays reserved for the menu toggle, and
/// keys outside the table have no stable label to persist.
fn keycode_to_binding(code: KeyCode) -> Option<InputBinding> {
    let named = |s: &str| Some(InputBinding::Named(s.to_string()));
    let ch = |c: char| Some(InputBinding::Character(c));
    match code {
        KeyCode::KeyA => ch('a'),
        KeyCode::KeyB => ch('b'),
        KeyCode::KeyC => ch('c'),
        KeyCode::KeyD => ch('d'),
        KeyCode::KeyE => ch('e'),
        KeyCode::KeyF => ch('f'),
        KeyCode::KeyG => ch('g'),
        KeyCode::KeyH => ch('h'),
        KeyCode::KeyI => ch('i'),
        KeyCode::KeyJ => ch('j'),
        KeyCode::KeyK => ch('k'),
        KeyCode::KeyL => ch('l'),
        KeyCode::KeyM => ch('m'),
        KeyCode::KeyN => ch('n'),
        KeyCode::KeyO => ch('o'),
        KeyCode::KeyP => ch('p'),
        KeyCode::KeyQ => ch('q'),
        KeyCode::KeyR => ch('r'),
        KeyCode::KeyS => ch('s'),
        KeyCode::KeyT => ch('t'),
        KeyCode::KeyU => ch('u'),
        KeyCode::KeyV => ch('v'),
        KeyCode::KeyW => ch('w'),
        KeyCode::KeyX => ch('x'),
        KeyCode::KeyY => ch('y'),
        KeyCode::KeyZ => ch('z'),
        KeyCode::Digit0 => ch('0'),
        KeyCode::Digit1 => ch('1'),
        KeyCode::Digit2 => ch('2'),
        KeyCode::Digit3 => ch('3'),
        KeyCode::Digit4 => ch('4'),
        KeyCode::Digit5 => ch('5'),
        KeyCode::Digit6 => ch('6'),
        KeyCode::Digit7 => ch('7'),
        KeyCode::Digit8 => ch('8'),
        KeyCode::Digit9 => ch('9'),
        KeyCode::ArrowUp => named("ArrowUp"),
        KeyCode::ArrowDown => named("ArrowDown"),
        KeyCode::ArrowLeft => named("ArrowLeft"),
        KeyCode::ArrowRight => named("ArrowRight"),
        KeyCode::Enter => named("Enter"),
        KeyCode::Backspace => named("Backspace"),
        KeyCode::ShiftLeft | KeyCode::ShiftRight => named("Shift"),
        KeyCode::Space => named("Space"),
        KeyCode::Tab => named("Tab"),
        KeyCode::F1 => named("F1"),
        KeyCode::F2 => named("F2"),
        KeyCode::F3 => named("F3"),
        KeyCode::F4 => named("F4"),
        KeyCode::F6 => named("F6"),
        KeyCode::F9 => named("F9"),
        KeyCode::F10 => named("F10"),
        KeyCode::F11 => named("F11"),
        KeyCode::Numpad0 => named("Numpad0"),
        KeyCode::Numpad1 => named("Numpad1"),
        KeyCode::Numpad2 => named("Numpad2"),
        KeyCode::Numpad3 => named("Numpad3"),
        KeyCode::Numpad4 => named("Numpad4"),
        KeyCode::Numpad5 => named("Numpad5"),
        KeyCode::Numpad6 => named("Numpad6"),
        KeyCode::Numpad7 => named("Numpad7"),
        KeyCode::Numpad8 => named("Numpad8"),
        KeyCode::Numpad9 => named("Numpad9"),
        KeyCode::NumpadAdd => named("NumpadAdd"),
        KeyCode::NumpadSubtract => named("NumpadSubtract"),
        KeyCode::NumpadMultiply => named("NumpadMultiply"),
        KeyCode::NumpadDivide => named("NumpadDivide"),
        KeyCode::NumpadDecimal => named("NumpadDecimal"),
        KeyCode::NumpadEnter => named("NumpadEnter"),
        KeyCode::ControlLeft => named("ControlLeft"),
        KeyCode::ControlRight => named("ControlRight"),
        KeyCode::AltLeft => named("AltLeft"),
        KeyCode::AltRight => named("AltRight"),
        KeyCode::Semicolon => named("Semicolon"),
        KeyCode::Comma => named("Comma"),
        KeyCode::Period => named("Period"),
        KeyCode::Slash => named("Slash"),
        KeyCode::Quote => named("Quote"),
        KeyCode::BracketLeft => named("BracketLeft"),
        KeyCode::BracketRight => named("BracketRight"),
        KeyCode::Backslash => named("Backslash"),
        KeyCode::Minus => named("Minus"),
        KeyCode::Equal => named("Equal"),
        KeyCode::Backquote => named("Backquote"),
        KeyCode::IntlRo => named("IntlRo"),
        KeyCode::IntlBackslash => named("IntlBackslash"),
        // Escape toggles the menu, F5/F7 save/load, F8 records input, and
        // F12 toggles the renderer display source. Keeping host commands out
        // of pad bindings prevents one key press from firing both actions.
        _ => None,
    }
}

impl ApplicationHandler for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return;
        }

        app_icon::set_application_icon();

        // Borderless-fullscreen on the primary monitor by default.
        // `--windowed` switches to a 1600×1000 floating window so
        // dev work next to a terminal / docs stays bearable.
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes()
            .with_title("PSoXide")
            .with_inner_size(winit::dpi::PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT))
            .with_min_inner_size(winit::dpi::PhysicalSize::new(MIN_WIDTH, MIN_HEIGHT));
        if let Some(icon) = app_icon::load_window_icon() {
            attrs = attrs.with_window_icon(Some(icon));
        }
        if self.fullscreen {
            attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }

        // Native: create the window and block on GPU init -- the OS owns the
        // window and blocking on adapter/device is fine off the main browser
        // thread.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
            self.graphics = Some(pollster::block_on(Graphics::new(window)));
        }

        // Web: build a canvas, append it to the document body, hand it to
        // winit, then kick GPU init off `spawn_local`. The browser main thread
        // cannot block on the async adapter/device request, so the finished
        // `Graphics` lands in `graphics_init` and the shell installs it on a
        // later tick (see `install_pending_graphics`).
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;

            let canvas = web_sys::window()
                .and_then(|win| win.document())
                .and_then(|doc| {
                    let canvas = doc
                        .create_element("canvas")
                        .ok()?
                        .dyn_into::<web_sys::HtmlCanvasElement>()
                        .ok()?;
                    doc.body()?.append_child(&canvas).ok()?;
                    Some(canvas)
                })
                .expect("append canvas to document body");

            attrs = attrs.with_canvas(Some(canvas));
            let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

            let slot = self.graphics_init.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let graphics = Graphics::new(window).await;
                *slot.borrow_mut() = Some(graphics);
            });
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gfx) = self.graphics.as_mut() else {
            return;
        };

        let consumed = gfx.egui_winit.on_window_event(&gfx.window, &event).consumed;

        match event {
            WindowEvent::CloseRequested => {
                self.state.stop_input_recording_if_active();
                #[cfg(feature = "editor")]
                self.state.stop_embedded_playtest();
                self.state.flush_pending_input_profile_capture();
                self.state.stop_examples_build();
                // Flush any dirty memory card so save progress
                // survives a window-close. A hard crash still
                // loses whatever hasn't been flushed -- the run
                // loop could call this periodically; for now
                // graceful exit is enough.
                if let Err(e) = self.state.flush_memcard_port1() {
                    eprintln!("[frontend] memcard flush on exit: {e}");
                }
                #[cfg(feature = "editor")]
                if let Err(e) = self.state.save_editor_project() {
                    eprintln!("[frontend] editor save on exit: {e}");
                }
                // Persist current settings (BIOS path, library
                // root, etc.) so the next launch picks up any
                // user tweaks without needing a manual save step.
                if let Err(e) = self.state.save_settings() {
                    eprintln!("[frontend] settings save on exit: {e}");
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                gfx.resize(size);
                gfx.window.request_redraw();
            }
            // Losing OS focus (Alt-Tab, a notification stealing focus, an
            // overlay like Discord/Game Bar, clicking a second monitor...)
            // does not reliably deliver a `Released` for whatever's
            // physically still held down -- on Windows in particular, the
            // key-up can fire after focus already moved elsewhere, or not
            // reach this window at all. Left unhandled, that bit (or
            // analog-stick / freelook direction) gets stuck "held" until
            // the same key happens to be pressed and released again,
            // which reads as "I have to let go of one key to use another"
            // once a second, genuinely-held key ORs into an already-stuck
            // mask. Clear every host-key-derived input state on focus
            // loss so a stuck bit can never survive past it; the pad
            // simply reports "nothing held" until the player presses
            // fresh keys after refocusing.
            WindowEvent::Focused(false) => {
                self.host_input.clear();
            }
            WindowEvent::Focused(true) => {}
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        physical_key,
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                // A controls-panel rebind capture owns the keyboard
                // outright: the next key press becomes the binding
                // (Escape cancels), and nothing leaks through to the
                // game, the menu, or the host shortcuts below.
                if let Some(target) = self.state.menu.controls_capture() {
                    // Capture consumes this event unconditionally --
                    // including releases, whose normal processing is
                    // what clears held bits. Drop the cached keyboard
                    // state up front so no exit from capture (bind,
                    // Escape, closing the panel) can leave a swallowed
                    // release's button wedged into the mask.
                    self.host_input.clear();
                    if state == ElementState::Pressed && !repeat {
                        if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                            self.state.menu.clear_controls_capture();
                        } else if let PhysicalKey::Code(code) = physical_key {
                            if let Some(binding) = keycode_to_binding(code) {
                                self.state.apply_rebind(target, binding);
                                self.state.menu.clear_controls_capture();
                            } else {
                                self.state
                                    .status_message_set("That key can't be bound".to_string());
                            }
                        }
                    }
                    return;
                }
                // Pad state tracks both press AND release continuously
                // so held buttons keep polling as "pressed". Auto-repeat
                // events are ignored -- the key is already down, and the
                // BIOS polls every frame anyway.
                // Without the editor feature the game always owns the
                // keyboard (there is no editor workspace to take it over).
                #[cfg(feature = "editor")]
                let route_keyboard_to_game = !self.state.workspace.is_editor()
                    || self.state.embedded_playtest_input_captured();
                #[cfg(not(feature = "editor"))]
                let route_keyboard_to_game = true;
                if !repeat && route_keyboard_to_game {
                    let bindings = &self.state.settings.input.port1;
                    self.host_input
                        .apply_key_event(&physical_key, state, bindings);
                    let press_analog = state == ElementState::Pressed
                        && key_is_analog_button(&physical_key, bindings);
                    if press_analog {
                        let analog = self.input.toggle_keyboard_analog();
                        self.state.status_message_set(format!(
                            "Keyboard controller mode: {}",
                            if analog { "Analog" } else { "Digital" }
                        ));
                    }
                }
                // The Menu *does* honour OS-level key-repeat: holding
                // down-arrow scrolls through a long Examples list one
                // row per repeat tick, matching GUI-standard behaviour.
                // Only press events (including repeats) trigger menu
                // navigation; releases don't.
                if state == ElementState::Pressed {
                    self.pending_input = merge_key(self.pending_input, &logical_key);
                }
                // F12 -- toggle the display source between the CPU
                // rasterizer's VRAM and the compute backend's. Only
                // meaningful when the compute backend is active
                // (i.e. `--gpu-compute` was passed). No-op otherwise.
                if state == ElementState::Pressed
                    && !repeat
                    && matches!(&logical_key, Key::Named(NamedKey::F12))
                {
                    self.display_gpu_compute = !self.display_gpu_compute;
                    eprintln!(
                        "[gpu-compute] display source: {}",
                        if self.display_gpu_compute {
                            "GPU compute"
                        } else {
                            "CPU rasterizer"
                        }
                    );
                }
                // F5 / F7 -- quick-save (pushes a new save, and pins it
                // as the new quick-load target) / quick-load whichever
                // save is pinned "on top" (see `ConfigPaths::read_top_slot`),
                // falling back to the most recent one. F7 stays running
                // after loading (unlike the save-states panel's Load
                // button, which defaults its "Resume paused" checkbox
                // to on) -- it's meant as a fast, in-the-moment rewind,
                // not a deliberate jump you'd want to stop and look
                // around after. F9 is already the DualShock analog-mode
                // toggle (see `key_is_analog_button`), so quick-save/-load
                // land on F5/F7 instead of the more conventional F5/F9.
                // F8 toggles a deterministic port-1 recording saved below the
                // current game's config directory (web: reboots the game and
                // records from cold boot; stopping downloads a CSV tape).
                if state == ElementState::Pressed && !repeat {
                    match &logical_key {
                        Key::Named(NamedKey::F5) => self.state.save_state(),
                        Key::Named(NamedKey::F7) => self.state.load_latest_state(false),
                        Key::Named(NamedKey::F8) => self.state.toggle_input_recording(),
                        _ => {}
                    }
                }
                gfx.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Service any queued MCP tool calls against the live emulator
                // before this frame runs. Runs whether or not we're in run mode,
                // so `step`/`screenshot`/`read_ram` work while paused.
                #[cfg(all(feature = "mcp", not(target_arch = "wasm32")))]
                if let Some(bridge) = self.mcp.as_mut() {
                    bridge.drain(&mut self.state);
                }

                let profile_start = Instant::now();
                let now = Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(0.1);
                self.last_frame = now;
                let cpu_tick_before = self.state.cpu.tick();
                let gte_profile_before = self.state.cpu.cop2().profile_snapshot();
                let bus_cycles_before =
                    self.state.bus.as_ref().map(|bus| bus.cycles()).unwrap_or(0);
                let mut profile = FrameProfileSample {
                    host_dt_ms: dt * 1000.0,
                    ..FrameProfileSample::default()
                };

                let input_start = Instant::now();
                let mut input = std::mem::take(&mut self.pending_input);

                // Poll the gamepad router BEFORE doing anything
                // else for this frame: the event drain is what
                // lets gilrs notice hot-plugged Bluetooth pads, so
                // we can't gate it on run state. We then merge the
                // frame's edges into `MenuInput` and keep the merged
                // mask handy for the run branch further down.
                let pad_frame = self.input.poll();
                if !pad_frame.notices.is_empty() {
                    let msg = pad_frame
                        .notices
                        .iter()
                        .map(|notice| notice.message())
                        .collect::<Vec<_>>()
                        .join(" · ");
                    self.state.status_message_set(msg);
                }
                self.state.poll_burner_hotplug();
                if pad_frame.toggle_menu {
                    // Select+Start is the gamepad equivalent of
                    // Escape -- route it into the same `toggle_open`
                    // path so there's exactly one place that decides
                    // what "PS button" does based on current state.
                    input.toggle_open = true;
                }
                if pad_frame.analog_button {
                    self.state
                        .status_message_set("Controller mode toggled".to_string());
                }

                // Merge keyboard-emulated and real-pad sticks once; used both
                // to drive the freelook camera and (unless freelook steals
                // them) to feed the guest pad below. The gamepad's press
                // edges fold into SOCD recency *before* resolving, so a
                // newer pad direction beats an older keyboard one. All
                // A Reset Controls action clears the keyboard cache below;
                // guest port samples are built afterward from that live cache.
                self.host_input.note_gamepad_edges(pad_frame.pressed_mask);
                let merged_left =
                    merge_sticks(pad_frame.left_stick, self.host_input.left_stick.vector());
                let merged_right =
                    merge_sticks(pad_frame.right_stick, self.host_input.right_stick.vector());
                let merged_mask = self.host_input.resolved_mask(pad_frame.pad1_mask);

                // Feed the controls panel its live held-target set so
                // held keys light up on the drawing -- an in-app
                // rollover/ghosting tester. Only while it's open; the
                // Vec is rebuilt per frame but tops out at 25 entries.
                if self.state.menu.controls_panel_open() {
                    use crate::ui::menu::PadBindTarget as T;
                    let mut held = Vec::new();
                    for (bit, target) in [
                        (button::UP, T::Up),
                        (button::DOWN, T::Down),
                        (button::LEFT, T::Left),
                        (button::RIGHT, T::Right),
                        (button::CROSS, T::Cross),
                        (button::CIRCLE, T::Circle),
                        (button::SQUARE, T::Square),
                        (button::TRIANGLE, T::Triangle),
                        (button::L1, T::L1),
                        (button::L2, T::L2),
                        (button::R1, T::R1),
                        (button::R2, T::R2),
                        (button::START, T::Start),
                        (button::SELECT, T::Select),
                        (button::L3, T::L3),
                        (button::R3, T::R3),
                    ] {
                        if merged_mask & bit != 0 {
                            held.push(target);
                        }
                    }
                    for (on, target) in [
                        (self.host_input.left_stick.up, T::LStickUp),
                        (self.host_input.left_stick.down, T::LStickDown),
                        (self.host_input.left_stick.left, T::LStickLeft),
                        (self.host_input.left_stick.right, T::LStickRight),
                        (self.host_input.right_stick.up, T::RStickUp),
                        (self.host_input.right_stick.down, T::RStickDown),
                        (self.host_input.right_stick.left, T::RStickLeft),
                        (self.host_input.right_stick.right, T::RStickRight),
                    ] {
                        if on {
                            held.push(target);
                        }
                    }
                    self.state.menu.set_controls_live_held(held);
                }

                // Gamepad input arrives by polling, not as window events,
                // so it cannot wake the redraw scheduler the way keyboard
                // and mouse do. Remember when the pad was last live; the
                // paused scheduler holds the active tick inside this window
                // so controller menu navigation stays responsive.
                if merged_mask != 0
                    || pad_frame.analog_button
                    || merged_left != (0.0, 0.0)
                    || merged_right != (0.0, 0.0)
                {
                    self.last_pad_activity = Instant::now();
                }

                // L3 + R3 is two gestures, not one (works from keyboard or pad;
                // the toolbar EYE button toggles the same flag):
                //
                //   TAP  -- switch between freecam and the game's own controls,
                //           LEAVING THE CAMERA WHERE IT IS. Toggling used to
                //           reset the view on the way out, which threw away the
                //           framing you had just lined up and made the chord
                //           unusable for glancing back and forth.
                //   HOLD -- put the camera back at the game's own viewpoint,
                //           without changing which mode you are in.
                //
                // The hold fires as soon as the threshold passes rather than on
                // release, so the reset is felt while the buttons are still
                // down, and the release that follows is not also read as a tap.
                const FREELOOK_CHORD_HOLD: Duration = Duration::from_millis(400);
                let chord = merged_mask & (button::L3 | button::R3) == (button::L3 | button::R3);
                if chord && self.freelook_chord_down_at.is_none() {
                    self.freelook_chord_down_at = Some(Instant::now());
                }
                let hold_reached = self
                    .freelook_chord_down_at
                    .is_some_and(|t| t.elapsed() >= FREELOOK_CHORD_HOLD);
                match self.freelook_chord.update(chord, hold_reached) {
                    Some(FreelookChordAction::Toggle) => {
                        self.state.freelook.enabled = !self.state.freelook.enabled;
                        self.state.status_message_set(if self.state.freelook.enabled {
                            "Freecam ON - pad drives the camera, game input paused (hold L3+R3 to reset)"
                        } else {
                            "Freecam controls off - framing preserved; pad returns to the game"
                        });
                    }
                    Some(FreelookChordAction::Reset) => {
                        let was_enabled = self.state.freelook.enabled;
                        self.state.freelook = emulator_core::FreelookState::default();
                        self.state.freelook.enabled = was_enabled;
                        self.state
                            .status_message_set("Freecam reset to the game camera");
                    }
                    None => {}
                }
                if !chord {
                    self.freelook_chord_down_at = None;
                }

                // Feed the camera from the sticks while engaged; R2 boosts.
                self.state.freelook_input = crate::app::FreelookInput {
                    left: merged_left,
                    right: merged_right,
                    boost: merged_mask & button::R2 != 0,
                };

                // When the Menu is open OR currently paused, the
                // gamepad doubles as the menu navigator. D-pad /
                // left-stick edges become up/down/left/right, Cross
                // is Enter, Circle is Back. `|=` so keyboard and
                // pad can both contribute -- last-one-wins semantics
                // don't matter at this granularity.
                input.up |= pad_frame.menu_up;
                input.down |= pad_frame.menu_down;
                input.left |= pad_frame.menu_left;
                input.right |= pad_frame.menu_right;
                input.confirm |= pad_frame.menu_confirm;
                input.back |= pad_frame.menu_back;

                // Escape is the "PS button" -- it toggles between
                // "game running" and "game paused + menu open".
                // Intercept it here so the Menu doesn't also interpret
                // it as a navigation input. The user pressed Escape
                // (or Select+Start, now) to swap contexts, not to
                // press "back" on whatever menu item happened to
                // be highlighted.
                if input.toggle_open {
                    input.toggle_open = false;
                    input.back = false;
                    // With the controls panel up, Escape means "close
                    // the panel", not "toggle the whole menu" -- the
                    // nearest thing on screen should dismiss first.
                    // (A pending key capture never reaches here: the
                    // capture arm consumes Escape as its cancel.)
                    let closed_controls_panel = self.state.menu.controls_panel_open() && {
                        self.state.menu.close_controls();
                        true
                    };
                    // In the editor, Escape first releases an active embedded-play
                    // input capture instead of toggling the menu.
                    #[cfg(feature = "editor")]
                    let editor_released_capture = !closed_controls_panel
                        && self.state.workspace.is_editor()
                        && self.state.embedded_playtest_running()
                        && self.state.embedded_playtest_input_captured()
                        && {
                            self.state.release_embedded_playtest_input();
                            true
                        };
                    #[cfg(not(feature = "editor"))]
                    let editor_released_capture = false;
                    if closed_controls_panel || editor_released_capture {
                        // Handled above: input capture released.
                    } else if self.state.running {
                        // Game mode → menu mode: pause and open overlay.
                        self.state.running = false;
                        self.state.menu.sync_run_label(false);
                        self.state.menu.open = true;
                    } else if self.state.menu.open {
                        // Menu mode → game mode: resume if we have a
                        // live game to resume; otherwise just close
                        // the overlay.
                        self.state.menu.open = false;
                        #[cfg(feature = "editor")]
                        let resumable_embedded = self.state.embedded_playtest_input_captured();
                        #[cfg(not(feature = "editor"))]
                        let resumable_embedded = false;
                        if self.state.bus.is_some()
                            && (self.state.current_game.is_some() || resumable_embedded)
                        {
                            self.state.running = true;
                            self.state.menu.sync_run_label(true);
                        }
                    } else {
                        // No game running and Menu already closed --
                        // Escape just opens the menu.
                        self.state.menu.open = true;
                    }
                }

                if let Some(action) = self.state.menu.update(&input) {
                    match ui::apply_menu_action(&mut self.state, action) {
                        MenuOutcome::None => {}
                        MenuOutcome::ClearHostKeyboardInput => {
                            // The action (Reset Controls) replaced the
                            // bindings, so every cached keyboard bit was
                            // set under a mapping that no longer exists
                            // -- a later release can't clear it. Drop
                            // the cache and rebuild this frame's merged
                            // sample from the gamepad alone, so not even
                            // one stale keyboard frame reaches the guest.
                            self.host_input.clear();
                        }
                        MenuOutcome::Quit => {
                            self.state.stop_input_recording_if_active();
                            #[cfg(feature = "editor")]
                            self.state.stop_embedded_playtest();
                            self.state.flush_pending_input_profile_capture();
                            self.state.stop_examples_build();
                            if let Err(e) = self.state.flush_memcard_port1() {
                                eprintln!("[frontend] memcard flush on quit: {e}");
                            }
                            #[cfg(feature = "editor")]
                            if let Err(e) = self.state.save_editor_project() {
                                eprintln!("[frontend] editor save on quit: {e}");
                            }
                            if let Err(e) = self.state.save_settings() {
                                eprintln!("[frontend] settings save on quit: {e}");
                            }
                            event_loop.exit();
                            return;
                        }
                    }
                }
                #[cfg(feature = "editor")]
                self.state.poll_embedded_playtest_build();
                self.state.poll_examples_build();
                profile.input_ms = elapsed_ms(input_start);

                // Keep the guest SIO topology in sync with the routing panel.
                // A machine generation change means a boot or save-state load
                // replaced the Bus, so the same host layout must be applied
                // again even when the user did not touch the panel.
                let controller_layout_stamp = (
                    self.state.gpu_resync_generation,
                    self.input.routing_generation(),
                );
                if self.controller_layout_stamp != Some(controller_layout_stamp) {
                    if let Some(bus) = self.state.bus.as_mut() {
                        self.input.apply_layout(bus);
                    }
                    self.controller_layout_stamp = Some(controller_layout_stamp);
                }

                // Arm GPU command capture before stepping so the HW /
                // compute sidecars see the frame that is about to run.
                // Re-arming clears the log, so only do this once per
                // Bus lifetime.
                if let Some(bus) = self.state.bus.as_mut() {
                    if self.compute_backend.is_some() {
                        if bus.gpu.pixel_owner.is_none() {
                            bus.gpu.enable_pixel_tracer();
                        }
                    } else if !bus.gpu.cmd_log_enabled() {
                        bus.gpu.enable_cmd_log();
                    }
                }
                // Convert wall-clock into whole guest frames up front so the
                // frame-start VRAM snapshot below is taken only when the
                // guest will actually run this redraw. The snapshot is a
                // 1 MB clone; on redraws that retire zero guest frames
                // (paused, or the off-beat of a 120 Hz host pacing a 60 Hz
                // guest) it would be identical to live VRAM anyway, which
                // is exactly what the replay path falls back to.
                let active_frame_dt =
                    guest_frame_dt(self.state.bus.as_ref().map(|bus| bus.vblank_period()));
                let frames_to_run = if self.state.running {
                    self.emu_frame_accum = (self.emu_frame_accum + dt).min(0.25);
                    ((self.emu_frame_accum / active_frame_dt) as u32).min(MAX_CATCHUP_FRAMES)
                } else {
                    0
                };
                let hw_frame_start_vram = if frames_to_run > 0 {
                    self.state
                        .bus
                        .as_ref()
                        .map(|bus| bus.gpu.vram.words().to_vec())
                } else {
                    None
                };

                // Run loop: retire one video frame's worth of PSX cycles
                // if we're in run mode. Any execution error auto-pauses
                // and surfaces via the register panel. History captures
                // only the tail via `push_history`'s ring-buffer semantics.
                if self.state.running {
                    // Route keyboard input and each physical controller to the
                    // selected guest port. UI/freecam shortcuts still use the
                    // all-device merged state above.
                    let keyboard_left = self.host_input.left_stick.vector();
                    let keyboard_right = self.host_input.right_stick.vector();
                    let mut port1 = pad_frame.port1;
                    let mut port2 = pad_frame.port2;
                    match self.input.keyboard_port() {
                        input::PsxPort::One => {
                            port1.mask = self.host_input.resolved_mask(port1.mask);
                            port1.left_stick = merge_sticks(port1.left_stick, keyboard_left);
                            port1.right_stick = merge_sticks(port1.right_stick, keyboard_right);
                        }
                        input::PsxPort::Two => {
                            port2.mask = self.host_input.resolved_mask(port2.mask);
                            port2.left_stick = merge_sticks(port2.left_stick, keyboard_left);
                            port2.right_stick = merge_sticks(port2.right_stick, keyboard_right);
                        }
                        input::PsxPort::Off => {}
                    }

                    // Feed the guest the routed masks + sticks computed above.
                    // While freecam is engaged the pad belongs to the CAMERA,
                    // so both guest ports see neutral controllers: sticks
                    // centred AND no buttons. Neutralising only the sticks left the
                    // face and shoulder buttons live, so lining up a shot still
                    // fired the weapon, opened menus, or walked the player off
                    // the spot being framed. The chord itself is read from
                    // `merged_mask` above, before this, so L3+R3 keeps working.
                    if self.state.freelook.enabled {
                        port1 = input::RoutedPadInput::default();
                        port2 = input::RoutedPadInput::default();
                    }
                    let live_pad_sample =
                        Port1PadSample::from_host(port1.mask, port1.right_stick, port1.left_stick);
                    let live_port2_sample =
                        Port1PadSample::from_host(port2.mask, port2.right_stick, port2.left_stick);
                    for _ in 0..frames_to_run {
                        // Recording/replay happens at one authoritative video-
                        // frame port-1 boundary for emulator, editor and headless
                        // runs alike.
                        let pad_sample = self.state.input_sample_for_frame(live_pad_sample);
                        if let Some(bus) = self.state.bus.as_mut() {
                            pad_sample.apply_to_bus(bus);
                            live_port2_sample.apply_to_bus_port2(bus);
                        }
                        let polls_before = self
                            .state
                            .bus
                            .as_ref()
                            .map(|bus| bus.port1_completed_polls())
                            .unwrap_or(0);
                        let draw_log_start = self
                            .state
                            .bus
                            .as_ref()
                            .map(|bus| bus.gpu.cmd_log.len())
                            .unwrap_or(0);
                        let emu_start = Instant::now();
                        let step_report = app::step_one_frame(&mut self.state);
                        // The pad state above was live for the whole frame, so
                        // every poll the guest completed inside it read exactly
                        // `pad_sample`. Recording one tape entry per poll makes
                        // the tape indexed by the guest's input clock.
                        let polls_after = self
                            .state
                            .bus
                            .as_ref()
                            .map(|bus| bus.port1_completed_polls())
                            .unwrap_or(polls_before);
                        self.state
                            .input_note_polls(pad_sample, polls_after.saturating_sub(polls_before));
                        profile.emu_ms += elapsed_ms(emu_start);
                        profile.frames_run += 1.0;
                        profile.psx_budget_cycles += step_report.target_cycles as f32;
                        profile.psx_vblanks += step_report.vblanks as f32;
                        if step_report.vblanks > 0
                            && self
                                .state
                                .bus
                                .as_ref()
                                .map(|bus| gpu_log_has_draw(&bus.gpu.cmd_log[draw_log_start..]))
                                .unwrap_or(false)
                        {
                            profile.psx_draw_vblanks += 1.0;
                        }
                        if step_report.hit_step_cap {
                            profile.psx_step_cap_misses += 1.0;
                        }

                        // Pump the SPU by however much emulated time the
                        // CPU just advanced, not by "one host redraw".
                        // This keeps audio pacing tied to the PSX master
                        // clock even on 120 Hz / 144 Hz hosts or slow
                        // frames, matching the SPU's 768-cycles/sample
                        // timing model.
                        let audio_start = Instant::now();
                        let effective_audio_volume = self.state.effective_audio_volume();
                        let (guest_events, guest_debug_logs) =
                            if let Some(bus) = self.state.bus.as_mut() {
                                bus.run_spu_to_current_cycle();
                                if let Some(audio) = self.audio.as_ref() {
                                    audio.set_volume(effective_audio_volume);
                                    let samples = bus.spu.drain_audio();
                                    if !samples.is_empty() {
                                        audio.push_samples(&samples);
                                    }
                                    // Surface the cpal ring depth in the HUD.
                                    self.state.hud.set_audio_queue_len(audio.queue_len());
                                } else {
                                    // No output device -- drain and discard so the
                                    // SPU's internal queue doesn't grow unbounded.
                                    let _ = bus.spu.drain_audio();
                                }
                                let events = bus.telemetry.drain_events();
                                let logs = bus.telemetry.drain_debug_logs();
                                (events, logs)
                            } else {
                                (Vec::new(), Vec::new())
                            };
                        let guest_profile = self.state.profiler.consume_guest_events(&guest_events);
                        if !guest_debug_logs.is_empty() {
                            // Surface guest debug logs to the host console. The
                            // editor routes them to its Play debug terminal; in
                            // the plain emulator/library frontend there is no such
                            // panel, so without this they were silently dropped.
                            for line in &guest_debug_logs {
                                eprintln!("[guest f{} c{}] {}", line.frame, line.cycles, line.text);
                            }
                            self.state.append_guest_debug_logs(guest_debug_logs);
                        }
                        profile.add_guest_profile(guest_profile);
                        profile.audio_ms += elapsed_ms(audio_start);
                    }
                    self.emu_frame_accum -= (frames_to_run as f32) * active_frame_dt;
                } else {
                    self.emu_frame_accum = 0.0;
                }
                profile.cpu_ticks = self.state.cpu.tick().saturating_sub(cpu_tick_before) as f32;
                profile.bus_cycles = self
                    .state
                    .bus
                    .as_ref()
                    .map(|bus| bus.cycles().saturating_sub(bus_cycles_before))
                    .unwrap_or(0) as f32;
                let gte_profile_after = self.state.cpu.cop2().profile_snapshot();
                profile.gte_ops =
                    gte_profile_after.ops.saturating_sub(gte_profile_before.ops) as f32;
                profile.gte_estimated_cycles = gte_profile_after
                    .estimated_cycles
                    .saturating_sub(gte_profile_before.estimated_cycles)
                    as f32;

                let state = &mut self.state;
                let input_router = &mut self.input;

                // Post-step machine stamp. Guest VRAM can only change when
                // the bus advances (run loop above, or an MCP `step` /
                // `load_game` drained at the top of this redraw, both of
                // which move `cycles` or bump the resync generation), so an
                // unchanged stamp means every VRAM-derived sync below --
                // compute-backend mirror, RGBA debug view, HW sampler
                // upload -- would rewrite identical bytes and is skipped.
                // Wireframe sits in the stamp so toggling it while paused
                // still repaints the HW target.
                let vram_stamp = state.bus.as_ref().map(|bus| {
                    (
                        bus.cycles(),
                        state.gpu_resync_generation,
                        bus.gpu.wireframe_enabled,
                    )
                });
                let vram_dirty = vram_stamp != self.vram_synced_stamp;

                let cmd_log_start = Instant::now();
                let frame_log = if let Some(bus) = state.bus.as_mut() {
                    bus.gpu.drain_completed_cmd_log()
                } else {
                    Vec::new()
                };
                profile.cmd_log_ms = elapsed_ms(cmd_log_start);
                let (gpu_cmds, gpu_words, gpu_draw_cmds, gpu_image_cmds) =
                    gpu_log_counters(&frame_log);
                profile.gpu_cmds = gpu_cmds as f32;
                profile.gpu_words = gpu_words as f32;
                profile.gpu_draw_cmds = gpu_draw_cmds as f32;
                profile.gpu_image_cmds = gpu_image_cmds as f32;

                // Phase C: drain the CPU rasterizer's `cmd_log` and
                // replay each GP0 packet onto the compute backend.
                // This runs for every frame the bus advanced (or
                // not, when paused -- in which case `cmd_log` will
                // be empty and the loop is a no-op).
                let compute_start = Instant::now();
                if let (true, Some(backend), Some(bus)) = (
                    vram_dirty,
                    self.compute_backend.as_mut(),
                    state.bus.as_mut(),
                ) {
                    // Sync VRAM so any uploads / FMV writes / VRAM-to-
                    // VRAM copies are reflected on the compute side
                    // before we replay this frame's draw commands. With a
                    // clean stamp `frame_log` is empty and pixel_owner has
                    // not accumulated, so the whole body is skippable.
                    backend.sync_vram_from_cpu(bus.gpu.vram.words());
                    for entry in &frame_log {
                        backend.replay_packet(entry);
                    }
                    // pixel_owner needs resetting too -- we don't use
                    // its data here, but its `current_cmd_index`
                    // would otherwise drift past u32::MAX over time.
                    if let Some(owner) = bus.gpu.pixel_owner.as_mut() {
                        owner.fill(u32::MAX);
                    }
                }
                profile.compute_ms = elapsed_ms(compute_start);

                // The 24bpp/offset display fallback only reflects guest
                // scanout state, so a clean stamp skips it. The bus-went-
                // away transition still lands here once (Some -> None flips
                // the stamp) so `prepare_display` clears its texture.
                if vram_dirty {
                    let display_upload_start = Instant::now();
                    gfx.prepare_display(state.bus.as_ref().map(|b| &b.gpu));
                    profile.display_upload_ms = elapsed_ms(display_upload_start);
                }

                // Match the HW renderer's internal scale to the
                // current Native↔Window mode + framebuffer pixel budget.
                // Cheap when stable; reallocates the VRAM-shaped
                // target on change. Reallocation clears the target,
                // so we immediately resync it from CPU VRAM before
                // replaying this frame's command log.
                gfx.set_hw_texture_filter(state.texture_filter.mode());
                let scale_mode = match state.scale_mode {
                    app::ScaleMode::Native => psx_gpu_render::ScaleMode::Native,
                    app::ScaleMode::Window => psx_gpu_render::ScaleMode::Window,
                };
                let display_size = state
                    .bus
                    .as_ref()
                    .map(|b| {
                        let area = b.gpu.display_area();
                        (area.width as u32, area.height as u32)
                    })
                    .unwrap_or((320, 240));
                let hw_scale_start = Instant::now();
                let hw_scale_changed = gfx.update_hw_scale(
                    scale_mode,
                    state.framebuffer_present_size_px,
                    display_size,
                );
                profile.hw_scale_ms = elapsed_ms(hw_scale_start);
                profile.hw_scale = gfx.hw_internal_scale() as f32;
                let hw_target_needs_resync = {
                    let display_bpp24 = state
                        .bus
                        .as_ref()
                        .is_some_and(|bus| bus.gpu.display_area().bpp24);
                    hw_target_needs_resync(
                        &mut self.hw_seen_gpu_resync_generation,
                        &mut self.hw_last_display_bpp24,
                        state.gpu_resync_generation,
                        display_bpp24,
                    )
                };

                // Drive the hardware renderer when this redraw can have
                // changed it: guest VRAM moved (dirty stamp), the internal
                // scale was reallocated, or a resync was requested. The
                // VRAM-shaped target persists across frames the way PSX
                // VRAM does -- which is exactly why a clean, stable frame
                // can present the existing target without re-uploading a
                // megabyte of unchanged VRAM first. The framebuffer panel
                // UV-samples the active display sub-rect.
                if vram_dirty || hw_scale_changed || hw_target_needs_resync {
                    if let Some(bus) = state.bus.as_mut() {
                        let clone_start = Instant::now();
                        let frame_start_vram = hw_frame_start_vram
                            .as_deref()
                            .unwrap_or_else(|| bus.gpu.vram.words());
                        profile.hw_vram_clone_ms = elapsed_ms(clone_start);
                        if hw_scale_changed || hw_target_needs_resync {
                            gfx.sync_hw_target_from_vram(frame_start_vram);
                        }
                        let hw_render_start = Instant::now();
                        gfx.render_hw_frame(&bus.gpu, &frame_log, frame_start_vram);
                        profile.hw_render_ms = elapsed_ms(hw_render_start);
                    } else {
                        let empty_log: Vec<emulator_core::gpu::GpuCmdLogEntry> = Vec::new();
                        let empty_vram: Vec<u16> = vec![0; 1024 * 512];
                        let dummy_gpu = emulator_core::Gpu::new();
                        let hw_render_start = Instant::now();
                        gfx.render_hw_frame(&dummy_gpu, &empty_log, &empty_vram);
                        profile.hw_render_ms = elapsed_ms(hw_render_start);
                    }
                }
                self.vram_synced_stamp = vram_stamp;

                // VRAM debug view: GPU-side expand of the HW renderer's
                // R16Uint VRAM mirror (kept current by the block above)
                // into the texture the sidebar samples. Runs after the HW
                // frame so an open sidebar shows this frame, and only when
                // the sidebar can actually be seen -- a hidden panel costs
                // nothing at all.
                if state.panels.debug_sidebar && state.bus.is_some() {
                    let vram_upload_start = Instant::now();
                    gfx.prepare_vram();
                    profile.vram_upload_ms = elapsed_ms(vram_upload_start);
                }

                // Editor 3D preview: drive the editor-owned HwRenderer
                // while editing. During embedded Play, the viewport
                // paints the live emulator framebuffer instead. Gated on
                // the editor workspace actually being front: the refresh
                // stats every material file on disk for hot-reload, which
                // is pure per-frame syscall churn while playing a game in
                // the emulator workspace.
                #[cfg(feature = "editor")]
                if state.workspace.is_editor()
                    && !state.embedded_playtest_running()
                    && state.editor.editor_3d_preview_visible()
                {
                    let editor_camera = state.editor.viewport_3d_camera();
                    let editor_preview_bounds = state.editor.preview_bounds_enabled();
                    let editor_show_grid = state.editor.show_grid_enabled();
                    let editor_show_brush_surface_grid =
                        state.editor.show_brush_surface_grid_enabled();
                    let editor_grid_units = state.editor.grid_snap_units();
                    let editor_bsp_leak_path = state.editor.visible_bsp_leak_path();
                    let editor_bsp_leak_opening = state.editor.visible_bsp_leak_opening();
                    let editor_show_lights = state.editor.show_lights_enabled();
                    let editor_hidden_scene_nodes = state.editor.hidden_scene_nodes();
                    let editor_selected = state.editor.selected_node_id();
                    let editor_character_motion = state.editor.character_motion_preview();
                    let editor_root = state.editor.project_root();
                    let editor_selected_bounds = state.editor.selected_bounds_3d();
                    let editor_entity_bounds = state.editor.collect_entity_bounds(None);
                    let editor_hovered_entity = state.editor.hovered_entity_node();
                    gfx.render_editor_preview(
                        state.editor.project(),
                        editor_root,
                        editor_camera,
                        editor_preview_bounds,
                        editor_show_grid,
                        editor_show_brush_surface_grid,
                        editor_grid_units,
                        editor_bsp_leak_path,
                        editor_bsp_leak_opening,
                        editor_show_lights,
                        editor_hidden_scene_nodes,
                        editor_selected,
                        editor_character_motion,
                        editor_selected_bounds,
                        &editor_entity_bounds,
                        editor_hovered_entity,
                    );
                }
                #[cfg(feature = "editor")]
                let editor_camera_preview = if !state.embedded_playtest_running() {
                    state.editor.selected_camera_preview_request()
                } else {
                    None
                };
                #[cfg(feature = "editor")]
                if let Some(request) = editor_camera_preview {
                    let editor_hidden_scene_nodes = state.editor.hidden_scene_nodes();
                    let editor_root = state.editor.project_root();
                    gfx.render_editor_camera_preview(
                        state.editor.project(),
                        editor_root,
                        request,
                        editor_hidden_scene_nodes,
                    );
                }

                let vram_tex = gfx.vram_texture_id();
                let (display_tex, display_uv) = frontend_display(state.bus.as_ref(), gfx);
                #[cfg(feature = "editor")]
                let editor_viewport = {
                    let mut editor_viewport = if state.embedded_playtest_running() {
                        psxed_ui::EditorViewport3dPresentation::play(
                            display_tex,
                            display_uv,
                            state.editor_playtest_input_tape_status(),
                            editor_play_metrics(state),
                            state
                                .bus
                                .as_ref()
                                .is_some_and(|bus| bus.gpu.wireframe_enabled),
                        )
                    } else {
                        psxed_ui::EditorViewport3dPresentation::edit(
                            gfx.editor_hw_texture_id(),
                            gfx.editor_overlay_lines().to_vec(),
                        )
                    };
                    if editor_camera_preview.is_some() {
                        editor_viewport =
                            editor_viewport.with_camera_preview(gfx.camera_preview_texture_id());
                    }
                    editor_viewport
                };
                let mut pointer_menu_outcome = None;
                profile.egui = gfx.render(|ctx| {
                    app::build_ui(
                        ctx,
                        state,
                        input_router,
                        vram_tex,
                        display_tex,
                        #[cfg(feature = "editor")]
                        editor_viewport.clone(),
                        display_uv,
                        dt,
                    );
                    // Pointer actions are discovered while egui paints. Apply
                    // them in this same redraw so browser file inputs retain
                    // the click's transient user activation.
                    if let Some(action) = state.menu.take_pending_pointer_action() {
                        pointer_menu_outcome = Some(ui::apply_menu_action(state, action));
                    }
                });
                match pointer_menu_outcome {
                    Some(MenuOutcome::ClearHostKeyboardInput) => self.host_input.clear(),
                    Some(MenuOutcome::Quit) => {
                        state.stop_input_recording_if_active();
                        #[cfg(feature = "editor")]
                        state.stop_embedded_playtest();
                        state.flush_pending_input_profile_capture();
                        state.stop_examples_build();
                        if let Err(error) = state.flush_memcard_port1() {
                            eprintln!("[frontend] memcard flush on quit: {error}");
                        }
                        #[cfg(feature = "editor")]
                        if let Err(error) = state.save_editor_project() {
                            eprintln!("[frontend] editor save on quit: {error}");
                        }
                        if let Err(error) = state.save_settings() {
                            eprintln!("[frontend] settings save on quit: {error}");
                        }
                        event_loop.exit();
                        return;
                    }
                    Some(MenuOutcome::None) | None => {}
                }
                #[cfg(not(target_arch = "wasm32"))]
                for path in state.take_pending_savestate_thumbnails() {
                    let result = match state.bus.as_ref() {
                        Some(bus) => gfx.write_savestate_thumbnail(&bus.gpu, &path),
                        None => Err("emulator stopped after save".to_string()),
                    };
                    match result {
                        Ok(()) => state.refresh_save_state_menu_rows(),
                        Err(error) => {
                            eprintln!("[frontend] save-state thumbnail: {error}");
                        }
                    }
                }
                #[cfg(feature = "editor")]
                if let Some(request) = state.editor.take_playtest_request() {
                    state.handle_editor_playtest_request(request);
                }
                profile.total_ms = elapsed_ms(profile_start);
                if let Some(line) = state.profiler.record(profile) {
                    eprintln!("{line}");
                }
                state.flush_pending_input_profile_capture();
            }
            _ => {
                if !consumed {
                    gfx.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Native: schedule the next redraw instead of redrawing at host
        // vsync unconditionally (the old Poll + request-every-tick shell
        // rebuilt an unchanged UI at 120 Hz and burned half a core idle).
        #[cfg(not(target_arch = "wasm32"))]
        self.schedule_next_redraw(event_loop);

        // On the web, GPU init finishes asynchronously after `resumed`; pick it
        // up here so the first redraw can fire once it lands. The browser paces
        // rAF-driven redraws itself, so the web shell keeps redrawing per tick.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = event_loop;
            self.install_pending_graphics();
            // Apply any BIOS / game file the user picked since the last frame.
            self.state.poll_web_uploads();
            // winit on web does not auto-size the canvas to the page, so the
            // surface and egui's screen rect (both read from inner_size()) would
            // stay at the 1x1 init size and nothing would be visible. Match the
            // canvas to the browser window, but only when it actually changed
            // (setting a canvas's size clears it, which would flicker per frame).
            if let Some(gfx) = self.graphics.as_ref() {
                if let Some(desired) = web_window_logical_size() {
                    let sf = gfx.window.scale_factor();
                    let current = gfx.window.inner_size().to_logical::<f64>(sf);
                    if (current.width - desired.width).abs() > 0.5
                        || (current.height - desired.height).abs() > 0.5
                    {
                        let _ = gfx.window.request_inner_size(desired);
                    }
                }
            }
            if let Some(gfx) = self.graphics.as_ref() {
                gfx.window.request_redraw();
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Shell {
    /// Decide when the next redraw is due and park the loop until then.
    ///
    /// Running: wake exactly when the wall clock owes the guest its next
    /// active machine frame (`emu_frame_accum` holds the unpaid fraction, anchored
    /// at `last_frame`); the Fifo present keeps the actual paint aligned
    /// to vblank. Paused: take the earliest of egui's own repaint request
    /// (slide animations, caret blink) and a tick -- ACTIVE_TICK_DT while
    /// interaction is plausible (recent pad input, Menu overlay, editor
    /// workspace), IDLE_TICK_DT otherwise. Every tick's redraw still
    /// drains queued MCP tool calls and polls gilrs, so debug tooling and
    /// pad hot-plug keep working while idle. Keyboard and mouse never
    /// wait for a tick: their window events request immediate redraws in
    /// `window_event`.
    fn schedule_next_redraw(&mut self, event_loop: &ActiveEventLoop) {
        use std::time::Duration;

        let Some(gfx) = self.graphics.as_ref() else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        let now = Instant::now();
        let next = if self.state.running {
            let frame_dt = guest_frame_dt(self.state.bus.as_ref().map(|bus| bus.vblank_period()));
            let owed = (frame_dt - self.emu_frame_accum).max(0.0);
            self.last_frame
                .checked_add(Duration::from_secs_f32(owed))
                .unwrap_or(now)
        } else {
            let in_editor = self.state.workspace.is_editor();
            let pad_active =
                now.duration_since(self.last_pad_activity).as_secs_f32() < PAD_ACTIVITY_WINDOW_SECS;
            let tick = if self.state.menu.open || in_editor || pad_active {
                ACTIVE_TICK_DT
            } else {
                IDLE_TICK_DT
            };
            let tick_next = self
                .last_frame
                .checked_add(Duration::from_secs_f32(tick))
                .unwrap_or(now);
            // egui measures its repaint delay from the end of the last
            // frame; `Duration::MAX` (nothing animating) overflows to None.
            match self.last_frame.checked_add(gfx.repaint_delay()) {
                Some(egui_next) => tick_next.min(egui_next),
                None => tick_next,
            }
        };
        if next <= now {
            gfx.window.request_redraw();
            // Park on a short safety deadline rather than `Poll`: the
            // redraw event wakes the loop as soon as it is delivered, and
            // if the OS coalesces or suppresses it (occluded window) the
            // deadline re-runs this scheduler instead of busy-looping.
            event_loop.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(16)));
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl Shell {
    /// Move the deferred web GPU init into `self.graphics` once `spawn_local`
    /// has finished building it, and request the first redraw. Cheap to call
    /// every tick: it only does work on the tick the `Graphics` first arrives.
    fn install_pending_graphics(&mut self) {
        if self.graphics.is_some() {
            return;
        }
        if let Some(graphics) = self.graphics_init.borrow_mut().take() {
            graphics.window.request_redraw();
            self.graphics = Some(graphics);
        }
    }
}

/// The browser window's logical (CSS-pixel) size, used to keep the winit canvas
/// matched to the page. winit on web does not size the canvas automatically.
#[cfg(target_arch = "wasm32")]
fn web_window_logical_size() -> Option<winit::dpi::LogicalSize<f64>> {
    let win = web_sys::window()?;
    let w = win.inner_width().ok()?.as_f64()?;
    let h = win.inner_height().ok()?.as_f64()?;
    Some(winit::dpi::LogicalSize::new(w.max(1.0), h.max(1.0)))
}

/// OR a keypress into the next-frame Menu input. `Escape` both toggles
/// the overlay and acts as back when navigating; the combined semantics
/// are handled inside `MenuState::update`.
fn merge_key(mut input: MenuInput, key: &Key) -> MenuInput {
    match key {
        Key::Named(NamedKey::ArrowUp) => input.up = true,
        Key::Named(NamedKey::ArrowDown) => input.down = true,
        Key::Named(NamedKey::ArrowLeft) => input.left = true,
        Key::Named(NamedKey::ArrowRight) => input.right = true,
        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => input.confirm = true,
        Key::Named(NamedKey::Escape) => {
            input.toggle_open = true;
            input.back = true;
        }
        _ => {}
    }
    input
}

fn gpu_log_counters(log: &[emulator_core::gpu::GpuCmdLogEntry]) -> (usize, usize, usize, usize) {
    let mut words = 0usize;
    let mut draw_cmds = 0usize;
    let mut image_cmds = 0usize;
    for entry in log {
        words = words.saturating_add(entry.fifo.len());
        match entry.opcode {
            0x20..=0x7F => draw_cmds += 1,
            0x80..=0xBF => image_cmds += 1,
            _ => {}
        }
    }
    (log.len(), words, draw_cmds, image_cmds)
}

fn gpu_log_has_draw(log: &[emulator_core::gpu::GpuCmdLogEntry]) -> bool {
    log.iter().any(|entry| matches!(entry.opcode, 0x20..=0x7F))
}

fn hw_target_needs_resync(
    seen_generation: &mut u64,
    last_display_bpp24: &mut bool,
    current_generation: u64,
    current_display_bpp24: bool,
) -> bool {
    let generation_changed = *seen_generation != current_generation;
    *seen_generation = current_generation;
    let leaving_24bpp = *last_display_bpp24 && !current_display_bpp24;
    *last_display_bpp24 = current_display_bpp24;
    generation_changed || leaving_24bpp
}

fn frontend_display(
    bus: Option<&emulator_core::Bus>,
    gfx: &gfx::Graphics,
) -> (egui::TextureId, egui::Rect) {
    let area = display_area_or_default(bus);
    // Only true-colour (24bpp) frames must use the CPU display texture -- the HW
    // target is BGR15 and can't represent 24bpp pre-rendered backgrounds (some commercial titles
    // rooms). Everything else, INCLUDING 16bpp frames with a GP1(06/07) screen
    // offset, goes through the upscaling HW target so the Window/hi-res toggle
    // actually applies. Before, any non-zero display offset forced the native CPU
    // path, so a game with a constant 1px pan (h_off=-1, or h_off=-1/v_off=8)
    // silently lost hi-res while an offset-free sibling kept it.
    // ponytail: the fine display offset (a few px of CRT-window pan) is dropped in
    // the HW path -- imperceptible in a scale-to-fit window. Apply it at the paint
    // rect if an animated-offset (screen-shake) title ever needs it.
    if area.bpp24 {
        return (gfx.display_texture_id(), cpu_display_uv(area));
    }
    (gfx.hw_texture_id(), hw_display_uv(area))
}

fn display_area_or_default(bus: Option<&emulator_core::Bus>) -> emulator_core::DisplayArea {
    bus.map(|b| b.gpu.display_area())
        .unwrap_or(emulator_core::DisplayArea {
            x: 0,
            y: 0,
            width: 320,
            height: 240,
            bpp24: false,
        })
}

fn cpu_display_uv(area: emulator_core::DisplayArea) -> egui::Rect {
    let width = area.width.max(1) as f32;
    let height = area.height.max(1) as f32;
    egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2(
            width / gfx::MAX_DISPLAY_WIDTH as f32,
            height / gfx::MAX_DISPLAY_HEIGHT as f32,
        ),
    )
}

fn hw_display_uv(area: emulator_core::DisplayArea) -> egui::Rect {
    let width = area.width.max(1) as f32;
    let height = area.height.max(1) as f32;
    egui::Rect::from_min_max(
        egui::pos2(
            area.x as f32 / psx_gpu_render::VRAM_WIDTH as f32,
            area.y as f32 / psx_gpu_render::VRAM_HEIGHT as f32,
        ),
        egui::pos2(
            (area.x as f32 + width) / psx_gpu_render::VRAM_WIDTH as f32,
            (area.y as f32 + height) / psx_gpu_render::VRAM_HEIGHT as f32,
        ),
    )
}

#[cfg(feature = "editor")]
fn editor_play_metrics(state: &app::AppState) -> Option<psxed_ui::EditorPlaytestMetrics> {
    let latest = state.profiler.latest()?;
    let sample = state
        .profiler
        .live_average()
        .or_else(|| state.profiler.average())
        .unwrap_or(latest);
    let visual_hz = sample.guest_visual_frame_hz();
    let display_hz = visual_hz.unwrap_or_else(|| sample.psx_draw_hz());
    let visual_interval_vblanks = latest
        .guest_visual_interval_vblanks()
        .or_else(|| sample.guest_visual_interval_vblanks())
        .unwrap_or(0.0);
    let (visual_frame_times_ms, visual_frame_time_count) = latest.guest.visual_frame_intervals_ms();
    let visual_deadline_misses = latest
        .guest_visual_deadline_misses()
        .round()
        .clamp(0.0, u32::MAX as f32) as u32;
    let visual_lateness_vblanks = latest
        .guest_visual_max_lateness_vblanks()
        .round()
        .clamp(0.0, u32::MAX as f32) as u32;
    let frame_ms = if display_hz > 0.0 {
        1000.0 / display_hz
    } else {
        latest.total_ms
    };
    let task_ms_per_hit = |task_id: u16| {
        let task_id = task_id as usize;
        let hits = sample.guest.task_hits[task_id];
        if hits > 0.0 {
            sample.guest.task_cycles[task_id] / hits / PSX_CYCLES_PER_MS
        } else {
            0.0
        }
    };
    let task_max_ms =
        |task_id: u16| sample.guest.task_max_cycles[task_id as usize] / PSX_CYCLES_PER_MS;
    const DEBUG_MAP_POSITION_BIAS: i32 = 1_000_000;
    const CHUNK_MAP_COUNTERS: &[u16] = &[
        counter::ROOM_STREAM_RESIDENT_MASK_LO,
        counter::ROOM_STREAM_RESIDENT_MASK_HI,
        counter::ROOM_STREAM_LOADING_MASK_LO,
        counter::ROOM_STREAM_LOADING_MASK_HI,
        counter::ROOM_ACTIVE_CHUNK_MASK_LO,
        counter::ROOM_ACTIVE_CHUNK_MASK_HI,
        counter::ROOM_DRAWN_CHUNK_MASK_LO,
        counter::ROOM_DRAWN_CHUNK_MASK_HI,
        counter::ROOM_PLAYER_ROOM_INDEX,
        counter::PORTAL_VIS_CURRENT_ROOM,
        counter::ROOM_PLAYER_LOCAL_X_BIASED,
        counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        counter::ROOM_PLAYER_VIEW_YAW_Q12,
        counter::ROOM_CAMERA_LOCAL_X_BIASED,
        counter::ROOM_CAMERA_LOCAL_Y_BIASED,
        counter::ROOM_CAMERA_LOCAL_Z_BIASED,
        counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED,
        counter::PORTAL_VIS_VISIBLE_MASK_LO,
        counter::PORTAL_VIS_VISIBLE_MASK_HI,
        counter::PORTAL_VIS_FRONTIER_MASK_LO,
        counter::PORTAL_VIS_FRONTIER_MASK_HI,
        counter::PORTAL_VIS_MISSING_MASK_LO,
        counter::PORTAL_VIS_MISSING_MASK_HI,
        counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
        counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
        counter::PORTAL_VIS_TESTED_MASK_LO,
        counter::PORTAL_VIS_TESTED_MASK_HI,
        counter::PORTAL_VIS_ACCEPTED_MASK_LO,
        counter::PORTAL_VIS_ACCEPTED_MASK_HI,
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO,
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI,
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO,
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI,
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO,
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI,
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO,
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI,
    ];
    const RENDER_MAP_COUNTERS: &[u16] = &[
        counter::ROOM_ACTIVE_CHUNK_MASK_LO,
        counter::ROOM_ACTIVE_CHUNK_MASK_HI,
        counter::ROOM_DRAWN_CHUNK_MASK_LO,
        counter::ROOM_DRAWN_CHUNK_MASK_HI,
        counter::ROOM_STREAM_LOADING_MASK_LO,
        counter::ROOM_STREAM_LOADING_MASK_HI,
        counter::ROOM_PLAYER_ROOM_INDEX,
        counter::PORTAL_VIS_CURRENT_ROOM,
        counter::ROOM_PLAYER_LOCAL_X_BIASED,
        counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        counter::ROOM_PLAYER_VIEW_YAW_Q12,
        counter::ROOM_CAMERA_LOCAL_X_BIASED,
        counter::ROOM_CAMERA_LOCAL_Y_BIASED,
        counter::ROOM_CAMERA_LOCAL_Z_BIASED,
        counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED,
        counter::PORTAL_VIS_VISIBLE_MASK_LO,
        counter::PORTAL_VIS_VISIBLE_MASK_HI,
        counter::PORTAL_VIS_FRONTIER_MASK_LO,
        counter::PORTAL_VIS_FRONTIER_MASK_HI,
        counter::PORTAL_VIS_MISSING_MASK_LO,
        counter::PORTAL_VIS_MISSING_MASK_HI,
        counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
        counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
        counter::PORTAL_VIS_TESTED_MASK_LO,
        counter::PORTAL_VIS_TESTED_MASK_HI,
        counter::PORTAL_VIS_ACCEPTED_MASK_LO,
        counter::PORTAL_VIS_ACCEPTED_MASK_HI,
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO,
        counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI,
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO,
        counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI,
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO,
        counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI,
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO,
        counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO,
        counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI,
    ];
    const RENDER_MAP_POSE_COUNTERS: &[u16] = &[
        counter::ROOM_PLAYER_LOCAL_X_BIASED,
        counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
    ];
    let pose_sample = state
        .profiler
        .latest_with_all_guest_counters(RENDER_MAP_POSE_COUNTERS);
    let chunk_sample = state
        .profiler
        .latest_with_guest_counters(RENDER_MAP_COUNTERS)
        .or_else(|| {
            state
                .profiler
                .latest_with_guest_counters(CHUNK_MAP_COUNTERS)
        })
        .unwrap_or(sample);
    let pose_counter_sample = pose_sample.unwrap_or(chunk_sample);
    let recent_counter = |id: u16| profile_counter_u32(sample.guest.counter_max_value(id as usize));
    let chunk_mask = |lo: u16, hi: u16| {
        let lo = chunk_sample.guest.counter_latest_value(lo as usize) as u64;
        let hi = chunk_sample.guest.counter_latest_value(hi as usize) as u64;
        lo | (hi << 32)
    };
    let player_x_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_PLAYER_LOCAL_X_BIASED as usize);
    let player_z_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_PLAYER_LOCAL_Z_BIASED as usize);
    let camera_x_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_X_BIASED as usize);
    let camera_y_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_Y_BIASED as usize);
    let camera_z_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_Z_BIASED as usize);
    let camera_global_x_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_X_BIASED as usize);
    let camera_global_y_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_Y_BIASED as usize);
    let camera_global_z_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_Z_BIASED as usize);
    let camera_view_sin_yaw_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED as usize);
    let camera_view_cos_yaw_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED as usize);
    let camera_view_sin_pitch_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED as usize);
    let camera_view_cos_pitch_biased = pose_counter_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED as usize);
    let pose_valid = pose_sample.is_some();
    Some(psxed_ui::EditorPlaytestMetrics {
        sample_serial: latest.sample_serial,
        host_fps: sample.host_fps(),
        host_ms: sample.host_dt_ms,
        emu_hz: sample.emulated_vblank_hz(),
        visual_hz,
        draw_hz: sample.psx_draw_hz(),
        visual_frames: latest
            .guest_visual_frame_count()
            .round()
            .clamp(0.0, u32::MAX as f32) as u32,
        visual_interval_vblanks,
        visual_frame_times_ms,
        visual_frame_time_count,
        visual_deadline_misses,
        visual_lateness_vblanks,
        total_ms: sample.total_ms,
        frame_ms,
        emu_ms: sample.emu_ms,
        hw_ms: sample.hw_render_ms,
        ui_ms: sample.egui.total_ms,
        step_budget_percent: sample.psx_budget_percent(),
        fixed_update_task_ms: task_ms_per_hit(task::FIXED_UPDATE),
        fixed_update_task_max_ms: task_max_ms(task::FIXED_UPDATE),
        visual_render_task_ms: task_ms_per_hit(task::VISUAL_RENDER),
        visual_render_task_max_ms: task_max_ms(task::VISUAL_RENDER),
        chunk_visible: recent_counter(counter::ROOM_ACTIVE_CHUNKS),
        chunk_loaded: recent_counter(counter::ROOM_STREAM_RESIDENT_SLOTS),
        chunk_candidates: recent_counter(counter::ROOM_CHUNKS_CONSIDERED),
        chunk_built: recent_counter(counter::ROOM_WINDOW_BUILT_CHUNKS),
        chunk_cache_skips: recent_counter(counter::ROOM_CHUNK_CACHE_SKIPS),
        portal_visible_rooms: recent_counter(counter::PORTAL_VIS_VISIBLE_ROOMS),
        portal_frontier_rooms: recent_counter(counter::PORTAL_VIS_FRONTIER_ROOMS),
        portal_missing_resident: recent_counter(counter::PORTAL_VIS_VISIBLE_MISSING_RESIDENT),
        portal_build_failed: recent_counter(counter::PORTAL_VIS_VISIBLE_BUILD_FAILED),
        portal_tests: recent_counter(counter::PORTAL_VIS_PORTALS_TESTED),
        portal_accepts: recent_counter(counter::PORTAL_VIS_PORTALS_ACCEPTED),
        portal_bounds_fallbacks: recent_counter(counter::PORTAL_VIS_BOUNDS_FALLBACKS),
        portal_rejects: [
            recent_counter(counter::PORTAL_VIS_REJECT_BACKFACE),
            recent_counter(counter::PORTAL_VIS_REJECT_FRUSTUM),
            recent_counter(counter::PORTAL_VIS_REJECT_TINY),
        ],
        portal_caps: [
            recent_counter(counter::PORTAL_VIS_CAP_ROOM),
            recent_counter(counter::PORTAL_VIS_CAP_FRUSTUM),
            recent_counter(counter::PORTAL_VIS_CAP_DEPTH),
        ],
        stream_priorities: [
            recent_counter(counter::ROOM_STREAM_PRIORITY_CURRENT),
            recent_counter(counter::ROOM_STREAM_PRIORITY_VISIBLE),
            recent_counter(counter::ROOM_STREAM_PRIORITY_FRONTIER),
        ],
        stream_requests: recent_counter(counter::ROOM_STREAM_REQUESTS),
        stream_misses: recent_counter(counter::ROOM_STREAM_MISSES),
        stream_prefetches: recent_counter(counter::ROOM_STREAM_PREFETCH_REQUESTS),
        stream_evictions: recent_counter(counter::ROOM_STREAM_EVICTIONS),
        stream_slot_limit: recent_counter(counter::ROOM_STREAM_SLOT_LIMIT),
        stream_pending: recent_counter(counter::ROOM_STREAM_PENDING_LOADS),
        stream_failed: recent_counter(counter::ROOM_STREAM_FAILED_LOADS),
        stream_protected_full: recent_counter(counter::ROOM_STREAM_PROTECTED_FULL),
        vram_texture_drops: recent_counter(counter::ROOM_MATERIAL_TEXTURE_DROPS),
        vram_caps_full: [
            recent_counter(counter::VRAM_SLOT_TABLE_FULL),
            recent_counter(counter::VRAM_WINDOW_FULL),
            recent_counter(counter::VRAM_CLUT_FULL),
            recent_counter(counter::VRAM_UPLOAD_QUEUE_FULL),
        ],
        room_material_slot_overflow: recent_counter(counter::ROOM_MATERIAL_SLOT_OVERFLOW),
        room_visibility_fallback_draws: recent_counter(counter::ROOM_VISIBILITY_FALLBACK_DRAWS),
        chunk_loaded_mask: chunk_mask(
            counter::ROOM_STREAM_RESIDENT_MASK_LO,
            counter::ROOM_STREAM_RESIDENT_MASK_HI,
        ),
        chunk_loading_mask: chunk_mask(
            counter::ROOM_STREAM_LOADING_MASK_LO,
            counter::ROOM_STREAM_LOADING_MASK_HI,
        ),
        chunk_active_mask: chunk_mask(
            counter::ROOM_ACTIVE_CHUNK_MASK_LO,
            counter::ROOM_ACTIVE_CHUNK_MASK_HI,
        ),
        chunk_drawn_mask: chunk_mask(
            counter::ROOM_DRAWN_CHUNK_MASK_LO,
            counter::ROOM_DRAWN_CHUNK_MASK_HI,
        ),
        portal_visible_mask: chunk_mask(
            counter::PORTAL_VIS_VISIBLE_MASK_LO,
            counter::PORTAL_VIS_VISIBLE_MASK_HI,
        ),
        portal_frontier_mask: chunk_mask(
            counter::PORTAL_VIS_FRONTIER_MASK_LO,
            counter::PORTAL_VIS_FRONTIER_MASK_HI,
        ),
        portal_missing_mask: chunk_mask(
            counter::PORTAL_VIS_MISSING_MASK_LO,
            counter::PORTAL_VIS_MISSING_MASK_HI,
        ),
        portal_build_failed_mask: chunk_mask(
            counter::PORTAL_VIS_BUILD_FAILED_MASK_LO,
            counter::PORTAL_VIS_BUILD_FAILED_MASK_HI,
        ),
        portal_tested_mask: chunk_mask(
            counter::PORTAL_VIS_TESTED_MASK_LO,
            counter::PORTAL_VIS_TESTED_MASK_HI,
        ),
        portal_accepted_mask: chunk_mask(
            counter::PORTAL_VIS_ACCEPTED_MASK_LO,
            counter::PORTAL_VIS_ACCEPTED_MASK_HI,
        ),
        portal_reject_frustum_mask: chunk_mask(
            counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_LO,
            counter::PORTAL_VIS_REJECT_FRUSTUM_MASK_HI,
        ),
        portal_bounds_fallback_mask: chunk_mask(
            counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_LO,
            counter::PORTAL_VIS_BOUNDS_FALLBACK_MASK_HI,
        ),
        portal_tested_portal_mask: chunk_mask(
            counter::PORTAL_VIS_TESTED_PORTAL_MASK_LO,
            counter::PORTAL_VIS_TESTED_PORTAL_MASK_HI,
        ),
        portal_accepted_portal_mask: chunk_mask(
            counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_LO,
            counter::PORTAL_VIS_ACCEPTED_PORTAL_MASK_HI,
        ),
        portal_reject_frustum_portal_mask: chunk_mask(
            counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_LO,
            counter::PORTAL_VIS_REJECT_FRUSTUM_PORTAL_MASK_HI,
        ),
        portal_bounds_fallback_portal_mask: chunk_mask(
            counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_LO,
            counter::PORTAL_VIS_BOUNDS_FALLBACK_PORTAL_MASK_HI,
        ),
        player_map_valid: pose_valid,
        player_room_index: pose_counter_sample
            .guest
            .counter_latest_value(counter::ROOM_PLAYER_ROOM_INDEX as usize),
        portal_current_room_index: pose_counter_sample
            .guest
            .counter_latest_value(counter::PORTAL_VIS_CURRENT_ROOM as usize),
        player_local_x: profile_counter_i32_biased(player_x_biased, DEBUG_MAP_POSITION_BIAS),
        player_local_z: profile_counter_i32_biased(player_z_biased, DEBUG_MAP_POSITION_BIAS),
        player_view_yaw_q12: pose_counter_sample
            .guest
            .counter_latest_value(counter::ROOM_PLAYER_VIEW_YAW_Q12 as usize)
            .min(u16::MAX as u32) as u16,
        camera_view_basis_valid: pose_valid
            && (camera_view_sin_yaw_biased > 0
                || camera_view_cos_yaw_biased > 0
                || camera_view_sin_pitch_biased > 0
                || camera_view_cos_pitch_biased > 0),
        camera_view_sin_yaw_q12: profile_counter_i32_biased(camera_view_sin_yaw_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_cos_yaw_q12: profile_counter_i32_biased(camera_view_cos_yaw_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_sin_pitch_q12: profile_counter_i32_biased(camera_view_sin_pitch_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_cos_pitch_q12: profile_counter_i32_biased(camera_view_cos_pitch_biased, 4096)
            .clamp(-4096, 4096),
        camera_map_valid: pose_valid
            && (camera_x_biased > 0 || camera_y_biased > 0 || camera_z_biased > 0),
        camera_global_valid: pose_valid
            && (camera_global_x_biased > 0
                || camera_global_y_biased > 0
                || camera_global_z_biased > 0),
        camera_local_x: profile_counter_i32_biased(camera_x_biased, DEBUG_MAP_POSITION_BIAS),
        camera_local_y: profile_counter_i32_biased(camera_y_biased, DEBUG_MAP_POSITION_BIAS),
        camera_local_z: profile_counter_i32_biased(camera_z_biased, DEBUG_MAP_POSITION_BIAS),
        camera_global_x: profile_counter_i32_biased(
            camera_global_x_biased,
            DEBUG_MAP_POSITION_BIAS,
        ),
        camera_global_y: profile_counter_i32_biased(
            camera_global_y_biased,
            DEBUG_MAP_POSITION_BIAS,
        ),
        camera_global_z: profile_counter_i32_biased(
            camera_global_z_biased,
            DEBUG_MAP_POSITION_BIAS,
        ),
    })
}

#[cfg(feature = "editor")]
fn profile_counter_u32(value: f32) -> u32 {
    if value.is_finite() && value > 0.0 {
        value.round().min(u32::MAX as f32) as u32
    } else {
        0
    }
}

#[cfg(feature = "editor")]
fn profile_counter_i32_biased(value: u32, bias: i32) -> i32 {
    (value as i64 - bias as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_frame_cadence_tracks_emulated_vblank_period() {
        let ntsc_period = 571_236u64;
        let pal_period = 680_438u64;

        let ntsc_hz = 1.0 / guest_frame_dt(Some(ntsc_period));
        let pal_hz = 1.0 / guest_frame_dt(Some(pal_period));
        assert!((ntsc_hz - 59.29).abs() < 0.01);
        assert!((pal_hz - 49.77).abs() < 0.01);

        let ntsc_samples_per_second =
            (ntsc_period as f32 / emulator_core::spu::SAMPLE_CYCLES as f32) * ntsc_hz;
        assert!((ntsc_samples_per_second - 44_100.0).abs() < 0.1);

        let forced_60hz_samples =
            (ntsc_period as f32 / emulator_core::spu::SAMPLE_CYCLES as f32) * 60.0;
        assert!(forced_60hz_samples > 44_600.0);
    }

    #[test]
    fn keyboard_mapping_uses_default_settings() {
        let bindings = PortBindings::default();

        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyF), &bindings),
            Some(button::CROSS)
        );
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyG), &bindings),
            Some(button::CIRCLE)
        );
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyH), &bindings),
            Some(button::SQUARE)
        );
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyX), &bindings),
            Some(button::TRIANGLE)
        );
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::Backspace), &bindings),
            Some(button::SELECT)
        );
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyR), &bindings),
            Some(button::R3)
        );
        assert!(key_is_analog_button(
            &PhysicalKey::Code(KeyCode::F9),
            &bindings
        ));
    }

    #[test]
    fn keyboard_stick_mapping_uses_default_settings() {
        let bindings = PortBindings::default();
        let mut left = KeyboardStickState::default();
        let mut right = KeyboardStickState::default();

        assert!(left.update_key(
            &PhysicalKey::Code(KeyCode::ArrowUp),
            ElementState::Pressed,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, 1.0));
        assert!(left.update_key(
            &PhysicalKey::Code(KeyCode::ArrowDown),
            ElementState::Pressed,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, 0.0));
        assert!(left.update_key(
            &PhysicalKey::Code(KeyCode::ArrowUp),
            ElementState::Released,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, -1.0));

        assert!(right.update_key(
            &PhysicalKey::Code(KeyCode::KeyJ),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (-1.0, 0.0));
        assert!(right.update_key(
            &PhysicalKey::Code(KeyCode::KeyL),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (0.0, 0.0));
        assert!(!right.update_key(
            &PhysicalKey::Code(KeyCode::KeyX),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
    }

    #[test]
    fn socd_resolution_keeps_most_recent_direction() {
        // Holding LEFT, then pressing RIGHT without letting go: the guest
        // sees only RIGHT (the recent press wins)...
        assert_eq!(
            socd_resolve(button::LEFT | button::RIGHT, button::RIGHT, 0),
            button::RIGHT
        );
        // ...and releasing RIGHT re-exposes the still-held LEFT (the
        // resolver never fires on a single held direction).
        assert_eq!(socd_resolve(button::LEFT, button::RIGHT, 0), button::LEFT);
        // No recorded recency resolves the clash to neutral.
        assert_eq!(socd_resolve(button::UP | button::DOWN, 0, 0), 0);
        // Pairs resolve independently; other buttons pass through.
        assert_eq!(
            socd_resolve(
                button::UP | button::DOWN | button::LEFT | button::RIGHT | button::CROSS,
                button::LEFT,
                button::DOWN,
            ),
            button::LEFT | button::DOWN | button::CROSS
        );
    }

    /// One keyboard press applied through the full event path.
    fn press(host: &mut HostKeyboardInput, code: KeyCode, bindings: &PortBindings) {
        host.apply_key_event(&PhysicalKey::Code(code), ElementState::Pressed, bindings);
    }

    /// One keyboard release applied through the full event path.
    fn release(host: &mut HostKeyboardInput, code: KeyCode, bindings: &PortBindings) {
        host.apply_key_event(&PhysicalKey::Code(code), ElementState::Released, bindings);
    }

    #[test]
    fn capture_swallowed_release_cannot_wedge_pad_state() {
        // Regression for: hold a bound key while the game runs, arm a
        // rebind capture, release the key while capture owns the
        // keyboard, then cancel with Escape (or close the panel). The
        // release is consumed by the capture branch, so its normal
        // processing never clears the bit -- the capture branch must
        // clear the whole cache on every event it consumes instead.
        let bindings = PortBindings::default();
        let mut host = HostKeyboardInput::default();

        // Hold Cross (default F) -- its bit and stick state are live.
        press(&mut host, KeyCode::KeyF, &bindings);
        assert_eq!(host.resolved_mask(0), button::CROSS);

        // A capture armed now consumes every event, starting with the
        // release of F. The shell clears the cache for each consumed
        // event; the swallowed release therefore cannot strand its bit,
        // regardless of how capture ends (bind / Escape / panel close).
        host.clear();
        assert_eq!(host, HostKeyboardInput::default());
        assert_eq!(host.resolved_mask(0), 0);

        // A stray release arriving after capture ends (key already up)
        // stays a no-op.
        release(&mut host, KeyCode::KeyF, &bindings);
        assert_eq!(host.resolved_mask(0), 0);

        // Focus loss and a successful rebind route through the same
        // clear() -- the invariant is identical: everything neutral,
        // including SOCD recency.
        press(&mut host, KeyCode::ArrowLeft, &bindings);
        assert_ne!(host.socd_last_horiz, 0);
        host.clear();
        assert_eq!(host.socd_last_horiz, 0);
        assert_eq!(host.resolved_mask(0), 0);
    }

    #[test]
    fn reset_controls_invalidates_bits_from_the_old_bindings() {
        // Regression for: bind Circle to J, hold J, click Reset
        // Controls, release J. After the reset J no longer matches
        // Circle, so its release can't clear the stale bit -- the
        // reset path must drop the cache outright, while gamepad
        // input (merged separately) is preserved.
        let custom = PortBindings {
            circle: InputBinding::Character('j'),
            ..PortBindings::default()
        };
        let mut host = HostKeyboardInput::default();

        press(&mut host, KeyCode::KeyJ, &custom);
        assert_eq!(host.resolved_mask(0), button::CIRCLE);

        // Reset Controls: MenuOutcome::ClearHostKeyboardInput makes the
        // shell clear the cache and rebuild the frame's merged sample
        // from the gamepad alone.
        let defaults = PortBindings::default();
        host.clear();
        let gamepad_mask = button::L1;
        assert_eq!(host.resolved_mask(gamepad_mask), button::L1);

        // The release under the *new* bindings no longer maps to
        // Circle (J is a right-stick direction in the defaults) and
        // must not resurrect anything.
        release(&mut host, KeyCode::KeyJ, &defaults);
        assert_eq!(host.resolved_mask(gamepad_mask), button::L1);
    }

    #[test]
    fn socd_recency_lets_the_newer_device_win() {
        let bindings = PortBindings::default();

        // Keyboard Left held, then gamepad Right pressed: the gamepad
        // edge updates recency, so Right wins the merged clash.
        let mut host = HostKeyboardInput::default();
        press(&mut host, KeyCode::ArrowLeft, &bindings);
        host.note_gamepad_edges(button::RIGHT);
        assert_eq!(
            host.resolved_mask(button::RIGHT) & (button::LEFT | button::RIGHT),
            button::RIGHT
        );

        // Gamepad Left held (edge noted earlier), then keyboard Right
        // pressed: the keyboard press updates recency, Right wins.
        let mut host = HostKeyboardInput::default();
        host.note_gamepad_edges(button::LEFT);
        press(&mut host, KeyCode::ArrowRight, &bindings);
        assert_eq!(
            host.resolved_mask(button::LEFT) & (button::LEFT | button::RIGHT),
            button::RIGHT
        );

        // Releasing the winner re-exposes the still-held opposite: the
        // resolver only intervenes while both bits are actually down.
        release(&mut host, KeyCode::ArrowRight, &bindings);
        assert_eq!(
            host.resolved_mask(button::LEFT) & (button::LEFT | button::RIGHT),
            button::LEFT
        );

        // Opposite edges in the same gamepad poll carry no ordering
        // information: recency resets and the pair resolves neutral.
        let mut host = HostKeyboardInput::default();
        press(&mut host, KeyCode::ArrowLeft, &bindings);
        host.note_gamepad_edges(button::LEFT | button::RIGHT);
        assert_eq!(
            host.resolved_mask(button::LEFT | button::RIGHT) & (button::LEFT | button::RIGHT),
            0
        );

        // The vertical pair tracks independently through the same path.
        let mut host = HostKeyboardInput::default();
        press(&mut host, KeyCode::ArrowUp, &bindings);
        host.note_gamepad_edges(button::DOWN);
        assert_eq!(
            host.resolved_mask(button::DOWN) & (button::UP | button::DOWN),
            button::DOWN
        );
    }

    #[test]
    fn captured_keys_round_trip_through_binding_match() {
        // Every key the controls panel can capture must be recognised
        // by the matcher afterwards, or a rebind would save a binding
        // that never fires. Spot-check each table family: letters,
        // digits, named keys, function keys, numpad.
        for code in [
            KeyCode::KeyA,
            KeyCode::KeyZ,
            KeyCode::Digit0,
            KeyCode::Digit9,
            KeyCode::ArrowLeft,
            KeyCode::Enter,
            KeyCode::Space,
            KeyCode::ShiftLeft,
            KeyCode::F1,
            KeyCode::F11,
            KeyCode::Numpad0,
            KeyCode::Numpad9,
            KeyCode::NumpadAdd,
            KeyCode::NumpadEnter,
            KeyCode::Semicolon,
            KeyCode::Comma,
            KeyCode::ControlLeft,
            KeyCode::AltRight,
        ] {
            let binding =
                keycode_to_binding(code).unwrap_or_else(|| panic!("{code:?} should be bindable"));
            assert!(
                binding_matches_key(&binding, &PhysicalKey::Code(code)),
                "{code:?} captured as {binding:?} but does not match back"
            );
        }
        // Host commands are deliberately unavailable as pad bindings.
        for code in [
            KeyCode::Escape,
            KeyCode::F5,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F12,
        ] {
            assert_eq!(keycode_to_binding(code), None);
        }
        // The default Circle key must remain ordinary pad input rather than
        // also toggling the renderer display source.
        assert_eq!(
            keycode_to_binding(KeyCode::KeyG),
            Some(InputBinding::Character('g'))
        );
    }

    #[test]
    fn keyboard_mapping_honors_rebound_button() {
        let bindings = PortBindings {
            cross: InputBinding::Character('j'),
            ..PortBindings::default()
        };

        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyJ), &bindings),
            Some(button::CROSS)
        );
        // Cross's default key (F) no longer maps to anything.
        assert_eq!(
            key_to_pad_button(&PhysicalKey::Code(KeyCode::KeyF), &bindings),
            None
        );
    }

    #[test]
    fn keyboard_stick_mapping_honors_rebound_direction() {
        let bindings = PortBindings {
            right_stick: StickBindings {
                left: InputBinding::Character('u'),
                ..StickBindings::default()
            },
            ..PortBindings::default()
        };
        let mut right = KeyboardStickState::default();

        assert!(right.update_key(
            &PhysicalKey::Code(KeyCode::KeyU),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (-1.0, 0.0));
        assert!(!right.update_key(
            &PhysicalKey::Code(KeyCode::KeyJ),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
    }

    #[test]
    fn keyboard_stick_axes_override_matching_gamepad_axes() {
        assert_eq!(merge_sticks((0.25, -0.5), (0.0, 1.0)), (0.25, 1.0));
        assert_eq!(merge_sticks((0.25, -0.5), (-1.0, 0.0)), (-1.0, -0.5));
    }

    #[test]
    fn hw_resync_tracks_cpu_vram_generation_changes() {
        let mut seen = 0;
        let mut last_24bpp = false;

        assert!(!hw_target_needs_resync(
            &mut seen,
            &mut last_24bpp,
            0,
            false
        ));
        assert!(hw_target_needs_resync(&mut seen, &mut last_24bpp, 1, false));
        assert!(!hw_target_needs_resync(
            &mut seen,
            &mut last_24bpp,
            1,
            false
        ));
    }

    #[test]
    fn hw_resync_when_leaving_24bpp_scanout() {
        let mut seen = 7;
        let mut last_24bpp = false;

        assert!(!hw_target_needs_resync(&mut seen, &mut last_24bpp, 7, true));
        assert!(hw_target_needs_resync(&mut seen, &mut last_24bpp, 7, false));
    }

    #[test]
    fn display_uv_honors_256_wide_modes() {
        let area = emulator_core::DisplayArea {
            x: 0,
            y: 0,
            width: 256,
            height: 240,
            bpp24: false,
        };

        let hw = hw_display_uv(area);
        assert_eq!(hw.min, egui::pos2(0.0, 0.0));
        assert_eq!(hw.max.x, 256.0 / psx_gpu_render::VRAM_WIDTH as f32);
        assert_eq!(hw.max.y, 240.0 / psx_gpu_render::VRAM_HEIGHT as f32);

        let cpu = cpu_display_uv(area);
        assert_eq!(cpu.min, egui::pos2(0.0, 0.0));
        assert_eq!(cpu.max.x, 256.0 / gfx::MAX_DISPLAY_WIDTH as f32);
        assert_eq!(cpu.max.y, 240.0 / gfx::MAX_DISPLAY_HEIGHT as f32);
    }
}

#[cfg(test)]
mod freelook_chord_tests {
    use super::{FreelookChord, FreelookChordAction};

    /// A quick press switches mode. Nothing fires while the buttons are still
    /// down, so the mode does not flip until the user lets go.
    #[test]
    fn tap_toggles_on_release() {
        let mut c = FreelookChord::default();
        assert_eq!(c.update(true, false), None, "press alone does nothing");
        assert_eq!(c.update(true, false), None, "still held, still nothing");
        assert_eq!(c.update(false, false), Some(FreelookChordAction::Toggle));
    }

    /// Repeated short presses only switch freecam mode. A second tap must not
    /// be mistaken for the deliberate hold gesture that resets the camera.
    #[test]
    fn a_second_tap_toggles_without_resetting() {
        let mut c = FreelookChord::default();
        for _ in 0..2 {
            assert_eq!(c.update(true, false), None);
            assert_eq!(c.update(false, false), Some(FreelookChordAction::Toggle));
        }
    }

    /// The reset lands while the buttons are down, fires once however long the
    /// hold lasts, and -- the bug this guards -- the release that ends it does
    /// NOT also toggle the mode.
    #[test]
    fn hold_resets_once_and_release_does_not_toggle() {
        let mut c = FreelookChord::default();
        assert_eq!(c.update(true, false), None);
        assert_eq!(c.update(true, true), Some(FreelookChordAction::Reset));
        assert_eq!(c.update(true, true), None, "reset must not repeat");
        assert_eq!(c.update(true, true), None);
        assert_eq!(
            c.update(false, true),
            None,
            "releasing after a hold must not be read as a tap"
        );
    }

    /// State does not leak between presses: a hold followed by a tap gives a
    /// reset and then a toggle, not two resets.
    #[test]
    fn a_tap_after_a_hold_still_toggles() {
        let mut c = FreelookChord::default();
        c.update(true, false);
        assert_eq!(c.update(true, true), Some(FreelookChordAction::Reset));
        c.update(false, true);
        assert_eq!(c.update(true, false), None);
        assert_eq!(c.update(false, false), Some(FreelookChordAction::Toggle));
    }

    /// An idle pad produces nothing at all.
    #[test]
    fn idle_chord_is_silent() {
        let mut c = FreelookChord::default();
        for _ in 0..4 {
            assert_eq!(c.update(false, false), None);
        }
    }
}
