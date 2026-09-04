# Cortex Ignition: menu music replacement and combat music (plan)

Date: 2026-09-04. Written for whoever implements it; nothing here is done yet.
Project: `editor/projects/cortex-ignition-tech-demo-0.5` (the disc entry since
psx-demo-disc 9f76941). Keep 0.4 untouched.

## Inputs

| File | Format | Purpose |
|---|---|---|
| `~/Downloads/Menu2.wav` | stereo, 16-bit, 44.1 kHz, 59.1 s | replaces the Suno menu track `assets/audio/cdda/CRT Lab Loop.wav` |
| `~/Downloads/Cortex Boss.wav` | stereo, 16-bit, 44.1 kHz, 77.1 s | the combat track. Supersedes `assets/audio/cdda/Cortex Ignition Boss Theme.wav` (1 Sep), which is in the project but referenced by nothing |

Both are already Red Book shaped, so the cook's WAV to CD-DA conversion
(`editor/crates/psxed-project/src/playtest/manifest.rs::write_cdda_tracks`)
needs no preprocessing. Keep filenames ASCII and without spaces to spare
the cue tooling: `menu2.wav`, `cortex_boss.wav`.

## How music works today (facts, not guesses)

- Music is a UI node: `UiNodeKind::Music { wav_path, volume, volume_option,
  playback_speed_q12, loop_track }` (`editor/crates/psxed-project/src/ui_types.rs`
  around line 1444). The cook turns each distinct WAV into a CD-DA track
  number (`cook_ui.rs::cook_cdda_track_number`) and writes `trackNN.cdda`
  plus `cdda_tracks.txt` for mkisopsx.
- Three Music nodes exist, all "Menu Music" on `CRT Lab Loop.wav`: Main
  Menu (volume 76), Settings (25), Credits (76). The HUD scene has none, so
  gameplay is silent today. Every node carries `volume_option: Some((3))`,
  the music-volume slider.
- Playback lives in `engine/crates/psx-engine/src/game_app.rs`
  (`CddaPlayer`, lines ~330-455). Each tick GameApp derives one `MusicCue
  { track, volume_percent, loop_track }` from the current UI scene's first
  Music node (`scene_music_cue`, ~line 1566) and calls
  `self.cdda.request(cue, tick)` (~line 2211), gated on
  `front_end_assets_ready()` because one laser cannot stream a UI image and
  play CD-DA at once. A track change runs the SetMode / Demute / Play
  sequence over a few ticks (a real seek on silicon); `loop_track` polls
  GetStat and restarts the track when the drive reports stopped.
- Volume is NOT a drive operation. `cdda_set_volume` writes the SPU CD
  input gain registers (`psx_spu::set_cd_volume`, `CD_VOL_LEFT/RIGHT` at
  0x1F801DB0/2, `CdVolume::linear(num, den)`). It is a two-register write,
  free to do every tick. So fading CD-DA in and out is entirely possible on
  PS1; what is impossible is a crossfade, since only one track plays at a
  time, and a track switch always costs a seek.
- Cortex 0.5 is one room (`ROOMS` has a single record) and the whole level
  is resident after the loading screen, so gameplay issues no CD reads; the
  only persistent write is the memory card over SIO. CD-DA during gameplay
  is therefore safe on hardware, unlike a streamed level. Verify that claim
  once with `PSOXIDE_TRACE_SECTOR_DROP=1` on a headless gameplay run: no
  reads after the loading screen.
- Enemy AI states: `GameEntityState::{Idle?, Patrol, Aggro, Windup, Attack?,
  Recover}` in `engine/crates/psx-game-runtime/src/entities.rs` (the raw
  numbering at line ~76: 1 Patrol, 2 Aggro, 3 Windup, 5 Recover). The
  playtest reads them through `entities.state(index)` (line ~826). "Aggro"
  is the state an enemy enters when the player is inside `aggro_radius`
  (2335 for Custodians, 2048 for the Heavy Enemy) and it starts reacting.
  There is no "in combat" flag today; it has to be derived.

## Part A: menu music (small)

1. Copy `Menu2.wav` to `assets/audio/cdda/menu2.wav`.
2. Point the three Music nodes at it (Main Menu, Settings, Credits; keep
   their volumes and `volume_option`). Delete `CRT Lab Loop.wav` if nothing
   else references it (grep the project and `docs/`).
3. Cook (`cargo run --release -p psxed-project --bin cook-playtest --
   <project.ron>` from `editor/`), confirm `cdda_tracks.txt` lists the new
   track, then `PSOXIDE_TRACE_CDDA_PLAY=1 frontend launch` on the baked cue
   and confirm the menu requests the new track number.

Do not resample or trim the WAV; the loop point is the file boundary, so if
the ending does not loop cleanly that is an audio edit, not a cook option.

## Part B: combat music

### Behaviour to build

- Gameplay starts silent (as now).
- When combat begins, start `cortex_boss.wav` at volume 0 and ramp to the
  authored volume over about 1 s (60 ticks).
- When combat ends, ramp to 0 over about 2 s, then stop the track and
  release the drive (`release_for_data_reads`), so the next combat starts
  the track from the top. No attempt to resume mid-track: a restart is a
  seek either way, and a fresh intro reads better than a random bar.
- The Heavy Enemy (the boss) uses the same rule as the Custodians. If a
  boss-specific track is wanted later it is one more Music node with a
  different trigger, not a different mechanism.
- Menu volume slider (option 3) still scales the combat track.

### "In combat" definition

`in_combat = any enemy in the current room that is alive and whose state is
not Patrol/Idle` (Aggro, Windup, Attack, Recover all count). Apply
hysteresis so a Custodian flickering between Patrol and Aggro at the edge of
its radius does not pump the music: combat ENDS only after `in_combat` has
been false for 180 consecutive ticks (3 s), or immediately when the player
dies or the scene leaves gameplay. Combat STARTS on the first true tick.
Put the detector in the playtest (`engine/examples/editor-playtest/src/
game_logic_runtime.rs` next to the enemy update loop) as a small struct
`CombatMusicState { engaged: bool, quiet_ticks: u16 }` ticked once per fixed
update.

### Authoring and cook

Add a `trigger` field to `UiNodeKind::Music`: `Always` (default, today's
behaviour) or `Combat`. Serde default keeps every existing project loading.
Author one `Music { wav_path: "assets/audio/cdda/cortex_boss.wav", volume:
80, volume_option: Some((3)), loop_track: true, trigger: Combat }` node in
the HUD scene (UI scene 1, the gameplay scene). Cook: the existing
`LevelUiNodeRecord` has a `flags` field with `MUSIC_LOOP`; add
`MUSIC_TRIGGER_COMBAT` and set it from the trigger. `scene_music_cue` must
then skip Combat-trigger nodes when picking the always-on cue, otherwise the
HUD's node would autoplay at gameplay start. Inspector: one combo box in
`editor/crates/psxed-ui/src/inspector_ui_node.rs` (or wherever the Music
fields are drawn; grep `playback_speed_q12`).

### Engine

1. `CddaPlayer` gains a volume ramp: `target_volume_percent`,
   `current_volume_percent` (exists), `ramp_step_q8` per tick, and a
   `pending_stop: bool`. `update()` moves current toward target each tick,
   writes `cdda_set_volume` only when the integer percent changes, and when
   `pending_stop` is set and current reaches 0 it calls
   `release_for_data_reads`. Starting a track with a fade: request the cue
   with `volume_percent` at the authored value but set current to 0 and
   target to the cue's value before `cdda_route_audio` runs (route must
   apply the current, not the target). Keep the existing `request` semantics
   for menu scenes (instant volume) by passing a `fade_ticks: u16` of 0.
2. `Scene` trait (`engine/crates/psx-engine/src/scene.rs:301`) gains
   `fn music_override(&self) -> Option<MusicRequest> { None }` where
   `MusicRequest { cue: MusicCue, fade_in_ticks: u16, fade_out_ticks: u16 }`.
   In GameApp's tick (line ~2205) the override, when `Some`, wins over
   `scene_music_cue`; when it goes from `Some` to `None` the player fades
   out and stops. The playtest implements it from `CombatMusicState` and
   the HUD scene's Combat-trigger node (GameApp can hand the scene the
   resolved cue for that node once at scene entry so the playtest does not
   parse UI nodes itself).
3. Nothing in psx-spu or the CD driver changes.

### Hardware caveats to respect

- Never start CD-DA while the loading screen still streams. The existing
  `front_end_assets_ready()` gate covers menus; for gameplay the override
  must only be honoured once the world is resident (the same condition that
  ends the loading state). Starting it earlier is the "music dies on real
  hardware" failure the engine already documents.
- A track switch mid-combat (say boss track later) means a seek and up to
  a second of silence on silicon; the emulator's seek is instant, so time
  the design on the console, not headless.
- The loop check restarts the track when GetStat says stopped. With a 77 s
  track and combats of 20 to 40 s that mostly never triggers; it is fine.
- Fading by SPU CD volume is inaudible to the drive; no risk there.

### Verification

Headless, before any burn (`docs/playtest-profiling.md` for the commands):

1. `PSOXIDE_TRACE_CDDA_PLAY=1` prints every Play command with its track:
   expect none until the first Custodian aggro, then track N, then
   nothing until the next combat.
2. Add two counters to the counter log (`emu/crates/frontend/src/cli/
   headless_log.rs`, same pattern as `player_attack_starts_total`):
   `combat_music_engaged` (0/1) and `cdda_volume_percent`. Run the
   whole-level tape (`editor/archive/fixtures/cortex-0.4/whole-level.pxtape`
   still applies to 0.5, same map) and plot: volume ramps 0 to 80 over 60
   ticks at the first aggro, holds, ramps to 0 over 120 ticks starting 180
   ticks after the last enemy leaves Aggro.
3. Confirm no sector reads during gameplay with
   `PSOXIDE_TRACE_SECTOR_DROP=1`.
4. Then the console: burn per the mandated drutil protocol (memory
   `disc-burn-drutil-relative-cue-gotcha`) and listen for the two things the
   emulator cannot show: the seek gap at combat start and whether the
   fade-in hides it.

Frames or logs go to Manny before the disc is re-pinned, as with every
visual or audio change.

### Out of scope, decide later

- Boss-only track on top of the generic combat track.
- Resuming a track at its previous position (needs the drive's position
  report; not worth it for a tech demo).
- Ducking SFX under music, or music under the loading screen.
