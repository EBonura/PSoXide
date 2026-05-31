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

use psx_level::{FlowState, GameFlow, LevelUiNodeRecord, LevelUiScene, LevelUiValueBinding};
use psx_pad::button;

use crate::scene::{Ctx, Scene};
use crate::ui;

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
    /// Menu focus index for the active UI scene. Skeletal: navigation
    /// does not move it yet.
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
            menu_focus: 0,
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
    /// Borrowed gameplay scene. Not owned, so the caller keeps it.
    pub gameplay: &'a mut S,
    /// Where in the flow we currently are.
    pub cursor: FlowCursor,
}

impl<'a, S: Scene> GameApp<'a, S> {
    /// Build a driver over `flow`, borrowing `gameplay`. The cursor is
    /// positioned at `flow.entry`; nothing runs until [`Scene::init`].
    #[inline]
    pub fn new(
        flow: &'static GameFlow,
        scenes: &'static [LevelUiScene],
        nodes: &'static [LevelUiNodeRecord],
        gameplay: &'a mut S,
    ) -> Self {
        Self {
            flow,
            scenes,
            nodes,
            gameplay,
            cursor: FlowCursor::new(flow.entry),
        }
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

    /// Look up the node slice backing a UI scene id. Returns an empty
    /// slice for an unknown id or an out-of-range pool range, so the
    /// renderer simply draws nothing.
    fn scene_nodes(&self, scene_id: u16) -> &'static [LevelUiNodeRecord] {
        let Some(scene) = self.scenes.iter().find(|scene| scene.id == scene_id) else {
            return &[];
        };
        let first = scene.node_first as usize;
        let end = first.saturating_add(scene.node_count as usize);
        self.nodes.get(first..end).unwrap_or(&[])
    }
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
            StateTag::UiScene { .. } => {
                // TODO(p5): real navigation. For now the only action a
                // UI state handles is "confirm/start", which advances
                // toward the first Gameplay state (and inits gameplay on
                // that transition if it has not run yet). Focus movement,
                // back/cancel, per-node actions, and timed transitions
                // are a later lane.
                if ctx.just_pressed(button::START) {
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
                // Copy the &'static node slice out of self first, so the
                // stub resolvers below capture nothing from self and the
                // borrow checker stays happy.
                let nodes = self.scene_nodes(scene);
                // TODO(p5): thread real texture / value resolvers through
                // run_with_flow so image nodes and data-bound bars draw.
                // Stub resolvers skip images (None) and report zero for
                // every binding, so rects and labels still paint.
                let mut textures = |_asset| None;
                let value = |_binding: LevelUiValueBinding| 0;
                ui::draw_scene(nodes, None, &mut textures, &value);
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
        let mut app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &mut scene);
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
        let mut app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &mut scene);
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
        let mut app = GameApp::new(&FLOW, SCENES, &[], &mut scene);
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
    fn unknown_scene_id_yields_empty_node_slice() {
        let mut scene = CountingScene::default();
        let app = GameApp::new(&GAMEPLAY_ONLY, &[], &[], &mut scene);
        assert!(app.scene_nodes(999).is_empty());
    }
}
