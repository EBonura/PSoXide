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
mod cli;
mod disasm;
mod editor_assets;
mod editor_preview;
mod editor_textures;
mod embedded_playtest;
mod gfx;
mod icons;
mod input;
mod playtest_input;
mod theme;
mod ui;

use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::app::AppState;
use crate::cli::Cli;
use crate::gfx::Graphics;
use crate::playtest_input::Port1PadSample;
use crate::ui::profiler::FrameProfileSample;
use crate::ui::{menu::MenuInput, MenuOutcome};

use emulator_core::{button, pad::PadMode, spu::SAMPLE_CYCLES, telemetry::counter};
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
/// Frontend run cadence target. The toolbar, "advance one frame"
/// control, and sample pump all assume an NTSC-ish 60 Hz shell.
const TARGET_FRAME_DT: f32 = 1.0 / 60.0;
/// Don't try to catch up an arbitrarily long stall in one redraw;
/// cap the burst so a debugger stop or window drag doesn't spend
/// seconds chewing through delayed emu frames.
const MAX_CATCHUP_FRAMES: u32 = 4;

fn elapsed_ms(start: Instant) -> f32 {
    start.elapsed().as_secs_f32() * 1000.0
}

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

    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Poll);

    let mut app = Shell::new(config_dir, fullscreen, gpu_compute);
    event_loop.run_app(&mut app).expect("event loop");
}

struct Shell {
    graphics: Option<Graphics>,
    state: AppState,
    pending_input: MenuInput,
    last_frame: Instant,
    /// Live port-1 pad mask. Key press/release events toggle bits
    /// here; the shell flushes it into `bus.set_port1_buttons` each
    /// frame before running CPU steps so the guest always sees the
    /// latest state.
    pad1_mask: u16,
    /// Keyboard-emulated left analog stick state.
    keyboard_left_stick: KeyboardStickState,
    /// Keyboard-emulated right analog stick state.
    keyboard_right_stick: KeyboardStickState,
    /// Whether to open the window in borderless-fullscreen mode.
    /// Decision is made at startup via CLI flag and then captured
    /// here; changing it at runtime would need a window recreation.
    fullscreen: bool,
    /// Host audio output. `None` when no device is available
    /// (headless CI, devices that can't open a stereo stream).
    /// Emulation keeps running regardless -- silence is fine.
    audio: Option<audio::AudioOut>,
    /// Host input router -- tracks every connected gamepad, emits
    /// merged PSX pad-1 masks, detects the Select+Start chord
    /// that opens the Menu, and logs connect / disconnect events
    /// for diagnosing missing controllers. Always constructible;
    /// a failed gilrs init just produces empty frames so the
    /// keyboard path keeps working.
    input: input::InputRouter,
    /// Wall-clock debt waiting to be converted into emulated
    /// "frames". Without this, the current `ControlFlow::Poll`
    /// shell runs the guest as fast as redraws can arrive, which
    /// massively overfills the audio queue and produces crackle
    /// from dropped samples.
    emu_frame_accum: f32,
    /// Residual emulated CPU cycles that haven't yet been converted
    /// into SPU sample ticks. Redux clocks the SPU at 44.1 kHz from
    /// the PSX master clock (768 cycles/sample); tying audio to host
    /// redraws instead produces under/over-runs on anything that
    /// isn't an exact 60 Hz render loop.
    audio_cycle_accum: u64,
    /// Phase C -- when `Some`, the experimental compute-shader
    /// rasterizer is shadowing the CPU rasterizer: each frame the
    /// CPU's `cmd_log` is drained and replayed onto the GPU compute
    /// path, and the display reads from the GPU's VRAM.
    compute_backend: Option<psx_gpu_compute::ComputeBackend>,
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
}

impl Default for Shell {
    fn default() -> Self {
        Self::new(None, false, false)
    }
}

impl Shell {
    fn new(config_dir: Option<std::path::PathBuf>, fullscreen: bool, gpu_compute: bool) -> Self {
        let audio = audio::AudioOut::open();
        if let Some(a) = audio.as_ref() {
            eprintln!("[audio] opened host stream @ {} Hz", a.host_sample_rate());
        } else {
            eprintln!("[audio] no host output device available — running silent");
        }
        let input = input::InputRouter::new();
        if input.is_connected() {
            eprintln!(
                "[input] already-connected pads: {}",
                input.connected_names()
            );
        } else {
            eprintln!("[input] no pads connected at startup — watching for hot-plug");
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
            Some(psx_gpu_compute::ComputeBackend::new_headless())
        } else {
            None
        };
        Self {
            graphics: None,
            state: AppState::with_config_dir(config_dir),
            pending_input: MenuInput::default(),
            last_frame: Instant::now(),
            pad1_mask: 0,
            keyboard_left_stick: KeyboardStickState::default(),
            keyboard_right_stick: KeyboardStickState::default(),
            fullscreen,
            audio,
            input,
            emu_frame_accum: 0.0,
            audio_cycle_accum: 0,
            compute_backend,
            display_gpu_compute: gpu_compute,
            hw_seen_gpu_resync_generation: 0,
            hw_last_display_bpp24: false,
        }
    }
}

fn press_port1_analog_button(state: &mut AppState) {
    let Some(bus) = state.bus.as_mut() else {
        state.status_message_set("No running pad to toggle");
        return;
    };

    let changed = bus.press_port1_analog_button();
    let mode = match bus.port1_pad_mode() {
        Some(PadMode::Digital) => "Digital",
        Some(PadMode::Analog) => "Analog",
        Some(PadMode::Config) => "Config",
        None => "No pad",
    };
    if changed {
        state.status_message_set(format!("DualShock Analog button: {mode}"));
    } else {
        state.status_message_set(format!("DualShock Analog unchanged: {mode}"));
    }
}

/// Map a winit logical key to a PSX digital-pad bitmask using the
/// persisted port-1 bindings. Returns `None` for keys that aren't
/// bound.
fn key_to_pad_button(key: &Key, bindings: &PortBindings) -> Option<u16> {
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
    ]
    .into_iter()
    .find_map(|(mask, binding)| binding_matches_key(binding, key).then_some(mask))
}

/// `true` when the key should act as the DualShock Analog button.
fn key_is_analog_button(key: &Key, bindings: &PortBindings) -> bool {
    binding_matches_key(&bindings.analog, key)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct KeyboardStickState {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl KeyboardStickState {
    fn update_key(&mut self, key: &Key, state: ElementState, bindings: &StickBindings) -> bool {
        let pressed = state == ElementState::Pressed;
        let mut matched = false;
        if binding_matches_key(&bindings.up, key) {
            self.up = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.down, key) {
            self.down = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.left, key) {
            self.left = pressed;
            matched = true;
        }
        if binding_matches_key(&bindings.right, key) {
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

fn binding_matches_key(binding: &InputBinding, key: &Key) -> bool {
    match (binding, key) {
        (InputBinding::Unbound, _) => false,
        (InputBinding::Character(expected), Key::Character(actual)) => actual
            .chars()
            .next()
            .is_some_and(|c| c.eq_ignore_ascii_case(expected)),
        (InputBinding::Named(expected), Key::Named(actual)) => {
            named_key_label(actual).is_some_and(|name| expected.eq_ignore_ascii_case(name))
        }
        _ => false,
    }
}

fn named_key_label(key: &NamedKey) -> Option<&'static str> {
    match key {
        NamedKey::ArrowUp => Some("ArrowUp"),
        NamedKey::ArrowDown => Some("ArrowDown"),
        NamedKey::ArrowLeft => Some("ArrowLeft"),
        NamedKey::ArrowRight => Some("ArrowRight"),
        NamedKey::Enter => Some("Enter"),
        NamedKey::Backspace => Some("Backspace"),
        NamedKey::Shift => Some("Shift"),
        NamedKey::Space => Some("Space"),
        NamedKey::Tab => Some("Tab"),
        NamedKey::Escape => Some("Escape"),
        NamedKey::F9 => Some("F9"),
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
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        self.graphics = Some(pollster::block_on(Graphics::new(window)));
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
                self.state.stop_embedded_playtest();
                self.state.stop_examples_build();
                // Flush any dirty memory card so save progress
                // survives a window-close. A hard crash still
                // loses whatever hasn't been flushed -- the run
                // loop could call this periodically; for now
                // graceful exit is enough.
                if let Err(e) = self.state.flush_memcard_port1() {
                    eprintln!("[frontend] memcard flush on exit: {e}");
                }
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
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state,
                        repeat,
                        ..
                    },
                ..
            } => {
                // Pad state tracks both press AND release continuously
                // so held buttons keep polling as "pressed". Auto-repeat
                // events are ignored -- the key is already down, and the
                // BIOS polls every frame anyway.
                let route_keyboard_to_game = !self.state.workspace.is_editor()
                    || self.state.embedded_playtest_input_captured();
                if !repeat && route_keyboard_to_game {
                    let bindings = &self.state.settings.input.port1;
                    if let Some(mask) = key_to_pad_button(&logical_key, bindings) {
                        match state {
                            ElementState::Pressed => self.pad1_mask |= mask,
                            ElementState::Released => self.pad1_mask &= !mask,
                        }
                    }
                    self.keyboard_left_stick
                        .update_key(&logical_key, state, &bindings.left_stick);
                    self.keyboard_right_stick.update_key(
                        &logical_key,
                        state,
                        &bindings.right_stick,
                    );
                    let press_analog = state == ElementState::Pressed
                        && key_is_analog_button(&logical_key, bindings);
                    if press_analog {
                        press_port1_analog_button(&mut self.state);
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
                gfx.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
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
                if pad_frame.toggle_menu {
                    // Select+Start is the gamepad equivalent of
                    // Escape -- route it into the same `toggle_open`
                    // path so there's exactly one place that decides
                    // what "PS button" does based on current state.
                    input.toggle_open = true;
                }
                if pad_frame.analog_button {
                    press_port1_analog_button(&mut self.state);
                }
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
                    if self.state.workspace.is_editor()
                        && self.state.embedded_playtest_running()
                        && self.state.embedded_playtest_input_captured()
                    {
                        self.state.release_embedded_playtest_input();
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
                        if self.state.bus.is_some()
                            && (self.state.current_game.is_some()
                                || self.state.embedded_playtest_input_captured())
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
                    if ui::apply_menu_action(&mut self.state, action) == MenuOutcome::Quit {
                        self.state.stop_embedded_playtest();
                        self.state.stop_examples_build();
                        if let Err(e) = self.state.flush_memcard_port1() {
                            eprintln!("[frontend] memcard flush on quit: {e}");
                        }
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
                self.state.poll_embedded_playtest_build();
                self.state.poll_examples_build();
                profile.input_ms = elapsed_ms(input_start);

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
                let hw_frame_start_vram = self
                    .state
                    .bus
                    .as_ref()
                    .map(|bus| bus.gpu.vram.words().to_vec());

                // Run loop: retire one video frame's worth of PSX cycles
                // if we're in run mode. Any execution error auto-pauses
                // and surfaces via the register panel. History captures
                // only the tail via `push_history`'s ring-buffer semantics.
                if self.state.running {
                    self.emu_frame_accum = (self.emu_frame_accum + dt).min(0.25);
                    let frames_to_run =
                        ((self.emu_frame_accum / TARGET_FRAME_DT) as u32).min(MAX_CATCHUP_FRAMES);
                    // Merge the current keyboard-derived pad mask with
                    // gamepad input before stepping, so the game/homebrew
                    // sees fresh input this frame. `pad_frame.pad1_mask`
                    // already has the Select+Start chord stripped for
                    // the frame the chord fires -- prevents in-game
                    // handlers from seeing the "open menu" combo.
                    let right_stick =
                        merge_sticks(pad_frame.right_stick, self.keyboard_right_stick.vector());
                    let left_stick =
                        merge_sticks(pad_frame.left_stick, self.keyboard_left_stick.vector());
                    let live_pad_sample = Port1PadSample::from_host(
                        self.pad1_mask | pad_frame.pad1_mask,
                        right_stick,
                        left_stick,
                    );
                    for _ in 0..frames_to_run {
                        let pad_sample = self
                            .state
                            .editor_playtest_input_sample_for_frame(live_pad_sample);
                        if let Some(bus) = self.state.bus.as_mut() {
                            pad_sample.apply_to_bus(bus);
                        }
                        let cycles_before =
                            self.state.bus.as_ref().map(|bus| bus.cycles()).unwrap_or(0);
                        let draw_log_start = self
                            .state
                            .bus
                            .as_ref()
                            .map(|bus| bus.gpu.cmd_log.len())
                            .unwrap_or(0);
                        let emu_start = Instant::now();
                        let step_report = app::step_one_frame(&mut self.state);
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
                        let guest_events = if let Some(bus) = self.state.bus.as_mut() {
                            let cycles_after = bus.cycles();
                            self.audio_cycle_accum = self
                                .audio_cycle_accum
                                .saturating_add(cycles_after.saturating_sub(cycles_before));
                            let sample_count = (self.audio_cycle_accum / SAMPLE_CYCLES) as usize;
                            self.audio_cycle_accum %= SAMPLE_CYCLES;
                            if sample_count > 0 {
                                bus.run_spu_samples(sample_count);
                            }
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
                            bus.telemetry.drain_events()
                        } else {
                            Vec::new()
                        };
                        let guest_profile = self.state.profiler.consume_guest_events(&guest_events);
                        profile.add_guest_profile(guest_profile);
                        profile.audio_ms += elapsed_ms(audio_start);
                    }
                    self.emu_frame_accum -= (frames_to_run as f32) * TARGET_FRAME_DT;
                } else {
                    self.emu_frame_accum = 0.0;
                    // Throw away any fractional carry when emulation is
                    // paused or no game is running so a later launch or
                    // resume doesn't inherit cycles from an older run.
                    self.audio_cycle_accum = 0;
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
                if let (Some(backend), Some(bus)) =
                    (self.compute_backend.as_mut(), state.bus.as_mut())
                {
                    // Sync VRAM so any uploads / FMV writes / VRAM-to-
                    // VRAM copies are reflected on the compute side
                    // before we replay this frame's draw commands.
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

                let vram_upload_start = Instant::now();
                gfx.prepare_vram(state.bus.as_ref().map(|b| &b.gpu.vram));
                profile.vram_upload_ms = elapsed_ms(vram_upload_start);

                let display_upload_start = Instant::now();
                gfx.prepare_display(state.bus.as_ref().map(|b| &b.gpu));
                profile.display_upload_ms = elapsed_ms(display_upload_start);

                // Match the HW renderer's internal scale to the
                // current Native↔Window mode + framebuffer pixel budget.
                // Cheap when stable; reallocates the VRAM-shaped
                // target on change. Reallocation clears the target,
                // so we immediately resync it from CPU VRAM before
                // replaying this frame's command log.
                let scale_mode = match state.scale_mode {
                    app::ScaleMode::Native => psx_gpu_render::ScaleMode::Native,
                    app::ScaleMode::Window => psx_gpu_render::ScaleMode::Window,
                };
                let display_size = state
                    .bus
                    .as_ref()
                    .map(|b| {
                        let area = b.gpu.display_area();
                        ((area.width as u32).max(320), (area.height as u32).max(240))
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

                // Drive the hardware renderer once per frame. The
                // VRAM-shaped target persists across frames the way
                // PSX VRAM does; the framebuffer panel UV-samples
                // the active display sub-rect.
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

                // Editor 3D preview: drive the editor-owned HwRenderer
                // while editing. During embedded Play, the viewport
                // paints the live emulator framebuffer instead.
                if !state.embedded_playtest_running() {
                    let editor_camera = state.editor.viewport_3d_camera();
                    let editor_preview_fog = state.editor.preview_fog_enabled();
                    let editor_preview_backface_wireframe =
                        state.editor.preview_backface_wireframe_enabled();
                    let editor_preview_bounds = state.editor.preview_bounds_enabled();
                    let editor_show_grid = state.editor.show_grid_enabled();
                    let editor_show_portals = state.editor.show_portals_enabled();
                    let editor_show_lights = state.editor.show_lights_enabled();
                    let editor_hidden_scene_nodes = state.editor.hidden_scene_nodes();
                    let editor_selected = state.editor.selected_node_id();
                    let editor_root = state.editor.project_root();
                    let editor_hover = state.editor.hovered_primitive();
                    let editor_selection = state.editor.selected_primitive();
                    let editor_selected_primitives = state.editor.selected_primitives();
                    let editor_validation_issues = state.editor.validation_issue_primitives();
                    let editor_selected_bounds = state.editor.selected_bounds_3d();
                    let editor_selected_sector_faces = state.editor.selected_sector_faces();
                    let editor_paint_preview = state.editor.paint_target_preview();
                    let editor_active_room = state.editor.active_room_id();
                    let editor_entity_bounds =
                        state.editor.collect_entity_bounds(editor_active_room);
                    let editor_hovered_entity = state.editor.hovered_entity_node();
                    gfx.render_editor_preview(
                        state.editor.project(),
                        editor_root,
                        editor_camera,
                        editor_preview_fog,
                        editor_preview_backface_wireframe,
                        editor_preview_bounds,
                        editor_show_grid,
                        editor_show_portals,
                        editor_show_lights,
                        editor_hidden_scene_nodes,
                        editor_selected,
                        editor_hover,
                        editor_selection,
                        &editor_selected_primitives,
                        &editor_validation_issues,
                        editor_selected_bounds,
                        &editor_selected_sector_faces,
                        editor_paint_preview,
                        &editor_entity_bounds,
                        editor_hovered_entity,
                    );
                }

                let vram_tex = gfx.vram_texture_id();
                let (display_tex, display_uv) = frontend_display(state.bus.as_ref(), gfx);
                let editor_viewport = if state.embedded_playtest_running() {
                    psxed_ui::EditorViewport3dPresentation::play(
                        display_tex,
                        display_uv,
                        state.editor_playtest_input_tape_status(),
                        editor_play_metrics(state),
                    )
                } else {
                    psxed_ui::EditorViewport3dPresentation::edit(
                        gfx.editor_hw_texture_id(),
                        gfx.editor_overlay_lines().to_vec(),
                    )
                };
                profile.egui = gfx.render(|ctx| {
                    app::build_ui(
                        ctx,
                        state,
                        vram_tex,
                        display_tex,
                        editor_viewport.clone(),
                        display_uv,
                        dt,
                    )
                });
                if let Some(request) = state.editor.take_playtest_request() {
                    state.handle_editor_playtest_request(request);
                }
                profile.total_ms = elapsed_ms(profile_start);
                if let Some(line) = state.profiler.record(profile) {
                    eprintln!("{line}");
                }
            }
            _ => {
                if !consumed {
                    gfx.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(gfx) = self.graphics.as_ref() {
            gfx.window.request_redraw();
        }
    }
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
    let width = area.width.max(320) as f32;
    let height = area.height.max(240) as f32;
    egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2(
            width / gfx::MAX_DISPLAY_WIDTH as f32,
            height / gfx::MAX_DISPLAY_HEIGHT as f32,
        ),
    )
}

fn hw_display_uv(area: emulator_core::DisplayArea) -> egui::Rect {
    let width = area.width.max(320) as f32;
    let height = area.height.max(240) as f32;
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

fn editor_play_metrics(state: &app::AppState) -> Option<psxed_ui::EditorPlaytestMetrics> {
    let latest = state.profiler.latest()?;
    let sample = state.profiler.average().unwrap_or(latest);
    let visual_hz = sample.guest_visual_frame_hz();
    let display_hz = visual_hz.unwrap_or_else(|| sample.psx_draw_hz());
    let visual_interval_vblanks = latest
        .guest_visual_interval_vblanks()
        .or_else(|| sample.guest_visual_interval_vblanks())
        .unwrap_or(0.0);
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
    const RENDER_MAP_REQUIRED_COUNTERS: &[u16] = &[
        counter::ROOM_PLAYER_LOCAL_X_BIASED,
        counter::ROOM_PLAYER_LOCAL_Z_BIASED,
        counter::ROOM_CAMERA_GLOBAL_X_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Y_BIASED,
        counter::ROOM_CAMERA_GLOBAL_Z_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED,
        counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED,
    ];
    let chunk_sample = state
        .profiler
        .latest_with_all_guest_counters(RENDER_MAP_REQUIRED_COUNTERS)
        .or_else(|| state.profiler.latest_with_guest_counters(RENDER_MAP_COUNTERS))
        .or_else(|| {
            state
                .profiler
                .latest_with_guest_counters(CHUNK_MAP_COUNTERS)
        })
        .unwrap_or(sample);
    let recent_counter = |id: u16| profile_counter_u32(sample.guest.counter_max_value(id as usize));
    let chunk_mask = |lo: u16, hi: u16| {
        let lo = chunk_sample.guest.counter_latest_value(lo as usize) as u64;
        let hi = chunk_sample.guest.counter_latest_value(hi as usize) as u64;
        lo | (hi << 32)
    };
    let player_x_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_PLAYER_LOCAL_X_BIASED as usize);
    let player_z_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_PLAYER_LOCAL_Z_BIASED as usize);
    let camera_x_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_X_BIASED as usize);
    let camera_y_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_Y_BIASED as usize);
    let camera_z_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_LOCAL_Z_BIASED as usize);
    let camera_global_x_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_X_BIASED as usize);
    let camera_global_y_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_Y_BIASED as usize);
    let camera_global_z_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_GLOBAL_Z_BIASED as usize);
    let camera_view_sin_yaw_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_SIN_YAW_Q12_BIASED as usize);
    let camera_view_cos_yaw_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_COS_YAW_Q12_BIASED as usize);
    let camera_view_sin_pitch_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_SIN_PITCH_Q12_BIASED as usize);
    let camera_view_cos_pitch_biased = chunk_sample
        .guest
        .counter_latest_value(counter::ROOM_CAMERA_VIEW_COS_PITCH_Q12_BIASED as usize);
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
        visual_deadline_misses,
        visual_lateness_vblanks,
        total_ms: sample.total_ms,
        frame_ms,
        emu_ms: sample.emu_ms,
        hw_ms: sample.hw_render_ms,
        ui_ms: sample.egui.total_ms,
        step_budget_percent: sample.psx_budget_percent(),
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
        player_map_valid: player_x_biased > 0 || player_z_biased > 0,
        player_room_index: chunk_sample
            .guest
            .counter_latest_value(counter::ROOM_PLAYER_ROOM_INDEX as usize),
        portal_current_room_index: chunk_sample
            .guest
            .counter_latest_value(counter::PORTAL_VIS_CURRENT_ROOM as usize),
        player_local_x: profile_counter_i32_biased(player_x_biased, DEBUG_MAP_POSITION_BIAS),
        player_local_z: profile_counter_i32_biased(player_z_biased, DEBUG_MAP_POSITION_BIAS),
        player_view_yaw_q12: chunk_sample
            .guest
            .counter_latest_value(counter::ROOM_PLAYER_VIEW_YAW_Q12 as usize)
            .min(u16::MAX as u32) as u16,
        camera_view_basis_valid: camera_view_sin_yaw_biased > 0
            || camera_view_cos_yaw_biased > 0
            || camera_view_sin_pitch_biased > 0
            || camera_view_cos_pitch_biased > 0,
        camera_view_sin_yaw_q12: profile_counter_i32_biased(camera_view_sin_yaw_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_cos_yaw_q12: profile_counter_i32_biased(camera_view_cos_yaw_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_sin_pitch_q12: profile_counter_i32_biased(camera_view_sin_pitch_biased, 4096)
            .clamp(-4096, 4096),
        camera_view_cos_pitch_q12: profile_counter_i32_biased(camera_view_cos_pitch_biased, 4096)
            .clamp(-4096, 4096),
        camera_map_valid: camera_x_biased > 0 || camera_y_biased > 0 || camera_z_biased > 0,
        camera_global_valid: camera_global_x_biased > 0
            || camera_global_y_biased > 0
            || camera_global_z_biased > 0,
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

fn profile_counter_u32(value: f32) -> u32 {
    if value.is_finite() && value > 0.0 {
        value.round().min(u32::MAX as f32) as u32
    } else {
        0
    }
}

fn profile_counter_i32_biased(value: u32, bias: i32) -> i32 {
    (value as i64 - bias as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_mapping_uses_default_settings() {
        let bindings = PortBindings::default();

        assert_eq!(
            key_to_pad_button(&Key::Character("x".into()), &bindings),
            Some(button::CROSS)
        );
        assert_eq!(
            key_to_pad_button(&Key::Character("c".into()), &bindings),
            Some(button::CIRCLE)
        );
        assert_eq!(
            key_to_pad_button(&Key::Character("z".into()), &bindings),
            Some(button::SQUARE)
        );
        assert_eq!(
            key_to_pad_button(&Key::Named(NamedKey::Backspace), &bindings),
            Some(button::SELECT)
        );
        assert_eq!(
            key_to_pad_button(&Key::Character("r".into()), &bindings),
            Some(button::R3)
        );
        assert!(key_is_analog_button(&Key::Named(NamedKey::F9), &bindings));
    }

    #[test]
    fn keyboard_stick_mapping_uses_default_settings() {
        let bindings = PortBindings::default();
        let mut left = KeyboardStickState::default();
        let mut right = KeyboardStickState::default();

        assert!(left.update_key(
            &Key::Named(NamedKey::ArrowUp),
            ElementState::Pressed,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, 1.0));
        assert!(left.update_key(
            &Key::Named(NamedKey::ArrowDown),
            ElementState::Pressed,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, 0.0));
        assert!(left.update_key(
            &Key::Named(NamedKey::ArrowUp),
            ElementState::Released,
            &bindings.left_stick,
        ));
        assert_eq!(left.vector(), (0.0, -1.0));

        assert!(right.update_key(
            &Key::Character("j".into()),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (-1.0, 0.0));
        assert!(right.update_key(
            &Key::Character("l".into()),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (0.0, 0.0));
        assert!(!right.update_key(
            &Key::Character("x".into()),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
    }

    #[test]
    fn keyboard_mapping_honors_rebound_button() {
        let bindings = PortBindings {
            cross: InputBinding::Character('j'),
            ..PortBindings::default()
        };

        assert_eq!(
            key_to_pad_button(&Key::Character("j".into()), &bindings),
            Some(button::CROSS)
        );
        assert_eq!(
            key_to_pad_button(&Key::Character("x".into()), &bindings),
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
            &Key::Character("u".into()),
            ElementState::Pressed,
            &bindings.right_stick,
        ));
        assert_eq!(right.vector(), (-1.0, 0.0));
        assert!(!right.update_key(
            &Key::Character("j".into()),
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
}
