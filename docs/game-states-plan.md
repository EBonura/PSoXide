# Game States Plan

Status: PLAN, not yet implemented. Sequenced to land AFTER the in-flight 3D/streaming
refactor settles (see "Sequencing and coordination"). Authored 2026-05-31.

## Goal

Introduce an engine-level game-state system that applies to every project, where each
non-gameplay state (intro logo, main menu, loading screen, options) is backed by a 2D
`UiScene` authored and edited individually in the existing editor. The system is a single
unified runtime spine (`GameApp`); a project that only wants gameplay jumps straight into
it via a trivial flow. State transitions, menu buttons, sliders, and focus navigation are
authored as data, not hand-coded per game.

## Key finding: this is mostly generalization, not greenfield

The 2D UI scene system already runs end to end. It is wired for a single HUD, but the
authoring model, the 2D canvas editor, the cook, the runtime types, and a runtime renderer
all exist today. The work is to generalize "one HUD" into "N named, switchable, navigable
scenes that drive a state machine," plus add interactivity (buttons, sliders, navigation).

## Current-state inventory

| Capability | Status | Anchor |
| --- | --- | --- |
| Authoring data model (Canvas/Group/Rect/Label/Image/Bar, anchors, tags, value bindings) | EXISTS | `UiScene`/`UiNodeKind` `editor/crates/psxed-project/src/lib.rs:397` |
| 2D canvas editor (place/move/resize/select, numeric rects, color/text/tag editors, add/delete/reparent) | EXISTS | `WorkspaceView::Ui` `psxed-ui/src/lib.rs:2462`; `draw_ui_scene_preview` `:27560` |
| Cook to runtime | EXISTS, single scene only | `cook_ui_nodes` uses `active_ui_scene()` `editor/crates/psxed-project/src/playtest.rs:1272` |
| Cooked runtime types (24-byte packed, no_std) | EXISTS | `LevelUiNodeRecord` `engine/crates/psx-level/src/lib.rs:1484` |
| Runtime renderer (rect/label/image/bar, anchors, align, wrap) | EXISTS, in the example | `overlay.rs::draw_player_hud` `engine/examples/editor-playtest/src/overlay.rs:16` |
| Engine main loop / Scene contract | EXISTS | `App::run` `engine/crates/psx-engine/src/app.rs:175`; `Scene`/`Ctx` `scene.rs` |
| Internal-state-machine precedent | EXISTS | Pong `winner` flag `engine/examples/game-pong/src/main.rs:242` |

### The four gaps to close

1. Multi-scene is hardwired to `.first()` everywhere: editor (43 call sites), cook, and
   manifest all read `active_ui_scene()` = `ui_scenes.first()`
   (`editor/crates/psxed-project/src/lib.rs:9068`). The project holds `Vec<UiScene>` but only
   index 0 is ever used.
2. The renderer lives in the example, not the engine, and its image path reaches into
   demo10's asset/VRAM machinery (`overlay.rs:167` calls `ensure_texture_uploaded`/`ASSETS`),
   which is exactly the streaming code under refactor.
3. No interactivity: there is no Button or Slider node kind (only Canvas/Group/Rect/Label/
   Image/Bar exist at `LevelUiNodeKind` `psx-level/src/lib.rs:1452`), no menu navigation, no
   actions.
4. The label `tag` is cooked into `LevelUiNodeRecord` but ignored at render
   (`draw_ui_label` `overlay.rs:106` draws the static `node.text`), so dynamic text such as
   "SFX: 7" has no runtime path.

## Target architecture

`GameApp<S>` is the mandatory, unified runtime spine. Every project runs through it. The
gameplay scene (e.g. `Playtest`) is the single project-specific plug-in; everything else is
authored data.

### Runtime spine (psx-engine, new module)

```rust
// Generic over the project's gameplay scene; borrows it (no ownership change vs today).
pub struct GameApp<'a, S: Scene> {
    flow:     &'static GameFlow,            // per-project: states + entry
    scenes:   &'static [LevelUiScene],      // cooked UI scenes, one per non-gameplay state
    nodes:    &'static [LevelUiNodeRecord], // shared node pool, sliced per scene
    gameplay: &'a mut S,                    // e.g. Playtest; its init() is DEFERRED
    cursor:   FlowCursor,                   // current state, return-to, menu focus, timers
    text:     UiTextBindings,               // resolves label `tag` -> dynamic &str
    options:  OptionStore,                  // live option values; sliders read/write
}

pub struct GameFlow { pub states: &'static [FlowState], pub entry: FlowStateId }
pub enum FlowState {
    UiScene { scene: UiSceneId }, // render + navigate this cooked scene
    Gameplay,                     // delegate to GameApp.gameplay
}

pub enum UiAction {
    GotoScene(UiSceneId),
    StartGameplay,
    Back,                                   // pops to the 1-deep return state
    SetOption { option: OptionId, delta: i32 },
    Game(u16),                              // the gameplay scene interprets this
}

impl<'a, S: Scene> Scene for GameApp<'a, S> {
    fn init(&mut self, ctx)   { /* upload UI font; enter(entry). If entry == Gameplay,
                                   this calls gameplay.init(ctx) now (boot), same timing
                                   as today's direct Playtest::init. */ }
    fn update(&mut self, ctx) { match self.current_state() {
                                    Gameplay => self.gameplay.update(ctx),
                                    UiScene  => self.update_ui_state(ctx), } }
    fn render(&mut self, ctx) { match self.current_state() {
                                    Gameplay => self.gameplay.render(ctx),
                                    UiScene  => ui::render(self.active_nodes(), &self.text,
                                                           &self.options, self.cursor.focus), } }
}
```

### Mandatory, but zero migration: `App::run` auto-wraps

All 13 examples enter uniformly through `App::run(config, &mut scene)`
(`engine/crates/psx-engine/src/app.rs:175`). To make `GameApp` the mandatory spine without
touching any of them:

- `App::run(config, &mut scene)` keeps its signature and internally builds a
  `GameApp` that borrows `scene` with the engine-provided trivial flow
  `GAMEPLAY_ONLY = { states: [Gameplay], entry: Gameplay }` and empty UI slices. Behavior is
  identical to today: entry is Gameplay, so `gameplay.init` runs at boot and every frame is
  one match arm to `gameplay.update`/`render`. This is the "only gameplay jumps straight in"
  case, and it falls out of the flow rather than being a special path.
- `App::run_with_flow(config, &FLOW, UI_SCENES, UI_NODES, &mut gameplay)` runs the same
  `GameApp` with real UI states.

One dispatch path, one renderer, one navigation handler. The gameplay-only path costs one
branch per frame and never invokes the UI renderer, so it is effectively free (matters given
the Cpu-bound profile).

The init-timing seam: `App::run` stops calling `scene.init` directly; the wrapper owns when
`gameplay.init` fires (at boot for a Gameplay entry, deferred to the `StartGameplay`
transition for a full flow). Observable behavior for today's examples is unchanged.

### New node kinds: Button and Slider

- `Button { rect, label, align, color, action: UiAction }`. Focusable; activating it fires
  `action`.
- `Slider { rect, option: OptionId, track, fill, knob }`. Focusable; left/right adjusts the
  bound option. A Slider is an interactive `Bar` bound to a project option.

### Options model (substrate for sliders and dynamic labels)

A Slider needs something to bind to, so projects declare named options:

```rust
// project model
pub struct OptionDef {
    pub id: OptionId,
    pub name: String,
    pub kind: OptionKind, // IntRange { min, max, step, default } | Enum { variants } | Bool
}
```

Sliders bind to an `OptionDef`; Labels display one via the existing `tag` field (this is the
generalization that closes gap 4). The runtime `OptionStore` mirrors live values. Persistence
(memory card) is out of scope for v1; options reset on power cycle.

### Navigation with editor/runtime parity

Focus order over focusable nodes (Buttons + Sliders), two layers:

- Implicit spatial order (sort by Y then X) with an "auto-order" action, good for vertical
  menus.
- Explicit per-node `nav { up, down, left, right }` neighbor ids for grids/columns, plus a
  per-scene `default_focus`.

The resolver (given current focus + a direction, return the next node) lives in `psx-level`
(no_std, but compiles host-side like the rest of the engine types). The editor preview and
the PS1 runtime call the same resolver, so authored navigation matches console behavior with
no drift.

## Engine and runtime track (P1 to P5)

Each phase lands independently. This whole track shares hot files with the streaming refactor
(see coordination) and is sequenced after it.

- P1 multi-scene plumbing (project model): give `UiScene` a stable `UiSceneId`; add
  `default_focus`. Additive, `#[serde(default)]`.
- P2 cook all scenes + manifest: change `cook_ui_nodes`
  (`editor/crates/psxed-project/src/playtest.rs:1265`) to iterate every `UiScene` into one
  shared node pool; emit `pub static UI_SCENES: &[LevelUiScene]` ({ id, node_start, node_len })
  alongside the existing `UI_NODES` writer (`playtest/manifest.rs` ~`:914`); emit a `FLOW`
  static. Gameplay-only projects emit empty UI arrays + `{ entry: Gameplay }`. New
  `LevelUiScene`/`GameFlow` types in `psx-level`.
- P3 promote the renderer into `psx-engine::ui`: move `draw_player_hud` + helpers (absolute
  rect, anchors, align, wrap, bar/label) out of `overlay.rs` into the engine, rendering any
  `&[LevelUiNodeRecord]` slice. Give the image path a `fn(AssetId) -> Option<VramSlot>`
  resolver hook so it decouples from the streaming/VRAM system (the one runtime seam).
  demo10's HUD then calls the engine renderer with its own resolver; behavior unchanged.
- P4 state machine: `GameApp<S>`, `FlowCursor`, enter/exit hooks, transition handling,
  `App::run` auto-wrap + `run_with_flow`. Wrap demo10's `Playtest` as the first consumer with
  deferred init on `StartGameplay` (and `enable_analog_port1` there). Loading = present one
  frame, then synchronous `gameplay.init`, then Gameplay.
- P5 interactivity runtime: render Button/Slider; resolve label `tag` against
  `UiTextBindings`; engine focus cursor + highlight; CROSS activates, CIRCLE = Back; bind the
  navigation resolver.

## Editor track (E1 to E5)

This track is host-side only (egui + project model + cook). It does not touch world cook,
streaming, or 3D render, and it can proceed before any PS1 runtime support exists. All of it
lives inside the existing `WorkspaceView::Ui` mode (`psxed-ui/src/lib.rs:2462`).

The correctness pivot: today `active_ui_scene()` serves both "what the editor shows" and
"what the cook emits." Multi-scene splits these. Editor "active" becomes view state
(`active_ui_scene_index` on `EditorWorkspace` `psxed-ui/src/lib.rs:262`); the cook iterates
all scenes (P2). The 43 editor call sites route through the selected index.

- E1 scene browser: add `active_ui_scene_index`; a scene strip in the UI workspace header
  (left dock, gated by `active_workspace == WorkspaceView::Ui` like `:11611`) with New /
  Rename / Duplicate / Delete / reorder. Switching resets UI selection. Reuse the existing
  rename pattern (`renaming: Option<(NodeId, String)>` `:323`). Undo is snapshot-based
  (`UndoStack` `:330`), so each structural op is one snapshot.
- E2 Button/Slider authoring: extend `default_addable_ui_kinds` (`:27491`) and `add_ui_child`
  (`:17010`); add property editors in `draw_ui_inspector` (`:11892`) / `draw_ui_rect_editor`
  (`:24303`) reusing `color_editor` (`:20819`): Button gets label + action selector + target
  dropdown (populated from the scene list); Slider gets an option picker + colors. Render both
  in `draw_ui_scene_preview` (`:27560`) with the existing selected-outline + handles. Add the
  `OptionDef` list editor (project settings).
- E3 navigation authoring: inspector up/down/left/right neighbor pickers + default-focus
  checkbox on focusable nodes; a canvas nav overlay toggle that draws resolved-order arrows
  and lets you drag a node edge to another node to set an explicit neighbor (new
  `UiCanvasDragMode` variant `:1756`, reusing `ui_scene_hit_test` `:27867`). Resolver shared
  from `psx-level`.
- E4 in-editor navigation preview: a "play UI" toggle that simulates the cursor in egui
  (arrow keys move focus via the shared resolver, Enter fires a Button action and switches the
  previewed scene for `GotoScene`, Slider focus + left/right nudges its option). Gives real
  menu feel without a playtest rebuild.
- E5 flow + entry: an entry selector (boot into a UI scene, or straight into Gameplay) in a
  small Flow section; button targets already encode transitions. A visual flow-graph view is a
  later nicety, auto-derivable from button actions.

New `EditorWorkspace` state, all additive: `active_ui_scene_index`, `ui_nav_overlay: bool`,
`ui_preview_active: bool`, `ui_preview_focus: Option<UiNodeId>`, and one new `Interaction`
variant for nav-link wiring.

## Data model and serialization

All project-model additions are additive with `#[serde(default)]` (matching the existing
discipline on `UiNodeKind` fields), so old projects load unchanged.

- `UiScene`: `UiSceneId`, `default_focus: Option<UiNodeId>`.
- `UiNodeKind`: `Button`, `Slider` variants; optional `nav` neighbors on focusable nodes.
- `UiAction` enum.
- `ProjectDocument`: `options: Vec<OptionDef>`, and a per-project flow (`states` + `entry`),
  defaulting to `{ entry: Gameplay }` so simple/new projects are unaffected.

Note: streamtest carries a `ui_scenes` block (`editor/projects/streamtest/project.ron:119`)
and is actively regenerated, so it parses through this model. Additive defaults keep it
loading and round-tripping.

## Runtime manifest

The manifest writer (`editor/crates/psxed-project/src/playtest/manifest.rs`) already emits
`pub static UI_NODES: &[LevelUiNodeRecord]` (placeholder empty at
`engine/examples/editor-playtest/generated/level_manifest.rs:61`). Add `UI_SCENES` (scene
table over the node pool) and `FLOW`. Uniform shape across projects: a gameplay-only project
emits empty `UI_SCENES`/`UI_NODES` and a trivial `FLOW`.

## Loading-state seam

v1: present one loading frame, run `gameplay.init` synchronously, enter Gameplay. Animated or
progress-driven loading depends on the streaming refactor exposing an incremental load API and
is deferred until then. This is one of only two contact points with the streaming work; keep
it thin.

## Sequencing and coordination

Another agent is actively refactoring the 3D/streaming system and running perf-optimization
tests against project streamtest. The two efforts share hot files, so they must be sequenced,
not run together.

Shared hot files:

- `editor/crates/psxed-project/src/playtest.rs` is the collision epicenter: `build_package`
  (`:153`), the streaming/residency cook, `sweep_stream_requests`, and the streaming tests
  (~`:6530`) live here, alongside `cook_ui_nodes` (`:1265`) that P2 rewrites.
- `engine/crates/psx-level/src/lib.rs` holds the room/portal/stream-pack types and the UI
  types together.
- `editor/crates/psxed-project/src/lib.rs` holds the world/room/streaming model and the
  `UiScene` model together.
- `engine/crates/psx-engine/src/app.rs` (the `App::run` change) and
  `engine/examples/editor-playtest/src/main.rs` (wrapping `Playtest`).

Ordering:

1. Let the streaming refactor land and settle first.
2. Then run the engine + cook track (P1 to P5), which shares the files above.
3. The editor-UX slice (E1 to E4) is the only part safe to start early, because it lives in
   `psxed-ui/src/lib.rs`, which the streaming work does not touch. Guardrails if started in
   parallel: keep `psxed-project` model additions strictly additive and in a separate region
   from the room/stream structs; do it on a separate branch or git worktree so it never
   touches the other agent's working tree; do not run heavy cargo/editor/emulator builds during
   the other agent's perf-measurement windows (CPU contention skews its timing). Do not touch
   `playtest.rs`, `psx-level`, `app.rs`, or `editor-playtest/main.rs` until the refactor lands.

Hazards beyond merge conflicts: perf-measurement pollution (the other agent is timing
sensitive), and a shared parse/cook path (model or cook changes rerun through streamtest's
load and regeneration).

## Decisions

Locked:

- `GameApp` is the mandatory unified runtime spine; gameplay-only is the trivial flow.
- `App::run` auto-wraps so the 13 examples and streamtest stay untouched (chosen over making
  every `main` construct a `GameApp` explicitly).
- Per-project, data-driven flow; each named UI scene is a state; gameplay is the one plug-in.
- Button is a first-class node kind; Slider added alongside it.
- Navigation resolver shared in `psx-level` for editor/runtime parity.

Defaulted (overridable):

- Scene identity is a stable `UiSceneId` with the name as a label (renames do not break button
  targets).
- Loading v1 is single-frame present then synchronous init.

Deferred:

- Visual flow-graph editor (auto-derive from button actions later).
- In-game music source (XA interleaved vs SPU-sequenced); menu/intro can use Red Book CD-DA
  since they stream nothing, but in-game cannot while the drive streams geometry.
- Vibration option: no guest rumble API in `psx-pad` today, and nothing rumbles yet.
- Options persistence to memory card.
- Screen-position option needs a small `psx-gpu` runtime setter that re-issues
  `gp1::h_display_range`/`v_display_range` (`sdk/crates/psx-gpu/src/lib.rs:120`,`:126`); audio
  options use `spu::set_cd_volume` (`sdk/crates/psx-spu/src/lib.rs:545`) and per-voice volume.

## Test strategy

- Editor: the headless `ViewportHarness` (`psxed-ui/src/lib.rs:35219`) drives the real editor
  paths. Cover scene create/switch/rename/delete, node add/edit, nav-link wiring, and the
  in-editor preview focus path.
- Cook: golden manifest for a project with multiple scenes, buttons, and sliders.
- Engine: host stubs plus a tiny example exercising `GameApp` dispatch, transitions, and
  navigation; `psx-level` navigation-resolver unit tests (shared by editor and runtime).

## File touch-point index

- `engine/crates/psx-engine/src/app.rs` (P4): `App::run` auto-wrap + `run_with_flow`.
- `engine/crates/psx-engine/src/ui.rs` (P3, new): promoted renderer + texture-resolver hook.
- `engine/crates/psx-engine/src/game_app.rs` (P4, new): `GameApp`, `FlowCursor`, dispatch.
- `engine/crates/psx-level/src/lib.rs` (P1/P2/P5): `LevelUiScene`, `GameFlow`, Button/Slider
  kinds, nav neighbors, nav resolver.
- `editor/crates/psxed-project/src/lib.rs` (E1/E2/P1): `UiSceneId`, Button/Slider, `UiAction`,
  `OptionDef`, flow/entry, `default_focus`, nav fields.
- `editor/crates/psxed-project/src/playtest.rs` (P2): cook all scenes; emit `UI_SCENES`/`FLOW`.
- `editor/crates/psxed-project/src/playtest/manifest.rs` (P2): `UI_SCENES`/`FLOW` writers.
- `editor/crates/psxed-ui/src/lib.rs` (E1 to E5): scene browser, Button/Slider editors, nav
  overlay, in-editor preview, flow/entry UI.
- `engine/examples/editor-playtest/src/overlay.rs` (P3): renderer moves to the engine; this
  file shrinks to a thin call site.
- `engine/examples/editor-playtest/src/main.rs` (P4): wrap `Playtest` in the flow.
