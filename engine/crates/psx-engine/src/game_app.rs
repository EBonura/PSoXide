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
//! # Gameplay-only is the default, and it is identical to before
//!
//! [`App::run`][crate::app::App::run] keeps its old signature and
//! wraps the supplied gameplay scene in a [`GameApp`] over
//! [`GAMEPLAY_ONLY`] -- a one-state flow whose only state is
//! [`FlowState::Gameplay`]. In that configuration:
//!
//! - `init` resolves the entry state to `Gameplay` and forwards
//!   straight to `gameplay.init`, reproducing the boot-time init the
//!   old runner did inline.
//! - `update` / `render` only ever match the `Gameplay` arm and
//!   forward to `gameplay.update` / `gameplay.render`.
//!
//! So the gameplay-only path is the old behaviour plus one already-
//! taken `match` branch. The UI arms are dead code for the 13 existing
//! examples.
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

use psx_level::{
    first_focus, next_focus, FlowState, GameFlow, LevelOptionDef, LevelUiAction, LevelUiNodeKind,
    LevelUiNodeRecord, LevelUiScene, LevelUiValueBinding, NavDir, NavRect, UI_OPTION_NONE,
};
use psx_pad::button;

use crate::scene::{Ctx, Scene};
use crate::ui;

/// Upper bound on focusable controls a single UI scene can navigate.
/// Focus gathering writes into a fixed stack array of this size to stay
/// `no_std` / alloc-free; a scene with more focusable nodes than this
/// simply ignores the overflow (still navigable up to the cap). Menus
/// in practice carry a handful of buttons, so the cap is generous.
const MAX_FOCUSABLE_NODES: usize = 64;

/// Sentinel [`FlowCursor::menu_focus`] value meaning "no focus
/// resolved yet". Real focus is a node-slice index, always far below
/// this, so the entry path can tell an uninitialised cursor from a
/// genuine focus on node 0.
const MENU_FOCUS_NONE: u16 = u16::MAX;

/// Upper bound on project options the runtime value store tracks. The
/// store is a fixed `[i32; MAX_OPTIONS]` so the driver stays `no_std` /
/// alloc-free; a project with more options than this keeps the overflow
/// at its cooked default (read-only, never adjusted). Menus tune a
/// handful of options in practice, so the cap is generous.
const MAX_OPTIONS: usize = 32;

/// The implicit single-state flow every plain [`App::run`] call uses.
///
/// One state, [`FlowState::Gameplay`], entered immediately. A
/// [`GameApp`] built over this is behaviourally identical to running
/// the bare gameplay scene under the old runner.
pub const GAMEPLAY_ONLY: GameFlow = GameFlow {
    states: &[FlowState::Gameplay],
    entry: 0,
};

/// `Copy` reduction of the resolved [`FlowState`] for the current
/// cursor position. Dispatch matches on this so it never has to hold
/// a borrow into the flow table while also touching `self.gameplay`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum StateTag {
    /// Hand the tick to the borrowed gameplay scene.
    Gameplay,
    /// Show the UI scene with this [`LevelUiScene::id`].
    UiScene {
        /// Target scene id.
        scene: u16,
    },
}

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
    /// Frame countdown for timed UI states (intro/splash). Skeletal:
    /// not yet decremented.
    intro_timer: u16,
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
    /// Cooked project options. Sliders and `SetOption` actions bind to
    /// these by id; the live value store ([`Self::option_values`]) is
    /// seeded from each option's `default`.
    pub options: &'static [LevelOptionDef],
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
}

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
        options: &'static [LevelOptionDef],
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
            options,
            gameplay,
            cursor: FlowCursor::new(flow.entry),
            option_values,
            option_len,
        }
    }

    /// Adjust the option with id `option_id` by `delta`, clamping the
    /// result to that option's `[min, max]`. No-op for the unbound
    /// sentinel or an unknown id, so a stray binding cannot panic or write
    /// out of range.
    fn adjust_option(&mut self, option_id: u16, delta: i32) {
        if option_id == UI_OPTION_NONE {
            return;
        }
        let Some(index) = self.options[..self.option_len]
            .iter()
            .position(|option| option.id == option_id)
        else {
            return;
        };
        let option = self.options[index];
        let next = self.option_values[index].saturating_add(delta);
        self.option_values[index] = next.clamp(option.min, option.max);
    }

    /// Resolve a flow-state index to its `Copy` [`StateTag`]. An
    /// out-of-range index falls back to `Gameplay` so a malformed flow
    /// degrades to "just run the game" rather than wedging.
    #[inline]
    fn tag_at(&self, index: u16) -> StateTag {
        match self.flow.states.get(index as usize) {
            Some(FlowState::Gameplay) | None => StateTag::Gameplay,
            Some(FlowState::UiScene { scene }) => StateTag::UiScene { scene: *scene },
        }
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
            .position(|state| matches!(state, FlowState::Gameplay))
            .map(|index| index as u16)
    }

    /// Move the cursor onto a gameplay state, running `gameplay.init`
    /// exactly once on the first such transition.
    ///
    /// This is the single funnel for "start the game": it is how the
    /// gameplay-only path reaches init at boot and how a UI state hands
    /// off to gameplay later. Idempotent init keeps a flow that bounces
    /// between menu and gameplay from re-initialising the world.
    fn enter_gameplay(&mut self, index: u16, ctx: &mut Ctx) {
        self.cursor.current = index;
        if !self.cursor.gameplay_inited {
            self.gameplay.init(ctx);
            self.cursor.gameplay_inited = true;
        }
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

    /// Index into [`GameFlow::states`] of the first `UiScene` state
    /// targeting `scene_id`, if any. A button's `GotoScene` names a
    /// scene id; the flow cursor addresses states, so this resolves one
    /// to the other.
    fn ui_state_index_for_scene(&self, scene_id: u16) -> Option<u16> {
        self.flow
            .states
            .iter()
            .position(|state| matches!(state, FlowState::UiScene { scene } if *scene == scene_id))
            .map(|index| index as u16)
    }

    /// Switch the cursor onto UI-scene flow `state_index`, remembering
    /// `return_to` for a later `Back`, and clear the resolved focus so
    /// the new scene re-seeds from [`first_focus`] on its first update.
    fn enter_ui_state(&mut self, state_index: u16, return_to: Option<u16>) {
        self.cursor.current = state_index;
        self.cursor.return_to = return_to;
        self.cursor.menu_focus = MENU_FOCUS_NONE;
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
        let mut rects = [NavRect { x: 0, y: 0, w: 0, h: 0 }; MAX_FOCUSABLE_NODES];
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
        let mut rects = [NavRect { x: 0, y: 0, w: 0, h: 0 }; MAX_FOCUSABLE_NODES];
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
            self.cursor.menu_focus = node_indices[next_slot] as u16;
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
            if let Some(node) = self.nodes.get(node_index) {
                if matches!(node.kind, LevelUiNodeKind::Slider) && node.option != UI_OPTION_NONE {
                    let step = self.option_step(node.option);
                    let delta = if right { step } else { -step };
                    self.adjust_option(node.option, delta);
                    return;
                }
            }
        }
        self.move_focus(first, count, if right { NavDir::Right } else { NavDir::Left });
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
    /// in the value store; `Game` is a no-op until game dispatch lands.
    fn activate_focus(&mut self, first: usize, count: usize, ctx: &mut Ctx) {
        let Some(node_index) = self.resolved_focus(first, count) else {
            return;
        };
        let Some(node) = self.nodes.get(node_index) else {
            return;
        };
        match node.action {
            LevelUiAction::GotoScene { scene } => {
                if let Some(state_index) = self.ui_state_index_for_scene(scene) {
                    let return_to = Some(self.cursor.current);
                    self.enter_ui_state(state_index, return_to);
                }
            }
            LevelUiAction::StartGameplay => {
                if let Some(gameplay_index) = self.first_gameplay_index() {
                    self.enter_gameplay(gameplay_index, ctx);
                }
            }
            LevelUiAction::Back => self.go_back(),
            // Nudge the bound option by the authored delta (clamped). A
            // dynamic-label refresh from the new value is a later step.
            LevelUiAction::SetOption { option, delta } => self.adjust_option(option, delta),
            // TODO(menu-step3): dispatch game-specific actions by id.
            LevelUiAction::Game { .. } => {}
        }
    }

    /// Pop to the remembered `return_to` state, if one is set. The
    /// return target is itself a UI-scene state in this step (gameplay
    /// is reached through `StartGameplay`, never popped into), so this
    /// re-enters it as a UI scene and clears the one-deep return slot.
    fn go_back(&mut self) {
        if let Some(return_to) = self.cursor.return_to {
            self.enter_ui_state(return_to, None);
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

impl<'a, S: Scene> Scene for GameApp<'a, S> {
    fn init(&mut self, ctx: &mut Ctx) {
        // Enter the configured entry state. For GAMEPLAY_ONLY this
        // resolves to Gameplay and forwards straight to gameplay.init,
        // reproducing the boot-time init the old App::run did inline.
        // A UI entry only parks the cursor; gameplay.init is deferred
        // until the flow transitions into a Gameplay state.
        match self.current_tag() {
            StateTag::Gameplay => {
                let index = self.cursor.current;
                self.enter_gameplay(index, ctx);
            }
            StateTag::UiScene { .. } => {
                // Cursor already at the UI entry from FlowCursor::new.
                // TODO(p5): run any per-scene enter hook here.
            }
        }
    }

    fn update(&mut self, ctx: &mut Ctx) {
        match self.current_tag() {
            StateTag::Gameplay => self.gameplay.update(ctx),
            StateTag::UiScene { scene } => {
                // Resolve the scene's block in the shared pool once.
                // Focus geometry walks the whole pool (parents are
                // pool-relative), so the helpers take the range, not a
                // sub-slice.
                let (first, count) = self.scene_node_range(scene);

                // Seed (or repair) focus before reading input, so the
                // first press acts on the default control even on the
                // frame the scene is entered, and so a flow driven
                // headless (update without render) still tracks focus.
                let _ = self.resolved_focus(first, count);

                // D-pad moves focus among the scene's focusable controls.
                // Each press is a discrete step, so use just_pressed; the
                // resolver no-ops when nothing lies that way.
                if ctx.just_pressed(button::UP) {
                    self.move_focus(first, count, NavDir::Up);
                }
                if ctx.just_pressed(button::DOWN) {
                    self.move_focus(first, count, NavDir::Down);
                }
                // LEFT / RIGHT scrub a focused slider's bound option, or
                // move focus horizontally for any other control.
                if ctx.just_pressed(button::LEFT) {
                    self.horizontal_press(first, count, false);
                }
                if ctx.just_pressed(button::RIGHT) {
                    self.horizontal_press(first, count, true);
                }

                // CROSS activates the focused control. CIRCLE is a
                // dedicated back/cancel even when the focused control is
                // not a Back button.
                if ctx.just_pressed(button::CROSS) {
                    self.activate_focus(first, count, ctx);
                } else if ctx.just_pressed(button::CIRCLE) {
                    self.go_back();
                } else if ctx.just_pressed(button::START) {
                    // START keeps its "confirm / jump to gameplay"
                    // shortcut so a title screen advances without the
                    // player hunting for the Start button first.
                    if let Some(gameplay_index) = self.first_gameplay_index() {
                        self.enter_gameplay(gameplay_index, ctx);
                    }
                }
            }
        }
    }

    fn render(&mut self, ctx: &mut Ctx) {
        match self.current_tag() {
            StateTag::Gameplay => self.gameplay.render(ctx),
            StateTag::UiScene { scene } => {
                // Resolve the scene's pool block, then resolve focus, so
                // the highlighted control matches the one input acts on.
                let (first, count) = self.scene_node_range(scene);
                let focused = self.resolved_focus(first, count);
                // Copy the node pool + option store out of `self` first so
                // the resolver closures borrow only these Copy locals, not
                // `self` (draw_scene already borrows `self.nodes`).
                let nodes = self.nodes;
                let options = self.options;
                let option_values = self.option_values;
                let option_len = self.option_len;
                // TODO(p5): thread real texture / value resolvers through
                // run_with_flow so image nodes and data-bound bars draw.
                // Stub resolvers skip images (None) and report zero for
                // every binding, so rects and labels still paint.
                let mut textures = |_asset| None;
                let value = |_binding: LevelUiValueBinding| 0;
                // Slider fill reads the live option value by id from the
                // copied store, through the same resolver the input path
                // uses so the knob position matches what scrubbing changed.
                let option_value =
                    |option_id: u16| resolve_option_value(options, &option_values, option_len, option_id);
                ui::draw_scene(
                    nodes,
                    first,
                    count,
                    None,
                    focused,
                    &mut textures,
                    &value,
                    options,
                    &option_value,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frames::{SimTick, VideoHz, VisualFrame};
    use psx_gpu::framebuf::FrameBuffer;
    use psx_pad::{ButtonState, PadState};

    /// Gameplay scene that records how many times each hook ran and in
    /// what order, so tests can assert the gameplay-only path matches
    /// the old runner's one-init-then-loop shape.
    #[derive(Default)]
    struct CountingScene {
        inits: u32,
        updates: u32,
        renders: u32,
    }

    impl Scene for CountingScene {
        fn init(&mut self, _ctx: &mut Ctx) {
            self.inits += 1;
        }
        fn update(&mut self, _ctx: &mut Ctx) {
            self.updates += 1;
        }
        fn render(&mut self, _ctx: &mut Ctx) {
            self.renders += 1;
        }
    }

    fn test_ctx() -> Ctx {
        Ctx {
            sim_tick: SimTick::ZERO,
            visual_frame: VisualFrame::ZERO,
            video_hz: VideoHz::NTSC,
            pad: PadState::NONE,
            pad_prev: PadState::NONE,
            fb: FrameBuffer::new(320, 240),
        }
    }

    #[test]
    fn gameplay_only_inits_once_then_forwards() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &[], &mut scene);
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        app.update(&mut ctx);
        app.render(&mut ctx);
        app.update(&mut ctx);
        app.render(&mut ctx);

        assert_eq!(app.gameplay.inits, 1, "gameplay.init runs exactly once");
        assert_eq!(app.gameplay.updates, 2, "each update forwards to gameplay");
        assert_eq!(app.gameplay.renders, 2, "each render forwards to gameplay");
    }

    #[test]
    fn gameplay_entry_initialises_at_boot() {
        // Mirrors the old App::run shape: init is paid before the first
        // update tick, not lazily on first update.
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &[], &mut scene);
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 0);
    }

    #[test]
    fn ui_entry_defers_gameplay_init_until_start() {
        static SCENES: &[LevelUiScene] = &[LevelUiScene {
            id: 7,
            name: "title",
            node_first: 0,
            node_count: 0,
        }];
        static FLOW: GameFlow = GameFlow {
            states: &[FlowState::UiScene { scene: 7 }, FlowState::Gameplay],
            entry: 0,
        };

        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&FLOW, SCENES, &[], &[], &mut scene);
        let mut ctx = test_ctx();

        app.init(&mut ctx);
        assert_eq!(app.gameplay.inits, 0, "UI entry does not init gameplay");

        // No START yet: still on the UI state, gameplay untouched.
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 0);
        assert_eq!(app.gameplay.updates, 0);

        // Press START: transition into gameplay, init runs once here.
        ctx.pad_prev = PadState::NONE;
        ctx.pad.buttons = ButtonState::from_bits(button::START);
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1, "transition inits gameplay once");

        // Now resolved to Gameplay: updates forward, no re-init.
        ctx.pad_prev = ctx.pad;
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 1);
    }

    #[test]
    fn unknown_scene_id_yields_empty_node_range() {
        let mut scene = CountingScene::default();
        let app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &[], &mut scene);
        assert_eq!(app.scene_node_range(999), (0, 0));
    }

    use psx_level::{AssetId, LevelUiNodeKind, UI_OPTION_NONE};

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
            value: LevelUiValueBinding::ConstantQ12(0),
            max: LevelUiValueBinding::ConstantQ12(1),
            texture_asset: AssetId(u16::MAX),
            text: "",
            tag: "",
            action,
            option: UI_OPTION_NONE,
            flags: 0,
        }
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
        value: LevelUiValueBinding::ConstantQ12(0),
        max: LevelUiValueBinding::ConstantQ12(1),
        texture_asset: AssetId(u16::MAX),
        text: "",
        tag: "",
        action: LevelUiAction::Back,
        option: UI_OPTION_NONE,
        flags: 0,
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
        },
        LevelUiScene {
            id: 2,
            name: "options",
            node_first: 3,
            node_count: 2,
        },
    ];
    static MENU_FLOW: GameFlow = GameFlow {
        states: &[
            FlowState::UiScene { scene: 1 },
            FlowState::UiScene { scene: 2 },
            FlowState::Gameplay,
        ],
        entry: 0,
    };

    /// One UI update tick with no buttons held, so focus seeds without
    /// triggering any action. Render is the GPU draw path and cannot run
    /// in a host unit test (it dereferences raw MMIO), so navigation is
    /// driven and observed entirely through update here.
    fn idle_tick(app: &mut GameApp<'_, CountingScene>, ctx: &mut Ctx) {
        ctx.pad_prev = PadState::NONE;
        ctx.pad = PadState::NONE;
        app.update(ctx);
    }

    #[test]
    fn menu_seeds_focus_to_first_control() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&MENU_FLOW, MENU_SCENES, MENU_NODES, &[], &mut scene);
        let mut ctx = test_ctx();
        app.init(&mut ctx);

        // The first idle update resolves focus to the top-left-most
        // focusable control: the "Play" button at slice index 1.
        idle_tick(&mut app, &mut ctx);
        assert_eq!(app.cursor.menu_focus, 1);
    }

    #[test]
    fn dpad_moves_focus_between_buttons() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&MENU_FLOW, MENU_SCENES, MENU_NODES, &[], &mut scene);
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
    fn cross_on_start_button_enters_gameplay() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&MENU_FLOW, MENU_SCENES, MENU_NODES, &[], &mut scene);
        let mut ctx = test_ctx();
        app.init(&mut ctx);
        assert_eq!(app.gameplay.inits, 0);

        // CROSS on a fresh menu seeds focus to the "Play"
        // (StartGameplay) button and activates it in the same tick.
        press(&mut ctx, button::CROSS);
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1, "CROSS on Play starts gameplay once");

        // Now in gameplay: updates forward to the scene, no re-init.
        ctx.pad_prev = ctx.pad;
        app.update(&mut ctx);
        assert_eq!(app.gameplay.inits, 1);
        assert_eq!(app.gameplay.updates, 1);
    }

    #[test]
    fn goto_scene_then_circle_returns() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&MENU_FLOW, MENU_SCENES, MENU_NODES, &[], &mut scene);
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
        let mut app = GameApp::new(&MENU_FLOW, MENU_SCENES, MENU_NODES, &[], &mut scene);
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
        let mut app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &[], &mut scene);
        let mut ctx = test_ctx();
        app.init(&mut ctx);

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
            value: LevelUiValueBinding::ConstantQ12(0),
            max: LevelUiValueBinding::ConstantQ12(1),
            texture_asset: AssetId(u16::MAX),
            text: "",
            tag: "",
            action: LevelUiAction::Back,
            option,
            flags: 0,
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
        },
        LevelUiScene {
            id: 2,
            name: "setoption",
            node_first: 2,
            node_count: 2,
        },
    ];
    static OPT_FLOW_SLIDER: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 1 }],
        entry: 0,
    };
    static OPT_FLOW_BUTTON: GameFlow = GameFlow {
        states: &[FlowState::UiScene { scene: 2 }],
        entry: 0,
    };

    #[test]
    fn option_store_seeds_from_default() {
        // The value store seeds from each option's default at construction,
        // so a slider reads its default before any input.
        let mut scene = CountingScene::default();
        let app = GameApp::new(&OPT_FLOW_SLIDER, OPT_SCENES, OPT_NODES, OPTIONS, &mut scene);
        assert_eq!(value_of(&app, OPT_ID), 4);
        // An unbound / unknown id resolves to zero, never panics.
        assert_eq!(value_of(&app, UI_OPTION_NONE), 0);
        assert_eq!(value_of(&app, 999), 0);
    }

    #[test]
    fn slider_left_right_scrubs_bound_option_clamped() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&OPT_FLOW_SLIDER, OPT_SCENES, OPT_NODES, OPTIONS, &mut scene);
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
    fn set_option_button_adjusts_and_clamps() {
        let mut scene = CountingScene::default();
        let mut app = GameApp::new(&OPT_FLOW_BUTTON, OPT_SCENES, OPT_NODES, OPTIONS, &mut scene);
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
        let mut app = GameApp::new(&OPT_FLOW_SLIDER, OPT_SCENES, OPT_NODES, OPTIONS, &mut scene);
        app.adjust_option(UI_OPTION_NONE, 3);
        app.adjust_option(12345, 3);
        assert_eq!(value_of(&app, OPT_ID), 4, "store untouched by stray ids");
    }
}
