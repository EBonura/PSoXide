# Multi-gameplay-scene support (design note)

Status: design pending a decision; no code change yet. Written 2026-07 during
the downstream audit.

## The gap, precisely

The engine already has state flow: `GameApp` drives a cooked `GameFlow` of
`FlowState::{SceneState, UiScene, Gameplay}` (`psx-level`) with
`LevelTransitionKind::StartGameplay/StartGameplayTransition` dispatched in
`game_app.rs`. What it cannot do is hold more than one GAMEPLAY scene:
`GameApp` borrows exactly one `&'a mut S: Scene`, so a game with two distinct
gameplay modes (a rhythm stage and an overworld, a puzzle board and a level
select that is itself gameplay) multiplexes them inside one `Scene` impl by
hand.

Evidence from the six downstream projects: gh-psx runs Title, Select,
Countdown, Play and Results as a phase enum inside one scene and documents
why; PSXcel runs seven modes inside its one `Editor` scene. Nobody has hit a
hard wall; the cost is boilerplate `match self.phase` at the top of update
and render plus manual state resets on phase change.

## Constraints

- `no_std`, no heap requirement in the hot path; scenes are big statics or
  stack structs owned by the game.
- The engine loop (fixed 60 Hz sim, paced visuals, pipelined present) must
  not change; this is routing only.
- Existing single-scene games must keep compiling unchanged.

## Candidate shapes

A. Status quo, documented: bless the phase-enum pattern in
   `docs/downstream-projects.md` and move on. Zero engine risk. The
   boilerplate stays per-game.

B. Scene table: `App::run_set(&mut [&mut dyn Scene], start: usize)` plus a
   `Ctx::request_scene(usize)` the loop applies between frames. Dynamic
   dispatch is one indirect call per frame (irrelevant); scenes stay owned
   by the game. Small engine diff; needs a rule for what happens to the
   outgoing scene (keep-alive, no drop). `GameFlow`'s `Gameplay` state gains
   an optional scene index.

C. Scene stack (push/pop for pause screens and sub-modes). Superset of B;
   the audit found no downstream demand for true stacking (pause menus are
   overlays today, and overlays already work), so this is speculative.

## Recommendation

B, and only when a second real consumer appears; gh-psx alone does not
justify it (its phase enum is 30 lines). If cortex_ignition_v1 grows a
second gameplay mode (the concrete trigger), implement B behind the existing
GameFlow so cooked flows can name gameplay scenes.

## Open questions for the owner

- Does cortex_ignition_v1's roadmap add a second gameplay scene (vehicle
  section, minigame)? If yes, B lands with it; if no, keep A.
- Should `UiScene` flows be able to jump to a NAMED gameplay scene from the
  cooked manifest, or is a runtime index enough?
