# Handoff, 2026-08-20

Written for whoever picks this up next. Everything below is committed and on
`origin/main`; the working tree is clean and both live projects cook.

The session ran through three connected things: getting hl-psx's model format
into the SDK, finding out where the animation RAM actually goes and building
the tools to control it, and giving the Rust Mantis a moveset.

## Where it stands

| | |
| --- | --- |
| resident assets | **670,900 B of 704,512 B (95%)** |
| headroom | 33,612 B, about 16 pages |
| editor workspace tests | 2045 passing |
| frontend tests (`--features editor`) | 175 passing, 1 ignored |
| `psx-asset` tests | 29 passing |

Resident breakdown: Aletha 452,152, Rust Mantis 149,868, Sword1 Light 67,732,
Sword1 Heavy 1,148.

**95% is the headline problem.** Everything below is either about that number
or about the mantis.

## What shipped

### The SDK now owns hl-psx's model container

`0b8b3fa5` moved hl-psx's HMD8 reader into `sdk/crates/psx-asset/src/hmd8.rs`.
The lift was nearly free: the file's only outside dependencies were
`psx_gte::math`, already an SDK crate, and one game-local vertex cap, now
`DEFAULT_MAX_VERTS` with `load_with_vertex_cap`.

Verified against **232 real cooked chunks** from hl-psx's model pack, every
invariant holding. Those bytes are Half-Life-derived so none are committed; the
test reads them only when `HMD8_FIXTURE_DIR` points at a pack and otherwise
builds a blob by hand. Nothing consumes the module yet, so the guest build is
unaffected.

**Not done: pointing hl-psx at it.** hl-psx pins `psoxide-link` at rev
`9c298d83` with path deps into a local `.psoxide` directory, so it cannot see
this until that pin moves, and moving it pulls in everything since. That is a
deliberate bump, not a drive-by.

### The cook now refuses builds the linker would have refused

`e5cdffee` added `audit_resident_assets` in
`editor/crates/psxed-project/src/playtest/budget.rs`. It sums every
`PersistentGameplay` payload word-padded, which is exactly the sum the manifest
turns into `PERSISTENT_ASSET_PAGE_COUNT`, and budgets in the arena's own 2 KiB
pages so its figure and the guest's compiled constant cannot drift apart.
`write_package` refuses over the cap before touching the filesystem and prints
a breakdown above 90%.

The cap is measured, not guessed: bisecting `PERSISTENT_ASSET_PAGE_COUNT`
against the linker puts the ceiling at **354 pages (724,992 B)**, with 355
overflowing by 368 bytes. The cap sits at 344, ten under, so ordinary code
growth cannot put the linker back in front of the audit. Re-measure the same
way after any large code change.

### Two cook-time levers on clip size

Both default OFF and are per-project settings.

**`animation_error_budget_degrees`** (`16f4e12b`) drops each clip to the lowest
sample rate holding a worst-case rotation error. It is self-selecting: fast
clips blow the budget at the first step down and keep their rate, slow ones give
up most of their frames. It resamples rather than dropping frames because clips
are endpoint-inclusive and the idle's 169 intervals only divide by 13.
**Currently 0.** Worth about 15% and it moves clip duration by up to half an
output frame, which shifts anything deriving motion from clip length.

**`animation_trim_still_percent`** (`7e0678a1`) drops leading and trailing runs
under a percentage of a clip's own peak motion. **Currently 8.** At 8% the cuts
are almost entirely head-side, 2 to 7 frames, tails left alone, so the wind-up
delay goes and the follow-through stays. 15% was too aggressive: it took 17
frames off the back of `gen_vert_combo_attack`, which is recovery, not
stillness. Looping clips are never trimmed, and the cook tells them apart by
asking the data (a loop's last frame repeats its first) rather than a flag.

### The Character is a live type now

`9253ea2f` made `CharacterController::settings` an
`Option<CharacterControllerSettings>`. Placement leaves it `None` so a fresh
controller follows its Character; an override is materialised only when someone
edits a placed controller. Nothing needed migrating, because the field
deserializes through the same wrap-in-Some helper `world_format` uses.

Before this, editing an enemy type reached nothing already in a scene.

### The Rust Mantis

Locomotion (idle, walk, run, strafe pair) from earlier in the session, then
`2df70588` added attack, stagger and death, `a502247b` fixed the death to
actually fall, and `3545fe51` replaced the idle with hand-authored poses.

Current clips, 122,500 B total:

| clip | frames | rate |
| --- | --- | --- |
| rust_mantis_idle (unbound now) | 46 | 15 Hz |
| walk / run | 30 / 25 | 15 Hz |
| strafe left / right | 31 / 31 | 15 Hz |
| mantis_combat_death | 27 | 12 Hz |
| mantis_combat_hit_react | 15 | 12 Hz |
| mantis_combat_light_attack | 31 | 12 Hz |
| mantis_combat_idle (authored) | 42 | 12 Hz |

`tools/mantis_authored_clip.py` writes clips from a hand-written pose table
rather than sampling a model. The idle is four poses on a six-beat loop with
uneven holds, each change landing in one frame so the runtime's linear blend
turns it into a snap. `--probe` cooks a clip whose frames ARE the bone-axis
mapping; use it before editing angles.

## Measurements worth not re-deriving

**hl-psx's animation is not more efficiently encoded than ours.** Its bytes per
frame are 465 against our 520, the same 20-byte record. The 42x gap in pose
bytes per model is 7x more frames per clip multiplied by 6x more clips resident:
it stores a median of **4 frames per clip** and 5.3 clips per model, a third of
its clips being one or two frames.

**Residency is worth 13.8x at the model level and ~13% at the clip level.**
Across hl-psx's 101 maps, holding every model resident is 4,377 KB against 317
KB for the worst single map. Trimming a resident model to only the clips its map
can play, which is what a merged per-map variant buys on top, is worth a median
13% and nothing at all on 8 of 101 maps.

**Two compression ideas are dead.** Lossless static-joint elision saves 1%;
freezing joints that move under 5 degrees saves 2%. Both fail because the
records are composed world-space matrices, so a toe inherits the whole chain's
motion and nothing is ever still. Local-space storage would be sparse but costs
per-joint CPU we do not have.

**Against a 40/30/30 player/enemy/boss split** (shares of 281,805 / 211,354 /
211,354): the player is at 521,032, **185% of its share**. The mantis is at
149,868, 71%. The boss has its whole share untouched. The player is the entire
problem and compression will not fix it.

## Known-wrong, in priority order

1. **The mantis scale went to the placement, not the type.** The placed
   controller carries an override at radius 211 / height 1126 and renders at
   `visual_scale_q8` 282, but the `Rust Mantis Enemy` Character resource is
   still at radius 184 / height 1024. So a newly placed mantis renders at
   baseline and collides small. Now that settings inherit, the scale belongs on
   the Character. This is the first thing to fix.
2. **`6d65ada2` (psx-vram) is on `main` because I pushed from the wrong
   branch.** It was the user's own work-in-progress on
   `vram-layout-disjoint-check`, not mine to publish. Decide whether it stays.
3. **14 ported cortex_v1 materials were flipped to single-sided** (`b482d506`).
   The reasoning was performance (double-sided means `CullMode::None`, and
   clearing it on the mantis measured render 821k to 494k cycles), but
   `FENCE_4B` and the menu overlays were plausibly two-sided on purpose.
4. **The mantis still cannot hurt anything.** It has an attack clip but no
   damage volume; the hitbox belongs on `LeftHand`, not the inherited
   `right_hand_grip` on joint 13.
5. **`emu/crates/frontend/src/editor_preview/tests.rs`** was fixed
   (`fcb03d61`), but note `editor` is a *default* feature, so that module
   failing to compile had silently disabled the whole frontend test binary.
   Worth a periodic check that it still builds.
6. **`--no-default-features` runs 83 tests with 5 failures**, all in
   `ui::menu::tests`, which hardcode 7 menu categories including "Editor". That
   is the wasm/web target, so it wants its own change.

## Next steps

**Get under the ceiling before adding content.** At 95% with 16 pages left,
nothing substantial fits. The measurements point at four places, none of which
is compression:

- The light sword's atlas is **66,076 B**, 9.4% of the whole cap, for one
  weapon's texture, and it sits in the player's bucket. Cheapest single win.
- Aletha's idle is **88,420 B**, 170 frames at 12 Hz, the largest single asset
  in the game, and its first 8 seconds are near-still.
- Aletha carries **30 bound clips** against hl-psx's 5.3 per model, plus 13 more
  that resolve against her skeleton but are bound to no action.
- Per-scene residency is designed but not built, and today it would save 0 KB
  because there is one scene. It starts paying at the 9-map Episode 1 target.

**Then finish the mantis.** Damage volume, then variants 2 and 3 (the no-claw
mesh is registered; it needs `left_hand_grip` tuning and Equipment records),
then the legless crawler. `docs/rust-mantis-variants.md` has the plan and the
per-axis costs.

**Authoring more clips by hand costs frames.** A held frame costs the same as a
moving one, so the authored idle is bigger than the generated one it replaced
(18.1 KB against 15.1). If much more gets authored this way, the obvious win is
a run-length flag on identical frames, which the format does not have.

## Traps that cost time here

- **The preview wraps the last stored frame back to frame 0.** Asking for
  `frame_count - 1` renders frame 0 and reads as a violent pop that is not in the
  animation. The runtime does the same: one-shot actions stop at
  `frame_count - 2`. This produced two wrong diagnoses and one wrong video.
- **Clips are addressed by their index in `resolved_model_animation_clips`**,
  which is not the animation set's `clips:` order and not the cooked filename
  order. Aletha resolves 43 clips, not the 30 that are bound. Getting this wrong
  renders the wrong clip at the wrong length and looks like broken animation.
- **Author at the rate the clip cooks at.** Blender defaults to 24 fps and the
  import bakes at 12, which halves every hold and smears one-frame snaps.
- **The retargeter's rest pose holds both arms straight out to the sides.**
  Small angle deltas on an already-extended limb produce poses that all look
  identical. Use `--probe`.
- **zsh does not word-split unquoted parameters**, so `set -- $cfg` in a loop
  silently passes the whole string as `$1`. This broke three separate camera
  sweeps before it was noticed.
- **A single-run visual A/B is not evidence** when the scene has a live enemy in
  it: its AI diverges between runs and the difference you are looking at may be
  the enemy, not the change.
