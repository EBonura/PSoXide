# Animation RAM: where we are and what hl-psx does differently

Measured, both sides, before proposing anything.

## Where PSoXide is

| | |
| --- | --- |
| resident animation, default project | **483.1 KB** (Aletha 30 clips 438.3, mantis 3 clips 44.7) |
| measured ceiling | **~599 KB** |
| headroom | **~116 KB** |

The ceiling is not an estimate: 438 KB of Aletha plus 198 KB of mantis
overflowed guest `.bss` by exactly 33,136 bytes, which pins it.

Format is **already v3: 20 bytes** per bone per frame, nine 12-bit Q11 rotation
codes in fourteen bytes plus a shifted `i16` translation. That is the same
packed encoding hl-psx ships on silicon, and the cook selects it automatically
whenever a clip's matrix elements fit, which every rigid clip does. Aletha's 26
joints cost 520 bytes a frame, the mantis's 22 cost 440. Every cooked clip is
`StreamedClass::PersistentGameplay`, so **every clip of every character is
resident for the entire game**.

Finishing the mantis alone (strafe pair, two claw attacks, two weapon attacks)
costs about 113 KB against the 116 available. There is no room for a second
variant, let alone a boss.

## Where hl-psx is

It runs the whole Half-Life campaign, 103 maps and 237 transitions, with a
model pool of **55,936 words (223.7 KB)** covering every actor's mesh *and*
animation, audited to a measured worst-case peak of 56,206 words on the
`c1a2b` cook.

Half our animation budget, for an entire game rather than two characters. Three
things get it there, and they are not equally important.

**Map-local clip residency.** "Map-local variants retain only clips reachable
by that map." This is the structural difference and it dwarfs the rest. We hold
every clip forever; it holds what the current map can actually play.

**A tighter affine, which we already match.** `HMD8_AFFINE_BYTES = 20`, and our
v3 record is also 20 with the same Q11 packing. No gap here.

**One bone-local mesh.** Vertices are stored once, grouped into contiguous
bone ranges so each range pays one matrix load, rather than duplicating skinned
vertices per pose. We already do the equivalent.

It also has something we don't: `cargo run -- audit` re-measures the peak
across every map and transition and **fails the build** if a growing model
would hide an actor on hardware. We found our ceiling by hitting a linker
error, which is the same information arriving too late and less precisely.

## The plan, ranked by payoff

**1. Residency (structural, biggest win, but read the split below).** Stop
marking model clips `PersistentGameplay` so a scene holds only what its
characters can reach. Correcting an earlier claim in this doc: this is NOT
merely reclassification. `StreamedClass` has four values, `None`, `UiImage`,
`Gameplay` and `PersistentGameplay`
(`editor/crates/psxed-project/src/playtest/schema.rs:158`), and the finest
model-facing scope is the whole gameplay session. Per-scene residency is new
mechanism whichever format wins.

**2. Dedupe cooked animations by content. DONE, and measured.** Textures
dedupe on `asset.bytes == bytes`; the animation push had no such check. With
the claw and no-claw mantis variants both registered in `projects/mantis`:

| | clip files | bytes |
| --- | --- | --- |
| with dedupe | 5 | 73,140 |
| without | 10 | 146,280 |

Exactly double, so one variant sharing five clips costs 71.4 KB without it.

No regression test guards this, and not for want of trying: three attempts to
reproduce it on the `legacy-grid-starter` fixture all passed with the dedupe
removed, because the fixture's second model never cooks clips of its own. The
verification is the real project above. If someone wants a test later, the
recipe that DOES reproduce it is a variant with its own `Model`, its own clip
resources targeting that model, its own `AnimationSet`, and its own
`Character`; clone fewer of those and the variant silently cooks no clips.

**3. Trim the affine further, 20 to about 16 bytes.** The Q11 packing is
already in; the remaining lossless win is that the third row of an orthonormal
matrix is the cross product of the other two, so six of the nine codes suffice.
That is roughly 20% more for a cross product at decode. Smaller than it looked
before, because the easy 17% was already taken.

**4. Audit the peak at build time. DONE.** `audit_resident_assets` in
`editor/crates/psxed-project/src/playtest/budget.rs` sums every
`PersistentGameplay` payload, word-padded, which is exactly the sum the manifest
turns into `PERSISTENT_ASSET_PAGE_COUNT`, so the audit's figure and the guest's
compiled constant cannot disagree. `write_package` refuses a cook that exceeds
the cap, before touching the filesystem, and prints a breakdown above 90%.

The cap is measured, not guessed. Raising `PERSISTENT_ASSET_PAGE_COUNT` in the
generated manifest and relinking puts the ceiling at **354 pages (724,992 B)**:
355 overflows the RAM region by 368 bytes. The linker gives static sections
`2M - 64K BIOS - 32K stack` = 1,998,848 B, so that ceiling is whatever this
build's code and other arenas leave behind, and it moves when they do. The cap
sits at 344 pages, ten under the measured point, so ordinary code growth cannot
put the linker back in front of the audit. Re-measure the same way after any
large code change.

What it said the first time it ran on the default project:

```
resident assets 643900 B in 315 pages of 344 (91%), 43 payloads
  Aletha Delivered: 474512 B
  Rust Mantis: 100508 B
  Sword1 Light: 67732 B
  Sword1 Heavy: 1148 B
  largest payloads:
    clip_27_aletha_idle.psxanim   88420 B
    sword1_light/atlas.psxt       66076 B
    clip_17_gen_vert_combo_attack.psxanim  36940 B
```

Two things fall out immediately. The largest single payload in the game is an
IDLE clip at 88 KB, 12% of the whole resident budget. The second largest is a
sword's texture atlas at 66 KB, which is not animation at all and had never
been counted against this ceiling. Headroom is 29 pages, about 59 KB, which is
less than one more mantis variant.

## Lifting HMD8 into the SDK

The direction is HMD8 upward, not `.psxanim` outward. hl-psx already solved
this under harder constraints, so the SDK should carry its format and both
games should use it.

Two facts make that more practical than it sounds.

**Its optional parts are already flag-gated.** `HMD_FLAG_BODY_MASKS`, `MOUTH`,
`PACKED_NORMALS`, `I8_NORMALS`, `HITBOXES`, `FRAME_TIMES`,
`ALIGNED_MODEL_DATA`, `VERTEX_SOA`. The Half-Life specifics (mouth transforms,
body groups, studio hitboxes) are already sections you can leave out, so
promoting the format does not drag GoldSrc concepts into the SDK; it inherits a
container that was designed to have parts omitted.

**The reusable core is small.** The 20-byte packed affine, bone-local vertices
grouped into contiguous ranges so each range pays one matrix load, and pose
interpolation between two frames. That is what both games want, and it is what
`psx_asset` should own.

One design fork was supposed to be settled first: HMD8 is a MERGED container
(`HMRG`) carrying poses inside the model blob, and hl-psx gets residency by
cooking a per-map VARIANT of each model holding only that map's reachable
clips, where PSoXide keeps every clip as its own asset. Measuring hl-psx's own
build reports shows that is the wrong fork to agonise over. See below.

Sequence, with the first step done.

**1. Extract the reader into `psx-asset`. DONE.** It is
`sdk/crates/psx-asset/src/hmd8.rs`, lifted whole from hl-psx's
`game/src/model.rs`. The lift was almost free: the file's only outside
dependencies were `psx_gte::math`, which is already an SDK crate and already a
`psx-asset` dependency, and one game-local vertex-count constant, now
`DEFAULT_MAX_VERTS` with `Model::load_with_vertex_cap` for a caller policing its
own arena. Everything else is `core`.

Verified two ways, because a parser that compiles proves nothing:

- **232 real cooked HMD8 chunks** from hl-psx's model pack parse through the
  extracted reader with every invariant holding (ranges inside the vertex
  stream, bones inside the pose stream, clip frames inside the frame count,
  triangle indices inside the mesh). Those chunks are Half-Life-derived so none
  are committed; the test reads them only when `HMD8_FIXTURE_DIR` points at a
  pack, and otherwise runs against a hand-built blob.
- The committed test is not vacuous: changing the range stride by one byte
  fails two of its four cases.

The reader also rejects damage rather than trusting it, which is what makes it
safe against streamed chunks, and there is now a case per failure mode. That
guard is load-bearing: pointed at a texture chunk by mistake, it returned the
null model instead of reading wild memory.

2. Decide residency. Measured below, with a recommendation.
3. Move PSoXide's cook onto it, keeping `.psxanim` readable until the games
   have migrated.
4. Point hl-psx at the SDK type and delete its private copy, which is the
   check that the extraction actually generalised.

Step 4 needs staging that is worth knowing before starting it. hl-psx does not
track PSoXide's `main`; it consumes a pinned rev (`psoxide-link` at
`9c298d83`) with path dependencies into a local `.psoxide` checkout. So it
cannot see this module until that pin moves, and moving it pulls in everything
else that has landed since. That makes step 4 a deliberate bump rather than
something to slip in alongside a format change.

## Why hl-psx's animation is so much smaller

Not the format. Measured across its 232 cooked chunks against ours:

| | hl-psx | PSoXide (Aletha) |
| --- | --- | --- |
| bytes per frame | 465 (23.3 bones) | 520 (26 joints) |
| frames per clip | **3.9 median 4** | **28.5** |
| clips per model | **5.3** | **30** |
| pose bytes per model | **10.6 KB** | **449 KB** |

The per-frame cost is the same, because it is the same 20-byte record. The 42x
gap is 7x more frames in each clip multiplied by 6x more clips kept resident.
hl-psx stores roughly four keyframes per animation and only the handful a map
can reach. A third of its clips are one or two frames.

So the lever was never encoding. It is how many frames a clip holds and how
many clips a character keeps.

## Cook-time resampling, and what it can and cannot pay for

`ProjectDocument::animation_error_budget_degrees` (default 0, off) lets the cook
drop each clip to the lowest rate that holds a worst-case rotation error. See
`animation_resample.rs` for why it resamples rather than dropping frames.

Measured on the default project, total session-resident bytes:

| budget | resident | Aletha | notes |
| --- | --- | --- | --- |
| off | 643,900 | 474,512 | idle 12 Hz, 170 frames |
| 1 deg | 633,700 | 468,272 | |
| 2 deg | 590,700 | 431,872 | idle to 7 Hz, 100 frames |
| 4 deg | 542,300 | 408,992 | idle to 5 Hz, 72 frames |

Attacks keep their authored rate at every budget tried, which is the
self-selection working: their motion blows the budget at the first step down.

**It is worth about 15%, not 40%.** Two other techniques were measured and
rejected. Lossless static-joint elision saves 1%, and freezing joints that move
under 5 degrees saves 2%, because the records are composed world-space matrices,
so a toe inherits the whole chain's motion and nothing is ever still. Local-space
storage would be sparse, but composing it back costs per-joint CPU we do not
have. Adaptive non-uniform keyframes reach 26% at the same budget, and need a
format change to store frame indices.

### Against a 40/30/30 player/enemy/boss split

The cap is 344 pages, 704,512 B, so the shares are 281,805 / 211,354 / 211,354.

| | now | at 4 deg | share | |
| --- | --- | --- | --- | --- |
| player (Aletha + both swords) | 543,392 | 477,872 | 281,805 | **170% over at 4 deg** |
| enemy (Rust Mantis) | 100,508 | 64,428 | 211,354 | fine, room for variants |
| boss | 0 | 0 | 211,354 | untouched |

The enemy and boss shares are comfortable. The player is the whole problem, and
resampling closes about a quarter of the gap. The remaining ~196 KB has to come
from content, and the measurements say where:

- **Dead head and tail on the attack clips.** 16 to 31% of each long attack is
  motion under 15% of its peak, and the cooked character record plays the FULL
  range, so that is half a second of nothing before a swing. Trimming is worth
  roughly 45 KB across six clips and makes the attacks more responsive.
- **The idle's first 8 seconds.** 98 of its 170 frames are near-still with the
  gesture at the end. Trimming beats resampling here by a wide margin.
- **The light sword's atlas, 66,076 B.** Not animation at all, and 9.4% of the
  entire cap for one weapon's texture, sitting in the player's bucket.
- **Clip count.** 30 player clips against hl-psx's 5.3 per model. Six of ours
  are 8-frame windup/winddown transitions.

## Residency, measured

Numbers below come from hl-psx's own build reports (`.hlpsx/reports/`,
`model-clip-residency.csv`, `model-clip-ram.csv`, `model-residency.csv`) and
from measuring its 108 cooked model chunks directly. It has already run the
experiment across 101 maps, so neither case needs to be argued from taste.

### The win is at the model, not the clip

| | |
| --- | --- |
| every model chunk resident at once | **4377.2 KB** |
| worst single map | **317.3 KB** |
| ratio | **13.8x** |

That 13.8x is the entire prize, and it comes from one decision: don't hold
every level's models resident. Its build audit puts the same thing in absolute
terms, worst resident 205.8 KB against a worst transient peak of 227.5 KB
during a map transition, so the overlap while one level hands over to the next
costs about 11% headroom.

Now the part that decides the fork. Trimming a resident model down to only the
clips ITS map can play, which is what the merged per-map variant buys on top of
plain model-level scoping, is worth far less:

| | |
| --- | --- |
| worst map, all clips of its models | 116.4 KB |
| worst map, only that map's clips | 103.6 KB |
| per-map cut | **median 13%, max 70%** |
| maps that gain nothing | 8 of 101 |

So the heaviest map saves about 13 KB from clip-level trimming, after saving
roughly 4 MB from model-level scoping.

### What merging costs

Cooking a variant per (map, model) duplicates the mesh, because poses ride
inside the model blob. Measured across the pack: 31 distinct models become 108
chunks, one model has 21 variants, and **1487.4 KB of the chunk bytes are
duplicate mesh copies**. Poses are only 44% of a chunk, so more than half of
what a variant re-cooks is geometry that already existed. No map holds two
variants of one model, so this is disc rather than RAM, but disc is load time.

It also costs a build pipeline: roster extraction per map, clip manifests, a
dedupe pass, a residency report, and an unresolved-actors list for scripted
spawns the planner could not predict (hl-psx's answer for those is "already
unsupported"). A variant is a cook-time prediction of what a level can play.

### Our budget is shaped differently

Of PSoXide's 483.1 KB resident animation, **438.3 KB is Aletha** (90.7%). She
is the player, so she is resident in every level by definition, and no
level-scoped residency scheme touches her. Per-level residency scopes the
enemy clips, which are 44.7 KB today. The win therefore scales with the enemy
roster and variant count, not with the level count, and it does nothing for
nine tenths of the current budget. The lever on Aletha is fewer or shorter
clips, a tighter affine, or residency keyed to something else entirely (equipped
weapon moveset, combat versus traversal), which is a different axis and not
this decision.

Also worth stating plainly: the project has exactly one scene, `Main`, holding
Aletha, the Rust Mantis and all equipment. Per-scene residency saves **0 KB
today** whichever design wins. It starts paying at the 9-map Episode 1 target.

### Recommendation

Take the merged format, skip the variant planner, for now.

Adopt HMD8 as the container, since the reader is already in the SDK and reads
merged blobs, and cook ONE blob per model carrying all its clips. Scope
residency at the model level per scene. That captures the 13.8x, which is the
part that matters, without the 1487 KB of duplicated mesh or the variant
pipeline. Revisit per-map clip variants only if a measured scene lands within
about 15% of the cap, because 13% median is what they would buy back.

Do the build-time audit first regardless of the above. It is independent of the
fork, it is cheap, and it turns our ceiling from a linker error into a named
clip with a byte count.

## Why this direction

hl-psx wrote HMD8 privately in `game/src/model.rs` rather than using
`psx_asset`'s `.psxanim`, which says the SDK format did not do what the harder
consumer needed. The fix is to adopt the better format, not to export the
weaker one: promoting today's 24-byte always-resident `.psxanim` into a shared
crate would hand hl-psx our problem.
