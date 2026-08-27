//! Unified runtime spine -- one [`Scene`] that drives a cooked
//! [`GameFlow`] of UI-scene and gameplay states.
//!
//! # Why this exists
//!
//! Every project used to be *only* its gameplay scene: the engine
//! booted straight into it and looped. Real games need front-end
//! states too -- a title screen, a pause menu, a game-over card --
//! and those want to share the exact same fixed-shape main loop the
//! gameplay scene runs under (pacing, telemetry, the
//! poll/update/render/present cadence). [`GameApp`] is the single
//! [`Scene`] the engine actually runs: it owns a small flow cursor,
//! dispatches each engine tick to either the borrowed gameplay scene
//! or the screen-space UI renderer, and never touches the loop.
//!
//! # Gameplay-only is the default
//!
//! [`App::run`][crate::app::App::run] keeps its old signature and
//! wraps the supplied gameplay scene in a [`GameApp`] over
//! [`GAMEPLAY_ONLY`] -- a one-state flow whose only state is
//! [`FlowState::Gameplay`]. In that configuration:
//!
//! - `init` parks on the loading screen, then resolves the entry state to
//!   `Gameplay` after the loading screen has rendered once. This gives
//!   streaming scenes a place to finish boot residency work before the first
//!   world frame is shown.
//! - `update` / `render` only ever match the `Gameplay` arm and
//!   forward to `gameplay.update` / `gameplay.render`.
//!
//! So the gameplay-only path is still the same runtime arm as before once
//! loading clears. The UI arms remain dead code for gameplay-only examples.
//!
//! # Borrowing
//!
//! The gameplay scene is borrowed (`&'a mut S`), never owned, so the
//! call site keeps its scene value. Per-state scratch lives on a
//! `Copy` [`FlowCursor`] rather than inside the `FlowState` enum, and
//! the resolved state is reduced to a `Copy` [`StateTag`] before any
//! dispatch. That keeps `self.gameplay` borrowable without also
//! holding a borrow of a state field. The UI render path copies the
//! `&'static` node slice out of `self` first so the resolver closures
//! capture nothing from `self`.
//!
//! # `no_std`
//!
//! Plain `Copy` data, no allocator, integer-only. Flow / scene / node
//! tables are `&'static` slices that the linker pins.

use psx_gpu::draw_quad_flat;
use psx_level::{
    first_focus, next_focus, scene_state_flags, ui_node_flags, FlowState, GameFlow, LevelOptionDef,
    LevelSceneState, LevelTransition, LevelTransitionKind, LevelUiAction, LevelUiNodeKind,
    LevelUiNodeRecord, LevelUiScene, LevelUiSfxCueRecord, LevelUiSfxEvent, LevelUiSfxSampleRecord,
    LevelUiValueBinding, LevelWorldLayer, NavDir, NavRect, SCENE_STATE_NONE, UI_OPTION_NONE,
    UI_SCENE_NONE, UI_SFX_NONE,
};
use psx_pad::{button, PadState};

use crate::scene::{Ctx, RenderSubmission, Scene, SceneStateRef};
use crate::transitions::render_transition_overlay;
use crate::ui;

/// Upper bound on focusable controls a single UI scene can navigate.
/// Focus gathering writes into a fixed stack array of this size to stay
/// `no_std` / alloc-free; a scene with more focusable nodes than this
/// simply ignores the overflow (still navigable up to the cap). Menus
/// in practice carry a handful of buttons, so the cap is generous.
const MAX_FOCUSABLE_NODES: usize = 64;

/// Cooked manifests compact their used UI fonts into at most eight slots.
/// Keep the flow driver's borrowed table at the same capacity: shortening it
/// makes valid higher selectors silently fall back to slot zero in `ui::draw_scene`.
const MAX_UI_FONT_SLOTS: usize = 8;

fn collect_ui_font_table<'font>(
    mut resolve: impl FnMut(u8) -> Option<&'font psx_font::FontAtlas>,
) -> [Option<&'font psx_font::FontAtlas>; MAX_UI_FONT_SLOTS] {
    core::array::from_fn(|index| resolve(index as u8))
}

/// Sentinel [`FlowCursor::menu_focus`] value meaning "no focus
/// resolved yet". Real focus is a node-slice index, always far below
/// this, so the entry path can tell an uninitialised cursor from a
/// genuine focus on node 0.
const MENU_FOCUS_NONE: u16 = u16::MAX;
const MENU_FOCUS_LOADING_UNRENDERED: u16 = u16::MAX - 1;
const MENU_FOCUS_LOADING_RENDERED: u16 = u16::MAX - 2;
const MENU_FOCUS_LOADING_INITED: u16 = u16::MAX - 3;
const MENU_FOCUS_LOADING_READY: u16 = u16::MAX - 4;

/// [`FlowCursor::active_resource_key`] sentinel meaning "no resource set held
/// yet". `u32::MAX` so it never collides with a [`Scene::state_resource_key`]
/// value (those default to a `u16` state id, always `<= 0xFFFF`).
const RESOURCE_KEY_NONE: u32 = u32::MAX;
/// Synthesised [`SceneStateRef::id`] for a bare [`FlowState::Gameplay`] (one
/// with no cooked [`LevelSceneState`]).
const SYNTH_GAMEPLAY_ID: u16 = u16::MAX;

/// Upper bound on project options the runtime value store tracks. The
/// store is a fixed `[i32; MAX_OPTIONS]` so the driver stays `no_std` /
/// alloc-free; a project with more options than this keeps the overflow
/// at its cooked default (read-only, never adjusted). Menus tune a
/// handful of options in practice, so the cap is generous.
const MAX_OPTIONS: usize = 32;
const MAX_UI_SFX_SAMPLES: usize = 64;
const TRANSITION_SWITCH_MIN_FRAME: u16 = 1;
#[cfg(target_arch = "mips")]
const UI_SFX_SAMPLE_BASE_BYTES: u32 = 0x30000;
#[cfg(target_arch = "mips")]
const UI_SFX_VOICE_BASE: u8 = 20;
#[cfg(target_arch = "mips")]
const UI_SFX_VOICE_COUNT: u8 = 4;
const CDDA_RETRY_TICKS: u32 = 60;
const CDDA_STATUS_TICKS: u32 = 30;
const CDDA_DEFAULT_VOLUME_PERCENT: u8 = 25;
#[cfg(any(target_arch = "mips", test))]
const CDDA_PLAYBACK_MODE: u8 = psx_io::cdrom::MODE_CDDA | psx_io::cdrom::MODE_AUTO_PAUSE;
#[cfg(target_arch = "mips")]
const CDDA_STATUS_PLAYING: u8 = 1 << 7;
/// Set while the head is still travelling. Play is a seek followed by
/// playback and the two bits are mutually exclusive, so a drive that has
/// accepted Play and not yet arrived reports neither. Reading that as
/// "finished" is how a track restarts itself for as long as the seek lasts.
#[cfg(target_arch = "mips")]
const CDDA_STATUS_SEEKING: u8 = 1 << 6;
#[cfg(target_arch = "mips")]
const CDDA_COMMAND_SPINS: u32 = 131_072;
/// Spin budget for the in-playback loop-status poll. Tiny on purpose: a CD
/// controller busy streaming CD-DA is slow to answer, so a generous budget here
/// would spin the whole menu loop for milliseconds every poll. With a small
/// budget the poll simply returns "couldn't tell" mid-playback (we keep playing)
/// and only reads a definite answer once the drive has auto-paused at track end.
#[cfg(target_arch = "mips")]
const CDDA_STATUS_SPINS: u32 = 1_024;
/// Consecutive definite "stopped" reads required before we treat the track as
/// finished and loop it. Guards against one stray/garbled status read triggering
/// a mid-playback reseek (which kills the audio on real hardware). Used by
/// `maybe_loop`, which is compiled on host too, so it is not target-gated.
const CDDA_STOPPED_CONFIRMATIONS: u8 = 2;

/// The implicit single-state flow every plain [`App::run`] call uses.
///
/// One state, [`FlowState::Gameplay`], entered immediately. A
/// [`GameApp`] built over this is behaviourally identical to running
/// the bare gameplay scene under the old runner.
pub const GAMEPLAY_ONLY: GameFlow = GameFlow {
    states: &[FlowState::Gameplay],
    scene_states: &[],
    entry: 0,
};

/// `Copy` reduction of the resolved [`FlowState`] for the current cursor
/// position. Dispatch reads this before borrowing `self.gameplay`, so a
/// composed state can update/render a world layer and an optional UI overlay
/// without holding a borrow into the flow tables.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct StateTag {
    world: LevelWorldLayer,
    ui_scene: u16,
    flags: u16,
}

impl StateTag {
    const GAMEPLAY: Self = Self {
        world: LevelWorldLayer::Gameplay,
        ui_scene: UI_SCENE_NONE,
        flags: 0,
    };

    const fn ui_only(scene: u16) -> Self {
        Self {
            world: LevelWorldLayer::None,
            ui_scene: scene,
            flags: scene_state_flags::UI_INPUT,
        }
    }

    const fn from_scene_state(state: LevelSceneState) -> Self {
        Self {
            world: state.world,
            ui_scene: state.ui_scene,
            flags: state.flags,
        }
    }

    const fn has_gameplay(self) -> bool {
        matches!(self.world, LevelWorldLayer::Gameplay)
    }

    const fn ui_scene(self) -> Option<u16> {
        if self.ui_scene == UI_SCENE_NONE {
            None
        } else {
            Some(self.ui_scene)
        }
    }

    const fn ui_accepts_input(self) -> bool {
        self.flags & scene_state_flags::UI_INPUT != 0
    }

    const fn world_is_paused(self) -> bool {
        self.flags & scene_state_flags::PAUSE_WORLD != 0
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum CddaStartStep {
    SetMode,
    Demute,
    Play,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct MusicCue {
    track: u8,
    volume_percent: u8,
    loop_track: bool,
}

impl MusicCue {
    const SILENT: Self = Self {
        track: 0,
        volume_percent: CDDA_DEFAULT_VOLUME_PERCENT,
        loop_track: false,
    };

    const fn normalized(self) -> Self {
        Self {
            track: if self.track > 99 { 0 } else { self.track },
            volume_percent: if self.volume_percent > 100 {
                100
            } else {
                self.volume_percent
            },
            loop_track: self.loop_track,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CddaPlayer {
    requested: MusicCue,
    current_track: u8,
    current_volume_percent: u8,
    step: CddaStartStep,
    next_retry_tick: u32,
    next_status_tick: u32,
    routed: bool,
    /// Consecutive definite "stopped" status reads seen by the loop poll. Reset
    /// on any "playing"/inconclusive read; a track is only re-played once this
    /// reaches [`CDDA_STOPPED_CONFIRMATIONS`].
    stopped_polls: u8,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct UiSfxRuntimeSample {
    addr_bytes: u32,
    base_pitch_q12: u16,
    loaded: bool,
}

impl UiSfxRuntimeSample {
    const EMPTY: Self = Self {
        addr_bytes: 0,
        base_pitch_q12: 0x1000,
        loaded: false,
    };
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlowTransition {
    target: u16,
    return_to: Option<u16>,
    pub(crate) spec: LevelTransition,
    pub(crate) elapsed: u16,
    switched: bool,
}

impl FlowTransition {
    pub(crate) const fn new(target: u16, return_to: Option<u16>, spec: LevelTransition) -> Self {
        Self {
            target,
            return_to,
            spec,
            elapsed: 0,
            switched: false,
        }
    }

    pub(crate) const fn frames(self) -> u16 {
        if self.spec.frames == 0 {
            1
        } else {
            self.spec.frames
        }
    }

    pub(crate) const fn switch_frame(self) -> u16 {
        let half = self.frames() / 2;
        if half < TRANSITION_SWITCH_MIN_FRAME {
            TRANSITION_SWITCH_MIN_FRAME
        } else {
            half
        }
    }
}

impl CddaPlayer {
    const fn new() -> Self {
        Self {
            requested: MusicCue::SILENT,
            current_track: 0,
            current_volume_percent: CDDA_DEFAULT_VOLUME_PERCENT,
            step: CddaStartStep::SetMode,
            next_retry_tick: 0,
            next_status_tick: 0,
            routed: false,
            stopped_polls: 0,
        }
    }

    fn request(&mut self, cue: MusicCue, tick: u32) {
        let cue = cue.normalized();
        if self.requested == cue {
            return;
        }

        let track_changed = self.requested.track != cue.track;
        self.requested = cue;

        if cue.track == 0 {
            self.release_for_data_reads(tick);
            return;
        }

        if self.routed && self.current_volume_percent != cue.volume_percent {
            cdda_set_volume(cue.volume_percent);
            self.current_volume_percent = cue.volume_percent;
        }

        if track_changed || self.current_track != cue.track {
            self.current_track = 0;
            self.step = CddaStartStep::SetMode;
            self.next_retry_tick = tick;
            self.next_status_tick = tick.saturating_add(CDDA_STATUS_TICKS);
        }
    }

    fn release_for_data_reads(&mut self, tick: u32) {
        self.requested = MusicCue::SILENT;
        self.current_track = 0;
        self.step = CddaStartStep::SetMode;
        self.next_retry_tick = tick;
        self.next_status_tick = 0;
        self.routed = false;
        cdda_release_for_data_reads();
    }

    fn update(&mut self, tick: u32) {
        if self.requested.track == 0 {
            return;
        }
        if self.current_track == self.requested.track {
            self.maybe_loop(tick);
            return;
        }
        if tick < self.next_retry_tick {
            return;
        }
        if !self.routed {
            cdda_route_audio(self.requested.volume_percent);
            self.current_volume_percent = self.requested.volume_percent;
            self.routed = true;
        }
        if cdda_issue_step(self.step, self.requested.track) {
            match self.step {
                CddaStartStep::SetMode => {
                    self.step = CddaStartStep::Demute;
                    self.next_retry_tick = tick.saturating_add(2);
                }
                CddaStartStep::Demute => {
                    self.step = CddaStartStep::Play;
                    self.next_retry_tick = tick.saturating_add(2);
                }
                CddaStartStep::Play => {
                    self.current_track = self.requested.track;
                    self.step = CddaStartStep::SetMode;
                    self.next_status_tick = tick.saturating_add(CDDA_STATUS_TICKS);
                }
            }
        } else {
            self.next_retry_tick = tick.saturating_add(CDDA_RETRY_TICKS);
        }
    }

    fn maybe_loop(&mut self, tick: u32) {
        if !self.requested.loop_track || tick < self.next_status_tick {
            return;
        }
        self.next_status_tick = tick.saturating_add(CDDA_STATUS_TICKS);
        // Non-blocking, restart-averse loop check. `cdda_drive_stopped` polls
        // GetStat with a tiny spin budget, so mid-playback -- when the busy
        // controller is slow to answer -- it returns `None` and we just keep
        // playing, never stalling the menu loop. Only a run of definite
        // "stopped" reads loops the track; the drive gives those cleanly once
        // MODE_AUTO_PAUSE halts it at the track boundary. Restarting on a single
        // missed/late read would reseek the laser mid-song and kill the audio on
        // real hardware (the exact failure this whole path is fixing).
        match cdda_drive_stopped() {
            Some(true) => {
                self.stopped_polls = self.stopped_polls.saturating_add(1);
                if self.stopped_polls >= CDDA_STOPPED_CONFIRMATIONS {
                    self.stopped_polls = 0;
                    self.current_track = 0;
                    self.step = CddaStartStep::SetMode;
                    self.next_retry_tick = tick;
                }
            }
            // Playing, or could not tell within the tiny budget: keep playing.
            _ => self.stopped_polls = 0,
        }
    }
}

fn music_cue_from_node(
    node: &LevelUiNodeRecord,
    options: &[LevelOptionDef],
    values: &[i32; MAX_OPTIONS],
    len: usize,
) -> MusicCue {
    MusicCue {
        track: node.option as u8,
        volume_percent: music_volume_percent(node.value, options, values, len),
        loop_track: node.flags & psx_level::ui_node_flags::MUSIC_LOOP != 0,
    }
    .normalized()
}

fn music_volume_percent(
    value: LevelUiValueBinding,
    options: &[LevelOptionDef],
    values: &[i32; MAX_OPTIONS],
    len: usize,
) -> u8 {
    match value {
        LevelUiValueBinding::ConstantQ12(value) => value.clamp(0, 100) as u8,
        LevelUiValueBinding::Option(option_id) => {
            resolve_option_value(options, values, len, option_id).clamp(0, 100) as u8
        }
        _ => CDDA_DEFAULT_VOLUME_PERCENT,
    }
}

#[cfg(target_arch = "mips")]
fn menu_audio_init() {
    psx_spu::init();
}

#[cfg(not(target_arch = "mips"))]
fn menu_audio_init() {}

#[cfg(target_arch = "mips")]
fn scaled_pitch(base_q12: u16, multiplier_q12: u16) -> psx_spu::Pitch {
    let raw = ((base_q12 as u32) * (multiplier_q12.max(1) as u32) + 2048) / 4096;
    psx_spu::Pitch::raw(raw.clamp(1, 0x3FFF) as u16)
}

#[cfg(target_arch = "mips")]
fn cdda_route_audio(volume_percent: u8) {
    cdda_set_volume(volume_percent);
    psx_spu::enable_cd_audio(true);
}

#[cfg(not(target_arch = "mips"))]
fn cdda_route_audio(_volume_percent: u8) {}

#[cfg(target_arch = "mips")]
fn cdda_set_volume(volume_percent: u8) {
    let volume = psx_spu::CdVolume::linear(volume_percent.min(100) as u16, 100);
    psx_spu::set_cd_volume(volume, volume);
}

#[cfg(not(target_arch = "mips"))]
fn cdda_set_volume(_volume_percent: u8) {}

#[cfg(all(target_arch = "mips", feature = "boot-trace"))]
#[inline(always)]
fn cdda_trace(message: &str) {
    psx_rt::tty::println(message);
}

#[cfg(all(target_arch = "mips", not(feature = "boot-trace")))]
#[inline(always)]
fn cdda_trace(_message: &str) {}

#[cfg(target_arch = "mips")]
fn cdda_issue_step(step: CddaStartStep, track: u8) -> bool {
    let ok = match step {
        CddaStartStep::SetMode => {
            psx_io::cdrom::try_set_mode(CDDA_PLAYBACK_MODE, CDDA_COMMAND_SPINS).is_some()
        }
        CddaStartStep::Demute => psx_io::cdrom::try_demute(CDDA_COMMAND_SPINS).is_some(),
        CddaStartStep::Play => psx_io::cdrom::try_play_track(track, CDDA_COMMAND_SPINS).is_some(),
    };
    if ok {
        match step {
            CddaStartStep::SetMode => cdda_trace("psx-engine: cdda setmode ok"),
            CddaStartStep::Demute => cdda_trace("psx-engine: cdda demute ok"),
            CddaStartStep::Play => cdda_trace("psx-engine: cdda play ok"),
        }
    }
    ok
}

#[cfg(not(target_arch = "mips"))]
fn cdda_issue_step(_step: CddaStartStep, _track: u8) -> bool {
    true
}

#[cfg(all(target_arch = "mips", feature = "boot-trace"))]
#[inline(always)]
fn flow_trace(message: &str) {
    psx_rt::tty::println(message);
}

#[cfg(not(all(target_arch = "mips", feature = "boot-trace")))]
#[inline(always)]
fn flow_trace(_message: &str) {}

/// Poll the drive's play state WITHOUT blocking the menu loop. Returns
/// `Some(true)` if it definitely reports stopped (neither playing nor seeking),
/// `Some(false)` if it reports playing, and `None` if it could not answer
/// within the tiny [`CDDA_STATUS_SPINS`] budget (the controller busy streaming
/// audio). The caller must read `None` as "assume still playing", never as
/// "stopped", or a busy answer would falsely loop-restart the track.
#[cfg(target_arch = "mips")]
fn cdda_drive_stopped() -> Option<bool> {
    psx_io::cdrom::try_get_stat(CDDA_STATUS_SPINS)
        .and_then(|response| response.bytes().first().copied())
        .map(|status| status & (CDDA_STATUS_PLAYING | CDDA_STATUS_SEEKING) == 0)
}

#[cfg(not(target_arch = "mips"))]
fn cdda_drive_stopped() -> Option<bool> {
    // No real drive off-target: report "playing" so the loop never restarts.
    Some(false)
}

#[cfg(target_arch = "mips")]
fn cdda_release_for_data_reads() {
    psx_spu::enable_cd_audio(false);
    let _ = psx_io::cdrom::try_pause_until_complete(CDDA_COMMAND_SPINS);
}

#[cfg(not(target_arch = "mips"))]
fn cdda_release_for_data_reads() {}

/// Cursor + small scratch tracking where in the [`GameFlow`] the
/// runtime currently sits.
///
/// `Copy` plain data: dispatch reads the current tag out of here, then
/// is free to take `&mut self.gameplay`. The `return_to` slot is a
/// single-deep stack so a transient state (a pause overlay, say) can
/// remember the state to come back to; deeper nesting is a later lane.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FlowCursor {
    /// Index into [`GameFlow::states`] of the active state.
    current: u16,
    /// One-deep "return to this state" slot, or `None`.
    return_to: Option<u16>,
    /// Whether `gameplay.init` has run yet. Gameplay init is deferred
    /// until the first transition into a `Gameplay` state so a flow
    /// that opens on a title screen does not pay gameplay boot cost
    /// until the player starts.
    gameplay_inited: bool,
    /// Node-slice index (within the active UI scene's node slice) of
    /// the focused control, or [`MENU_FOCUS_NONE`] when focus has not
    /// been resolved yet. Stored as a node-slice index (not a
    /// focusable-list position) so it feeds [`ui::draw_scene`]'s
    /// `focused` parameter directly. Reset to the sentinel on every
    /// scene change so the new scene re-seeds via [`first_focus`].
    menu_focus: u16,
    /// Small flow scratch. Timed UI states will use it as a countdown; while
    /// a loading screen is pending it stores the target flow-state index.
    intro_timer: u16,
    /// Resource-set key ([`Scene::state_resource_key`]) of the state whose
    /// resources are currently held, or [`RESOURCE_KEY_NONE`]. Drives
    /// exit/enter deduplication so a resource set shared across states is
    /// acquired once and not torn down on an intra-set transition.
    active_resource_key: u32,
}

impl FlowCursor {
    /// Fresh cursor positioned at `entry` with nothing initialised.
    #[inline]
    pub const fn new(entry: u16) -> Self {
        Self {
            current: entry,
            return_to: None,
            gameplay_inited: false,
            menu_focus: MENU_FOCUS_NONE,
            intro_timer: 0,
            active_resource_key: RESOURCE_KEY_NONE,
        }
    }

    /// Pool index of the currently focused control, or `None` when focus
    /// has not resolved yet (no scene, or no focusable control). This is the
    /// same value passed to [`ui::draw_scene`]'s `focused` parameter, so a
    /// caller can ask "which control is highlighted?" without reaching into
    /// the driver's internals.
    #[inline]
    pub fn focused_node(&self) -> Option<usize> {
        match self.menu_focus {
            MENU_FOCUS_NONE
            | MENU_FOCUS_LOADING_UNRENDERED
            | MENU_FOCUS_LOADING_RENDERED
            | MENU_FOCUS_LOADING_INITED
            | MENU_FOCUS_LOADING_READY => None,
            focus => Some(focus as usize),
        }
    }

    #[inline]
    fn loading_target(self) -> Option<u16> {
        match self.menu_focus {
            MENU_FOCUS_LOADING_UNRENDERED
            | MENU_FOCUS_LOADING_RENDERED
            | MENU_FOCUS_LOADING_INITED
            | MENU_FOCUS_LOADING_READY => Some(self.intro_timer),
            _ => None,
        }
    }

    #[inline]
    fn loading_has_rendered(self) -> bool {
        self.menu_focus == MENU_FOCUS_LOADING_RENDERED
    }

    /// True only before the loading screen's FIRST render of this
    /// loading pass (the focus advances RENDERED -> INITED afterwards,
    /// so `!loading_has_rendered()` is NOT a first-frame test).
    #[inline]
    fn loading_is_unrendered(self) -> bool {
        self.menu_focus == MENU_FOCUS_LOADING_UNRENDERED
    }

    #[inline]
    fn loading_is_inited(self) -> bool {
        self.menu_focus == MENU_FOCUS_LOADING_INITED
    }

    #[inline]
    fn begin_loading(&mut self, target: u16, return_to: Option<u16>) {
        self.return_to = return_to;
        self.menu_focus = MENU_FOCUS_LOADING_UNRENDERED;
        self.intro_timer = target;
    }

    #[inline]
    fn mark_loading_rendered(&mut self) {
        match self.menu_focus {
            MENU_FOCUS_LOADING_UNRENDERED => self.menu_focus = MENU_FOCUS_LOADING_RENDERED,
            MENU_FOCUS_LOADING_READY => {
                self.menu_focus = MENU_FOCUS_NONE;
                self.intro_timer = 0;
            }
            _ => {}
        }
    }

    #[inline]
    fn mark_loading_inited(&mut self, target: u16) {
        self.menu_focus = MENU_FOCUS_LOADING_INITED;
        self.intro_timer = target;
    }

    #[inline]
    fn mark_loading_ready(&mut self) {
        if self.menu_focus == MENU_FOCUS_LOADING_INITED {
            self.menu_focus = MENU_FOCUS_LOADING_READY;
        }
    }
}

/// The single [`Scene`] the engine runs: a cooked game-flow driver
/// over a borrowed gameplay scene.
///
/// Construct it directly (the fields are `pub` so
/// [`App::run`][crate::app::App::run] and
/// [`run_with_flow`][crate::app::App::run_with_flow] can build it from
/// their own data) and hand it to the same scheduled loop every scene
/// runs under.
pub struct GameApp<'a, S: Scene> {
    /// Flow graph: the state table and entry index.
    pub flow: &'static GameFlow,
    /// Addressable UI scenes, indexed into the shared node pool.
    pub scenes: &'static [LevelUiScene],
    /// Shared UI node pool the scenes slice into.
    pub nodes: &'static [LevelUiNodeRecord],
    /// Cooked UI gradient paints referenced by node colour roles.
    pub paints: &'static [psx_level::LevelUiPaintRecord],
    /// Cooked project options. Sliders and `SetOption` actions bind to
    /// these by id; the live value store ([`Self::option_values`]) is
    /// seeded from each option's `default`.
    pub options: &'static [LevelOptionDef],
    /// Cooked UI SFX sample blobs.
    pub ui_sfx_samples: &'static [LevelUiSfxSampleRecord],
    /// Cooked UI SFX cues, sliced by per-node `sfx_first` / `sfx_count`.
    pub ui_sfx_cues: &'static [LevelUiSfxCueRecord],
    /// Borrowed gameplay scene. Not owned, so the caller keeps it.
    pub gameplay: &'a mut S,
    /// Where in the flow we currently are.
    pub cursor: FlowCursor,
    /// Live option values, one slot per [`Self::options`] entry (parallel
    /// by index, capped at [`MAX_OPTIONS`]). Fixed array, no allocator.
    option_values: [i32; MAX_OPTIONS],
    /// Number of populated [`Self::option_values`] slots (`min(options.len(),
    /// MAX_OPTIONS)`).
    option_len: usize,
    /// Nonblocking CD-DA menu music driver.
    cdda: CddaPlayer,
    /// Uploaded UI SFX sample metadata, capped for no-alloc runtime lookup.
    #[cfg_attr(not(target_arch = "mips"), allow(dead_code))]
    ui_sfx_runtime_samples: [UiSfxRuntimeSample; MAX_UI_SFX_SAMPLES],
    /// Number of loaded UI SFX sample metadata slots.
    ui_sfx_runtime_len: usize,
    /// Round-robin cursor for choosing cue variants and voices.
    ui_sfx_cursor: u16,
    /// Active full-screen transition, if a button-triggered flow change is
    /// delaying the cursor switch.
    transition: Option<FlowTransition>,
    /// Loading-to-gameplay block dissolve. This is separate from authored
    /// flow transitions because the gameplay state is already initialized
    /// behind the loading card; its midpoint only changes which side renders.
    loading_exit_transition: Option<FlowTransition>,
    /// Authored loading-screen UI scene (`UI_SCENE_NONE` = built-in
    /// fallback). See [`crate::app::Config::loading_ui_scene`].
    loading_scene: u16,
    /// Live world-load progress in Q12, fed to
    /// [`LevelUiValueBinding::LoadingProgress`].
    loading_progress_q12: i32,
    /// The world and authored minimum hold are complete, so the loading card
    /// may reveal its confirm prompt and accept a fresh CROSS / START press.
    loading_confirm_ready: bool,
    /// Sim tick (vblank-anchored) at this loading pass's first render;
    /// drives the minimum-hold so an authored loading scene never
    /// flashes on a fast load. Vblank-clocked because the loading loop
    /// free-runs faster than the visual cadence, so a frame COUNT is
    /// not wall time.
    loading_hold_start_tick: u32,
    /// UI scene the timer pass last saw; timers re-arm when it changes.
    ui_timer_scene: u16,
    /// Tick the current UI scene was entered at, for Timer deadlines.
    ui_timer_entered_tick: u32,
    /// One bit per scene-local node index: Timer already fired this entry.
    /// A bitmask, not a counter: the width IS the per-scene Timer-node
    /// capacity (see the `local >= 64` bound in the timer pass), so
    /// narrowing this to u32 would silently halve it.
    /// psx-numeric-allow-next-line: fired-bit mask, see above
    ui_timers_fired: u64,
    /// Stable random-message selector for the currently shown UI scene.
    ui_text_seed: u16,
}

/// Minimum vblanks an AUTHORED loading scene stays up before handing
/// over (~1.7s NTSC). The built-in fallback screen keeps the old
/// immediate handover, so probe/profiling projects without a Loading
/// scene are unaffected.
const MIN_LOADING_HOLD_VBLANKS: u32 = 100;

/// Keep an authored loading bar legible when a load phase completes in one
/// coarse step. The value is still capped by real reported progress; this only
/// prevents a 0 -> 100% jump from being presented in a single UI update.
const LOADING_PROGRESS_MIN_STEP_Q12: i32 = 32;
const LOADING_PROGRESS_MAX_STEP_Q12: i32 = 128;

#[inline]
fn advance_loading_progress_q12(current: i32, target: i32) -> i32 {
    let current = current.clamp(0, 4096);
    let target = target.clamp(current, 4096);
    let delta = target - current;
    if delta == 0 {
        return current;
    }
    let step = (delta / 8)
        .clamp(LOADING_PROGRESS_MIN_STEP_Q12, LOADING_PROGRESS_MAX_STEP_Q12)
        .min(delta);
    current + step
}

/// PS1-cheap two-sided handoff: deterministic blocks cover the loading card,
/// gameplay replaces it at full coverage, then the same blocks peel away.
const LOADING_EXIT_DISSOLVE: LevelTransition = LevelTransition {
    kind: LevelTransitionKind::BlockDissolve,
    frames: 36,
    color: [4, 2, 4],
    seed: 0x71d5,
};

impl<'a, S: Scene> GameApp<'a, S> {
    /// Build a driver over `flow`, borrowing `gameplay`. The cursor is
    /// positioned at `flow.entry`; nothing runs until [`Scene::init`].
    /// The option value store is seeded from each [`LevelOptionDef::default`]
    /// (capped at [`MAX_OPTIONS`]) so sliders read a sensible value on the
    /// first frame.
    #[inline]
    pub fn new(
        flow: &'static GameFlow,
        scenes: &'static [LevelUiScene],
        nodes: &'static [LevelUiNodeRecord],
        paints: &'static [psx_level::LevelUiPaintRecord],
        options: &'static [LevelOptionDef],
        ui_sfx_samples: &'static [LevelUiSfxSampleRecord],
        ui_sfx_cues: &'static [LevelUiSfxCueRecord],
        loading_scene: u16,
        gameplay: &'a mut S,
    ) -> Self {
        let mut option_values = [0i32; MAX_OPTIONS];
        let option_len = options.len().min(MAX_OPTIONS);
        for (slot, option) in option_values[..option_len].iter_mut().zip(options) {
            *slot = option.default.clamp(option.min, option.max);
        }
        Self {
            flow,
            scenes,
            nodes,
            paints,
            options,
            ui_sfx_samples,
            ui_sfx_cues,
            gameplay,
            cursor: FlowCursor::new(flow.entry),
            option_values,
            option_len,
            cdda: CddaPlayer::new(),
            ui_sfx_runtime_samples: [UiSfxRuntimeSample::EMPTY; MAX_UI_SFX_SAMPLES],
            ui_sfx_runtime_len: 0,
            ui_sfx_cursor: 0,
            transition: None,
            loading_exit_transition: None,
            loading_scene,
            loading_progress_q12: 0,
            loading_confirm_ready: false,
            loading_hold_start_tick: 0,
            ui_timer_scene: u16::MAX,
            ui_timer_entered_tick: 0,
            ui_timers_fired: 0,
            ui_text_seed: 0x6d2b,
        }
    }

    /// True when the project authored a usable loading scene.
    fn loading_scene_active(&self) -> bool {
        self.loading_scene != psx_level::UI_SCENE_NONE
            && self.scene_node_range(self.loading_scene).1 != 0
    }

    /// Adjust the option with id `option_id` by `delta`, clamping the
    /// result to that option's `[min, max]`. No-op for the unbound
    /// sentinel or an unknown id, so a stray binding cannot panic or write
    /// out of range.
    fn adjust_option(&mut self, option_id: u16, delta: i32) -> bool {
        if option_id == UI_OPTION_NONE {
            return false;
        }
        let Some(index) = self.options[..self.option_len]
            .iter()
            .position(|option| option.id == option_id)
        else {
            return false;
        };
        let option = self.options[index];
        let next = self.option_values[index].saturating_add(delta);
        let next = next.clamp(option.min, option.max);
        if self.option_values[index] == next {
            return false;
        }
        self.option_values[index] = next;
        self.apply_current_options();
        true
    }

    /// Publish the live option store through the project hook. UI scenes call
    /// this after front-end edits so global presentation options (screen
    /// position, gamma, etc.) can preview immediately, and gameplay entry calls
    /// it so the same values are active for play.
    fn apply_current_options(&mut self) {
        let values = self.option_values;
        let len = self.option_len;
        self.apply_option_values(&values, len);
    }

    fn apply_option_values(&mut self, values: &[i32; MAX_OPTIONS], len: usize) {
        self.gameplay
            .apply_options(self.options, &values[..len.min(MAX_OPTIONS)]);
    }

    fn init_menu_audio(&mut self) {
        menu_audio_init();
        self.upload_ui_sfx_samples();
    }

    #[cfg(target_arch = "mips")]
    fn upload_ui_sfx_samples(&mut self) {
        let mut addr = UI_SFX_SAMPLE_BASE_BYTES;
        self.ui_sfx_runtime_len = 0;
        for (index, sample) in self
            .ui_sfx_samples
            .iter()
            .take(MAX_UI_SFX_SAMPLES)
            .enumerate()
        {
            let audio = psx_asset::Audio::from_bytes(sample.bytes).expect("ui psau sample");
            let spu_addr = psx_spu::SpuAddr::new(addr);
            let adpcm = audio.adpcm_bytes();
            psx_spu::upload_adpcm(spu_addr, adpcm);
            self.ui_sfx_runtime_samples[index] = UiSfxRuntimeSample {
                addr_bytes: addr,
                base_pitch_q12: psx_spu::Pitch::for_sample_rate(audio.sample_rate_hz()).as_u16(),
                loaded: true,
            };
            self.ui_sfx_runtime_len = index + 1;
            addr = addr.saturating_add(adpcm.len() as u32);
        }
    }

    #[cfg(not(target_arch = "mips"))]
    fn upload_ui_sfx_samples(&mut self) {
        self.ui_sfx_runtime_len = self.ui_sfx_samples.len().min(MAX_UI_SFX_SAMPLES);
    }

    fn play_node_sfx_event(&mut self, node: LevelUiNodeRecord, event: LevelUiSfxEvent) {
        if node.sfx_first == UI_SFX_NONE || node.sfx_count == 0 {
            return;
        }
        let first = node.sfx_first as usize;
        let end = first
            .saturating_add(node.sfx_count as usize)
            .min(self.ui_sfx_cues.len());
        if first >= end {
            return;
        }
        let mut matching = 0usize;
        for cue in &self.ui_sfx_cues[first..end] {
            if cue.event == event {
                matching += 1;
            }
        }
        if matching == 0 {
            return;
        }
        let choice = self.ui_sfx_cursor as usize % matching;
        self.ui_sfx_cursor = self.ui_sfx_cursor.wrapping_add(1);
        let mut seen = 0usize;
        let mut selected = None;
        for cue in &self.ui_sfx_cues[first..end] {
            if cue.event != event {
                continue;
            }
            if seen == choice {
                selected = Some(*cue);
                break;
            }
            seen += 1;
        }
        if let Some(cue) = selected {
            self.play_ui_sfx_cue(cue);
        }
    }

    #[cfg(target_arch = "mips")]
    fn play_ui_sfx_cue(&mut self, cue: LevelUiSfxCueRecord) {
        let Some(sample) = self
            .ui_sfx_runtime_samples
            .get(cue.sample as usize)
            .copied()
            .filter(|sample| sample.loaded)
        else {
            return;
        };
        let voice_index = UI_SFX_VOICE_BASE + (self.ui_sfx_cursor as u8 % UI_SFX_VOICE_COUNT);
        let voice = psx_spu::Voice::new(voice_index);
        let pitch = scaled_pitch(sample.base_pitch_q12, cue.pitch_q12);
        let volume = psx_spu::Volume::linear(cue.volume_percent.min(100) as u16, 100);
        // Through psx-sfx, which writes the repeat address. Setting the start
        // address and keying on leaves that register holding whatever the last
        // sound put there, and on END silicon jumps to it rather than stopping
        // -- so the cue ran on into some other sample's data.
        //
        // default_tone, never Adsr::sample(): on silicon the latter loops the
        // sample forever at full envelope once the END flag lands it in an
        // endless release (SB1, 2026-08-02). It is OneShot's default.
        //
        // Block count 0: the cue table records an address but no length, so
        // there is no cutoff to schedule and the envelope finishes the sound.
        let resident =
            psx_sfx::Sample::resident(psx_spu::SpuAddr::new(sample.addr_bytes), 44_100, 0);
        psx_sfx::OneShot::new(resident, volume)
            .with_pitch(pitch)
            .play(voice);
    }

    #[cfg(not(target_arch = "mips"))]
    fn play_ui_sfx_cue(&mut self, _cue: LevelUiSfxCueRecord) {}

    /// Resolve a flow-state index to its `Copy` [`StateTag`]. An
    /// out-of-range index falls back to `Gameplay` so a malformed flow
    /// degrades to "just run the game" rather than wedging.
    #[inline]
    fn tag_at(&self, index: u16) -> StateTag {
        match self.flow.states.get(index as usize) {
            Some(FlowState::SceneState { state }) => self
                .scene_state(*state)
                .map(StateTag::from_scene_state)
                .unwrap_or(StateTag::GAMEPLAY),
            Some(FlowState::Gameplay) | None => StateTag::GAMEPLAY,
            Some(FlowState::UiScene { scene }) => StateTag::ui_only(*scene),
        }
    }

    #[inline]
    fn scene_state(&self, state_id: u16) -> Option<LevelSceneState> {
        self.flow
            .scene_states
            .iter()
            .find(|state| state.id == state_id)
            .copied()
    }

    /// Resolve a flow-state index to a `Copy` [`SceneStateRef`] for the scene
    /// resource-lifecycle hooks. Mirrors [`tag_at`](Self::tag_at) but carries a
    /// stable id (synthesised for bare `Gameplay` / `UiScene` flow states).
    fn state_ref_at(&self, index: u16) -> SceneStateRef {
        let gameplay = SceneStateRef {
            id: SYNTH_GAMEPLAY_ID,
            world: LevelWorldLayer::Gameplay,
            ui_scene: UI_SCENE_NONE,
            flags: 0,
        };
        match self.flow.states.get(index as usize) {
            Some(FlowState::SceneState { state }) => match self.scene_state(*state) {
                Some(ss) => SceneStateRef {
                    id: ss.id,
                    world: ss.world,
                    ui_scene: ss.ui_scene,
                    flags: ss.flags,
                },
                None => gameplay,
            },
            Some(FlowState::Gameplay) | None => gameplay,
            Some(FlowState::UiScene { scene }) => SceneStateRef {
                id: SYNTH_GAMEPLAY_ID.wrapping_sub(1).wrapping_sub(*scene),
                world: LevelWorldLayer::None,
                ui_scene: *scene,
                flags: scene_state_flags::UI_INPUT,
            },
        }
    }

    /// Run the scene resource lifecycle for a transition onto `next_index`,
    /// *before* the cursor moves there. Calls `on_exit_state` on the outgoing
    /// state then `on_enter_state` on the incoming one, but skips both when the
    /// two states share a resource key (so a resource shared across states is
    /// acquired once and never thrashed). Idempotent for same-set transitions.
    fn switch_resources(&mut self, next_index: u16, ctx: &mut Ctx) {
        let next = self.state_ref_at(next_index);
        let next_key = self.gameplay.state_resource_key(next);
        if next_key == self.cursor.active_resource_key {
            return;
        }
        if self.cursor.active_resource_key != RESOURCE_KEY_NONE {
            let prev = self.state_ref_at(self.cursor.current);
            self.gameplay.on_exit_state(prev, ctx);
        }
        self.gameplay.on_enter_state(next, ctx);
        self.cursor.active_resource_key = next_key;
    }

    /// Tag of the state the cursor currently sits on.
    #[inline]
    fn current_tag(&self) -> StateTag {
        self.tag_at(self.cursor.current)
    }

    /// Index of the first [`FlowState::Gameplay`] in the table, if any.
    #[inline]
    fn first_gameplay_index(&self) -> Option<u16> {
        self.flow
            .states
            .iter()
            .enumerate()
            .find(|(index, _)| self.tag_at(*index as u16).has_gameplay())
            .map(|(index, _)| index as u16)
    }

    /// Index into [`GameFlow::states`] of the first state targeting
    /// `state_id`, if any. A `GotoState` action names a composed state id;
    /// the cursor addresses the flow table, so this resolves one to the
    /// other.
    fn flow_index_for_scene_state(&self, state_id: u16) -> Option<u16> {
        self.flow
            .states
            .iter()
            .position(
                |state| matches!(state, FlowState::SceneState { state } if *state == state_id),
            )
            .map(|index| index as u16)
    }

    /// Resolve the active composed state's authored START target to a flow
    /// cursor index. Compatibility `Gameplay` / `UiScene` entries have no
    /// binding and keep their existing input behavior.
    fn current_start_state_index(&self) -> Option<u16> {
        let FlowState::SceneState { state } =
            *self.flow.states.get(self.cursor.current as usize)?
        else {
            return None;
        };
        let target = self.scene_state(state)?.start_state;
        if target == SCENE_STATE_NONE || target == state {
            return None;
        }
        self.flow_index_for_scene_state(target)
    }

    /// Move the cursor onto `state_index`, running gameplay init exactly
    /// once if the target state carries a gameplay/world layer.
    ///
    /// This is the transition funnel. Today it preserves the old "start
    /// gameplay" semantics; future loading/unloading phases belong here
    /// because every state handoff now resolves through one path.
    fn enter_flow_state(&mut self, state_index: u16, return_to: Option<u16>, ctx: &mut Ctx) {
        // Acquire/release scene resources before the cursor moves and before
        // any gameplay init below reads them.
        self.switch_resources(state_index, ctx);
        self.cursor.current = state_index;
        self.cursor.return_to = return_to;
        self.cursor.menu_focus = MENU_FOCUS_NONE;
        self.cursor.intro_timer = 0;
        if !self.current_tag().has_gameplay() {
            return;
        }
        let option_values = self.option_values;
        let option_len = self.option_len;
        self.cdda.release_for_data_reads(ctx.sim_tick.as_u32());
        if !self.cursor.gameplay_inited {
            self.gameplay.init(ctx);
            self.cursor.gameplay_inited = true;
            // Deferred gameplay init runs after front-end option previews have
            // already programmed global presentation state. The publish after
            // init should use the menu store that triggered this transition,
            // not any state touched while bootstrapping gameplay.
            self.option_values = option_values;
            self.option_len = option_len;
            ctx.request_timing_realign();
        }
        // Hand the current option values to gameplay on every entry (not just
        // first init), so a setting changed in a front-end menu before Play
        // takes effect this session.
        self.apply_option_values(&option_values, option_len);
    }

    fn enter_or_load_flow_state(
        &mut self,
        state_index: u16,
        return_to: Option<u16>,
        ctx: &mut Ctx,
    ) {
        if self.tag_at(state_index).has_gameplay() && !self.cursor.gameplay_inited {
            self.cdda.release_for_data_reads(ctx.sim_tick.as_u32());
            flow_trace("psx-engine: loading begin");
            self.ui_text_seed = (ctx.sim_tick.as_u32() as u16)
                .wrapping_mul(109)
                .wrapping_add(state_index.wrapping_mul(257))
                .wrapping_add(1);
            self.loading_confirm_ready = false;
            self.loading_exit_transition = None;
            self.cursor.begin_loading(state_index, return_to);
            return;
        }
        self.enter_flow_state(state_index, return_to, ctx);
    }

    fn request_flow_state(&mut self, state_index: u16, return_to: Option<u16>, ctx: &mut Ctx) {
        self.enter_or_load_flow_state(state_index, return_to, ctx);
    }

    fn request_flow_state_transition(
        &mut self,
        state_index: u16,
        return_to: Option<u16>,
        transition: LevelTransition,
        ctx: &mut Ctx,
    ) {
        if !transition.is_active() {
            self.enter_or_load_flow_state(state_index, return_to, ctx);
            return;
        }
        self.transition = Some(FlowTransition::new(state_index, return_to, transition));
    }

    fn request_gameplay(&mut self, index: u16, ctx: &mut Ctx) {
        let return_to = self.cursor.return_to;
        self.request_flow_state(index, return_to, ctx);
    }

    fn request_gameplay_transition(
        &mut self,
        index: u16,
        transition: LevelTransition,
        ctx: &mut Ctx,
    ) {
        let return_to = self.cursor.return_to;
        self.request_flow_state_transition(index, return_to, transition, ctx);
    }

    fn update_transition(&mut self, ctx: &mut Ctx) -> bool {
        let Some(mut transition) = self.transition else {
            return false;
        };
        transition.elapsed = transition.elapsed.saturating_add(1);
        if !transition.switched && transition.elapsed >= transition.switch_frame() {
            transition.switched = true;
            self.enter_or_load_flow_state(transition.target, transition.return_to, ctx);
        }
        if transition.elapsed >= transition.frames() {
            self.transition = None;
        } else {
            self.transition = Some(transition);
        }
        true
    }

    fn update_loading_exit_transition(&mut self) {
        let Some(mut transition) = self.loading_exit_transition else {
            return;
        };
        transition.elapsed = transition.elapsed.saturating_add(1);
        if !transition.switched && transition.elapsed >= transition.switch_frame() {
            transition.switched = true;
            // LOADING_READY deliberately clears only here, under a fully
            // opaque dissolve, so the first gameplay frame cannot flash in.
            self.cursor.mark_loading_rendered();
            self.loading_confirm_ready = false;
        }
        if transition.elapsed >= transition.frames() {
            self.loading_exit_transition = None;
        } else {
            self.loading_exit_transition = Some(transition);
        }
    }

    fn finish_loading_transition(&mut self, ctx: &mut Ctx) {
        if self.cursor.loading_is_inited() {
            // This is the generic front-end -> gameplay asset handoff. It runs
            // after the target resource set and gameplay scene are initialised,
            // but before the first world-streaming tick can overwrite the RAM
            // cache that supplied an authored loading screen.
            self.gameplay.prepare_loading_assets(self.loading_scene);
            let world_ready = self.gameplay.loading_update(ctx);
            // Feed the authored loading scene's bound progress bar. Keep the
            // displayed value behind (never ahead of) actual load progress so
            // coarse phases remain visible instead of jumping 0 -> READY.
            let target_progress_q12 = if world_ready {
                4096
            } else {
                self.gameplay.loading_progress_q12().clamp(0, 4096)
            };
            if self.loading_scene_active() && target_progress_q12 > self.loading_progress_q12 {
                // A productive CD burst has explicitly paused at its next LBA.
                // Discard only the clock debt accumulated by that synchronous
                // slice and present its new progress before resuming the read.
                ctx.request_visual_checkpoint();
            }
            self.loading_progress_q12 = if self.loading_scene_active() {
                advance_loading_progress_q12(self.loading_progress_q12, target_progress_q12)
            } else {
                target_progress_q12
            };
            let hold_done = !self.loading_scene_active()
                || ctx
                    .sim_tick
                    .as_u32()
                    .wrapping_sub(self.loading_hold_start_tick)
                    >= MIN_LOADING_HOLD_VBLANKS;
            self.loading_confirm_ready =
                world_ready && hold_done && self.loading_progress_q12 == 4096;
            // Authored loading scenes double as readable lore cards: once the
            // world and minimum hold are complete, wait for a fresh confirm
            // instead of tearing the card away. The built-in fallback keeps
            // its automatic handoff for projects that did not author a screen.
            let confirmed = !self.loading_scene_active()
                || ctx.just_pressed(button::CROSS)
                || ctx.just_pressed(button::START);
            if self.loading_confirm_ready && confirmed {
                flow_trace("psx-engine: loading ready");
                self.cursor.mark_loading_ready();
                if self.loading_scene_active() {
                    self.loading_exit_transition = Some(FlowTransition::new(
                        self.cursor.current,
                        self.cursor.return_to,
                        LOADING_EXIT_DISSOLVE,
                    ));
                }
            }
            return;
        }
        let Some(state_index) = self.cursor.loading_target() else {
            return;
        };
        if !self.cursor.loading_has_rendered() {
            return;
        }
        let return_to = self.cursor.return_to;
        self.enter_flow_state(state_index, return_to, ctx);
        self.cursor.mark_loading_inited(state_index);
    }

    #[inline]
    fn loading_pending(&self) -> bool {
        self.cursor.loading_target().is_some()
    }

    /// Resolve a UI scene id to its `[first, count)` block in the shared
    /// node pool. Returns `(0, 0)` for an unknown id, so callers simply
    /// see an empty range and draw / navigate nothing.
    ///
    /// The range addresses the *full* pool ([`Self::nodes`]); cooked
    /// parent indices are pool-relative, so all focus geometry and the
    /// draw resolve parents against the whole pool, never a sub-slice.
    fn scene_node_range(&self, scene_id: u16) -> (usize, usize) {
        match self.scenes.iter().find(|scene| scene.id == scene_id) {
            Some(scene) => (scene.node_first as usize, scene.node_count as usize),
            None => (0, 0),
        }
    }

    fn scene_music_cue(&self, scene_id: u16) -> MusicCue {
        let (first, count) = self.scene_node_range(scene_id);
        let end = first.saturating_add(count).min(self.nodes.len());
        self.nodes[first..end]
            .iter()
            .find(|node| {
                matches!(node.kind, LevelUiNodeKind::Music)
                    && node.option != UI_OPTION_NONE
                    && node.option <= 99
                    && node.option != 0
            })
            .map(|node| {
                music_cue_from_node(node, self.options, &self.option_values, self.option_len)
            })
            .unwrap_or(MusicCue::SILENT)
    }

    /// Index into [`GameFlow::states`] of the first `UiScene` state
    /// targeting `scene_id`, if any. A button's `GotoScene` names a
    /// scene id; the flow cursor addresses states, so this resolves one
    /// to the other.
    fn ui_state_index_for_scene(&self, scene_id: u16) -> Option<u16> {
        self.flow
            .states
            .iter()
            .enumerate()
            .find(|(index, _)| {
                let tag = self.tag_at(*index as u16);
                tag.ui_scene() == Some(scene_id) && !tag.has_gameplay()
            })
            .map(|(index, _)| index as u16)
            .or_else(|| {
                self.flow
                    .states
                    .iter()
                    .enumerate()
                    .find(|(index, _)| self.tag_at(*index as u16).ui_scene() == Some(scene_id))
                    .map(|(index, _)| index as u16)
            })
    }

    /// Switch the cursor onto UI-scene flow `state_index`, remembering
    /// `return_to` for a later `Back`, and clear the resolved focus so
    /// the new scene re-seeds from [`first_focus`] on its first update.
    fn enter_ui_state(&mut self, state_index: u16, return_to: Option<u16>, ctx: &mut Ctx) {
        self.ui_text_seed = (ctx.sim_tick.as_u32() as u16)
            .wrapping_mul(109)
            .wrapping_add(state_index.wrapping_mul(257))
            .wrapping_add(1);
        self.switch_resources(state_index, ctx);
        self.cursor.current = state_index;
        self.cursor.return_to = return_to;
        self.cursor.menu_focus = MENU_FOCUS_NONE;
        self.cursor.intro_timer = 0;
    }

    /// Resolve, and lazily seed, the focused *pool* index for the scene
    /// occupying `[first, first + count)` of the shared pool.
    ///
    /// The cursor stores focus as a pool index. When it is the
    /// uninitialised sentinel, falls outside this scene's block, or
    /// points at a node that is no longer focusable (e.g. after a scene
    /// change), this re-seeds it with [`first_focus`] over the scene's
    /// focusable controls. Returns the focused pool index, or `None`
    /// when the scene has no focusable control at all.
    fn resolved_focus(&mut self, first: usize, count: usize) -> Option<usize> {
        let focus = self.cursor.menu_focus as usize;
        let end = first.saturating_add(count).min(self.nodes.len());
        let current_ok = focus >= first
            && focus < end
            && self
                .nodes
                .get(focus)
                .is_some_and(|node| ui::is_focusable(node.kind));
        if current_ok {
            return Some(focus);
        }
        // Authored tab scenes describe their active category with the
        // `.selected` tag. Prefer it when a freshly entered scene seeds
        // focus so the first shoulder press advances from the content the
        // player is actually looking at, not merely the first button in
        // node order.
        if let Some(selected) = (first..end).find(|&index| {
            self.nodes
                .get(index)
                .is_some_and(|node| ui::is_focusable(node.kind) && node.tag.ends_with(".selected"))
        }) {
            self.cursor.menu_focus = selected as u16;
            return Some(selected);
        }
        let mut rects = [NavRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }; MAX_FOCUSABLE_NODES];
        let mut node_indices = [0usize; MAX_FOCUSABLE_NODES];
        let n = gather_focusable(self.nodes, first, count, &mut rects, &mut node_indices);
        let slot = first_focus(&rects[..n])?;
        let node_index = node_indices[slot];
        self.cursor.menu_focus = node_index as u16;
        Some(node_index)
    }

    /// Move focus one step in `dir` over the scene's focusable controls,
    /// updating [`FlowCursor::menu_focus`] (a pool index) in place. A
    /// move with no candidate in that direction leaves focus untouched.
    fn move_focus(&mut self, first: usize, count: usize, dir: NavDir) {
        let Some(current_node) = self.resolved_focus(first, count) else {
            return;
        };
        let mut rects = [NavRect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        }; MAX_FOCUSABLE_NODES];
        let mut node_indices = [0usize; MAX_FOCUSABLE_NODES];
        let n = gather_focusable(self.nodes, first, count, &mut rects, &mut node_indices);
        // Locate the current node's slot inside the focusable list the
        // resolver works over.
        let Some(current_slot) = node_indices[..n]
            .iter()
            .position(|&node_index| node_index == current_node)
        else {
            return;
        };
        if let Some(next_slot) = next_focus(&rects[..n], current_slot, dir) {
            let next_node = node_indices[next_slot];
            self.cursor.menu_focus = next_node as u16;
            if next_node != current_node {
                if let Some(node) = self.nodes.get(next_node).copied() {
                    self.play_node_sfx_event(node, LevelUiSfxEvent::Focus);
                }
            }
        }
    }

    /// Handle a horizontal d-pad press over the scene at `[first, count)`.
    ///
    /// When the focused control is a [`LevelUiNodeKind::Slider`] bound to a
    /// project option, LEFT / RIGHT nudge that option by `-step` / `+step`
    /// (clamped) and focus stays put: a slider owns the horizontal axis so
    /// the player can scrub its value. Otherwise the press falls through to
    /// ordinary horizontal focus movement. `right` selects the direction.
    fn horizontal_press(&mut self, first: usize, count: usize, right: bool) {
        if let Some(node_index) = self.resolved_focus(first, count) {
            if let Some(node) = self.nodes.get(node_index).copied() {
                if matches!(node.kind, LevelUiNodeKind::Slider) && node.option != UI_OPTION_NONE {
                    let step = self.option_step(node.option);
                    if step == 0 {
                        return;
                    }
                    let delta = if right { step } else { -step };
                    let changed = self.adjust_option(node.option, delta);
                    self.play_node_sfx_event(
                        node,
                        if changed {
                            LevelUiSfxEvent::SliderNudge
                        } else {
                            LevelUiSfxEvent::SliderLimit
                        },
                    );
                    return;
                }
            }
        }
        self.move_focus(
            first,
            count,
            if right { NavDir::Right } else { NavDir::Left },
        );
    }

    /// Activate the previous/next authored tab button, wrapping at the ends.
    ///
    /// A button opts into this rail by using a `tab.*` runtime tag. One tab in
    /// each scene may append `.selected`; that becomes the shoulder-navigation
    /// origin while focus is down in the submenu. Scenes without tagged tabs
    /// keep the legacy shoulder-as-horizontal-focus behaviour.
    fn shoulder_tab_press(
        &mut self,
        first: usize,
        count: usize,
        right: bool,
        ctx: &mut Ctx,
    ) -> bool {
        let end = first.saturating_add(count).min(self.nodes.len());
        let focused = self.resolved_focus(first, count);
        let mut first_tab = None;
        let mut last_tab = None;
        let mut selected_tab = None;
        let mut focused_tab = None;

        for index in first..end {
            let Some(node) = self.nodes.get(index) else {
                continue;
            };
            if !matches!(node.kind, LevelUiNodeKind::Button) || !node.tag.starts_with("tab.") {
                continue;
            }
            first_tab.get_or_insert(index);
            last_tab = Some(index);
            if node.tag.ends_with(".selected") {
                selected_tab = Some(index);
            }
            if focused == Some(index) {
                focused_tab = Some(index);
            }
        }

        let (Some(first_tab), Some(last_tab)) = (first_tab, last_tab) else {
            return false;
        };
        let current = focused_tab.or(selected_tab).unwrap_or(first_tab);
        let next = if right {
            ((current + 1)..=last_tab)
                .find(|&index| {
                    self.nodes.get(index).is_some_and(|node| {
                        matches!(node.kind, LevelUiNodeKind::Button) && node.tag.starts_with("tab.")
                    })
                })
                .unwrap_or(first_tab)
        } else {
            (first_tab..current)
                .rev()
                .find(|&index| {
                    self.nodes.get(index).is_some_and(|node| {
                        matches!(node.kind, LevelUiNodeKind::Button) && node.tag.starts_with("tab.")
                    })
                })
                .unwrap_or(last_tab)
        };

        self.cursor.menu_focus = next as u16;
        if let Some(node) = self.nodes.get(next).copied() {
            self.play_node_sfx_event(node, LevelUiSfxEvent::Focus);
            self.play_node_sfx_event(node, LevelUiSfxEvent::Activate);
            self.perform_action(node.action, ctx);
        }
        true
    }

    /// Step size of the option with id `option_id`, or `0` when the id is
    /// unbound or unknown. A zero step makes a LEFT/RIGHT press a no-op
    /// rather than panicking on a stray binding.
    fn option_step(&self, option_id: u16) -> i32 {
        if option_id == UI_OPTION_NONE {
            return 0;
        }
        self.options[..self.option_len]
            .iter()
            .find(|option| option.id == option_id)
            .map(|option| option.step)
            .unwrap_or(0)
    }

    /// Fire the focused control's action. `GotoScene` / `StartGameplay`
    /// / `Back` drive the flow cursor; `SetOption` nudges the bound option
    /// in the value store; `Game` dispatches its opaque id to the scene.
    fn activate_focus(&mut self, first: usize, count: usize, ctx: &mut Ctx) {
        let Some(node_index) = self.resolved_focus(first, count) else {
            return;
        };
        let Some(node) = self.nodes.get(node_index).copied() else {
            return;
        };
        self.play_node_sfx_event(node, LevelUiSfxEvent::Activate);
        self.perform_action(node.action, ctx);
    }

    /// Execute one cooked UI action. Shared by focused-control
    /// activation and [`Self::update_ui_timers`], so a Timer node's
    /// auto-advance walks exactly the same flow paths a button press
    /// does.
    fn perform_action(&mut self, action: LevelUiAction, ctx: &mut Ctx) {
        match action {
            LevelUiAction::GotoState { state } => {
                if let Some(state_index) = self.flow_index_for_scene_state(state) {
                    let return_to = Some(self.cursor.current);
                    self.request_flow_state(state_index, return_to, ctx);
                }
            }
            LevelUiAction::TransitionToState { state, transition } => {
                if let Some(state_index) = self.flow_index_for_scene_state(state) {
                    let return_to = Some(self.cursor.current);
                    self.request_flow_state_transition(state_index, return_to, transition, ctx);
                }
            }
            LevelUiAction::GotoScene { scene } => {
                if let Some(state_index) = self.ui_state_index_for_scene(scene) {
                    let return_to = Some(self.cursor.current);
                    self.request_flow_state(state_index, return_to, ctx);
                }
            }
            LevelUiAction::TransitionToScene { scene, transition } => {
                if let Some(state_index) = self.ui_state_index_for_scene(scene) {
                    let return_to = Some(self.cursor.current);
                    self.request_flow_state_transition(state_index, return_to, transition, ctx);
                }
            }
            LevelUiAction::StartGameplay => {
                if let Some(gameplay_index) = self.first_gameplay_index() {
                    self.request_gameplay(gameplay_index, ctx);
                }
            }
            LevelUiAction::StartGameplayTransition { transition } => {
                if let Some(gameplay_index) = self.first_gameplay_index() {
                    self.request_gameplay_transition(gameplay_index, transition, ctx);
                }
            }
            LevelUiAction::Back => self.go_back(ctx),
            // Nudge the bound option by the authored delta (clamped). A
            // dynamic-label refresh from the new value is a later step.
            LevelUiAction::SetOption { option, delta } => {
                let _ = self.adjust_option(option, delta);
            }
            LevelUiAction::Game { id } => self.gameplay.game_ui_action(id, ctx),
        }
    }

    fn update_ui_scene(&mut self, scene: u16, ctx: &mut Ctx) {
        // Resolve the scene's block in the shared pool once. Focus geometry
        // walks the whole pool (parents are pool-relative), so the helpers
        // take the range, not a sub-slice.
        let (first, count) = self.scene_node_range(scene);

        // Timer nodes fire before input is read, so an auto-advance and a
        // same-frame press cannot double-transition (the flow request set
        // by the timer parks the cursor; activate_focus on the next state
        // sees fresh focus).
        if self.update_ui_timers(scene, first, count, ctx) {
            return;
        }

        // Seed (or repair) focus before reading input, so the first press
        // acts on the default control even on the frame the scene is entered,
        // and so a flow driven headless (update without render) still tracks
        // focus.
        let _ = self.resolved_focus(first, count);

        // D-pad moves focus among the scene's focusable controls. Each press
        // is a discrete step, so use just_pressed; the resolver no-ops when
        // nothing lies that way.
        if ctx.just_pressed(button::UP) {
            self.move_focus(first, count, NavDir::Up);
        }
        if ctx.just_pressed(button::DOWN) {
            self.move_focus(first, count, NavDir::Down);
        }
        // LEFT / RIGHT scrub a focused slider's bound option, or move focus
        // horizontally for any other control.
        if ctx.just_pressed(button::LEFT) {
            self.horizontal_press(first, count, false);
        }
        if ctx.just_pressed(button::RIGHT) {
            self.horizontal_press(first, count, true);
        }
        // Shoulder navigation is reserved for menu/category focus rather
        // than slider adjustment. This lets a PS1 L1/R1 tab rail remain
        // usable even when the current submenu contains option sliders.
        if ctx.just_pressed(button::L1) && !self.shoulder_tab_press(first, count, false, ctx) {
            self.move_focus(first, count, NavDir::Left);
        }
        if ctx.just_pressed(button::R1) && !self.shoulder_tab_press(first, count, true, ctx) {
            self.move_focus(first, count, NavDir::Right);
        }

        // CROSS activates the focused control. CIRCLE is a dedicated
        // back/cancel even when the focused control is not a Back button.
        if ctx.just_pressed(button::CROSS) {
            flow_trace("psx-engine: ui cross");
            self.activate_focus(first, count, ctx);
        } else if ctx.just_pressed(button::CIRCLE) {
            self.go_back(ctx);
        } else if ctx.just_pressed(button::START) {
            // START keeps its "confirm / jump to gameplay" shortcut so a
            // title screen advances without the player hunting for the Start
            // button first.
            if let Some(gameplay_index) = self.first_gameplay_index() {
                self.request_gameplay(gameplay_index, ctx);
            }
        }
    }

    /// Arm, tick, and fire the scene's Timer nodes. Returns true when a
    /// timer performed an action this frame (the caller stops processing
    /// input against a scene the flow is already leaving).
    ///
    /// Timers arm when the scene becomes the active UI scene and fire
    /// exactly once per entry. A [`ui_node_flags::TIMER_SKIPPABLE`] timer
    /// also fires on a fresh CROSS when the scene has no focusable
    /// control, which is the splash-screen idiom: logo, tag line, no
    /// buttons, X hurries it along.
    fn update_ui_timers(&mut self, scene: u16, first: usize, count: usize, ctx: &mut Ctx) -> bool {
        let now = ctx.sim_tick.as_u32();
        if self.ui_timer_scene != scene {
            self.ui_timer_scene = scene;
            self.ui_timer_entered_tick = now;
            self.ui_timers_fired = 0;
        }
        let elapsed = now.wrapping_sub(self.ui_timer_entered_tick);
        let skip_press =
            ctx.just_pressed(button::CROSS) && self.resolved_focus(first, count).is_none();
        let end = first.saturating_add(count).min(self.nodes.len());
        for index in first..end {
            let node = self.nodes[index];
            if node.kind != LevelUiNodeKind::Timer {
                continue;
            }
            // psx-numeric-allow-next-line: shift count for the 64-bit fired mask
            let local = (index - first) as u64;
            if local >= 64 || self.ui_timers_fired & (1 << local) != 0 {
                continue;
            }
            let LevelUiValueBinding::ConstantQ12(delay_ticks) = node.value else {
                continue;
            };
            let due = elapsed >= delay_ticks.max(0) as u32;
            let skipped = skip_press && node.flags & psx_level::ui_node_flags::TIMER_SKIPPABLE != 0;
            if !(due || skipped) {
                continue;
            }
            self.ui_timers_fired |= 1 << local;
            self.perform_action(node.action, ctx);
            return true;
        }
        false
    }

    fn update_ui_music(&mut self, scene: u16, ctx: &mut Ctx) {
        // Hold the menu CD-DA until the whole front-end asset group is resident.
        // Starting music while menu UI is still streaming from the CD makes the
        // read hang and the music die on real hardware (one laser cannot read a
        // UI image and play a CD-DA track at once). Once every front-end scene's
        // assets are cached, the menu issues no more CD reads, so CD-DA plays
        // uninterrupted while the player navigates intro/menu/settings.
        if self.gameplay.front_end_assets_ready() {
            let music_cue = self.scene_music_cue(scene);
            self.cdda.request(music_cue, ctx.sim_tick.as_u32());
        }
        self.cdda.update(ctx.sim_tick.as_u32());
    }

    fn render_ui_scene(&mut self, scene: u16, ctx: &mut Ctx) {
        // Resolve the scene's pool block, then resolve focus, so the
        // highlighted control matches the one input acts on.
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (40, 80, 220), "37 UI RANGE BEGIN");
        let (first, count) = self.scene_node_range(scene);
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (40, 120, 220), "37 UI RANGE OK");
        let focused = self.resolved_focus(first, count);
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (40, 160, 220), "37 UI FOCUS OK");
        // Copy the node pool + option store out of `self` first so the
        // resolver closures borrow only these Copy locals, not `self`
        // (draw_scene already borrows `self.nodes`).
        let nodes = self.nodes;
        let paints = self.paints;
        let options = self.options;
        let option_values = self.option_values;
        let option_len = self.option_len;
        // The scene's authored focus-ring style; unknown ids keep the
        // classic static ring.
        let focus_style = self
            .scenes
            .iter()
            .find(|record| record.id == scene)
            .map(|record| record.focus_style)
            .unwrap_or(psx_level::LevelUiFocusStyle::DEFAULT);
        let ui_text_seed = self.ui_text_seed;
        let gameplay = &self.gameplay;
        let mut textures = |asset| gameplay.ui_texture(asset);
        let loading_progress_q12 = self.loading_progress_q12;
        let value = |binding: LevelUiValueBinding| {
            resolve_ui_value(
                binding,
                options,
                &option_values,
                option_len,
                loading_progress_q12,
                &|binding| gameplay.ui_value(binding),
            )
        };
        let text = |tag: &str| gameplay.ui_text(tag);
        // Slider fill reads the live option value by id from the copied
        // store, through the same resolver the input path uses so the knob
        // position matches what scrubbing changed.
        let option_value =
            |option_id: u16| resolve_option_value(options, &option_values, option_len, option_id);
        let analog_active = ctx.pad.is_analog();
        let loading_complete = self.loading_confirm_ready;
        let visible = |node: &LevelUiNodeRecord| {
            (node.flags & ui_node_flags::ANALOG_INACTIVE_ONLY == 0 || !analog_active)
                && (node.flags & ui_node_flags::LOADING_COMPLETE_ONLY == 0 || loading_complete)
        };
        // The gameplay scene lends its uploaded font atlases so menu labels
        // and buttons draw with the same glyphs the HUD uses. Empty slots
        // skip text or fall back to slot 0 in the renderer.
        let font_table = collect_ui_font_table(|index| self.gameplay.ui_font_at(index));
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (40, 200, 220), "37 UI DRAW BEGIN");
        ui::draw_scene(
            nodes,
            first,
            count,
            paints,
            &font_table,
            focused,
            &focus_style,
            (ctx.sim_tick.as_u32() & 0xffff) as u16,
            ui_text_seed,
            &mut textures,
            &value,
            options,
            &option_value,
            &visible,
            &text,
        );
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (80, 220, 220), "37 UI DRAW OK");
    }

    /// Pop to the remembered `return_to` state, if one is set. The
    /// return target is itself a UI-scene state in this step (gameplay
    /// is reached through `StartGameplay`, never popped into), so this
    /// re-enters it as a UI scene and clears the one-deep return slot.
    fn go_back(&mut self, ctx: &mut Ctx) {
        if let Some(return_to) = self.cursor.return_to {
            self.enter_ui_state(return_to, None, ctx);
        }
    }

    fn render_loading_screen(&mut self, ctx: &mut Ctx) {
        draw_quad_flat([(0, 0), (320, 0), (0, 240), (320, 240)], 4, 6, 10);
        if self.loading_scene_active() {
            // Authored loading scene: re-upload its images from the
            // front-end RAM cache (no CD contention with the world
            // stream), then draw it like any other UI scene. Deferred
            // until the state init ran (init releases the departing
            // menu's UI VRAM, which would free an earlier upload out
            // from under us) and RETRIED each frame: a VRAM slot can
            // be momentarily unavailable while the world's textures
            // stream in, and already-resident images short-circuit,
            // so the steady-state cost is one slot lookup per image.
            let scene = self.loading_scene;
            self.render_ui_scene(scene, ctx);
            return;
        }
        // Built-in fallback: dark fill, divider, centered label.
        draw_quad_flat([(96, 136), (224, 136), (96, 138), (224, 138)], 34, 48, 64);
        if let Some(font) = self.gameplay.ui_font_at(0) {
            let text = "loading";
            let x = ((i32::from(ui::UI_CANVAS_W) - i32::from(font.text_width(text))) / 2)
                .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            let y = ((i32::from(ui::UI_CANVAS_H) - i32::from(font.line_height())) / 2)
                .clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            font.draw_text(x, y, text, (210, 220, 232));
        }
    }
}

/// Fill `rects` / `node_indices` with the focusable controls in the
/// pool block `nodes[first..first + count]`, in pool order, and return
/// how many were written.
///
/// `rects[i]` is the absolute [`NavRect`] of the focusable control
/// (parents resolved against the *full* `nodes` pool) and
/// `node_indices[i]` is its pool index, so a resolver result (a
/// position in `rects`) maps straight back to a pool index. Writing
/// stops at [`MAX_FOCUSABLE_NODES`]; both output arrays must be at least
/// that long. Pure + alloc-free so it runs on the PS1.
fn gather_focusable(
    nodes: &[LevelUiNodeRecord],
    first: usize,
    count: usize,
    rects: &mut [NavRect; MAX_FOCUSABLE_NODES],
    node_indices: &mut [usize; MAX_FOCUSABLE_NODES],
) -> usize {
    let end = first.saturating_add(count).min(nodes.len());
    let mut written = 0;
    for index in first..end {
        if written >= MAX_FOCUSABLE_NODES {
            break;
        }
        if ui::is_focusable(nodes[index].kind) {
            rects[written] = ui::node_nav_rect(nodes, index);
            node_indices[written] = index;
            written += 1;
        }
    }
    written
}

/// Resolve option id `option_id` to its live value in the parallel store
/// `values[..len]` (one slot per `options[..len]` entry), or `0` when the
/// id is the unbound sentinel or no [`LevelOptionDef`] matches. Free
/// function so both [`GameApp::option_value`] and the render path's
/// resolver closure (which captures copied locals, not `&self`) share one
/// id-matching rule. Pure + alloc-free.
fn resolve_option_value(
    options: &[LevelOptionDef],
    values: &[i32; MAX_OPTIONS],
    len: usize,
    option_id: u16,
) -> i32 {
    if option_id == UI_OPTION_NONE {
        return 0;
    }
    options[..len.min(MAX_OPTIONS)]
        .iter()
        .position(|option| option.id == option_id)
        .map(|index| values[index])
        .unwrap_or(0)
}

fn resolve_ui_value(
    binding: LevelUiValueBinding,
    options: &[LevelOptionDef],
    values: &[i32; MAX_OPTIONS],
    len: usize,
    loading_progress_q12: i32,
    gameplay_value: &impl Fn(LevelUiValueBinding) -> Option<i32>,
) -> i32 {
    match binding {
        LevelUiValueBinding::ConstantQ12(value) => value,
        LevelUiValueBinding::Option(option_id) => {
            resolve_option_value(options, values, len, option_id)
        }
        LevelUiValueBinding::LoadingProgress => loading_progress_q12,
        LevelUiValueBinding::PlayerHealth
        | LevelUiValueBinding::PlayerHealthMax
        | LevelUiValueBinding::PlayerHealthSecondary
        | LevelUiValueBinding::PlayerHealthSecondaryMax
        | LevelUiValueBinding::PlayerHealthEmptyInfluence
        | LevelUiValueBinding::PlayerHealthFullInfluence
        | LevelUiValueBinding::PlayerHealthSecondaryEmptyInfluence
        | LevelUiValueBinding::PlayerHealthSecondaryFullInfluence
        | LevelUiValueBinding::PlayerStamina
        | LevelUiValueBinding::PlayerStaminaMax => gameplay_value(binding).unwrap_or(0),
    }
}

impl<'a, S: Scene> Scene for GameApp<'a, S> {
    fn render_submission(&self) -> RenderSubmission {
        if !self.loading_pending() && self.current_tag().has_gameplay() {
            self.gameplay.render_submission()
        } else {
            RenderSubmission::Immediate
        }
    }

    fn init(&mut self, ctx: &mut Ctx) {
        // Menu music and UI SFX share SPU state. Initialise once here and
        // upload the generated UI SFX bank before any UI scene starts routing
        // CD-DA audio.
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (255, 80, 0), "04 MENU AUDIO BEGIN");
        self.init_menu_audio();
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (255, 0, 80), "05 MENU AUDIO OK");

        // Legacy boot-time shared-asset hook. Scenes that have migrated to the
        // per-state resource lifecycle leave this a no-op and acquire their
        // resources in `on_enter_state` instead.
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (255, 160, 0), "06 SHARED ASSETS BEGIN");
        self.gameplay.load_shared_assets(ctx);
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (0, 200, 80), "07 SHARED ASSETS OK");

        // Acquire the entry state's resource set (e.g. the UI font atlas) up
        // front, so both the loading screen and the first menu frame have it.
        // This is the per-scene enter hook the flow previously left as a TODO.
        let entry = self.cursor.current;
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (255, 0, 255), "08 RESOURCES BEGIN");
        self.switch_resources(entry, ctx);
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (80, 255, 80), "09 RESOURCES OK");

        // Enter the configured entry state. Any Gameplay entry, including the
        // gameplay-only default, first parks on loading so streaming scenes can
        // finish boot residency work before the first world frame is shown.
        // A UI entry only parks the cursor; gameplay.init is deferred until the
        // flow transitions into a Gameplay state.
        if self.current_tag().has_gameplay() {
            self.ui_text_seed = (ctx.sim_tick.as_u32() as u16)
                .wrapping_mul(109)
                .wrapping_add(entry.wrapping_mul(257))
                .wrapping_add(1);
            self.loading_confirm_ready = false;
            self.loading_exit_transition = None;
            self.cursor.begin_loading(entry, None);
        } else {
            // Cursor already at the UI-only entry from FlowCursor::new.
            // Apply defaults once so global presentation options preview
            // correctly even before the player starts gameplay.
            crate::app::boot_visual_checkpoint(&mut ctx.fb, (255, 255, 255), "11 OPTIONS BEGIN");
            self.apply_current_options();
            crate::app::boot_visual_checkpoint(&mut ctx.fb, (0, 255, 160), "12 OPTIONS OK");
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        if self.loading_exit_transition.is_some() {
            self.update_loading_exit_transition();
            if self.loading_pending() {
                return;
            }
        }
        if self.loading_pending() {
            self.finish_loading_transition(ctx);
            return;
        }
        if self.update_transition(ctx) {
            return;
        }
        // A composed state's START binding is evaluated before either world
        // simulation or UI navigation. This is what lets a gameplay+HUD state
        // open a paused gameplay+menu state even though its HUD does not capture
        // input. States without a binding retain the title-screen START shortcut
        // in `update_ui_scene` below.
        if ctx.just_pressed(button::START) {
            if let Some(target) = self.current_start_state_index() {
                let return_to = Some(self.cursor.current);
                self.request_flow_state(target, return_to, ctx);
                return;
            }
        }
        let tag = self.current_tag();
        if tag.has_gameplay() && !tag.world_is_paused() {
            if tag.ui_accepts_input() {
                // A live inventory keeps simulation running but owns the pad:
                // L1/R1 must switch tabs, not also fire gameplay attacks. The
                // world receives a neutral sample while the UI reads the real
                // sample immediately afterwards.
                let pad = ctx.pad;
                let pad_prev = ctx.pad_prev;
                ctx.pad = PadState::NONE;
                ctx.pad_prev = PadState::NONE;
                self.gameplay.update(ctx);
                ctx.pad = pad;
                ctx.pad_prev = pad_prev;
            } else {
                self.gameplay.update(ctx);
            }
        }
        if let Some(scene) = tag.ui_scene() {
            let state = self.state_ref_at(self.cursor.current);
            self.gameplay.update_ui_resources(state, ctx);
            if tag.ui_accepts_input() {
                self.update_ui_scene(scene, ctx);
                if self.loading_pending() {
                    return;
                }
            }
            if let Some(scene) = self.current_tag().ui_scene() {
                self.update_ui_music(scene, ctx);
            }
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (120, 40, 200), "34 GAMEAPP RENDER BEGIN");
        if self.loading_pending() {
            crate::app::boot_visual_checkpoint(
                &mut ctx.fb,
                (140, 40, 220),
                "35 LOADING RENDER BEGIN",
            );
            if self.cursor.loading_is_unrendered() {
                // First frame of this loading pass: anchor the hold timer.
                self.loading_hold_start_tick = ctx.sim_tick.as_u32();
                self.loading_progress_q12 = 0;
            }
            self.render_loading_screen(ctx);
            self.gameplay.render_post_process(ctx);
            if let Some(transition) = self.loading_exit_transition {
                render_transition_overlay(transition);
            } else {
                self.cursor.mark_loading_rendered();
            }
            return;
        }
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (140, 80, 220), "35 TAG RESOLVE BEGIN");
        let tag = self.current_tag();
        crate::app::boot_visual_checkpoint(&mut ctx.fb, (160, 80, 220), "36 TAG RESOLVE OK");
        if tag.has_gameplay() {
            crate::app::boot_visual_checkpoint(
                &mut ctx.fb,
                (180, 80, 220),
                "37 GAMEPLAY RENDER BEGIN",
            );
            self.gameplay.render(ctx);
            crate::app::boot_visual_checkpoint(
                &mut ctx.fb,
                (200, 80, 220),
                "38 GAMEPLAY RENDER OK",
            );
            // The gameplay scene may have kicked its ordering-table DMA
            // asynchronously; everything that composites over it (pause
            // UI, transition fades) draws in render_overlay, after the
            // app runner drains the walker.
            return;
        }
        if let Some(scene) = tag.ui_scene() {
            crate::app::boot_visual_checkpoint(&mut ctx.fb, (200, 120, 220), "37 UI RENDER BEGIN");
            self.render_ui_scene(scene, ctx);
            crate::app::boot_visual_checkpoint_hold(
                &mut ctx.fb,
                (220, 120, 220),
                "38 UI RENDER OK",
                60,
            );
        }
    }

    fn submit_render(&mut self, ctx: &mut Ctx) {
        if !self.loading_pending() && self.current_tag().has_gameplay() {
            self.gameplay.submit_render(ctx);
        }
    }

    fn render_overlay(&mut self, ctx: &mut Ctx) {
        if self.loading_pending() {
            return;
        }
        let tag = self.current_tag();
        if tag.has_gameplay() {
            self.gameplay.render_overlay(ctx);
            // Pause/options UI over the live gameplay frame. UI-only
            // states draw their scene in render(); only the
            // over-gameplay composite moves here, behind the DMA drain.
            if let Some(scene) = tag.ui_scene() {
                self.render_ui_scene(scene, ctx);
            }
        }
        self.gameplay.render_post_process(ctx);
        if let Some(transition) = self.transition {
            render_transition_overlay(transition);
        }
        if let Some(transition) = self.loading_exit_transition {
            render_transition_overlay(transition);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{SimTick, VideoHz, VisualFrame};
    use psx_gpu::framebuf::FrameBuffer;
    use psx_pad::ButtonState;

    #[test]
    fn authored_loading_progress_is_monotonic_and_visibly_catches_up() {
        assert_eq!(advance_loading_progress_q12(1000, 900), 1000);
        assert_eq!(advance_loading_progress_q12(0, 64), 32);

        let mut displayed = 0;
        let mut updates = 0;
        while displayed < 4096 {
            let next = advance_loading_progress_q12(displayed, 4096);
            assert!(next > displayed);
            assert!(next <= 4096);
            displayed = next;
            updates += 1;
        }
        assert!(
            updates > 1 && updates <= 64,
            "a coarse load phase should animate to full in a bounded number of updates"
        );
    }

    #[test]
    fn ui_font_table_requests_every_cooked_slot() {
        let mut requested = [false; MAX_UI_FONT_SLOTS];
        let table = collect_ui_font_table(|index| {
            requested[index as usize] = true;
            None
        });

        assert!(requested.iter().all(|requested| *requested));
        assert!(table.iter().all(Option::is_none));
    }

    /// Gameplay scene that records how many times each hook ran and in
    /// what order, so tests can assert the gameplay-only path matches
    /// the old runner's one-init-then-loop shape.
    #[derive(Default)]
    struct CountingScene {
        inits: u32,
        updates: u32,
        renders: u32,
        option_applications: u32,
        last_option_value: i32,
        enters: u32,
        exits: u32,
        loading_prepares: u32,
        game_actions: u32,
        last_game_action: u16,
        last_update_buttons: u16,
        enter_ran_before_init: bool,
        /// When `Some`, returned as the resource key for every state (a shared
        /// set); when `None`, the trait default (`state.id`) applies.
        shared_resource_key: Option<u32>,
    }

    impl Scene for CountingScene {
        fn init(&mut self, _ctx: &mut Ctx) {
            if self.enters > 0 {
                self.enter_ran_before_init = true;
            }
            self.inits += 1;
        }
        fn update(&mut self, ctx: &mut Ctx) {
            self.updates += 1;
            self.last_update_buttons = ctx.pad.buttons.bits();
        }
        fn render(&mut self, _ctx: &mut Ctx) {
            self.renders += 1;
        }
        fn apply_options(&mut self, options: &[LevelOptionDef], values: &[i32]) {
            self.option_applications += 1;
            self.last_option_value = options
                .iter()
                .position(|option| option.id == OPT_ID)
                .and_then(|index| values.get(index).copied())
                .unwrap_or(0);
        }
        fn on_enter_state(&mut self, _state: SceneStateRef, _ctx: &mut Ctx) {
            self.enters += 1;
        }
        fn on_exit_state(&mut self, _state: SceneStateRef, _ctx: &mut Ctx) {
            self.exits += 1;
        }
        fn prepare_loading_assets(&mut self, _scene: u16) {
            self.loading_prepares += 1;
        }
        fn game_ui_action(&mut self, id: u16, _ctx: &mut Ctx) {
            self.game_actions += 1;
            self.last_game_action = id;
        }
        fn state_resource_key(&self, state: SceneStateRef) -> u32 {
            self.shared_resource_key.unwrap_or(state.id as u32)
        }
    }

    fn test_ctx() -> Ctx {
        Ctx::new(
            SimTick::ZERO,
            VisualFrame::ZERO,
            VideoHz::NTSC,
            PadState::NONE,
            PadState::NONE,
            FrameBuffer::new(320, 240),
        )
    }

    #[test]
    fn gameplay_ui_bindings_delegate_to_the_scene_value_resolver() {
        let values = [0; MAX_OPTIONS];
        let gameplay_value = |binding| match binding {
            LevelUiValueBinding::PlayerHealth => Some(3072),
            LevelUiValueBinding::PlayerHealthMax => Some(4096),
            LevelUiValueBinding::PlayerHealthSecondary => Some(2048),
            LevelUiValueBinding::PlayerHealthSecondaryMax => Some(4096),
            _ => None,
        };

        assert_eq!(
            resolve_ui_value(
                LevelUiValueBinding::PlayerHealth,
                &[],
                &values,
                0,
                1234,
                &gameplay_value,
            ),
            3072,
        );
        assert_eq!(
            resolve_ui_value(
                LevelUiValueBinding::PlayerHealthMax,
                &[],
                &values,
                0,
                1234,
                &gameplay_value,
            ),
            4096,
        );
        assert_eq!(
            resolve_ui_value(
                LevelUiValueBinding::PlayerHealthSecondary,
                &[],
                &values,
                0,
                1234,
                &gameplay_value,
            ),
            2048,
        );
        assert_eq!(
            resolve_ui_value(
                LevelUiValueBinding::PlayerStamina,
                &[],
                &values,
                0,
                1234,
                &gameplay_value,
            ),
            0,
        );
        assert_eq!(
            resolve_ui_value(
                LevelUiValueBinding::LoadingProgress,
                &[],
                &values,
                0,
                1234,
                &gameplay_value,
            ),
            1234,
        );
    }

    #[test]
    fn gameplay_only_inits_once_then_forwards() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert!(
            app.loading_pending(),
            "gameplay-only boot starts on loading"
        );
        assert_eq!(app.gameplay.inits, 0, "loading defers gameplay init");
        complete_loading(&mut app, &mut ctx);

        assert!(
            app.gameplay.loading_prepares > 0,
            "built-in loading path prepares the target asset set"
        );

        app.update(&mut ctx);
        app.render(&mut ctx);
        app.update(&mut ctx);
        app.render(&mut ctx);

        assert_eq!(app.gameplay.inits, 1, "gameplay.init runs exactly once");
        assert_eq!(app.gameplay.updates, 2, "each update forwards to gameplay");
        assert_eq!(app.gameplay.renders, 2, "each render forwards to gameplay");
    }

    #[test]
    fn opaque_game_ui_actions_dispatch_to_the_gameplay_scene() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.perform_action(LevelUiAction::Game { id: 203 }, &mut ctx);

        assert_eq!(app.gameplay.game_actions, 1);
        assert_eq!(app.gameplay.last_game_action, 203);
    }

    #[test]
    fn gameplay_only_enters_resource_set_once_before_init() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        complete_loading(&mut app, &mut ctx);

        assert_eq!(app.gameplay.enters, 1, "resource set entered exactly once");
        assert_eq!(app.gameplay.exits, 0, "single-state flow never exits a set");
        assert!(
            app.gameplay.enter_ran_before_init,
            "on_enter_state runs before gameplay init"
        );
    }

    #[test]
    fn shared_resource_key_acquires_once_across_ui_to_gameplay() {
        static SCENES: &[LevelUiScene] = &[LevelUiScene {
            id: 7,
            name: "title",
            node_first: 0,
            node_count: 0,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        }];
        static FLOW: GameFlow = GameFlow {
            states: &[FlowState::UiScene { scene: 7 }, FlowState::Gameplay],
            scene_states: &[],
            entry: 0,
        };
        let mut scene = CountingScene {
            shared_resource_key: Some(42),
            ..Default::default()
        };
        let mut app = GameApp::new(
            &FLOW,
            SCENES,
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(
            app.gameplay.enters, 1,
            "UI entry acquires the shared set once"
        );

        let gameplay = app.first_gameplay_index().expect("gameplay state exists");
        app.request_gameplay(gameplay, &mut ctx);
        complete_loading(&mut app, &mut ctx);

        assert_eq!(
            app.gameplay.enters, 1,
            "shared set is not re-acquired on UI->gameplay"
        );
        assert_eq!(app.gameplay.exits, 0, "shared set is not torn down");
        assert_eq!(app.gameplay.inits, 1, "gameplay still inits exactly once");
    }

    #[test]
    fn default_resource_key_churns_on_state_change() {
        static SCENES: &[LevelUiScene] = &[LevelUiScene {
            id: 7,
            name: "title",
            node_first: 0,
            node_count: 0,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        }];
        static FLOW: GameFlow = GameFlow {
            states: &[FlowState::UiScene { scene: 7 }, FlowState::Gameplay],
            scene_states: &[],
            entry: 0,
        };
        // No shared key: each state is its own set, so UI->gameplay exits the
        // UI set and enters the gameplay set.
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &FLOW,
            SCENES,
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(app.gameplay.enters, 1);
        assert_eq!(app.gameplay.exits, 0);

        let gameplay = app.first_gameplay_index().expect("gameplay state exists");
        app.request_gameplay(gameplay, &mut ctx);
        complete_loading(&mut app, &mut ctx);

        assert_eq!(app.gameplay.exits, 1, "default key exits the old set");
        assert_eq!(app.gameplay.enters, 2, "default key enters the new set");
    }

    #[test]
    fn gameplay_entry_loads_before_initialising() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert!(app.loading_pending(), "gameplay entry parks on loading");
        assert_eq!(app.gameplay.inits, 0);
        assert_eq!(app.gameplay.updates, 0);
        assert!(
            !ctx.take_timing_realign_request(),
            "no timing reset before gameplay init actually runs"
        );

        complete_loading(&mut app, &mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert!(
            ctx.take_timing_realign_request(),
            "deferred boot gameplay init requests one scheduler realign"
        );
    }

    #[test]
    fn ui_entry_defers_gameplay_init_until_start() {
        static SCENES: &[LevelUiScene] = &[LevelUiScene {
            id: 7,
            name: "title",
            node_first: 0,
            node_count: 0,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        }];
        static FLOW: GameFlow = GameFlow {
            states: &[FlowState::UiScene { scene: 7 }, FlowState::Gameplay],
            scene_states: &[],
            entry: 0,
        };

        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &FLOW,
            SCENES,
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(app.gameplay.inits, 0, "UI entry does not init gameplay");
        assert!(!ctx.take_timing_realign_request());

        // No START yet: still on the UI state, gameplay untouched.
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 0);
        assert_eq!(app.gameplay.updates, 0);
        assert!(!ctx.take_timing_realign_request());

        // Press START: the UI leaves input behind and shows loading; the
        // blocking gameplay init is not paid until that loading screen has
        // rendered once.
        ctx.pad_prev = PadState::NONE;
        ctx.pad.buttons = ButtonState::from_bits(button::START);
        app.update(&mut ctx);
        assert!(app.loading_pending(), "transition parks on loading");
        assert_eq!(app.gameplay.inits, 0, "loading defers gameplay init");
        assert!(
            !ctx.take_timing_realign_request(),
            "no timing reset before gameplay init actually runs"
        );

        complete_loading(&mut app, &mut ctx);
        assert_eq!(app.gameplay.inits, 1, "transition inits gameplay once");
        assert!(
            ctx.take_timing_realign_request(),
            "deferred gameplay init requests one scheduler realign"
        );
        assert!(
            !ctx.take_timing_realign_request(),
            "the timing reset request is one-shot"
        );

        // Now resolved to Gameplay: updates forward, no re-init.
        ctx.pad_prev = ctx.pad;
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 1);
        assert!(!ctx.take_timing_realign_request());
    }

    #[test]
    fn unknown_scene_id_yields_empty_node_range() {
        let mut scene = CountingScene::default();
        let app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        assert_eq!(app.scene_node_range(999), (0, 0));
    }

    use psx_level::{AssetId, LevelUiImageEffect, LevelUiNodeKind, UI_OPTION_NONE, UI_PAINT_NONE};

    /// Release every button, then press exactly `button`, so the next
    /// `update` sees it as `just_pressed`. Mirrors the pad-edge idiom
    /// the gameplay-init test above uses, factored out for the menu
    /// navigation tests that press several buttons in sequence.
    fn press(ctx: &mut Ctx, button: u16) {
        ctx.pad_prev = PadState::NONE;
        ctx.pad.buttons = ButtonState::from_bits(button);
    }

    /// Build a focusable button at `(x, y)` with the given size and
    /// action, parented to the canvas at slice index 0.
    const fn button(x: i16, y: i16, action: LevelUiAction) -> LevelUiNodeRecord {
        LevelUiNodeRecord {
            parent: Some(psx_level::UiNodeIndex::new(0)),
            kind: LevelUiNodeKind::Button,
            x,
            y,
            width: 80,
            height: 20,
            color: [40, 48, 64],
            background: [0, 0, 0],
            accent: [236, 240, 248],
            color_paint: UI_PAINT_NONE,
            background_paint: UI_PAINT_NONE,
            accent_paint: UI_PAINT_NONE,
            value: LevelUiValueBinding::ConstantQ12(0),
            max: LevelUiValueBinding::ConstantQ12(1),
            texture_asset: AssetId(u16::MAX),
            image_effect: LevelUiImageEffect::None,
            text: "",
            tag: "",
            action,
            option: UI_OPTION_NONE,
            rotation_degrees: 0,
            flags: 0,
            sfx_first: UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: 256,
            letter_spacing: 0,
        }
    }

    const fn tagged_button(
        x: i16,
        y: i16,
        tag: &'static str,
        action: LevelUiAction,
    ) -> LevelUiNodeRecord {
        let mut node = button(x, y, action);
        node.tag = tag;
        node
    }

    const CANVAS: LevelUiNodeRecord = LevelUiNodeRecord {
        parent: None,
        kind: LevelUiNodeKind::Canvas,
        x: 0,
        y: 0,
        width: 320,
        height: 240,
        color: [0, 0, 0],
        background: [0, 0, 0],
        accent: [0, 0, 0],
        color_paint: UI_PAINT_NONE,
        background_paint: UI_PAINT_NONE,
        accent_paint: UI_PAINT_NONE,
        value: LevelUiValueBinding::ConstantQ12(0),
        max: LevelUiValueBinding::ConstantQ12(1),
        texture_asset: AssetId(u16::MAX),
        image_effect: LevelUiImageEffect::None,
        text: "",
        tag: "",
        action: LevelUiAction::Back,
        option: UI_OPTION_NONE,
        rotation_degrees: 0,
        flags: 0,
        sfx_first: UI_SFX_NONE,
        sfx_count: 0,
        font: 0,
        font_scale: 256,
        letter_spacing: 0,
    };

    // A title scene (id 1) with two stacked buttons: "Play" (StartGameplay)
    // on top, "Options" (GotoScene -> 2) below. Scene id 2 is the options
    // screen with a single "Back" button. The shared node pool holds the
    // title scene at [0..3) and the options scene at [3..5).
    static MENU_NODES: &[LevelUiNodeRecord] = &[
        CANVAS,
        button(120, 80, LevelUiAction::StartGameplay),
        button(120, 120, LevelUiAction::GotoScene { scene: 2 }),
        CANVAS,
        button(120, 100, LevelUiAction::Back),
    ];
    static MENU_SCENES: &[LevelUiScene] = &[
        LevelUiScene {
            id: 1,
            name: "title",
            node_first: 0,
            node_count: 3,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        },
        LevelUiScene {
            id: 2,
            name: "options",
            node_first: 3,
            node_count: 2,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        },
    ];
    static MENU_FLOW: GameFlow = GameFlow {
        states: &[
            FlowState::UiScene { scene: 1 },
            FlowState::UiScene { scene: 2 },
            FlowState::Gameplay,
        ],
        scene_states: &[],
        entry: 0,
    };

    static HORIZONTAL_MENU_NODES: &[LevelUiNodeRecord] = &[
        CANVAS,
        button(40, 80, LevelUiAction::Game { id: 1 }),
        button(140, 80, LevelUiAction::Game { id: 2 }),
    ];
    static HORIZONTAL_MENU_SCENES: &[LevelUiScene] = &[LevelUiScene {
        id: 3,
        name: "tabs",
        node_first: 0,
        node_count: 3,
        focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
    }];
    static HORIZONTAL_MENU_FLOW: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 3 }],
        scene_states: &[],
        entry: 0,
    };

    static TAGGED_TAB_NODES: &[LevelUiNodeRecord] = &[
        CANVAS,
        button(40, 100, LevelUiAction::Game { id: 99 }),
        tagged_button(200, 20, "tab.player", LevelUiAction::Game { id: 10 }),
        tagged_button(
            230,
            20,
            "tab.armament.selected",
            LevelUiAction::Game { id: 11 },
        ),
        tagged_button(260, 20, "tab.system", LevelUiAction::Game { id: 12 }),
    ];
    static TAGGED_TAB_SCENES: &[LevelUiScene] = &[LevelUiScene {
        id: 4,
        name: "tagged-tabs",
        node_first: 0,
        node_count: 5,
        focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
    }];
    static TAGGED_TAB_FLOW: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 4 }],
        scene_states: &[],
        entry: 0,
    };

    static COMPOSED_STATES: &[LevelSceneState] = &[LevelSceneState {
        id: 42,
        name: "gameplay-with-ui",
        world: LevelWorldLayer::Gameplay,
        ui_scene: 1,
        flags: scene_state_flags::UI_INPUT,
        start_state: SCENE_STATE_NONE,
    }];
    static COMPOSED_FLOW: GameFlow = GameFlow {
        states: &[FlowState::SceneState { state: 42 }],
        scene_states: COMPOSED_STATES,
        entry: 0,
    };

    static PAUSE_COMPOSED_STATES: &[LevelSceneState] = &[
        LevelSceneState {
            id: 42,
            name: "gameplay",
            world: LevelWorldLayer::Gameplay,
            ui_scene: UI_SCENE_NONE,
            flags: 0,
            start_state: 43,
        },
        LevelSceneState {
            id: 43,
            name: "pause",
            world: LevelWorldLayer::Gameplay,
            ui_scene: 1,
            flags: scene_state_flags::UI_INPUT | scene_state_flags::PAUSE_WORLD,
            start_state: 42,
        },
    ];
    static PAUSE_COMPOSED_FLOW: GameFlow = GameFlow {
        states: &[
            FlowState::SceneState { state: 42 },
            FlowState::SceneState { state: 43 },
        ],
        scene_states: PAUSE_COMPOSED_STATES,
        entry: 0,
    };

    #[test]
    fn scene_state_can_run_gameplay_with_ui_overlay_input() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &COMPOSED_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert!(app.loading_pending());
        assert_eq!(app.gameplay.inits, 0);
        complete_loading(&mut app, &mut ctx);
        assert_eq!(app.gameplay.inits, 1);

        press(&mut ctx, button::R1);
        app.update(&mut ctx);
        assert_eq!(app.gameplay.updates, 1);
        assert_eq!(app.gameplay.last_update_buttons, 0);
        assert!(ctx.pad.buttons.is_held(button::R1));
        assert_eq!(app.cursor.focused_node(), Some(1));
    }

    #[test]
    fn authored_start_target_opens_and_resumes_a_paused_world_overlay() {
        let mut scene = CountingScene {
            shared_resource_key: Some(7),
            ..Default::default()
        };
        let mut app = GameApp::new(
            &PAUSE_COMPOSED_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        complete_loading(&mut app, &mut ctx);
        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.current, 0);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 1);

        press(&mut ctx, button::START);
        app.update(&mut ctx);
        assert_eq!(app.cursor.current, 1, "START should enter Pause Menu");
        assert_eq!(
            app.gameplay.updates, 1,
            "the opening tick must not advance gameplay"
        );

        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.focused_node(), Some(1));
        assert_eq!(
            app.gameplay.updates, 1,
            "paused states keep the world frozen"
        );

        press(&mut ctx, button::START);
        app.update(&mut ctx);
        assert_eq!(app.cursor.current, 0, "START should resume Gameplay");
        assert_eq!(
            app.gameplay.inits, 1,
            "shared gameplay resources stay resident"
        );
        assert_eq!(app.gameplay.updates, 1);

        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.gameplay.updates, 2);
    }

    /// One UI update tick with no buttons held, so focus seeds without
    /// triggering any action. Render is the GPU draw path and cannot run
    /// in a host unit test (it dereferences raw MMIO), so navigation is
    /// driven and observed entirely through update here.
    fn idle_tick(app: &mut GameApp<'_, CountingScene>, ctx: &mut Ctx) {
        ctx.pad_prev = PadState::NONE;
        ctx.pad = PadState::NONE;
        app.update(ctx);
    }

    fn complete_loading(app: &mut GameApp<'_, CountingScene>, ctx: &mut Ctx) {
        assert!(
            app.loading_pending(),
            "loading transition should be pending"
        );
        app.cursor.mark_loading_rendered();
        app.update(ctx);
        assert!(
            app.loading_pending(),
            "gameplay init keeps loading visible until one post-init render"
        );
        app.cursor.mark_loading_rendered();
        assert!(
            app.loading_pending(),
            "loading remains visible while the scene finishes load-only work"
        );
        app.update(ctx);
        assert!(
            app.loading_pending(),
            "ready loading transition clears after one final loading render"
        );
        app.cursor.mark_loading_rendered();
        assert!(
            !app.loading_pending(),
            "rendered loading transition should enter the target state"
        );
    }

    #[test]
    fn authored_loading_scene_waits_for_fresh_confirm_after_ready() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            1,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert!(app.loading_pending());

        // First loading render permits deferred gameplay init.
        app.cursor.mark_loading_rendered();
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);

        // Make the authored minimum hold complete. A held/old press and an
        // idle tick must not dismiss the lore card.
        app.loading_hold_start_tick = u32::MAX - MIN_LOADING_HOLD_VBLANKS;
        ctx.pad_prev = ctx.pad;
        app.update(&mut ctx);
        assert!(app.loading_pending());
        assert!(
            ctx.take_visual_checkpoint_request(),
            "each authored loading slice must yield to a visible frame"
        );
        assert!(
            !app.loading_confirm_ready && app.loading_progress_q12 < 4096,
            "READY must wait for the visibly animated progress bar"
        );

        ctx.pad = PadState::NONE;
        ctx.pad_prev = PadState::NONE;
        for _ in 0..64 {
            app.update(&mut ctx);
            assert!(ctx.take_visual_checkpoint_request());
            if app.loading_confirm_ready {
                break;
            }
        }
        assert_eq!(app.loading_progress_q12, 4096);
        assert!(app.loading_confirm_ready);
        assert!(app.loading_pending(), "READY still waits for fresh confirm");

        // A fresh confirm begins the authored block dissolve. Loading remains
        // visible through the cover half, then gameplay is swapped in only at
        // full coverage and revealed through the second half.
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert!(app.loading_pending());
        let transition = app
            .loading_exit_transition
            .expect("confirm starts loading exit dissolve");
        assert_eq!(transition.spec.kind, LevelTransitionKind::BlockDissolve);

        ctx.pad = PadState::NONE;
        ctx.pad_prev = PadState::NONE;
        for _ in 1..transition.switch_frame() {
            app.update(&mut ctx);
            assert!(app.loading_pending());
        }
        app.update(&mut ctx);
        assert!(!app.loading_pending());
        assert!(app.loading_exit_transition.is_some());

        while app.loading_exit_transition.is_some() {
            app.update(&mut ctx);
        }
    }

    #[test]
    fn menu_seeds_focus_to_first_control() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        // The first idle update resolves focus to the top-left-most
        // focusable control: the "Play" button at slice index 1.
        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);
    }

    #[test]
    fn ui_sfx_event_uses_matching_node_cue_range() {
        static CUES: &[LevelUiSfxCueRecord] = &[
            LevelUiSfxCueRecord {
                sample: 0,
                event: LevelUiSfxEvent::Focus,
                volume_percent: 50,
                pitch_q12: 4096,
                flags: 0,
            },
            LevelUiSfxCueRecord {
                sample: 0,
                event: LevelUiSfxEvent::Activate,
                volume_percent: 80,
                pitch_q12: 4096,
                flags: 0,
            },
        ];
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            CUES,
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let node = LevelUiNodeRecord {
            sfx_first: 0,
            sfx_count: 2,
            ..button(0, 0, LevelUiAction::Back)
        };

        app.play_node_sfx_event(node, LevelUiSfxEvent::SliderNudge);
        assert_eq!(app.ui_sfx_cursor, 0, "unmatched events do not advance");

        app.play_node_sfx_event(node, LevelUiSfxEvent::Activate);
        assert_eq!(app.ui_sfx_cursor, 1);
    }

    #[test]
    fn dpad_moves_focus_between_buttons() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx); // seed focus to index 1
        assert_eq!(app.cursor.menu_focus, 1);

        // Down moves to the lower button (index 2).
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 2);

        // Up moves back to the top button.
        press(&mut ctx, button::UP);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);

        // Up again has nowhere to go: focus stays put.
        press(&mut ctx, button::UP);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);
    }

    #[test]
    fn shoulder_buttons_move_focus_across_menu_tabs() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &HORIZONTAL_MENU_FLOW,
            HORIZONTAL_MENU_SCENES,
            HORIZONTAL_MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);

        press(&mut ctx, button::R1);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 2);

        press(&mut ctx, button::L1);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);
    }

    #[test]
    fn tagged_shoulder_tabs_activate_immediately_and_wrap() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &TAGGED_TAB_FLOW,
            TAGGED_TAB_SCENES,
            TAGGED_TAB_NODES,
            &[],
            &[],
            &[],
            &[],
            UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx);
        assert_eq!(
            app.cursor.menu_focus, 3,
            "a fresh tab scene must focus its authored selected category"
        );

        // Focus is inside the submenu, so the selected tag anchors shoulder
        // navigation at the middle tab rather than stealing submenu focus.
        app.cursor.menu_focus = 1;
        press(&mut ctx, button::R1);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 4);
        assert_eq!(app.gameplay.last_game_action, 12);

        press(&mut ctx, button::R1);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 2);
        assert_eq!(app.gameplay.last_game_action, 10);

        press(&mut ctx, button::L1);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 4);
        assert_eq!(app.gameplay.last_game_action, 12);
        assert_eq!(app.gameplay.game_actions, 3);
    }

    #[test]
    fn cross_held_from_boot_does_not_auto_enter_gameplay() {
        // Regression: editor embedded Play captures input from frame 1, so a
        // button still held as Play starts must NOT count as a fresh press on
        // the menu's first frame (which would activate PLAY and skip straight
        // into gameplay). `run_with_flow` seeds pad == pad_prev from the initial
        // poll; model that here by seeding both to CROSS held.
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        // Held from boot: pressed this frame AND last frame (the seeded state).
        ctx.pad.buttons = ButtonState::from_bits(button::CROSS);
        ctx.pad_prev.buttons = ButtonState::from_bits(button::CROSS);
        app.init(&mut ctx);

        app.update(&mut ctx);
        assert_eq!(
            app.gameplay.inits, 0,
            "a CROSS held from boot must not activate the menu and enter gameplay"
        );

        // Releasing then pressing CROSS is a real press and DOES activate.
        ctx.pad_prev = ctx.pad; // still held -> becomes prev
        ctx.pad.buttons = ButtonState::NONE; // release
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 0, "release alone does nothing");
        press(&mut ctx, button::CROSS); // fresh press
        app.update(&mut ctx);
        assert!(app.loading_pending());
        assert_eq!(
            app.gameplay.inits, 0,
            "activation shows loading before gameplay init"
        );
        complete_loading(&mut app, &mut ctx);
        assert_eq!(
            app.gameplay.inits, 1,
            "a genuine CROSS press after release activates Play"
        );
    }

    #[test]
    fn cross_on_start_button_enters_gameplay() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        assert_eq!(app.gameplay.inits, 0);

        // CROSS on a fresh menu seeds focus to the "Play"
        // (StartGameplay) button and activates it in the same tick.
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert!(app.loading_pending(), "CROSS on Play enters loading");
        assert_eq!(app.gameplay.inits, 0, "loading defers gameplay init");

        complete_loading(&mut app, &mut ctx);
        assert_eq!(app.gameplay.inits, 1, "CROSS on Play starts gameplay once");

        // Now in gameplay: updates forward to the scene, no re-init.
        ctx.pad_prev = ctx.pad;
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 1);
    }

    #[test]
    fn start_gameplay_republishes_current_front_end_options() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        assert_eq!(app.gameplay.last_option_value, 4);

        app.option_values[0] = 6;
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert!(app.loading_pending(), "Play shows loading before init");
        assert_eq!(app.gameplay.inits, 0);

        complete_loading(&mut app, &mut ctx);

        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.option_applications, 2);
        assert_eq!(
            app.gameplay.last_option_value, 6,
            "gameplay entry publishes the front-end option value active at Play"
        );
    }

    #[test]
    fn goto_scene_then_circle_returns() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx); // seed focus on the title scene

        // Move down to the "Options" button and activate it.
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 2);
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        // Cursor now sits on the options UI state (flow index 1) with the
        // title state remembered for Back, and focus reset.
        assert_eq!(app.cursor.current, 1);
        assert_eq!(app.cursor.return_to, Some(0));
        assert_eq!(app.cursor.menu_focus, MENU_FOCUS_NONE);

        // An idle tick seeds focus to the options scene's only button.
        // menu_focus is a pool index, so the Back button at pool index 4
        // (the options block is [3..5): canvas at 3, Back at 4) is 4.
        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.menu_focus, 4);

        // CIRCLE pops back to the title state and clears the return slot.
        press(&mut ctx, button::CIRCLE);
        app.update(&mut ctx);
        assert_eq!(app.cursor.current, 0);
        assert_eq!(app.cursor.return_to, None);

        // Gameplay was never entered along the way.
        assert_eq!(app.gameplay.inits, 0);
    }

    #[test]
    fn back_button_via_cross_pops_to_return_state() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MENU_FLOW,
            MENU_SCENES,
            MENU_NODES,
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx);

        // Title -> Options.
        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert_eq!(app.cursor.current, 1);

        // The options scene's focused control is a Back button: CROSS on
        // it pops, same as CIRCLE. The activating update also seeds focus
        // first, so a single press is enough.
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert_eq!(app.cursor.current, 0);
        assert_eq!(app.cursor.return_to, None);
    }

    #[test]
    fn gameplay_only_flow_has_nothing_to_navigate() {
        // A gameplay-only flow never enters a UI arm, so d-pad presses do
        // not touch menu_focus and updates forward straight to gameplay.
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &GAMEPLAY_ONLY,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        complete_loading(&mut app, &mut ctx);

        press(&mut ctx, button::DOWN);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, MENU_FOCUS_NONE);
        assert_eq!(app.gameplay.updates, 1);
    }

    // --- Option store: slider scrub + SetOption ---------------------------

    /// Read the live value of option `option_id` out of an app's store,
    /// through the same resolver the runtime render/input paths use.
    fn value_of(app: &GameApp<'_, CountingScene>, option_id: u16) -> i32 {
        resolve_option_value(app.options, &app.option_values, app.option_len, option_id)
    }

    /// Build a focusable slider at `(x, y)` bound to option id `option`,
    /// parented to the canvas at slice index 0.
    const fn slider(x: i16, y: i16, option: u16) -> LevelUiNodeRecord {
        LevelUiNodeRecord {
            parent: Some(psx_level::UiNodeIndex::new(0)),
            kind: LevelUiNodeKind::Slider,
            x,
            y,
            width: 96,
            height: 8,
            color: [11, 12, 13],
            background: [21, 22, 23],
            accent: [31, 32, 33],
            color_paint: UI_PAINT_NONE,
            background_paint: UI_PAINT_NONE,
            accent_paint: UI_PAINT_NONE,
            value: LevelUiValueBinding::ConstantQ12(0),
            max: LevelUiValueBinding::ConstantQ12(1),
            texture_asset: AssetId(u16::MAX),
            image_effect: LevelUiImageEffect::None,
            text: "",
            tag: "",
            action: LevelUiAction::Back,
            option,
            rotation_degrees: 0,
            flags: 0,
            sfx_first: UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: 256,
            letter_spacing: 0,
        }
    }

    // One option, id 1: range [0, 10], step 2, default 4.
    const OPT_ID: u16 = 1;
    static OPTIONS: &[LevelOptionDef] = &[LevelOptionDef {
        id: OPT_ID,
        min: 0,
        max: 10,
        step: 2,
        default: 4,
    }];

    // Scene 1: a single slider bound to option 1, at pool [0..2).
    // Scene 2: a single SetOption(+5) button bound to option 1, at [2..4).
    static OPT_NODES: &[LevelUiNodeRecord] = &[
        CANVAS,
        slider(100, 100, OPT_ID),
        CANVAS,
        button(
            100,
            100,
            LevelUiAction::SetOption {
                option: OPT_ID,
                delta: 5,
            },
        ),
    ];
    static OPT_SCENES: &[LevelUiScene] = &[
        LevelUiScene {
            id: 1,
            name: "slider",
            node_first: 0,
            node_count: 2,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        },
        LevelUiScene {
            id: 2,
            name: "setoption",
            node_first: 2,
            node_count: 2,
            focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
        },
    ];
    static OPT_FLOW_SLIDER: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 1 }],
        scene_states: &[],
        entry: 0,
    };
    static OPT_FLOW_BUTTON: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 2 }],
        scene_states: &[],
        entry: 0,
    };

    const VOL_ID: u16 = 9;
    static MUSIC_OPTIONS: &[LevelOptionDef] = &[LevelOptionDef {
        id: VOL_ID,
        min: 0,
        max: 100,
        step: 5,
        default: 25,
    }];
    static MUSIC_NODES: &[LevelUiNodeRecord] = &[
        CANVAS,
        LevelUiNodeRecord {
            parent: Some(psx_level::UiNodeIndex::new(0)),
            kind: LevelUiNodeKind::Music,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            color: [0, 0, 0],
            background: [0, 0, 0],
            accent: [0, 0, 0],
            color_paint: UI_PAINT_NONE,
            background_paint: UI_PAINT_NONE,
            accent_paint: UI_PAINT_NONE,
            value: LevelUiValueBinding::Option(VOL_ID),
            max: LevelUiValueBinding::ConstantQ12(0),
            texture_asset: AssetId(u16::MAX),
            image_effect: LevelUiImageEffect::None,
            text: "",
            tag: "",
            action: LevelUiAction::Back,
            option: 2,
            rotation_degrees: 0,
            flags: psx_level::ui_node_flags::MUSIC_LOOP,
            sfx_first: UI_SFX_NONE,
            sfx_count: 0,
            font: 0,
            font_scale: 256,
            letter_spacing: 0,
        },
        slider(100, 100, VOL_ID),
    ];
    static MUSIC_SCENES: &[LevelUiScene] = &[LevelUiScene {
        id: 9,
        name: "music",
        node_first: 0,
        node_count: 3,
        focus_style: psx_level::LevelUiFocusStyle::DEFAULT,
    }];
    static MUSIC_FLOW: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 9 }],
        scene_states: &[],
        entry: 0,
    };

    #[test]
    fn menu_cdda_playback_mode_enables_cdda_and_auto_pause() {
        // CD-DA enable is required to play Red Book audio at all; auto-pause
        // makes the drive halt itself at the track boundary so the loop poll can
        // detect end-of-track and re-play without seeking the laser mid-song.
        assert_eq!(
            CDDA_PLAYBACK_MODE,
            psx_io::cdrom::MODE_CDDA | psx_io::cdrom::MODE_AUTO_PAUSE
        );
    }

    #[test]
    fn option_store_seeds_from_default() {
        // The value store seeds from each option's default at construction,
        // so a slider reads its default before any input.
        let mut scene = CountingScene::default();
        let app = GameApp::new(
            &OPT_FLOW_SLIDER,
            OPT_SCENES,
            OPT_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        assert_eq!(value_of(&app, OPT_ID), 4);
        // An unbound / unknown id resolves to zero, never panics.
        assert_eq!(value_of(&app, UI_OPTION_NONE), 0);
        assert_eq!(value_of(&app, 999), 0);
    }

    #[test]
    fn slider_left_right_scrubs_bound_option_clamped() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &OPT_FLOW_SLIDER,
            OPT_SCENES,
            OPT_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        idle_tick(&mut app, &mut ctx); // seed focus onto the slider
        assert_eq!(app.cursor.menu_focus, 1, "focus seeds onto the slider");
        assert_eq!(value_of(&app, OPT_ID), 4);

        // RIGHT nudges by +step (2), clamped at max (10).
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 6);
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 8);
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 10);
        // Already at max: another RIGHT clamps, value holds.
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 10);

        // Focus never left the slider while scrubbing.
        assert_eq!(app.cursor.menu_focus, 1);

        // LEFT nudges by -step, clamped at min (0).
        for expected in [8, 6, 4, 2, 0] {
            press(&mut ctx, button::LEFT);
            app.update(&mut ctx);
            assert_eq!(value_of(&app, OPT_ID), expected);
        }
        press(&mut ctx, button::LEFT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 0, "LEFT at min clamps");
    }

    #[test]
    fn front_end_option_edits_apply_options_for_preview() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &OPT_FLOW_SLIDER,
            OPT_SCENES,
            OPT_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(
            app.gameplay.option_applications, 1,
            "UI entry applies defaults so menu previews start from the live value"
        );
        assert_eq!(app.gameplay.last_option_value, 4);

        idle_tick(&mut app, &mut ctx); // seed focus onto the slider
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 6);
        assert_eq!(app.gameplay.option_applications, 2);
        assert_eq!(
            app.gameplay.last_option_value, 6,
            "scrubbing a front-end slider publishes the new value immediately"
        );

        // Clamp at max and do not republish when another press leaves the value
        // unchanged.
        for expected in [8, 10] {
            press(&mut ctx, button::RIGHT);
            app.update(&mut ctx);
            assert_eq!(value_of(&app, OPT_ID), expected);
        }
        assert_eq!(app.gameplay.option_applications, 4);
        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 10);
        assert_eq!(app.gameplay.option_applications, 4);
    }

    #[test]
    fn option_bound_music_volume_updates_on_slider_scrub() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &MUSIC_FLOW,
            MUSIC_SCENES,
            MUSIC_NODES,
            &[],
            MUSIC_OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(app.scene_music_cue(9).track, 2);
        assert_eq!(app.scene_music_cue(9).volume_percent, 25);

        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.menu_focus, 2);
        assert_eq!(app.cdda.requested.volume_percent, 25);

        press(&mut ctx, button::RIGHT);
        app.update(&mut ctx);

        assert_eq!(value_of(&app, VOL_ID), 30);
        assert_eq!(app.cdda.requested.track, 2);
        assert_eq!(app.cdda.requested.volume_percent, 30);
        assert!(app.cdda.requested.loop_track);
    }

    #[test]
    fn set_option_button_adjusts_and_clamps() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &OPT_FLOW_BUTTON,
            OPT_SCENES,
            OPT_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        // CROSS on the focused SetOption(+5) button: 4 -> 9.
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert_eq!(app.cursor.menu_focus, 3, "focus seeded onto the button");
        assert_eq!(value_of(&app, OPT_ID), 9);

        // Again: 9 + 5 = 14, clamped to max (10).
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert_eq!(value_of(&app, OPT_ID), 10);
    }

    #[test]
    fn unbound_and_unknown_option_adjust_is_noop() {
        // A slider bound to UI_OPTION_NONE and a SetOption to an unknown id
        // must not panic or write anywhere in the store.
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(
            &OPT_FLOW_SLIDER,
            OPT_SCENES,
            OPT_NODES,
            &[],
            OPTIONS,
            &[],
            &[],
            psx_level::UI_SCENE_NONE,
            &mut scene,
        );
        app.adjust_option(UI_OPTION_NONE, 3);
        app.adjust_option(12345, 3);
        assert_eq!(value_of(&app, OPT_ID), 4, "store untouched by stray ids");
    }
}
