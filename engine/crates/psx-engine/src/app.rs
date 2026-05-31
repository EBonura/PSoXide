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
use psx_level::{GameFlow, LevelOptionDef, LevelUiNodeRecord, LevelUiScene};
use psx_pad::{poll_port1, PadState};

use crate::game_app::{GameApp, GAMEPLAY_ONLY};
use crate::scene::{Ctx, Scene};
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
        // Auto-wrap the bare gameplay scene in the unified runtime
        // spine over GAMEPLAY_ONLY. That flow has a single Gameplay
        // state entered at boot, so `GameApp::init` forwards straight
        // to `scene.init` and `update`/`render` forward straight to
        // the scene every tick -- the old one-init-then-loop shape,
        // plus one already-taken `match` branch. No UI scenes, no
        // nodes, no options: the front-end arms are dead code on this path.
        Self::run_with_flow(config, &GAMEPLAY_ONLY, &[], &[], &[], scene)
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
        options: &'static [LevelOptionDef],
        scene: &mut S,
    ) -> ! {
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
        boot_trace("psx-engine: framebuffer ok");

        let mut ctx = Ctx {
            sim_tick: SimTick::ZERO,
            visual_frame: VisualFrame::ZERO,
            video_hz: config.video_hz(),
            pad: PadState::NONE,
            pad_prev: PadState::NONE,
            fb,
        };

        // The wrapper is the Scene the scheduled loop drives: its
        // init/update/render dispatch to the borrowed gameplay scene
        // (or the UI renderer) per flow state.
        let mut app = GameApp::new(flow, scenes, nodes, options, scene);

        boot_trace("psx-engine: scene init");
        app.init(&mut ctx);
        boot_trace("psx-engine: scene init ok");

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

        loop {
            let elapsed_sim_ticks = clock.elapsed_sim_ticks();
            match scheduler.next_action(elapsed_sim_ticks) {
                SchedulerAction::WaitForVBlank => {
                    if !traced_wait {
                        boot_trace("psx-engine: wait vblank");
                    }
                    clock.wait_next_vblank();
                    if !traced_wait {
                        boot_trace("psx-engine: vblank ok");
                        traced_wait = true;
                    }
                }
                SchedulerAction::RunFixedUpdate { tick } => {
                    if !traced_update {
                        boot_trace("psx-engine: update begin");
                    }
                    telemetry::task_begin(telemetry::task::FIXED_UPDATE);
                    telemetry::frame_begin(tick.as_u32());
                    ctx.sim_tick = tick;
                    emit_sim_tick_counters(visual_interval);
                    ctx.pad_prev = ctx.pad;
                    if !traced_update {
                        boot_trace("psx-engine: pad poll begin");
                    }
                    ctx.pad = poll_port1();
                    if !traced_update {
                        boot_trace("psx-engine: pad poll ok");
                    }

                    telemetry::stage_begin(telemetry::stage::UPDATE);
                    scene.update(&mut ctx);
                    telemetry::stage_end(telemetry::stage::UPDATE);
                    if !traced_update {
                        boot_trace("psx-engine: update ok");
                        traced_update = true;
                    }

                    let outcome = scheduler.complete_fixed_update();
                    if outcome.visual_intervals_due == 0 {
                        telemetry::counter(telemetry::counter::VISUAL_SKIPPED_VBLANKS, 1);
                    }
                    telemetry::task_end(telemetry::task::FIXED_UPDATE);
                }
                SchedulerAction::RunVisualFrame {
                    missed_visual_intervals,
                    fixed_update_clamped: _,
                } => {
                    if !traced_render {
                        boot_trace("psx-engine: render begin");
                    }
                    telemetry::task_begin(telemetry::task::VISUAL_RENDER);
                    telemetry::stage_begin(telemetry::stage::FRAME_CLEAR);
                    ctx.fb.clear(
                        config.clear_color.0,
                        config.clear_color.1,
                        config.clear_color.2,
                    );
                    telemetry::stage_end(telemetry::stage::FRAME_CLEAR);

                    telemetry::stage_begin(telemetry::stage::RENDER);
                    scene.render(&mut ctx);
                    telemetry::stage_end(telemetry::stage::RENDER);
                    if !traced_render {
                        boot_trace("psx-engine: render ok");
                        traced_render = true;
                    }
                    telemetry::task_end(telemetry::task::VISUAL_RENDER);

                    if !traced_present {
                        boot_trace("psx-engine: present begin");
                    }
                    telemetry::stage_begin(telemetry::stage::PRESENT);
                    clock.wait_next_vblank();
                    gpu::draw_sync();
                    ctx.fb.swap();
                    telemetry::stage_end(telemetry::stage::PRESENT);
                    if !traced_present {
                        boot_trace("psx-engine: present ok");
                        traced_present = true;
                    }

                    scheduler.complete_visual_frame();
                    emit_visual_frame_counters(missed_visual_intervals);
                    ctx.visual_frame = ctx.visual_frame.advance();
                }
            }
        }
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
