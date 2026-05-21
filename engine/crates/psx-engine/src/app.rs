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
//!     ctx.time     ← elapsed display-time snapshot
//!     scene.update(&mut ctx)
//!     if a visual frame is due:
//!       ctx.fb.clear(config.clear_color)
//!       scene.render(&mut ctx)
//!       display-clock wait + draw_sync + fb.swap
//!       ctx.frame += 1
//!     else:
//!       wait for the next display VBlank, leaving the last framebuffer visible
//! ```
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
use psx_pad::{poll_port1, PadState};

use crate::scene::{Ctx, Scene};
use crate::telemetry;
use crate::time::{EngineClock, EngineTime};

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
    pub const fn video_hz(self) -> u16 {
        match self.video_mode {
            VideoMode::Ntsc => 60,
            VideoMode::Pal => 50,
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

#[derive(Copy, Clone, Debug)]
struct VisualPacer {
    interval: u16,
    next_visual_tick: u32,
}

impl VisualPacer {
    const fn new(interval: u16) -> Self {
        Self {
            interval,
            next_visual_tick: 0,
        }
    }

    fn mark_due_intervals(&mut self, simulation_tick: u32) -> u16 {
        if simulation_tick < self.next_visual_tick {
            return 0;
        }
        let interval = self.interval.max(1) as u32;
        let due = simulation_tick
            .wrapping_sub(self.next_visual_tick)
            .checked_div(interval)
            .unwrap_or(0)
            .saturating_add(1);
        self.next_visual_tick = self
            .next_visual_tick
            .saturating_add(due.saturating_mul(interval));
        due.min(u16::MAX as u32) as u16
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
        gpu::init(config.video_mode, config.resolution);
        let mut clock = EngineClock::new(config.video_hz());
        let fb = FrameBuffer::new(config.screen_w, config.screen_h);
        gpu::set_draw_area(
            0,
            0,
            config.screen_w.saturating_sub(1),
            config.screen_h.saturating_sub(1),
        );
        gpu::set_draw_offset(0, 0);

        let mut ctx = Ctx {
            frame: 0,
            simulation_tick: 0,
            missed_visual_intervals: 0,
            time: EngineTime::start(config.video_hz()),
            pad: PadState::NONE,
            pad_prev: PadState::NONE,
            fb,
        };

        scene.init(&mut ctx);
        clock.reset();

        let visual_interval = config.visual_pacing.interval_vblanks();
        if visual_interval <= 1 {
            Self::run_every_vblank(config, scene, clock, ctx);
        }
        Self::run_paced_visuals(config, scene, clock, ctx, visual_interval);
    }

    fn run_every_vblank<S: Scene>(
        config: Config,
        scene: &mut S,
        mut clock: EngineClock,
        mut ctx: Ctx,
    ) -> ! {
        loop {
            telemetry::frame_begin(ctx.frame);
            ctx.time = clock.begin_frame(ctx.frame, ctx.simulation_tick);
            ctx.missed_visual_intervals = 0;
            emit_sim_tick_counters(1);
            ctx.pad_prev = ctx.pad;
            ctx.pad = poll_port1();

            telemetry::stage_begin(telemetry::stage::UPDATE);
            scene.update(&mut ctx);
            telemetry::stage_end(telemetry::stage::UPDATE);

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

            telemetry::stage_begin(telemetry::stage::PRESENT);
            clock.wait_next_vblank();
            gpu::draw_sync();
            ctx.fb.swap();
            telemetry::stage_end(telemetry::stage::PRESENT);
            emit_visual_frame_counters(ctx.time.delta_vblanks().saturating_sub(1));
            ctx.frame = ctx.frame.wrapping_add(1);
            ctx.simulation_tick = ctx.frame;
        }
    }

    fn run_paced_visuals<S: Scene>(
        config: Config,
        scene: &mut S,
        mut clock: EngineClock,
        mut ctx: Ctx,
        visual_interval: u16,
    ) -> ! {
        let mut pacer = VisualPacer::new(visual_interval);

        loop {
            let elapsed_vblanks = clock.elapsed_vblanks();
            if elapsed_vblanks < pacer.next_visual_tick {
                clock.wait_next_vblank();
                continue;
            }

            let due_visual_intervals = pacer.mark_due_intervals(elapsed_vblanks);
            if due_visual_intervals == 0 {
                continue;
            }
            ctx.missed_visual_intervals = due_visual_intervals.saturating_sub(1);
            telemetry::frame_begin(elapsed_vblanks);
            ctx.simulation_tick = elapsed_vblanks;
            ctx.time = clock.begin_frame(ctx.frame, elapsed_vblanks);
            emit_sim_tick_counters(visual_interval);
            telemetry::counter(
                telemetry::counter::VISUAL_SKIPPED_VBLANKS,
                visual_interval.saturating_sub(1) as u32,
            );
            ctx.pad_prev = ctx.pad;
            ctx.pad = poll_port1();

            telemetry::stage_begin(telemetry::stage::UPDATE);
            scene.update(&mut ctx);
            telemetry::stage_end(telemetry::stage::UPDATE);

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

            telemetry::stage_begin(telemetry::stage::PRESENT);
            clock.wait_next_vblank();
            gpu::draw_sync();
            ctx.fb.swap();
            telemetry::stage_end(telemetry::stage::PRESENT);
            emit_visual_frame_counters(ctx.missed_visual_intervals);
            ctx.frame = ctx.frame.wrapping_add(1);
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

    #[test]
    fn visual_pacer_marks_due_and_collapses_missed_intervals() {
        let mut pacer = VisualPacer::new(3);
        assert_eq!(pacer.mark_due_intervals(0), 1);
        assert_eq!(pacer.mark_due_intervals(1), 0);
        assert_eq!(pacer.mark_due_intervals(2), 0);
        assert_eq!(pacer.mark_due_intervals(3), 1);
        assert_eq!(pacer.mark_due_intervals(10), 2);
    }
}
