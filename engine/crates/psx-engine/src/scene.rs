//! Scene trait + per-frame context.
//!
//! A `Scene` is whatever your game wants to run. The engine calls
//! [`Scene::init`] once at boot, then [`Scene::update`] + [`Scene::render`]
//! in a loop, passing a [`Ctx`] that carries the live-per-frame
//! things the scene needs: the current pad state (with edge-detection
//! helpers), the simulation and visual-frame counters, the display cadence,
//! and a [`FrameBuffer`] ready to draw into.
//!
//! The split into `update` + `render` is cosmetic -- both get the
//! same `Ctx`. Keeping them separate reads better and makes it easy
//! to add determinism/replay hooks later (record `Ctx.pad` during
//! update, replay without re-rendering, etc).

use psx_font::FontAtlas;
use psx_gpu::framebuf::FrameBuffer;
use psx_level::{AssetId, LevelOptionDef};
use psx_pad::{button, PadState};

use crate::frames::{SimTick, VideoHz, VisualFrame};
use crate::ui::UiTextureSlot;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RuntimeRequests {
    timing_realign: bool,
}

/// Per-frame context passed to [`Scene::update`] and
/// [`Scene::render`]. The engine owns and updates this between
/// frames; the scene reads from it and draws through it.
pub struct Ctx {
    /// Fixed simulation/control tick. Advances once per display
    /// VBlank, including VBlanks where the app runner intentionally
    /// keeps the previous framebuffer visible.
    pub sim_tick: SimTick,
    /// Monotonic visible-frame counter. Advances once per rendered
    /// frame, so it can diverge from `sim_tick` when visuals are paced
    /// below the simulation cadence.
    pub visual_frame: VisualFrame,
    /// Display cadence used for time conversion (`60` NTSC, `50` PAL).
    pub video_hz: VideoHz,
    /// Port-1 pad state this frame.
    pub pad: PadState,
    /// Port-1 pad state last frame -- used by [`Ctx::just_pressed`]
    /// to distinguish "newly pressed this frame" from "held across
    /// multiple frames".
    pub pad_prev: PadState,
    /// Frame buffer the scene draws into. The engine clears it
    /// before [`Scene::render`] runs, and swaps it after.
    pub fb: FrameBuffer,
    runtime_requests: RuntimeRequests,
}

impl Ctx {
    pub(crate) fn new(
        sim_tick: SimTick,
        visual_frame: VisualFrame,
        video_hz: VideoHz,
        pad: PadState,
        pad_prev: PadState,
        fb: FrameBuffer,
    ) -> Self {
        Self {
            sim_tick,
            visual_frame,
            video_hz,
            pad,
            pad_prev,
            fb,
            runtime_requests: RuntimeRequests::default(),
        }
    }

    /// Fixed simulation delta as Q12 seconds.
    #[inline]
    pub fn fixed_delta_seconds_q12(&self) -> u32 {
        self.video_hz.fixed_delta_seconds_q12()
    }

    /// Elapsed simulation time as Q12 seconds.
    #[inline]
    pub fn elapsed_seconds_q12(&self) -> u32 {
        self.sim_tick.elapsed_seconds_q12(self.video_hz)
    }

    /// `true` if `button` is pressed right now (held).
    #[inline]
    pub fn is_held(&self, button: u16) -> bool {
        self.pad.buttons.is_held(button)
    }

    /// `true` if `button` transitioned from released to pressed
    /// *this frame*. Exactly the edge-detect pattern every game
    /// reinvents; factored here so menus / fire buttons / etc don't
    /// have to track `pad_prev` themselves.
    #[inline]
    pub fn just_pressed(&self, button: u16) -> bool {
        self.pad.buttons.is_held(button) && !self.pad_prev.buttons.is_held(button)
    }

    /// `true` if `button` transitioned from pressed to released
    /// this frame.
    #[inline]
    pub fn just_released(&self, button: u16) -> bool {
        !self.pad.buttons.is_held(button) && self.pad_prev.buttons.is_held(button)
    }

    /// Convenience: any D-pad direction currently held.
    #[inline]
    pub fn dpad_any_held(&self) -> bool {
        self.is_held(button::UP)
            || self.is_held(button::DOWN)
            || self.is_held(button::LEFT)
            || self.is_held(button::RIGHT)
    }

    /// Ask the app runner to discard display-clock debt accumulated by an
    /// intentional blocking load or scene transition. The request is consumed
    /// after the current fixed update; simulation tick order is unchanged.
    #[inline]
    pub fn request_timing_realign(&mut self) {
        self.runtime_requests.timing_realign = true;
    }

    #[inline]
    pub(crate) fn take_timing_realign_request(&mut self) -> bool {
        let requested = self.runtime_requests.timing_realign;
        self.runtime_requests.timing_realign = false;
        requested
    }
}

/// Implement this trait on your game type and hand an instance to
/// [`App::run`][crate::app::App::run].
///
/// All methods take `&mut self` so the scene can keep its own state
/// inline (no globals needed). All methods take `&mut Ctx` so the
/// scene can read pad state and draw into the framebuffer.
pub trait Scene {
    /// Called once at boot, before any flow state is entered, for assets
    /// the front-end UI shares with gameplay -- chiefly the [`ui_font`] atlas
    /// menus draw their text with. It runs even when the game boots into a
    /// UI scene (a title/menu), where full gameplay [`init`] is deferred
    /// until play actually starts, so the menu would otherwise have no font.
    /// Default is a no-op; a gameplay-only scene can upload everything in
    /// [`init`] as before.
    ///
    /// [`ui_font`]: Scene::ui_font
    /// [`init`]: Scene::init
    #[allow(unused_variables)]
    fn load_shared_assets(&mut self, ctx: &mut Ctx) {}

    /// Called once, before the main loop starts. Use for asset
    /// uploads (font atlas, textures), SPU sample loads, state
    /// initialisation. Default is a no-op.
    #[allow(unused_variables)]
    fn init(&mut self, ctx: &mut Ctx) {}

    /// Advance game state for the current fixed simulation tick.
    /// Called before [`render`]. Read pad input from `ctx` and use
    /// `ctx.sim_tick` for deterministic timers and animation phase.
    ///
    /// [`render`]: Scene::render
    fn update(&mut self, ctx: &mut Ctx);

    /// Draw the current frame. Called after [`update`] and after
    /// `ctx.fb` has been cleared. The engine submits the final
    /// swap after this returns.
    ///
    /// [`update`]: Scene::update
    fn render(&mut self, ctx: &mut Ctx);

    /// Font the flow driver uses to draw front-end UI scene text (menu
    /// labels and buttons). The gameplay scene owns the font atlas it
    /// uploads in [`init`](Scene::init), so it lends it here for the menu
    /// states to share rather than uploading a second copy. The default
    /// returns `None`, which skips UI text -- correct for a gameplay-only
    /// scene with no menus. Override to return your uploaded atlas.
    #[inline]
    fn ui_font(&self) -> Option<&FontAtlas> {
        None
    }

    /// Font selector lookup for cooked UI nodes. Slot `0` is the first cooked
    /// UI font; scenes that upload additional atlases override this to expose
    /// slots `1..`.
    #[inline]
    fn ui_font_at(&self, index: u8) -> Option<&FontAtlas> {
        if index == 0 {
            self.ui_font()
        } else {
            None
        }
    }

    /// Texture resolver for front-end UI image nodes. The flow driver owns only
    /// cooked asset ids; scenes know how their texture assets are packed and
    /// uploaded, so they translate an [`AssetId`] into GPU texture state here.
    /// Default skips image nodes, which is correct for projects with text/rect
    /// menus only.
    #[inline]
    fn ui_texture(&self, _asset: AssetId) -> Option<UiTextureSlot> {
        None
    }

    /// Receive the current project-option values. The flow driver calls this
    /// when a UI entry applies defaults, after front-end option edits, and each
    /// time it enters the gameplay state, handing the cooked [`LevelOptionDef`]
    /// table and the parallel live-value slice (`values[i]` is the current value
    /// of `options[i]`, already clamped to that option's range). A scene reads
    /// whatever settings it cares about by matching `option.id` and caches or
    /// applies them.
    ///
    /// Values are not delivered per frame: front-end menus publish only when an
    /// option changes, and live in-game adjustment is a separate, later concern.
    /// Default is a no-op.
    #[allow(unused_variables)]
    fn apply_options(&mut self, options: &[LevelOptionDef], values: &[i32]) {}
}
