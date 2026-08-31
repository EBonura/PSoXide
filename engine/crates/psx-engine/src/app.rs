//! App runner -- the fixed-shape main loop that every game inherits
//! instead of rewriting.
//!
//! # Shape of the loop
//!
//! ```text
//!   gpu::init + draw_area + draw_offset
//!   FrameBuffer::new
//!   scene.init(&mut ctx)
//!   loop:
//!     ctx.pad_prev ← ctx.pad           (one-frame input history)
//!     ctx.pad      ← poll_port1()
//!     ask FrameScheduler for the next task:
//!       fixed update  -> poll pad + scene.update(&mut ctx)
//!       visual render -> clear + scene.render(&mut ctx) + present
//!       wait          -> wait for the next display VBlank
//! ```
//!
//! The scheduler keeps fixed update and visual render as separate tasks.
//! Rendering can drop visual intervals when the machine is busy. Fixed update
//! is the critical clock and catches up before optional visuals unless a
//! project explicitly sets an emergency burst cap.
//!
//! This mirrors every `sdk/examples/game-*/src/main.rs` file's
//! inner loop by eye -- the engine just factors the shared cadence
//! out. If a scene wants a different cadence (custom clear, no
//! vsync, manual OT submission, …), the door's still open: the
//! scene's `update` / `render` methods can do whatever they want
//! with the ctx before the engine ticks over to the next frame.
//!
//! # No `!` impl on the scene
//!
//! [`App::run`] returns `!` because the main loop never terminates
//! on PSX (no OS to return to). The scene's methods return `()` --
//! they just tick and go. A scene that wants "exit" behaviour can
//! idle its own state machine in place.

use psx_gpu::framebuf::FrameBuffer;
use psx_gpu::{self as gpu, Resolution, VideoMode};
use psx_level::{
    GameFlow, LevelOptionDef, LevelUiNodeRecord, LevelUiPaintRecord, LevelUiScene,
    LevelUiSfxCueRecord, LevelUiSfxSampleRecord,
};
use psx_pad::{enable_analog_port1, poll_port1};

use crate::game_app::GameApp;
use crate::scene::{Ctx, RenderSubmission, Scene};
use crate::scheduler::{FrameScheduler, SchedulerAction, SchedulerConfig};
use crate::telemetry;
use crate::time::EngineClock;
use crate::{SimTick, VideoHz, VisualFrame};

#[cfg(all(target_arch = "mips", feature = "boot-trace"))]
#[inline(always)]
fn boot_trace(message: &str) {
    psx_rt::tty::println(message);
}

#[cfg(not(all(target_arch = "mips", feature = "boot-trace")))]
#[inline(always)]
fn boot_trace(_message: &str) {}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
pub(crate) fn boot_visual_checkpoint(fb: &mut FrameBuffer, color: (u8, u8, u8), message: &str) {
    boot_visual_checkpoint_hold(fb, color, message, 1);
}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
pub(crate) fn boot_visual_checkpoint_hold(
    fb: &mut FrameBuffer,
    color: (u8, u8, u8),
    message: &str,
    frames: u8,
) {
    // A checkpoint can fire while a scene's async ordering-table DMA is
    // still walking; drain the channel before issuing immediate draws so
    // the diagnostic itself cannot corrupt the GP0 stream.
    gpu::submit_linked_list_wait();
    for _ in 0..frames.max(1) {
        fb.clear(color.0, color.1, color.2);
        draw_boot_text(fb, message);
        gpu::draw_sync();
        // Deliberately the deprecated fixed 242-HBlank delay, not
        // rt::wait_vblank(): a checkpoint can fire before platform::init,
        // and wait_vblank's lazy install would rewrite the exception
        // vector and clobber I_MASK mid-boot. A fixed hold is all this
        // diagnostic needs, and it is what was verified on silicon.
        #[allow(deprecated)]
        gpu::vsync();
        fb.swap();
    }
}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
fn draw_boot_text(fb: &FrameBuffer, message: &str) {
    let scale = 2i16;
    let advance = 6 * scale;
    let line_height = 9 * scale;
    let start_x = 12i16;
    let mut x = start_x;
    let mut y = 16i16;
    draw_boot_text_line(start_x, y, "CORTEX_IGNITION_V1", scale);
    y += line_height;
    for ch in message.chars() {
        if ch == '\n' || x + advance >= fb.width as i16 - 8 {
            x = start_x;
            y += line_height;
            if ch == '\n' {
                continue;
            }
        }
        draw_boot_glyph(x, y, ch, scale);
        x += advance;
    }
}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
fn draw_boot_text_line(mut x: i16, y: i16, text: &str, scale: i16) {
    let advance = 6 * scale;
    for ch in text.chars() {
        draw_boot_glyph(x, y, ch, scale);
        x += advance;
    }
}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
fn draw_boot_glyph(x: i16, y: i16, ch: char, scale: i16) {
    let rows = boot_glyph(ch);
    for (row, bits) in rows.iter().enumerate() {
        for col in 0..5 {
            if bits & (1 << (4 - col)) == 0 {
                continue;
            }
            let px = x + col as i16 * scale;
            let py = y + row as i16 * scale;
            gpu::draw_quad_flat(
                [
                    (px, py),
                    (px + scale - 1, py),
                    (px, py + scale - 1),
                    (px + scale - 1, py + scale - 1),
                ],
                255,
                255,
                255,
            );
        }
    }
}

#[cfg(all(target_arch = "mips", feature = "hardware-boot-visual"))]
fn boot_glyph(ch: char) -> [u8; 7] {
    let ch = if ch >= 'a' && ch <= 'z' {
        (ch as u8 - b'a' + b'A') as char
    } else {
        ch
    };
    match ch {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b10010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b10101, 0b01010,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        ':' => [
            0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        '_' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b11111,
        ],
        '/' => [
            0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000,
        ],
        '.' => [
            0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100,
        ],
        '>' => [
            0b10000, 0b01000, 0b00100, 0b00010, 0b00100, 0b01000, 0b10000,
        ],
        ' ' => [0; 7],
        _ => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b00100, 0b00000, 0b00100,
        ],
    }
}

#[cfg(not(all(target_arch = "mips", feature = "hardware-boot-visual")))]
#[inline(always)]
pub(crate) fn boot_visual_checkpoint(_fb: &mut FrameBuffer, _color: (u8, u8, u8), _message: &str) {}

#[cfg(not(all(target_arch = "mips", feature = "hardware-boot-visual")))]
#[inline(always)]
pub(crate) fn boot_visual_checkpoint_hold(
    _fb: &mut FrameBuffer,
    _color: (u8, u8, u8),
    _message: &str,
    _frames: u8,
) {
}

/// Configuration passed to [`App::run`]. Sensible defaults via
/// [`Config::default`] so simple games can just write
/// `App::run(Config::default(), &mut game)`.
#[derive(Copy, Clone, Debug)]
pub struct Config {
    /// Visible framebuffer width in pixels.
    pub screen_w: u16,
    /// Visible framebuffer height in pixels.
    pub screen_h: u16,
    /// Video mode (NTSC / PAL). PAL games running in NTSC (or the
    /// reverse) show vertical compression / overscan -- match the
    /// region you're testing on.
    pub video_mode: VideoMode,
    /// GP1 display resolution. Must match `screen_w × screen_h`.
    pub resolution: Resolution,
    /// RGB triple used to clear `ctx.fb` before each
    /// [`Scene::render`] call. Scenes that want a more elaborate
    /// background (textured backdrop, gouraud gradient, etc.) can
    /// set this to black and draw their own full-screen quad.
    pub clear_color: (u8, u8, u8),
    /// Visual render cadence. The default renders every display
    /// VBlank; paced modes keep update/control ticking every VBlank
    /// while rendering only on selected VBlanks.
    pub visual_pacing: VisualPacing,
    /// Manual frame/task scheduler tuning.
    pub scheduler: SchedulerConfig,
    /// Cooked UI scene drawn while the engine streams the next state's
    /// world (the project's scene named "Loading"), or
    /// `psx_level::UI_SCENE_NONE` for the engine's built-in minimal
    /// loading screen. With an authored scene the loading screen also
    /// holds a short minimum duration so fast loads do not flash.
    pub loading_ui_scene: u16,
}

/// Engine-level visual render cadence.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VisualPacing {
    /// Preserve the legacy one-update, one-render, one-swap loop.
    EveryVBlank,
    /// Run update/control every VBlank and render once every `N`
    /// VBlanks. Values less than `2` are normalized to
    /// [`EveryVBlank`](Self::EveryVBlank).
    EveryNVBlanks(u16),
}

impl Config {
    /// Display cadence in whole frames per second.
    #[inline]
    pub const fn video_hz(self) -> VideoHz {
        match self.video_mode {
            VideoMode::Ntsc => VideoHz::NTSC,
            VideoMode::Pal => VideoHz::PAL,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            screen_w: 320,
            screen_h: 240,
            video_mode: VideoMode::Ntsc,
            resolution: Resolution::R320X240,
            clear_color: (0, 0, 0),
            visual_pacing: VisualPacing::EveryVBlank,
            scheduler: SchedulerConfig::new(),
            loading_ui_scene: psx_level::UI_SCENE_NONE,
        }
    }
}

impl VisualPacing {
    #[inline]
    const fn interval_vblanks(self) -> u16 {
        match self {
            Self::EveryVBlank => 1,
            Self::EveryNVBlanks(n) if n > 1 => n,
            Self::EveryNVBlanks(_) => 1,
        }
    }
}

#[inline(always)]
fn emit_sim_tick_counters(visual_interval: u16) {
    telemetry::counter(telemetry::counter::SIM_TICKS, 1);
    telemetry::counter(
        telemetry::counter::VISUAL_INTERVAL_VBLANKS,
        visual_interval.max(1) as u32,
    );
}

#[inline(always)]
fn emit_visual_frame_counters(lateness_vblanks: u16) {
    telemetry::counter(telemetry::counter::VISUAL_FRAMES, 1);
    if lateness_vblanks > 0 {
        telemetry::counter(telemetry::counter::VISUAL_DEADLINE_MISSES, 1);
    }
    telemetry::counter(
        telemetry::counter::VISUAL_MAX_LATENESS_VBLANKS,
        lateness_vblanks as u32,
    );
}

/// Engine entry point. Namespaced as a type (rather than a free
/// function) so future engine-level state (config getters, exit
/// handlers, debug introspection) has a natural home.
pub struct App;

impl App {
    /// Run `scene` under `config`. Never returns.
    ///
    /// Calls [`Scene::init`] once, then loops forever:
    /// poll-pad → update → clear → render → display-clock wait →
    /// draw-sync → swap.
    ///
    /// Internally this wraps `scene` in a [`GameApp`] over the implicit
    /// gameplay-only flow ([`GAMEPLAY_ONLY`]) and drives that wrapper
    /// through the same scheduled loop, so behaviour is identical to
    /// running the bare scene: one init at boot, then the unchanged
    /// per-tick cadence. Projects that want front-end UI states call
    /// [`run_with_flow`](Self::run_with_flow) with their own flow.
    ///
    /// Typical call site in `main`:
    ///
    /// ```ignore
    /// #[no_mangle]
    /// fn main() -> ! {
    ///     let mut game = MyGame::new();
    ///     App::run(Config::default(), &mut game);
    /// }
    /// ```
    pub fn run<S: Scene>(config: Config, scene: &mut S) -> ! {
        // Drive the gameplay scene directly. It is deliberately NOT
        // wrapped in `GameApp` over GAMEPLAY_ONLY any more.
        //
        // Wrapping cost ~115 KB of code in the linked binary. `GameApp`
        // holds the flow and the UI tables as *runtime* fields
        // (`flow: &'static GameFlow`), so the `match` on `flow.states`
        // is not provably monomorphic: LLVM cannot see that
        // GAMEPLAY_ONLY never yields `FlowState::UiScene`, and keeps
        // that arm together with everything it reaches --
        // `render_ui_scene`, transitions, focus ring, shape nodes,
        // text paints. The arms were dead in the sense that they never
        // execute, but they were still linked.
        //
        // `run_scheduled` is already generic over `S: Scene`, so the
        // gameplay scene can be driven without an adapter and the
        // front-end is never monomorphised at all.
        //
        // Behaviour note: `GameApp::init` parks on the built-in loading
        // screen and defers `gameplay.init` until it has rendered once,
        // which lets streaming scenes finish boot residency first. This
        // path calls `init` before the loop instead. Every in-tree
        // `App::run` caller is a self-contained example or game;
        // `editor-playtest`, the one streaming consumer, uses
        // `run_with_flow` and is unaffected.
        let (mut clock, mut ctx) = Self::boot(config);

        boot_trace("psx-engine: scene init");
        scene.load_shared_assets(&mut ctx);
        scene.init(&mut ctx);
        boot_trace("psx-engine: scene init ok");
        clock.reset_origin();

        let visual_interval = config.visual_pacing.interval_vblanks();
        boot_trace("psx-engine: loop");
        Self::run_scheduled(config, scene, clock, ctx, visual_interval)
    }

    /// Run `scene` as the gameplay state of a cooked [`GameFlow`].
    /// Never returns.
    ///
    /// Same boot + scheduled loop as [`run`](Self::run); the scene is
    /// driven through a [`GameApp`] so the flow can also surface cooked
    /// UI-scene states (title / pause / game-over) under the identical
    /// pacing and telemetry. `scenes` and `nodes` supply the
    /// addressable UI scenes and the shared node pool they slice into;
    /// `options` supplies the cooked project options sliders and
    /// `SetOption` actions bind to. Pass empty slices for a gameplay-only
    /// flow.
    pub fn run_with_flow<S: Scene>(
        config: Config,
        flow: &'static GameFlow,
        scenes: &'static [LevelUiScene],
        nodes: &'static [LevelUiNodeRecord],
        paints: &'static [LevelUiPaintRecord],
        options: &'static [LevelOptionDef],
        ui_sfx_samples: &'static [LevelUiSfxSampleRecord],
        ui_sfx_cues: &'static [LevelUiSfxCueRecord],
        scene: &mut S,
    ) -> ! {
        let (clock, ctx) = Self::boot(config);
        Self::run_with_flow_booted(
            config,
            flow,
            scenes,
            nodes,
            paints,
            options,
            ui_sfx_samples,
            ui_sfx_cues,
            scene,
            clock,
            ctx,
        )
    }

    /// Shared boot: GPU, clock, framebuffer, pad and `Ctx`.
    ///
    /// Split out so the gameplay-only path can reuse it without going
    /// through `GameApp` -- see [`run`](Self::run).
    fn boot(config: Config) -> (EngineClock, Ctx) {
        boot_trace("psx-engine: run");
        gpu::init(config.video_mode, config.resolution);
        boot_trace("psx-engine: gpu ok");
        let clock = EngineClock::new();
        boot_trace("psx-engine: clock ok");
        let fb = FrameBuffer::new(config.screen_w, config.screen_h);
        gpu::set_draw_area(
            0,
            0,
            config.screen_w.saturating_sub(1),
            config.screen_h.saturating_sub(1),
        );
        gpu::set_draw_offset(0, 0);
        let mut fb = fb;
        boot_visual_checkpoint(&mut fb, (160, 0, 0), "01 FRAMEBUFFER READY");
        boot_trace("psx-engine: framebuffer ok");

        // Ask a DualShock-compatible controller to enter and lock analog
        // mode. Original digital controllers safely ignore the request and
        // continue through the same button-input path.
        let _ = enable_analog_port1();

        // Seed pad + pad_prev from a real poll so a button already held at
        // boot does NOT register as `just_pressed` on the first frame. This
        // matters when booting into a UI scene with captured input (editor
        // embedded Play): the CROSS/START that may be down as Play starts must
        // not instantly activate a menu button and skip into gameplay. With
        // both seeded to the current state, an input must actually transition
        // (release then press) to count as a press.
        let initial_pad = poll_port1();
        let mut ctx = Ctx::new(
            SimTick::ZERO,
            VisualFrame::ZERO,
            config.video_hz(),
            initial_pad,
            initial_pad,
            fb,
        );
        boot_visual_checkpoint(&mut ctx.fb, (200, 96, 0), "02 CTX READY");

        boot_trace("psx-engine: ctx ok");
        (clock, ctx)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_with_flow_booted<S: Scene>(
        config: Config,
        flow: &'static GameFlow,
        scenes: &'static [LevelUiScene],
        nodes: &'static [LevelUiNodeRecord],
        paints: &'static [LevelUiPaintRecord],
        options: &'static [LevelOptionDef],
        ui_sfx_samples: &'static [LevelUiSfxSampleRecord],
        ui_sfx_cues: &'static [LevelUiSfxCueRecord],
        scene: &mut S,
        mut clock: EngineClock,
        mut ctx: Ctx,
    ) -> ! {
        // The wrapper is the Scene the scheduled loop drives: its
        // init/update/render dispatch to the borrowed gameplay scene
        // (or the UI renderer) per flow state.
        let mut app = GameApp::new(
            flow,
            scenes,
            nodes,
            paints,
            options,
            ui_sfx_samples,
            ui_sfx_cues,
            config.loading_ui_scene,
            scene,
        );

        boot_trace("psx-engine: scene init");
        boot_visual_checkpoint(&mut ctx.fb, (180, 180, 0), "03 APP INIT BEGIN");
        app.init(&mut ctx);
        boot_visual_checkpoint(&mut ctx.fb, (0, 120, 0), "13 APP INIT OK");
        boot_trace("psx-engine: scene init ok");
        clock.reset_origin();

        let visual_interval = config.visual_pacing.interval_vblanks();
        boot_trace("psx-engine: loop");
        Self::run_scheduled(config, &mut app, clock, ctx, visual_interval);
    }

    fn run_scheduled<S: Scene>(
        config: Config,
        scene: &mut S,
        mut clock: EngineClock,
        mut ctx: Ctx,
        visual_interval: u16,
    ) -> ! {
        let mut scheduler = FrameScheduler::new(config.scheduler, visual_interval);
        let mut traced_wait = false;
        let mut traced_update = false;
        let mut traced_render = false;
        let mut traced_present = false;
        let mut pad_was_connected = ctx.pad.is_connected();
        // A built visual frame whose flip has not happened yet. The scene's
        // ordering-table DMA may still be walking; fixed updates run in the
        // gap so the GPU draw overlaps CPU work. Holds the frame's missed
        // visual intervals for the deadline counters emitted at the flip.
        let mut pending_present: Option<u16> = None;
        // Set while a queued scene's display flip sits in the VBlank
        // handler's GP1 slot, carrying that frame's missed visual intervals
        // for the deadline counters. Until the edge arrives the newly selected
        // draw buffer is still the one on screen, so the frame clear and the
        // ordering-table kick that follow the flip are held back -- and the
        // fixed updates that were already due get to run in the gap that used
        // to be a spin. See `finish_deferred_flip`.
        let mut deferred_flip: Option<u16> = None;

        loop {
            // Resolve a queued flip as soon as the handler has taken the word,
            // or when the scheduler has nothing but a render or a wait left to
            // offer -- at which point blocking on the edge is all there is to
            // do. A due fixed update always goes first: covering the edge with
            // simulation the runner owed anyway is the entire point.
            if let Some(previous_misses) = deferred_flip {
                if !clock.display_flip_pending()
                    || !matches!(
                        scheduler.next_action(clock.elapsed_sim_ticks()),
                        SchedulerAction::RunFixedUpdate { .. }
                    )
                {
                    telemetry::stage_begin(telemetry::stage::PRESENT);
                    let landed = clock.wait_display_flip();
                    telemetry::stage_end(telemetry::stage::PRESENT);
                    if !landed {
                        telemetry::counter(telemetry::counter::VISUAL_DEADLINE_MISSES, 1);
                    }
                    // Counted here, not where the word was queued: the frame
                    // reaches the screen at the edge, so `ctx.visual_frame`
                    // advances at the same point in the cadence it did when
                    // the swap was synchronous.
                    emit_visual_frame_counters(previous_misses);
                    ctx.visual_frame = ctx.visual_frame.advance();
                    Self::finish_deferred_flip(config, scene, &mut ctx);
                    deferred_flip = None;
                }
            }
            let elapsed_sim_ticks = clock.elapsed_sim_ticks();
            match scheduler.next_action(elapsed_sim_ticks) {
                SchedulerAction::WaitForVBlank => {
                    if pending_present.is_some()
                        && scene.render_submission() == RenderSubmission::Immediate
                    {
                        if !traced_present {
                            boot_trace("psx-engine: present begin");
                        }
                        Self::present_pending(
                            scene,
                            &mut clock,
                            &mut ctx,
                            &mut pending_present,
                            true,
                        );
                        if !traced_present {
                            boot_visual_checkpoint_hold(
                                &mut ctx.fb,
                                (0, 220, 0),
                                "49 PRESENT OK",
                                60,
                            );
                            boot_trace("psx-engine: present ok");
                            traced_present = true;
                        }
                    } else {
                        if !traced_wait {
                            boot_trace("psx-engine: wait vblank");
                        }
                        clock.wait_next_vblank();
                        if !traced_wait {
                            boot_trace("psx-engine: vblank ok");
                            traced_wait = true;
                        }
                    }
                }
                SchedulerAction::RunFixedUpdate { tick } => {
                    if !traced_update {
                        boot_trace("psx-engine: update begin");
                        boot_visual_checkpoint(&mut ctx.fb, (0, 0, 180), "20 UPDATE BEGIN");
                    }
                    telemetry::task_begin(telemetry::task::FIXED_UPDATE);
                    telemetry::frame_begin(tick.as_u32());
                    ctx.sim_tick = tick;
                    if !traced_update {
                        boot_visual_checkpoint(&mut ctx.fb, (220, 0, 0), "21 FRAME BEGIN");
                    }
                    emit_sim_tick_counters(visual_interval);
                    ctx.pad_prev = ctx.pad;
                    if !traced_update {
                        boot_trace("psx-engine: pad poll begin");
                        boot_visual_checkpoint(&mut ctx.fb, (220, 100, 0), "22 PAD POLL BEGIN");
                    }
                    ctx.pad = poll_port1();
                    if ctx.pad.is_connected() && !pad_was_connected {
                        // A newly attached DualShock starts in digital mode.
                        // Negotiate again on the connection edge so hot-plug
                        // behaves the same as a controller present at boot.
                        let _ = enable_analog_port1();
                        ctx.pad = poll_port1();
                    }
                    pad_was_connected = ctx.pad.is_connected();
                    if !traced_update {
                        boot_trace("psx-engine: pad poll ok");
                        boot_visual_checkpoint(&mut ctx.fb, (220, 220, 0), "23 PAD POLL OK");
                    }

                    telemetry::stage_begin(telemetry::stage::UPDATE);
                    scene.update(&mut ctx);
                    telemetry::stage_end(telemetry::stage::UPDATE);
                    if !traced_update {
                        boot_visual_checkpoint(&mut ctx.fb, (0, 140, 180), "29 UPDATE OK");
                        boot_trace("psx-engine: update ok");
                        traced_update = true;
                    }

                    let outcome = scheduler.complete_fixed_update();
                    let visual_checkpoint = ctx.take_visual_checkpoint_request();
                    let timing_realign = ctx.take_timing_realign_request();
                    if visual_checkpoint {
                        // Keep the just-completed fixed tick authoritative but
                        // make the next one not-yet-due. If this tick opened a
                        // visual interval, `next_action` must therefore render
                        // it instead of catching up clock debt accumulated by
                        // the synchronous loading slice.
                        clock.align_origin_to_sim_tick(
                            scheduler.next_fixed_tick().as_u32().wrapping_sub(1),
                        );
                    } else if timing_realign {
                        clock.align_origin_to_sim_tick(scheduler.next_fixed_tick().as_u32());
                    }
                    if outcome.visual_intervals_due == 0 {
                        telemetry::counter(telemetry::counter::VISUAL_SKIPPED_VBLANKS, 1);
                    }
                    telemetry::task_end(telemetry::task::FIXED_UPDATE);
                }
                SchedulerAction::RunVisualFrame {
                    missed_visual_intervals,
                    fixed_update_clamped: _,
                } => {
                    let submission = scene.render_submission();
                    // A queued scene keeps one completed visual in flight.
                    // Drain only the linked-list DMA before reusing its packet
                    // RAM; the GPU may continue rasterising while the CPU
                    // prepares the next list. The final draw_sync is delayed
                    // until after that CPU work, which hides the raster tail.
                    let queued_previous = if submission == RenderSubmission::Queued {
                        pending_present.take().inspect(|_| {
                            telemetry::stage_begin(telemetry::stage::OT_WAIT);
                            gpu::submit_linked_list_wait();
                            telemetry::stage_end(telemetry::stage::OT_WAIT);
                        })
                    } else {
                        // Immediate scenes retain the original overload path:
                        // present before clearing/reusing the back buffer.
                        Self::present_pending(
                            scene,
                            &mut clock,
                            &mut ctx,
                            &mut pending_present,
                            false,
                        );
                        None
                    };
                    if !traced_render {
                        boot_trace("psx-engine: render begin");
                        boot_visual_checkpoint(&mut ctx.fb, (120, 0, 180), "30 RENDER BEGIN");
                    }
                    telemetry::task_begin(telemetry::task::VISUAL_RENDER);
                    if submission == RenderSubmission::Immediate {
                        telemetry::stage_begin(telemetry::stage::FRAME_CLEAR);
                        if !traced_render {
                            boot_visual_checkpoint(&mut ctx.fb, (80, 0, 120), "31 CLEAR BEGIN");
                        }
                        ctx.fb.clear(
                            config.clear_color.0,
                            config.clear_color.1,
                            config.clear_color.2,
                        );
                        if !traced_render {
                            boot_visual_checkpoint(&mut ctx.fb, (80, 40, 160), "32 CLEAR OK");
                        }
                        telemetry::stage_end(telemetry::stage::FRAME_CLEAR);
                    }

                    telemetry::stage_begin(telemetry::stage::RENDER);
                    if !traced_render {
                        boot_visual_checkpoint(
                            &mut ctx.fb,
                            (100, 40, 180),
                            "33 SCENE RENDER BEGIN",
                        );
                    }
                    scene.render(&mut ctx);
                    if !traced_render {
                        boot_visual_checkpoint_hold(
                            &mut ctx.fb,
                            (180, 180, 255),
                            "38 SCENE RENDER RETURNED",
                            60,
                        );
                    }
                    telemetry::stage_end(telemetry::stage::RENDER);

                    if submission == RenderSubmission::Queued {
                        if let Some(previous_misses) = queued_previous {
                            telemetry::stage_begin(telemetry::stage::OT_WAIT);
                            gpu::draw_sync();
                            telemetry::stage_end(telemetry::stage::OT_WAIT);

                            telemetry::stage_begin(telemetry::stage::RENDER);
                            scene.render_overlay(&mut ctx);
                            telemetry::stage_end(telemetry::stage::RENDER);

                            // Hand the display-start word to the VBlank
                            // handler instead of spinning out the rest of the
                            // display period waiting for the edge ourselves.
                            // The handler applies it in the blanking interval
                            // exactly as the synchronous swap did, so this is
                            // no more prone to shearing; what changes is that
                            // the CPU keeps the pending fixed updates it was
                            // about to run and does them inside the wait. The
                            // clear and the ordering-table kick come after the
                            // edge, in `finish_deferred_flip`.
                            telemetry::stage_begin(telemetry::stage::PRESENT);
                            clock.queue_display_flip(ctx.fb.begin_deferred_swap());
                            telemetry::stage_end(telemetry::stage::PRESENT);
                            deferred_flip = Some(previous_misses);
                        } else {
                            // No frame was in flight, so nothing flipped and
                            // the draw side did not move: prepare this one's
                            // buffer straight away.
                            Self::finish_deferred_flip(config, scene, &mut ctx);
                        }
                    }
                    if !traced_render {
                        boot_visual_checkpoint_hold(
                            &mut ctx.fb,
                            (220, 220, 220),
                            "39 RENDER OK",
                            60,
                        );
                        boot_trace("psx-engine: render ok");
                        traced_render = true;
                    }
                    telemetry::task_end(telemetry::task::VISUAL_RENDER);

                    // Immediate scenes defer the flip while their GPU walks.
                    // Queued scenes retain this frame for the next visual
                    // turn, when the following frame's CPU packets hide its
                    // remaining raster time.
                    scheduler.complete_visual_frame();
                    pending_present = Some(missed_visual_intervals);
                }
            }
        }
    }

    /// Take ownership of the buffer a queued flip just freed: point the GPU's
    /// draw area/offset at it, clear it, and kick the ordering table that was
    /// built for it.
    ///
    /// Every step here writes to the buffer the display was showing until the
    /// flip landed, so none of it may run while
    /// [`EngineClock::display_flip_pending`] is true -- that is the whole
    /// reason the work is split out of the present.
    fn finish_deferred_flip<S: Scene>(config: Config, scene: &mut S, ctx: &mut Ctx) {
        ctx.fb.apply_draw_target();
        telemetry::stage_begin(telemetry::stage::FRAME_CLEAR);
        ctx.fb.clear(
            config.clear_color.0,
            config.clear_color.1,
            config.clear_color.2,
        );
        telemetry::stage_end(telemetry::stage::FRAME_CLEAR);
        scene.submit_render(ctx);
    }

    /// Drain the in-flight ordering-table DMA, draw the scene's 2D
    /// overlay layer over the finished frame, and flip the display.
    ///
    /// `wait_edge` selects tear-free presentation: the swap is held to
    /// the next VBlank IRQ edge so GP1 display-start changes land in
    /// the blanking interval. The overload path passes `false` and
    /// flips immediately -- the slot is already blown and holding the
    /// swap would only fall further behind. No-op when no built frame
    /// is awaiting its flip.
    fn present_pending<S: Scene>(
        scene: &mut S,
        clock: &mut EngineClock,
        ctx: &mut Ctx,
        pending: &mut Option<u16>,
        wait_edge: bool,
    ) {
        let Some(missed_visual_intervals) = pending.take() else {
            return;
        };

        // After this drain the GPU has consumed the whole ordering
        // table; immediate GP0 draws can no longer race the walker. On
        // hardware this wait is the real GPU draw time left uncovered
        // by the fixed updates that ran since the kick.
        telemetry::stage_begin(telemetry::stage::OT_WAIT);
        gpu::submit_linked_list_wait();
        gpu::draw_sync();
        telemetry::stage_end(telemetry::stage::OT_WAIT);

        telemetry::stage_begin(telemetry::stage::RENDER);
        scene.render_overlay(ctx);
        telemetry::stage_end(telemetry::stage::RENDER);

        telemetry::stage_begin(telemetry::stage::PRESENT);
        if wait_edge {
            clock.wait_vblank_edge();
        }
        ctx.fb.swap();
        telemetry::stage_end(telemetry::stage::PRESENT);

        emit_visual_frame_counters(missed_visual_intervals);
        ctx.visual_frame = ctx.visual_frame.advance();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_pacing_normalizes_single_vblank_modes() {
        assert_eq!(VisualPacing::EveryVBlank.interval_vblanks(), 1);
        assert_eq!(VisualPacing::EveryNVBlanks(0).interval_vblanks(), 1);
        assert_eq!(VisualPacing::EveryNVBlanks(1).interval_vblanks(), 1);
        assert_eq!(VisualPacing::EveryNVBlanks(3).interval_vblanks(), 3);
    }
}
