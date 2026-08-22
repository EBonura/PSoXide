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
use psx_level::{AssetId, LevelOptionDef, LevelUiValueBinding, LevelWorldLayer};
use psx_pad::{button, poll_port2, ActionInput, ActionMap, PadState};

use crate::frames::{SimTick, VideoHz, VisualFrame};
use crate::ui::UiTextureSlot;

/// How a scene hands its prepared 3D frame to the GPU.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RenderSubmission {
    /// [`Scene::render`] performs any ordering-table submission itself.
    Immediate,
    /// [`Scene::render`] only builds CPU-side packets; the runner calls
    /// [`Scene::submit_render`] after the previous frame is presented.
    /// This overlaps frame N GPU rasterisation with frame N+1 CPU work
    /// without requiring a second packet arena.
    Queued,
}

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
    /// Port-2 pad state from the most recent explicit
    /// [`Ctx::refresh_second_pad`] call.
    pub pad2: PadState,
    /// Previous explicit port-2 sample, for edge detection.
    pub pad2_prev: PadState,
    /// Frame buffer the scene draws into. Immediate scenes receive it cleared
    /// before [`Scene::render`]; queued scenes prepare CPU packets first and
    /// receive the clear immediately before [`Scene::submit_render`].
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
            pad2: PadState::NONE,
            pad2_prev: PadState::NONE,
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

    /// The raw active-high mask of buttons held this frame. Feed it to a
    /// [`psx_pad::PadTracker`] for delay-then-rate auto-repeat or handoff
    /// suppression, which `Ctx` itself does not track.
    #[inline]
    pub fn held_mask(&self) -> u16 {
        self.pad.buttons.bits()
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

    /// Poll controller port 2 once, preserving its previous sample.
    ///
    /// Port 2 is deliberately opt-in: a single-player game should not pay for
    /// a second SIO transaction every simulation tick. Multiplayer games call
    /// this only in the screens and modes that can use a second controller.
    #[inline]
    pub fn refresh_second_pad(&mut self) -> PadState {
        self.pad2_prev = self.pad2;
        self.pad2 = poll_port2();
        self.pad2
    }

    /// Current pad sample for player index 0 or 1. Other indices are
    /// disconnected.
    #[inline]
    pub const fn pad_for(&self, player: usize) -> PadState {
        match player {
            0 => self.pad,
            1 => self.pad2,
            _ => PadState::NONE,
        }
    }

    /// Previous pad sample for player index 0 or 1. Other indices are
    /// disconnected.
    #[inline]
    pub const fn previous_pad_for(&self, player: usize) -> PadState {
        match player {
            0 => self.pad_prev,
            1 => self.pad2_prev,
            _ => PadState::NONE,
        }
    }

    /// Whether `button` is held by one player now.
    #[inline]
    pub const fn is_held_for(&self, player: usize, button: u16) -> bool {
        self.pad_for(player).buttons.is_held(button)
    }

    /// Whether `button` was newly pressed by one player this tick.
    #[inline]
    pub const fn just_pressed_for(&self, player: usize, button: u16) -> bool {
        self.pad_for(player).buttons.is_held(button)
            && !self.previous_pad_for(player).buttons.is_held(button)
    }

    /// Whether `button` was newly released by one player this tick.
    #[inline]
    pub const fn just_released_for(&self, player: usize, button: u16) -> bool {
        !self.pad_for(player).buttons.is_held(button)
            && self.previous_pad_for(player).buttons.is_held(button)
    }

    /// Interpret one player's samples through a logical action map.
    #[inline]
    pub fn actions<'a, const ACTIONS: usize>(
        &'a self,
        player: usize,
        map: &'a ActionMap<ACTIONS>,
    ) -> ActionInput<'a, ACTIONS> {
        match player {
            0 => map.input(self.pad, self.pad_prev),
            1 => map.input(self.pad2, self.pad2_prev),
            _ => map.input(PadState::NONE, PadState::NONE),
        }
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

/// A copyable view of a flow state, handed to the scene resource-lifecycle
/// hooks. Carries the cooked state identity by value so the hooks never borrow
/// the flow tables the driver owns.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SceneStateRef {
    /// Cooked `LevelSceneState` id (or a synthesised id for bare
    /// `Gameplay` / `UiScene` flow states).
    pub id: u16,
    /// Whether this state runs the gameplay world.
    pub world: LevelWorldLayer,
    /// UI scene overlaid on this state, or the project's "no UI scene" sentinel.
    pub ui_scene: u16,
    /// State flags (UI input / pause-world).
    pub flags: u16,
}

impl SceneStateRef {
    /// `true` if this state runs the gameplay world layer.
    #[inline]
    pub fn has_gameplay(self) -> bool {
        matches!(self.world, LevelWorldLayer::Gameplay)
    }
}

/// Implement this trait on your game type and hand an instance to
/// [`App::run`][crate::app::App::run].
///
/// All methods take `&mut self` so the scene can keep its own state
/// inline (no globals needed). All methods take `&mut Ctx` so the
/// scene can read pad state and draw into the framebuffer.
pub trait Scene {
    /// Select the rendering hand-off contract. Existing scenes use immediate
    /// submission unless they explicitly opt into queued preparation.
    #[inline]
    fn render_submission(&self) -> RenderSubmission {
        RenderSubmission::Immediate
    }

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

    /// Advance load-only work while a flow transition is holding the loading
    /// screen after [`init`](Scene::init). Return `true` once the scene is ready
    /// for its first gameplay render. The default is ready immediately for
    /// simple scenes with no asynchronous world streaming.
    #[allow(unused_variables)]
    fn loading_update(&mut self, ctx: &mut Ctx) -> bool {
        true
    }

    /// Advance game state for the current fixed simulation tick.
    /// Called before [`render`]. Read pad input from `ctx` and use
    /// `ctx.sim_tick` for deterministic timers and animation phase.
    ///
    /// [`render`]: Scene::render
    fn update(&mut self, ctx: &mut Ctx);

    /// Advance resource work for a front-end UI state. The flow driver calls
    /// this once per UI tick before handling input/rendering. Scenes can use it
    /// to progressively stream optional menu assets without blocking the
    /// boot-time [`on_enter_state`](Scene::on_enter_state) path.
    #[allow(unused_variables)]
    fn update_ui_resources(&mut self, state: SceneStateRef, ctx: &mut Ctx) {}

    /// Whether every front-end (menu/intro/settings) asset is resident in
    /// memory. The flow driver holds the menu CD-DA music until this returns
    /// `true`, so the front-end never reads the CD while a music track is
    /// playing -- on real hardware the single laser cannot stream a UI image
    /// from the disc and play a CD-DA track at the same time (the read hangs
    /// and the music dies; emulators have no laser/seek model so they hide it).
    /// Scenes progressively stream all front-end UI in
    /// [`update_ui_resources`](Scene::update_ui_resources) and flip this true
    /// once the whole group is cached. Default is always ready (a scene with no
    /// streamed front-end assets).
    #[inline]
    fn front_end_assets_ready(&self) -> bool {
        true
    }

    /// Live world-load progress in Q12 (0..=4096) while
    /// [`loading_update`](Scene::loading_update) streams the next
    /// state's world. Feeds UI nodes bound to
    /// `LevelUiValueBinding::LoadingProgress`. Default reports zero
    /// (the engine pins the bar full once `loading_update` is ready).
    #[inline]
    fn loading_progress_q12(&self) -> i32 {
        0
    }

    /// One-shot hook before the authored loading scene first draws:
    /// re-upload any of its streamed images into VRAM (from a RAM
    /// cache, never the CD -- the disc laser belongs to the world
    /// stream during loading). Default no-op for scenes whose loading
    /// art is baked.
    #[inline]
    fn prepare_loading_assets(&mut self, _scene: u16) {}

    /// Draw or prepare the current frame after [`update`]. Immediate scenes
    /// are called after `ctx.fb` has been cleared. Queued scenes must only
    /// build CPU-side packets here; the runner clears the next back buffer and
    /// calls [`submit_render`](Scene::submit_render) afterwards.
    ///
    /// A scene that kicks its ordering table asynchronously (via
    /// [`OtFrame::submit_async`](crate::OtFrame::submit_async) +
    /// [`OtSubmitInFlight::detach`](crate::OtSubmitInFlight::detach))
    /// must not issue any immediate GP0 draw after the kick; put that
    /// work in [`render_overlay`](Scene::render_overlay) instead, which
    /// the engine calls once the GPU has drained the table.
    ///
    /// [`update`]: Scene::update
    fn render(&mut self, ctx: &mut Ctx);

    /// Submit packets prepared by [`Scene::render`] when
    /// [`Scene::render_submission`] returns [`RenderSubmission::Queued`].
    /// Queued scenes must not issue GPU commands from `render`; this hook runs
    /// after the prior frame has finished and the next back buffer is ready.
    #[allow(unused_variables)]
    fn submit_render(&mut self, ctx: &mut Ctx) {}

    /// Draw the 2D overlay layer (HUD, prompts, debug readouts) on top
    /// of the frame built by the last [`render`](Scene::render) call.
    ///
    /// The app runner invokes this at presentation time: the frame's
    /// ordering-table DMA has been drained and the GPU is idle, but the
    /// display has not flipped yet, so immediate GP0 draws here
    /// composite over the finished 3D scene in the back buffer without
    /// racing the linked-list walker. Default: no overlay.
    #[allow(unused_variables)]
    fn render_overlay(&mut self, ctx: &mut Ctx) {}

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

    /// Resolve a gameplay-owned UI value such as player health or stamina.
    ///
    /// Constants, project options, and loading progress remain owned by the
    /// flow driver. Returning `None` for a gameplay binding draws it as zero;
    /// scenes with an authored gameplay HUD override this method so that HUD
    /// is rendered once by the same scene-state UI pass as every other
    /// authored overlay.
    #[inline]
    fn ui_value(&self, _binding: LevelUiValueBinding) -> Option<i32> {
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

    /// Acquire the VRAM/asset resources a flow state needs, through the
    /// project's VRAM allocator. The flow driver calls this once, immediately
    /// before the state becomes active, and only when the incoming state's
    /// resource *set* differs from the outgoing one (see
    /// [`state_resource_key`](Scene::state_resource_key)). For a gameplay
    /// state it runs before [`init`](Scene::init), so uploads land before init
    /// reads them. Default is a no-op.
    #[allow(unused_variables)]
    fn on_enter_state(&mut self, state: SceneStateRef, ctx: &mut Ctx) {}

    /// Release the resources acquired in
    /// [`on_enter_state`](Scene::on_enter_state), returning their reservations
    /// to the allocator. The driver calls this once after the state stops
    /// being active, and only when the destination state's resource set
    /// differs. Must be idempotent (tolerate already-released resources).
    /// Default is a no-op.
    #[allow(unused_variables)]
    fn on_exit_state(&mut self, state: SceneStateRef, ctx: &mut Ctx) {}

    /// Stable identity of the resource *set* a state owns. The driver skips the
    /// exit+enter pair when the outgoing and incoming keys match, so a resource
    /// shared across states -- e.g. a UI font atlas used by both menus and the
    /// gameplay HUD -- is acquired once and not torn down on intra-set hops.
    /// Default: every state is its own set (`state.id`), i.e. fully symmetric
    /// create/destroy on every transition.
    #[allow(unused_variables)]
    fn state_resource_key(&self, state: SceneStateRef) -> u32 {
        state.id as u32
    }
}
