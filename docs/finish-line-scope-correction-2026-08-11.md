# Finish-line scope correction (owner, 2026-08-11)

Authoritative. Where this disagrees with
`docs/finish-line-plan-2026-08-11.md` or the handoff, this document wins.
The plan's structure (work packages, gates, evidence levels) is unchanged;
what changed is the demo-disc default, the explicit deferrals, and the
definition of "done" for the editor and the engine demonstration.

## 1. Demo disc: Quake shareware is DEFAULT, not opt-in

- Quake 1.06 shareware Episode 1 ships in the normal PSoXide demo disc.
- `make disc` and `make disc-only` must produce a disc carrying a visible
  QUAKE SHAREWARE menu entry and the final verified Quake payload.
- The default headless, provenance, and release checks must FAIL when the
  Quake payload is missing, stale, wrongly pinned, or unable to chain-load.
- The `QUAKE=1` opt-in requirement is removed. Shareware is the default lane.
- Half-Life remains the opt-in variant. `HL=1` now means the normal default
  disc (Quake included) plus Half-Life, unless MEASURED capacity makes that
  impossible. If it does not fit, report the measurement and ask; never
  silently drop Quake.
- A future registered-Quake edition may become opt-in later. It is entirely
  out of scope now: do not implement or package registered content.
- Publication, upload, and push still require separate authorization. The
  local default build and its release recipes must nonetheless be
  Quake-ready.

## 2. Explicitly deferred (NOT finish-line requirements)

- Quake save, continue, or memory-card support. Episode 1 only needs to be
  playable start to finish in one continuous session.
- Migration or conversion of existing PSoXide grid projects, including
  Cortex. Only NEW BSP projects must use BSP spatial authority correctly.
- Advanced TrenchBroom features: full CSG union and subtract, prefabs,
  layers, configurable entity-definition systems.
- A dedicated Quake soundtrack. Reuse suitable existing demo-disc tracks
  where practical.
- Pushes, tags, public releases, and other release administration.
- Agent-performed original-hardware acceptance. The owner burns and tests
  the final disc (`docs/h1-owner-hardware-handoff.md`).

## 3. Editor: the required workflow, and nothing beyond it

The editor authors the owner's souls-like PSoXide game. It is not a Quake
map editor. "Done" is exactly this loop:

1. create a new simple BSP project;
2. build rooms, courtyards, floors, walls, openings, basic elevation
   changes;
3. use the 3D and orthographic views;
4. select, move, resize, clip, and texture brushes;
5. place and move gameplay entities;
6. save, undo and redo, cook, rebuild, press Play;
7. spawn the existing souls player and enemies into the authored level;
8. walk through and fight inside the resulting level.

Do not delay completion for editor features beyond this list.

## 4. Engine: one composed BSP tech demonstration is a finish-line gate

The product is the PSoXide souls-like technology demonstration, not an
isolated BSP editor. One composed level must prove all of the following
TOGETHER: textured BSP world geometry; functional lighting; skeletal player
and enemy animation; character movement and collision; weapon attachments;
authored hit and hurt capsules; basic souls combat and enemy death; one
checkpoint and respawn; one door or mover; one trigger; one liquid or
environmental hazard; entity visibility and PVS behaviour; and editor
Cook/Rebuild/Play integration.

Current coverage: the tracked `editor/projects/souls-bsp-vertical-slice`
authored through the production editor commands and gated by
`make editor-souls-bsp-check` satisfies this list. The mapping from each
required element to its pinned evidence is:

| Required element | Evidence in the composed slice |
|---|---|
| Textured BSP world geometry | cooked PXBSP with authored materials; drift-diffed against the tracked project |
| Functional lighting | two authored point lights in the cooked package |
| Skeletal player and enemy animation | player and Mantis with animator clips, driven from the retained pose |
| Movement and collision | route ticks with wall and door contact; body hulls derived from the authored characters |
| Weapon attachments | counter `player weapon attachments` = 2, one per life across two lives |
| Authored hit and hurt capsules | measured attack windows resolving 4 accepted hits with 44 duplicate rejections |
| Souls combat and enemy death | counters: stagger 1, enemy death 1, hits taken 4 |
| Checkpoint and respawn | checkpoint activation 1, player death 1, post-respawn position gauge at the checkpoint |
| Door or mover | logic door activation 1, plus the negative tape proving 0 without the trigger |
| Trigger | the TriggerVolume to Checkpoint chain that produces that activation |
| Liquid or environmental hazard | 6 lava damage events leading to the death |
| PVS behaviour | 2911 suppressions on the canonical tape, 812 on the negative tape |
| Cook/Rebuild/Play integration | the gate re-authors through production commands, cooks, builds a real MIPS guest and disc, and replays twice |

## 5. Quake: the outcome

Complete shareware Episode 1 single-player from Start through the normal
and secret routes, Chthon, intermission, and the return/end state. Every
authored shareware monster, weapon, item, trigger, mover, hazard, and
environmental system those routes require must work. Save support is not
required. Registered-only content stays excluded. The performance target is
sustained 30 fps with the complete workload; emulator telemetry and
performance captures are required, but only the owner's original-PlayStation
test can settle the 30 fps claim and hardware acceptance.

## 6. Final default demo-disc gate

The normal default build must: carry the exact final PSoXide pin; carry the
exact final Quake-PSX shareware build; show QUAKE SHAREWARE in the ordinary
menu with no build-time opt-in; chain-load Quake twice deterministically;
reach the Start map; preserve every other default program and CD-audio
ownership; carry regenerated provenance and payload hashes; and produce a
burn-ready BIN/CUE plus the owner hardware checklist.
