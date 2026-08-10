# Quake, PSoXide BSP, editor, and combat convergence handoff

Last updated: 2026-08-10 21:50 BST

Live integration branch: `codex/quake-psoxide-convergence`

Live integration worktree: `/Users/ebonura/Desktop/repos/PSoXide-convergence`

Snapshot HEAD when this document was created:
`420e9deb8fd35c546856bd4985cbc957b7ed9857`

This is the durable continuation packet for a completely new model or human
worker. Read it in full before editing. It deliberately records unfinished
work, uncertainty, dirty-worktree boundaries, validation evidence, and ways to
falsify the current implementation. It is not a claim that the campaign is
finished.

## 1. Owner objective

The owner wants one coherent PS1 development stack, not three adjacent demos:

1. `psx-bsp` is the single reusable BSP world, visibility, and collision
   mechanism.
2. Quake-PSX uses that shared mechanism while retaining Quake-specific
   movement, gameplay, entities, menus, rendering policy, and content rules.
3. The PSoXide editor becomes a practical brush level editor in the style of
   TrenchBroom, with fast Draft playtests and explicit Release validation.
4. PSoXide gameplay uses the BSP world for player movement, movers, enemies,
   dynamic blockers, weapons, hitboxes, damage, and hit reactions.
5. The tracked Cortex sample contains real weapon/socket/hitbox authoring and
   can be built, cooked, replayed, and inspected without relying on an
   untracked donor project.
6. Revisions and build instructions are reproducible across the PSoXide and
   Quake repositories.

The user's practical priority is earlier than the final architecture: they
want to start building and testing levels as soon as possible. Keep delivering
small, honest, runnable checkpoints while convergence continues.

## 2. What the user can test now

There is already a useful first checkpoint. It is not the weapon-combat
checkpoint.

From the integration worktree:

```sh
cd /Users/ebonura/Desktop/repos/PSoXide-convergence
make run
```

Open:

```text
editor/projects/brush-first-playable/project.ron
```

Then press Play in the editor.

What this checkpoint is intended to demonstrate:

- a tracked BSP-first project;
- BSP cooking and rendering;
- collision through the shared tracer/provider seam;
- a raised brush door and doorway route;
- normal Play/Stop lifecycle;
- synchronized Top, Front, and Side orthographic brush views;
- player, mover, and dynamic actor blocker composition in the runtime.

What it does not yet honestly demonstrate:

- a complete TrenchBroom-equivalent brush workflow;
- a real player-versus-enemy authored combat replay;
- enemy attacks driven by authored Mantis attack capsules;
- final Cortex RAM, VRAM, or packet-budget compliance;
- a final revision-pinned clean Quake/PSoXide build;
- original PlayStation hardware validation.

Do not launch the GUI on the user's behalf. The user launches it. Agents may
use host tests, guest builds, headless emulator replays, and image dumps.

## 3. Completion levels and honest language

Use these levels to avoid calling an intermediate state "done."

### Level A: BSP editor checkpoint, available now

The user can edit the tracked first-playable brush project and enter Play.
World collision, the brush door, common lifecycle, and orthographic views are
present.

### Level B: first real combat checkpoint, in progress

Required evidence:

- tracked BSP sample contains the verified sword, socket, equipment, attack
  window, attack capsule, and hurtbox content;
- the player weapon is visibly attached using the same retained pose that
  drives gameplay volumes;
- a live BSP enemy blocks the player and is blocked by the BSP world;
- one authored player attack can damage the enemy exactly once in its active
  window;
- one enemy-authored attack can damage the player exactly once, if a genuine
  source clip and volume exist;
- damage and hit reaction are visible in a deterministic headless replay;
- no legacy arc and authored capsule both apply on the same attack;
- MIPS build and relevant host suites pass.

Known content limitation: the verified donor has a Mantis hurtbox, but no
verified Mantis attack clip/capsule source. Do not invent one. Until real
authoring exists, label enemy reciprocal damage incomplete or use an explicit,
documented fallback rather than implying authored parity.

### Level C: practical BSP level-authoring checkpoint

Required editor workflow:

- New Project defaults to BSP;
- explicit Draft and Release cook modes;
- one-click Play always uses current saved/authored state;
- clear build freshness and cook errors;
- exact and estimated PS1 budget diagnostics with actionable focus targets;
- synchronized perspective, Top, Front, and Side work;
- brush creation, selection, face editing, edge editing, vertex editing,
  multi-brush operations, arbitrary plane handles, duplication, deletion,
  undo/redo, persistence, and reload;
- entity placement and inspector editing in the BSP world;
- a small level can be created from scratch, saved, cooked, played, changed,
  and replayed without manual generated-file intervention.

### Level D: full convergence

Required evidence:

- PSoXide has one canonical BSP runtime path for new projects;
- legacy grid runtime is either retired after parity or explicitly frozen with
  a documented compatibility boundary;
- Cortex content fits Release budgets without reducing intended visual
  quality;
- the combined PSoXide regression matrix passes;
- Quake consumes a reproducible final PSoXide revision or equivalent local
  source contract and passes its full host, MIPS, all-map, and replay gates;
- documentation describes the actual final architecture, not the old grid
  plan;
- hardware battery is run when a console is available, or explicitly recorded
  as not run with emulator evidence kept separate.

## 4. Repository and worktree map

### Primary PSoXide integration

```text
Repository: /Users/ebonura/Desktop/repos/PSoXide
Worktree:   /Users/ebonura/Desktop/repos/PSoXide-convergence
Branch:     codex/quake-psoxide-convergence
Snapshot:   420e9deb8fd35c546856bd4985cbc957b7ed9857
Upstream:   origin/main, integration branch was 90 commits ahead at snapshot
Status:     clean before this document was added
```

All final PSoXide convergence merges and combined tests should happen here.

### Quake convergence

```text
Repository/worktree: /Users/ebonura/Desktop/repos/quake-psx-convergence
Branch:              codex/quake-convergence
HEAD:                76cfd76
Status at snapshot:  clean
```

Relevant Quake commits, oldest to newest in the convergence sequence:

```text
5bf1503 test: add Quake collision parity oracle
990735c test: prove shared BSP trace parity
8cef817 core: use shared PSoXide BSP tracer
0b9d6df cook: preserve layered sky and flame transparency
c5e3b59 quake-core: add host-testable menu policy
f3e344d quake-core: add provider-neutral movement policy
8c533fa core: converge Quake BSP ownership
ba78e0b core: unify Quake mover collision policy
76cfd76 game: wire converged Quake runtime policy
```

Quake still pins old PSoXide revision
`ff826a3cb98ba30d7ebd0e1c8e44aeb4e9258bdc` in both root `Cargo.toml` and
`host/quake-build/main.rs::PSOXIDE_REV`. That revision predates this
convergence. The local `--psoxide PATH` override hydrates any checkout that
contains `sdk/psoxide.ld`, but does not prove its Git revision or cleanliness.
This is a reproducibility gap, not merely documentation polish.

### User-owned main PSoXide checkout

```text
Path:   /Users/ebonura/Desktop/repos/PSoXide
Branch: codex/windowed-classic-affine
HEAD:   ff826a3cb98ba30d7ebd0e1c8e44aeb4e9258bdc at the original freeze
```

This checkout contains or contained unrelated owner work. Treat it as
read-only unless the user explicitly authorizes a change. Do not clean,
reset, discard, or absorb unrelated modifications. The untracked donor
project under `editor/projects/cortex_v1` is an input source, not the final
tracked integration sample.

### Important auxiliary PSoXide worktrees

```text
/Users/ebonura/Desktop/repos/PSoXide-bsp-caller-owned-trace
  branch codex/bsp-caller-owned-trace
  commit adafa2bf710c466fd1bdbfa825b4db8f09aa171e

/Users/ebonura/Desktop/repos/PSoXide-quake-bsp
  branch quake-bsp-world
  commit 653353e86f849414d5584b45f89b5aa2066b2fa5
  preserves an intentional Cargo.lock delta in its original worktree

/Users/ebonura/Desktop/repos/PSoXide-weapon-attach
  branch weapon-attachment
  commit 2b89e3b3f2b4e2583e50917878471178644d4a39

/Users/ebonura/Desktop/repos/PSoXide-actor-weapon-authority
  branch codex/actor-weapon-authority
  commit 25df5a5d81b9ba396639c993bae0d7ec2d65b626

/Users/ebonura/Desktop/repos/PSoXide-instance-pose-authority
  branch codex/instance-pose-authority
  commit 1294cc6b38d2f0b1ffe634e148d3487afe951257

/Users/ebonura/Desktop/repos/PSoXide-dynamic-bsp-blockers
  branch codex/dynamic-bsp-blockers
  commit 937270d8bab13596e87df9887309963cae570547
```

Completed auxiliary worktrees are evidence and recovery points. Do not delete
them merely to tidy the workspace. If disk space is critical, targeted
`cargo clean` in a completed inactive worktree is acceptable; never delete
sources or use a broad recursive cleanup.

## 5. Current integration history

The important mainline sequence at the snapshot is:

```text
18c29b6c Merge scoped classic-affine packet support
c3672d6f Merge weapon attachment campaign
7b805c45 Merge BSP editor and runtime foundation
359537cf Merge caller-owned iterative BSP tracer
7a09c1e9 psx-engine: reserve shared arena words for BSP streams
002e1103 editor-playtest: allocate BSP packets from shared arena
8dbe7fd6 Merge BSP runtime trace provider
e1004990 editor: cook brush worlds through normal play package
f93a4908 editor: cook conservative brush PVS components
fece6676 editor: make new projects BSP first
64b68d02 editor: focus BSP cook diagnostics
919730ce Merge actor pose and weapon authority checkpoint
2d41a534 Merge normal BSP playtest lifecycle
ff1e4f99 Merge synchronized BSP orthographic editor views
08179eac Merge tick-authoritative instance poses
efaed1c3 editor-playtest: retain tick-authoritative actor poses
420e9deb Merge unified BSP dynamic blockers
```

Use `git show`, tests, and source inspection to verify any statement below.
Commit messages and this handoff are orientation aids, not substitutes for
code evidence.

## 6. Architectural north star

The intended ownership boundary is:

```text
Editor authoring data
  -> deterministic host cook
  -> typed psx-level/PXBSP records
  -> psx-bsp mechanism
       BSP residency, PVS, render traversal, hull tracing, transforms
  -> psx-engine providers
       allocation-free movement/collision composition
  -> psx-game-runtime policy
       actors, AI, combat, equipment, logic, reactions
  -> game/example policy
       Cortex or Quake-specific rules and presentation
```

Rules:

- `psx-bsp` owns reusable BSP mechanism, not Quake gameplay semantics.
- Quake retains Z-up source conventions at its application boundary.
- PXBSP is Y-up. Coordinate conversion belongs at the boundary and must be
  tested.
- Positions and plane distances are Q20.12. Plane normals and transform
  matrices are Q3.12.
- Guest hot paths are allocation-free, integer/fixed-point, bounded, and do
  not use recursive traversal.
- Actor visual pose, equipment pose, attack capsules, and hurtboxes must use
  one retained tick-authoritative snapshot. Rendering must not independently
  recompute a semantically different pose.
- Runtime sees cooked typed records. It should not parse editor strings or
  depend on editor UI concepts.
- New editor projects are BSP-first. Legacy grid support remains only behind
  an explicit compatibility boundary until it can be removed safely.

## 7. Completed PSoXide work

### 7.1 Weapon attachment campaign

The earlier weapon campaign is merged into the integration branch. It added:

- unscaled joint bases for weapon orientation while retaining scaled offsets;
- a weapon prop import harness with a one-frame bind pose for static weapons;
- socket authoring in the Animation view;
- equipped-weapon preview using runtime-equivalent transform composition;
- source bone names and readable joint labels;
- removal of a dead parallel weapon-hitbox evaluator;
- player and enemy equipment rendering;
- model-instance binding for non-player equipment;
- shared live instance pose composition.

Important details:

- host model scale must not be applied twice to weapon orientation;
- socket/capsule offsets are model-local and follow the scaled joint-matrix
  convention;
- weapon orientation uses the unscaled orthonormal joint basis;
- a weapon model needs a clip; rigid props use a one-frame identity bind pose;
- right hand on the shared 22-bone rig is joint 13, not joint 9;
- preview historically used `ModelResource.scale_q8`, while Play may use the
  Character's `visual_scale_q8`; this remains a parity risk unless the current
  code proves it was corrected;
- a spawn-adjacent tick once produced an `i32::MIN` weapon-origin X that then
  self-corrected. Re-test this after pose-authority changes rather than
  assuming it disappeared.

Primary prior documents:

- `docs/weapon-attachment-plan.md`
- original handoff attachment:
  `/Users/ebonura/.codex/attachments/430ee143-d441-4e35-8257-acc60807635c/pasted-text.txt`

### 7.2 Canonical caller-owned BSP tracing

Canonical implementation commit:
`adafa2bf710c466fd1bdbfa825b4db8f09aa171e`, merged through
`359537cf`.

Public shape:

```rust
trace_into(
    &self,
    start: &Vec3I32,
    end: &Vec3I32,
    scratch: &mut TraceScratch,
    output: &mut Trace,
) -> bool
```

Contract:

- `CollisionHull` and `TransformedCollisionHull` expose the same caller-owned
  contract;
- no allocation, recursion, float, or large result returned by value;
- scratch has 64 private continuation entries and is 2,560 bytes;
- 64 pending entries succeeds, a required 65th returns `false`;
- malformed nodes, planes, cycles, and overflow return `false`;
- failure leaves every output byte, including padding, unchanged;
- scratch resets logical depth on every call and is immediately reusable;
- `true` means a structurally valid trace was written, whether it hit or not;
- epsilon is 128, representing 1/32 unit in Q12;
- contents codes are EMPTY -1, SOLID -2, WATER -3, SLIME -4, LAVA -5,
  SKY -6.

The canonical lockfile delta is 11 lines because `psx-bsp` directly depends
on `psx-gpu` and `psx-gte`. The earlier nine-line delta was incomplete and
Cargo regenerated the missing edges.

Known risks retained from the implementation report:

- pathological clipnode DAG behavior beyond existing goldens;
- saturation at extreme coordinates;
- whether legacy `in_water` aggregation should classify SKY separately;
- the boolean result intentionally does not distinguish malformed input from
  stack overflow.

### 7.3 BSP editor/runtime foundation

The integration contains:

- a tracked `brush-first-playable` project and deterministic generator;
- PXBSP cooking through the normal play package;
- conservative brush PVS components;
- shared packet arena reservation for BSP streams;
- a runtime trace-provider seam;
- BSP-first New Project behavior;
- cook diagnostics that can focus the offending resource;
- normal Play/Stop lifecycle for BSP projects;
- synchronized Top, Front, and Side orthographic views.

The first-playable regression checkpoint originally produced a 13,000-byte
PXBSP, one 2,108-byte texture, one mover, and a canonical replay that crossed
the doorway boundary at x=544.

### 7.4 Tick-authoritative actor pose and weapon authority

The integration retains exactly one player pose snapshot and a fixed array of
optional instance pose snapshots per simulation tick. `Scene::update` runs
gameplay, refreshes snapshots, and then resolves player melee. The snapshot
tail is designed to run even if earlier update logic takes an early return.

Consumers include:

- player body;
- player equipment;
- model-instance bodies;
- non-player equipment;
- player attack capsules;
- enemy hurtboxes;
- player melee active frame.

The integration removed render-time and collision-time pose recomputation for
these paths. It also applies one frame-global non-player-equipment budget
across room passes and exposes equipment draw-stat accumulation.

Do not assume the authority model is correct merely because all consumers use
a type named `Snapshot`. Verify ordering, clip phase, crossfade semantics,
frame boundaries, spawn transitions, room transitions, attack-to-recover
transition, and whether a snapshot can remain stale after an early return.

### 7.5 Unified BSP dynamic blockers

Merged agent commit:
`937270d8bab13596e87df9887309963cae570547` via merge `420e9deb`.

Implemented behavior:

- `CharacterBlockerTraceProvider` composes dynamic actor cylinders after the
  static PXBSP and transformed mover provider;
- exact ties are deterministic: world first, mover order within the world
  provider, actor slice order after it;
- only a strictly earlier actor hit replaces a world/mover hit;
- provider failure leaves caller output unchanged and scratch can be reused;
- actor-cylinder sweep uses fixed-point math and bounded Q12 binary search;
- downward floor probes ignore actor heads;
- raised step probes cannot step over actors;
- tangent movement away from an actor is allowed;
- BSP player movement receives live blockers;
- BSP NPCs use the same trace-based full, X, and Z cascade rather than a grid
  fallback;
- grid projects retain their existing grid/AABB path for compatibility.

Known residual risks:

- every NPC currently uses cooked BSP hull index 1 regardless of authored body
  size;
- BSP BoxProp/ArchProp AABBs are not composed into this provider;
- multi-room NPC coordinate offsets need direct evidence;
- a real live-NPC BSP fixture must prove the path end to end;
- collision cost increased, although the measured regression remained small.

## 8. Completed Quake work

Quake's convergence branch is a clean local branch with shared BSP trace and
movement/provider seams wired while Quake policy remains local.

Preserve in Quake:

- Quake acceleration, friction, gravity, jump, water, landing, and step/slide
  policy;
- Quake movers, triggers, progression, menu, input, audio, and presentation;
- layered sky and flame transparency cook policy;
- Quake alias entity behavior and game-specific animation/presentation;
- diagnostic/parity fixtures until final convergence is independently proven.

Shared/upstream responsibility:

- BSP formats and residency where already covered by `psx-bsp`;
- caller-owned hull tracing and transformed brush support;
- reusable render traversal/materialization where parity exists;
- provider mechanism and fixed-point collision composition.

Previously reported green gates on the Quake convergence branch include core,
parity, cooker, tooling, `psx-bsp`, MIPS shipping, an all-map run of 140
frames across 12 loads, content hashes, and a shipping capture. Re-run them
from a final clean dependency revision. A prior green run against a hydrated
dirty or local checkout is not proof of reproducibility.

Final Quake work still required:

- point the build at a final, reproducible PSoXide convergence revision or a
  strict local-source manifest;
- make the local override report and optionally enforce source Git revision
  and dirty status;
- update stale README language that says gameplay migration is incomplete in
  ways the convergence may already have changed;
- run the complete Quake matrix from the final source contract;
- retain and compare Quake parity oracles for pathological traces, contents,
  movers, and coordinate conversion;
- document which Quake-local duplicated BSP modules were deleted, retained as
  oracles, or intentionally kept as game policy.

## 9. Active work at this snapshot

The statuses here are volatile. Query live agents before assuming they remain
accurate.

### 9.1 Draft/Release BSP Play

```text
Agent task: /root/bsp_draft_release_play
Worktree:   /Users/ebonura/Desktop/repos/PSoXide-bsp-draft-release-play
Branch:     codex/bsp-draft-release-play
Base:       efaed1c3
Status at 21:50 BST: about 40 percent
ETA reported: 60 to 90 minutes to a reviewed clean commit
```

Reported implementation:

- project-persisted `bsp_cook_mode` drives build-package/PXBSP;
- cook mode is stamped into the generated manifest;
- Draft/Release enum has labels, descriptions, and default;
- initial authored estimate plus exact cooked PS1 budget report covers BSP,
  PVS, lighting, textures, RAM, VRAM, and packet envelopes;
- typed focus targets/actions exist;
- `cargo check -p psxed-project` reached the crate without errors.

Remaining at snapshot:

- UI and New Project wiring;
- Play freshness wiring;
- focused tests;
- `psxed-project` and `psxed-ui` validation;
- final diff review and clean commit.

Scope boundary: no runtime collision, gameplay, or weapon changes.

### 9.2 Cortex weapon content migration

```text
Agent task: /root/cortex_weapon_content
Worktree:   /Users/ebonura/Desktop/repos/PSoXide-cortex-weapon-content
Branch:     codex/cortex-weapon-content
Base:       efaed1c3
Status at 21:50 BST: about 70 percent
ETA reported: 45 to 75 minutes to a clean content commit
```

Donor:

```text
/Users/ebonura/Desktop/repos/PSoXide/editor/projects/cortex_v1/project.ron
SHA-256: 78b4deb6272ad7f50c3a2fffef442256528fb2210c43e4f1f4404bec92141e52
```

The donor is read-only. The tracked integration sample is the destination.

Reported verified migration:

- two sockets;
- two equipment nodes;
- sword skeleton/models/bind clip;
- two Weapon resources;
- six binary/source assets;
- grips corrected to scale-specific values 15077 and 18462;
- player hurtbox;
- three active player attack capsules/windows;
- Mantis hurtbox;
- upright spawn;
- unrelated Aletha Delivered resources, UI, and world drift excluded.

Reported tests at snapshot:

- weapon cook 3/3;
- combat capsules 2/2;
- grip migration 1/1;
- project loads and cooks.

Known issue:

- packet envelope was 2015 against a 1536 budget. The agent must report this
  explicitly if it cannot make a visual-neutral content fix.
- one early cook reused a shared Cargo target binary whose compile-time
  `CARGO_MANIFEST_DIR` pointed at the dynamic-blockers worktree. It changed no
  tracked files but refreshed ignored generated outputs in that worktree. The
  content agent switched to a private target for path-correct evidence. Treat
  the earlier cook evidence as invalid for path provenance.

Scope boundary: content/assets/tests only. No runtime or editor UI changes.

### 9.3 Reciprocal retained-pose combat

```text
Agent task: /root/reciprocal_pose_combat
Worktree:   /Users/ebonura/Desktop/repos/PSoXide-reciprocal-pose-combat
Branch:     codex/reciprocal-pose-combat
Base:       420e9deb
Status at 21:50 BST: design/audit complete, implementation about 15 percent
ETA reported: 2 to 3 hours to a clean commit
```

Confirmed seam:

- `GameEntities` resolves a legacy enemy arc before Attack transitions to
  Recover;
- retained snapshots are refreshed afterward;
- this can produce a last-frame gameplay/visual mismatch.

Planned correction:

- fixed-capacity deferred-contact token/finalize handshake;
- authored capsule resolution using the retained instance pose;
- player hurtboxes from the retained player pose;
- legacy arc only as an explicit fallback when authored attack volumes are
  absent;
- no double damage;
- focused tests, runtime/engine validation, MIPS build, and replay.

Known limitation: no verified Mantis attack clip/capsule exists in the donor.
The agent must not fabricate one.

## 10. Validation evidence already obtained

### 10.1 Caller-owned tracer branch

Reported at commit `adafa2bf`:

```text
cargo fmt --check from engine/crates/psx-bsp: passed
cargo test -p psx-bsp --lib: 61 passed
cargo test -p psx-engine --lib render: 108 passed, 175 filtered
first-playable regression: passed
cargo test -p psxed-project --lib: 458 passed, 1 ignored
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench": passed
cargo metadata --locked --no-deps --format-version 1: passed
```

### 10.2 Tick-authoritative poses before dynamic blockers

Replay directory:

```text
/tmp/psoxide-pose-replay.YCJ70b
```

Evidence:

```text
route ticks: 220
pad polls: 223
final player x: 852
door logic fired: 1
VRAM FNV-1a: 0x5f0c6df40cf31729
display FNV-1a: 0x9eef0b6f25eb5c11
executable SHA-256: 59ee1f...  (abbreviated in prior log; do not use as a final pin)
PXBSP SHA-256: 0143cc130d3d73c7dbcc35bdcc2c9c91ba098579e9a8cf977814e2cded2cdac0
```

### 10.3 Integrated dynamic-blocker checkpoint

Replay directory:

```text
/tmp/psoxide-integrated-blockers.MdZSqy
```

Root independently reran:

```text
cargo test -p psx-engine --lib: 298 passed
cargo test -p psx-bsp --lib: 64 passed
cargo test -p psx-game-runtime --lib: 68 passed
make build-editor-playtest EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry": passed
disc build with mkisopsx: passed
headless canonical tape: completed
```

Replay evidence:

```text
route ticks: 220
pad polls: 223
final player x after telemetry bias removal: 852
door logic fired: 1
VRAM FNV-1a: 0x5f0c6df40cf31729
display FNV-1a: 0x9eef0b6f25eb5c11
executable SHA-256: a1384d6a5d2f9297ea8868437940e683d861a24d3f4e3108616496414a69af06
PXBSP SHA-256: 0143cc130d3d73c7dbcc35bdcc2c9c91ba098579e9a8cf977814e2cded2cdac0
render cycles per visual frame: 89348
update cycles per tick: 117437
sim collision cost: about 1389 cycles per tick
```

Visual output remained hash-identical to the pose checkpoint. There was one
existing deadline miss/loading-lateness event. Do not silently reinterpret it
as a new dynamic-blocker regression without an A/B, but do not ignore it in a
final performance gate.

The package-wide example formatting check had inherited formatting debt. New
runtime files and the relevant diff were formatting-clean. Do not mass-format
unrelated files to make a broad check green.

### 10.4 Earlier brush first-playable checkpoint

Commit `653353e86f849414d5584b45f89b5aa2066b2fa5` reported:

```text
final player: (840, 65, 384)
final camera: (840, 113, 384)
doorway x=544 crossed at replay frame 77, reaching x=548
route ticks: 180
pad polls: 180
draw ticks: 177
draw words: 166697
GPU commands: 26086
draws: 12688
textured primitives: 12684
VRAM FNV-1a: 0xcb660c978f0cddb9
display FNV-1a: 0x734009173efe64e4
```

Those hashes belong to the earlier checkpoint. Later hashes changed for
expected integration reasons. Always name the commit, executable, cooked
content, tape, and emulator build associated with a hash.

## 11. Exact build, cook, disc, and replay recipes

### Cook the tracked brush first-playable

From `/Users/ebonura/Desktop/repos/PSoXide-convergence`:

```sh
cargo run -p psxed-project --bin cook-playtest -- \
  editor/projects/brush-first-playable/project.ron
```

### Build the real MIPS guest

```sh
make build-editor-playtest \
  EDITOR_PLAYTEST_FEATURES="cd-stream-bench emulator-telemetry"
```

### Build the disc image

```sh
cd /Users/ebonura/Desktop/repos/PSoXide-convergence/tools/mkisopsx
cargo run --release -- \
  --exe ../../build/examples/mipsel-sony-psx/release/editor-playtest.exe \
  --out ../../build/examples/mipsel-sony-psx/release/editor-playtest.bin \
  --volume PSOXIDE \
  --cdtest-sectors 32 \
  --world-pack-rooms-dir ../../engine/examples/editor-playtest/generated/stream_chunks \
  --world-pack-order-file ../../engine/examples/editor-playtest/generated/world_pack_order.txt \
  --cdda-track-list ../../engine/examples/editor-playtest/generated/cdda_tracks.txt
```

If the cooked project emits UI packs, also pass its UI pack directory and
order file as required by the current `mkisopsx --help` output. Do not copy a
stale command blindly across projects.

### Headless canonical tape

Use the locally built PSoXide frontend appropriate to the branch. A prior root
run used `/tmp/psoxide-bsp-frontend`. Example shape:

```sh
/tmp/psoxide-bsp-frontend launch \
  --path /Users/ebonura/Desktop/repos/PSoXide-convergence/build/examples/mipsel-sony-psx/release/editor-playtest.cue \
  --embedded-playtest \
  --input-tape /Users/ebonura/Desktop/repos/PSoXide-convergence/editor/projects/brush-first-playable/walk-through-door.pxitape.csv \
  --guest-frames 220 \
  --steps 2000000000 \
  --dump-hash /tmp/psoxide-replay-hash.txt
```

Confirm the current CLI flags with `--help`; do not assume an old frontend has
the same telemetry schema as the checked-out source.

### Mandatory generated-content rule

The cooked manifest is compiled into the guest. The safe loop is always:

1. cook the intended project from the intended worktree;
2. rebuild the MIPS guest;
3. rebuild the disc;
4. replay that disc;
5. record exact paths and hashes.

Recooking without rebuilding the guest can replay stale content and produce a
convincing false result.

Avoid a shared Cargo target when compile-time paths such as
`CARGO_MANIFEST_DIR` affect generated-output destinations. A binary compiled
in one worktree can write ignored outputs into that worktree even when invoked
from another. For cross-worktree evidence, use a private target directory or
rebuild locally and prove generated paths.

## 12. Cortex content and budgets

The prior untracked weapon sandbox was:

```text
/Users/ebonura/Desktop/repos/PSoXide/editor/projects/cortex_anim
```

The current read-only game-project donor is:

```text
/Users/ebonura/Desktop/repos/PSoXide/editor/projects/cortex_v1
```

The tracked sample on the integration branch must become the durable source
of the minimum verified weapon/combat slice. Do not solve this by depending
on an ignored absolute path.

Verified authoring facts from the donor/audit:

- two weapons and two equipment assignments;
- two grip sockets;
- player attack windows and three player attack capsules;
- player hurtbox;
- Mantis hurtbox;
- no verified Mantis attack clip/capsule;
- old scene data contained a Mantis pitch of about -90 degrees that placed it
  into the floor;
- enemy flanking can deliberately move it behind the camera, making a valid
  enemy look absent in a fixed headless shot.

Known full Cortex budget problems reported before this snapshot:

```text
MIPS/RAM overflow: 54096 bytes
packet envelope: 2015 required against 1536 available
```

The content-migration agent must publish path-correct current numbers. Do not
reduce visual quality merely to turn a counter green. Prefer deduplication,
lifetime/residency corrections, capacity derivation, packet reuse, correct
room/PVS scoping, or an explicitly justified budget increase backed by memory
and performance evidence.

## 13. Remaining editor work for TrenchBroom-like usability

The editor is not yet "fully migrated" merely because it displays three
orthographic views. Audit and implement at least:

1. brush creation from each useful view;
2. direct face manipulation;
3. explicit edge manipulation;
4. explicit vertex manipulation;
5. arbitrary plane-handle manipulation for non-axis-aligned brushes;
6. multi-brush selection and transforms;
7. duplicate/delete/grouping behavior;
8. grid snapping and predictable numeric inspector fallback;
9. undo/redo transactions that group a drag into one operation;
10. persisted view focus, scale, and orientation per project/document;
11. synchronized selection across perspective and orthographic views;
12. entity creation, selection, movement, and inspector editing in BSP space;
13. material assignment and face-level texture controls;
14. clear invalid-brush diagnostics that select/focus the bad plane or brush;
15. Draft cook for fast iteration and Release cook for authoritative budgets;
16. Play freshness: save/cook/build/reload cannot silently use stale data;
17. reload after external asset/project changes;
18. a from-scratch workflow test performed by someone who did not implement
    the feature.

Do not cargo-cult TrenchBroom's UI. Match its productive grammar while keeping
PSoXide's PS1 constraints visible: fixed capacities, PVS, texture pages,
packet budgets, RAM, VRAM, and deterministic cooking.

## 14. Remaining runtime and combat work

After merging the three active workstreams:

1. Resolve merge conflicts by preserving one authority seam, not by keeping
   duplicate pose/combat paths.
2. Build a compact tracked BSP combat fixture containing one player, one
   armed enemy, one door or mover, player attack capsules, both hurtboxes, and
   enough space to prove world and actor collision.
3. Record a deterministic tape that approaches, attacks, hits once, observes
   hit reaction, collides with the enemy, and traverses the BSP route.
4. If no authored enemy attack exists, record that absence. Do not fabricate
   a clip to satisfy the milestone.
5. Prove exact active-window boundary behavior and attack-to-recover behavior.
6. Prove a weapon visual, attack volume, hurtbox, and damage event use the same
   simulation tick/pose.
7. Prove moving/rotated brush collision and actor blockers compose without
   stepping over or tunneling through each other.
8. Test actors in more than one room and across a room-origin boundary.
9. Decide how BSP props participate in collision. The current actor blocker
   provider does not include all BoxProp/ArchProp AABBs.
10. Replace the hard-coded NPC hull-index assumption with an authored or cooked
    body-size/hull selection rule.
11. Compare performance with zero, one, and several awake/visible enemies.
12. Keep grid behavior unchanged until its compatibility/retirement decision
    is supported by a migration and parity report.

## 15. Legacy grid boundary

The old runtime plan explicitly said "Grid world, not BSP." That document is
now historically useful but architecturally stale for new projects. Do not
quietly edit history and do not leave contradictory active documentation.

Required decision artifact:

- list every grid-only authoring, cook, runtime, collision, navigation,
  streaming, and project-data path;
- identify BSP equivalent, adapter, or missing parity for each;
- classify each path as migrate, compatibility-only, or retire;
- identify existing projects that still require grid behavior;
- specify the project-format discriminator and the point after which new grid
  authoring is frozen;
- add tests proving old grid projects still load during the compatibility
  window;
- remove fallback paths only after the owner accepts the migration boundary.

The current dynamic-blocker implementation intentionally preserves grid
movement for grid projects. That is a safe compatibility step, not final
retirement.

## 16. Reproducible Quake/PSoXide pinning

Current problem:

- Quake's remote Git dependency and `PSOXIDE_REV` point to `ff826a3...`;
- the converged PSoXide code exists only in local commits/worktrees;
- `--psoxide PATH` accepts a checkout based on the presence of
  `sdk/psoxide.ld`, without proving HEAD or dirty state;
- therefore a successful local build is not yet a reproducible clean build.

Preferred final contract:

1. finish and commit the PSoXide convergence checkpoint;
2. choose a specific PSoXide commit as the Quake dependency revision;
3. update both dependency metadata and human-readable build output;
4. make `check` print the effective source, revision, and dirty state;
5. if a local override is allowed, either require the expected revision by
   default or require an explicit flag to accept a different/dirty checkout;
6. hydrate into a fresh ignored `.psoxide` directory;
7. run Quake tests and shipping builds without relying on an older hydrated
   tree;
8. document the exact local-only workflow if the commit cannot yet be pushed;
9. only update a remote Git rev after that commit is reachable by the remote.

Never point a remote Git dependency to an unpushed commit and claim a fresh
checkout works.

## 17. Adversarial review mandate: challenge everything

The next session must begin with an adversarial audit before extending the
architecture. The goal is to find contradictions, accidental duplication,
fixture-only success, and claims that outran evidence.

### 17.1 Re-derive, do not inherit

For each major subsystem, answer from source and tests before trusting this
handoff:

- Who owns the canonical data?
- At what tick or frame is it produced?
- Which consumers read it?
- Can another path recompute or mutate a competing value?
- What are the coordinate system, fixed-point scale, and saturation rules?
- What is the failure contract?
- What fixed capacities can overflow?
- Which test proves the behavior, and can that test pass without exercising
  the intended guest path?
- Which result came from a dirty, stale, or incorrectly hydrated worktree?

### 17.2 Search for duplicate authorities

Specifically search for:

- remaining Quake-local collision/BSP traversal used in shipping code;
- grid fallback accidentally selected for BSP projects;
- render-time pose sampling outside retained snapshots;
- combat geometry evaluated from a different phase than the visible body;
- both legacy arc and authored capsule damage in one attack;
- player-only and instance-only equipment paths with divergent math;
- duplicate transformed-mover coordinate conversions;
- cooked and runtime defaults that disagree;
- generated manifest stubs mistaken for the real cooked manifest;
- test-only adapters not used by the MIPS game.

Use `rg` and call graphs, not filenames alone.

### 17.3 Try to falsify collision correctness

Add or rerun cases for:

- exact plane epsilon boundaries in both directions;
- zero-length traces inside empty and solid contents;
- 64 and 65 continuation entries;
- malformed cycles and indices with byte-preserving failure;
- axial and non-axial planes;
- extreme coordinates and multiply/add saturation;
- SKY, WATER, SLIME, and LAVA aggregation;
- translated and rotated movers;
- world/mover/actor exact ties;
- tangent actor motion toward and away;
- starting inside an actor;
- actor-to-actor crossing in one tick;
- floor probe at actor head height;
- stepping beside and into an actor;
- multiple actors in different rooms;
- authored body sizes that do not match hull index 1.

Compare with the retained Quake oracle where semantics overlap. A parity test
that copied the same bug into expected values is not independent evidence.

### 17.4 Try to falsify pose and combat authority

Test:

- the final active attack frame before Recover;
- the first Recover frame;
- crossfades at spawn and state transition;
- room activation/deactivation in the same tick;
- two attacks separated by the minimum recovery window;
- one capsule touching two hurtboxes;
- two capsules touching one hurtbox;
- exact tangent contact;
- mirrored/yaw-rotated actors;
- non-uniform visual scale assumptions;
- a weapon socket on a non-root animated joint;
- a player and enemy using different skeleton/visual scales;
- an enemy missing authored attack volumes;
- damage rejection/debounce after the first contact;
- hit reaction pose on the damage tick and following render.

Instrument pose phase, clip/frame, socket origin, capsule endpoints, hurtbox
endpoints, and damage token in one telemetry record. Prove equality where it
is required rather than visually guessing from a screenshot.

### 17.5 Try to falsify editor freshness and persistence

Run a from-scratch test:

1. create a new BSP project;
2. create and edit several brushes in different views;
3. save;
4. Play in Draft;
5. stop, change geometry, and Play again;
6. prove the second executable/cooked payload changed;
7. switch to Release and trigger one intentional budget failure;
8. use the diagnostic action to focus the offending authored item;
9. fix it, rebuild, close, reopen, and confirm view/project persistence;
10. reproduce the project from a clean checkout without ignored generated
    files.

Have someone other than the implementation author perform this before calling
the workflow practical.

### 17.6 Try to falsify budget reports

- independently calculate cooked PXBSP/PVS/texture sizes;
- compare authored estimate with exact cooked output and explain the delta;
- verify RAM includes static data, `.bss`, stacks, arenas, streamed buffers,
  and generated capacities without double counting;
- verify VRAM includes pages, CLUTs, framebuffers, draw buffers, and transient
  residency assumptions;
- provoke each envelope independently and confirm the UI focuses the right
  source;
- check that Draft does not silently permit content that Release later cannot
  identify clearly;
- prove packet budgets under worst visible/PVS content, not the smallest
  fixture.

### 17.7 Try to falsify reproducibility

- delete or move the hydrated Quake `.psoxide` tree and rebuild;
- use a fresh PSoXide worktree at the claimed revision;
- prove local override revision/dirty reporting;
- use private Cargo targets when build scripts embed paths;
- hash the executable, PXBSP, textures, tape, emulator binary, and display;
- repeat the replay twice and compare all deterministic outputs;
- make one authored change and prove the output changes for the intended
  reason;
- ensure ignored generated files are not required for a fresh cook.

### 17.8 Challenge documentation itself

Search active docs for contradictions such as grid-only architecture, weapon
systems described as missing after they landed, stale revision pins, and old
test counts. Mark historical documents as historical where appropriate. Do
not rewrite old evidence as if it had been produced by the final branch.

At the end of the audit, write a short discrepancy report with:

- claim;
- evidence checked;
- outcome: confirmed, contradicted, incomplete, or untested;
- impact;
- corrective action;
- whether a prior milestone label must be downgraded.

## 18. Safe operating rules

- Preserve user-owned dirty changes and unrelated worktrees.
- Never use `git reset --hard`, broad checkout/revert, or recursive deletion.
- No rebase, push, protected-ref update, or PR unless the user explicitly asks.
- No editor GUI launch on the user's behalf.
- No invented Mantis attack content.
- No visual-quality reduction merely to satisfy a budget counter.
- No float or heap allocation in guest collision/combat hot paths.
- No recursive BSP traversal on MIPS.
- Use `apply_patch` for source edits. Formatting tools may perform mechanical
  formatting after the diff is scoped.
- Inspect `git status` before and after every merge and test run.
- Merge completed agent commits with `--no-ff` into the integration branch so
  provenance remains visible.
- Review an agent diff and rerun relevant tests from the integration worktree;
  an agent's report is not the final merge gate.
- Do not let a shared Cargo target invalidate worktree-specific generated
  paths.
- Keep commentary/status honest. Separate implemented, tested, replayed, and
  hardware-verified states.

## 19. Ordered continuation plan

At this snapshot, proceed in this order while preserving parallelism where
file ownership permits:

1. Let the three active agents finish clean local commits.
2. Review and merge Draft/Release Play into the integration branch.
3. Review and merge tracked Cortex weapon content.
4. Review and merge reciprocal retained-pose combat.
5. Resolve integration conflicts around project records, generated manifests,
   pose phases, and combat ordering deliberately.
6. Run editor/project, `psx-bsp`, `psx-engine`, and `psx-game-runtime` suites.
7. Cook, MIPS-build, disc-build, and replay a compact real BSP combat fixture.
8. Give the user the first combat checkpoint as soon as that replay is honest.
9. Implement the remaining TrenchBroom-like manipulation and persistence
   workflow, preferably in isolated commits by concern.
10. Perform the legacy-grid parity/freeze/retirement audit.
11. Bring Cortex within Release budgets using visual-neutral architectural
    fixes.
12. Run the combined PSoXide regression/performance/determinism matrix.
13. Finalize the Quake dependency/source contract and rerun all Quake gates.
14. Run the adversarial discrepancy audit in section 17.
15. Update this document with final commits, exact commands, complete hashes,
    remaining hardware gap, and user-facing instructions.

## 20. Resume checklist for a completely new model

Before making any edit:

```sh
cd /Users/ebonura/Desktop/repos/PSoXide-convergence
git status --short --branch
git log --oneline --decorate -20
git worktree list --porcelain
```

Then:

1. Read this document fully.
2. Read `docs/weapon-attachment-plan.md`.
3. Read `docs/bsp-engine-overhaul.md`.
4. Read `docs/quake-bsp-migration-plan.md`.
5. Read `docs/brush-editor-integration.md`.
6. Treat `docs/game-runtime-plan.md` as historical where it mandates a grid
   world; compare its useful runtime principles with the newer BSP direction.
7. Query live agent/task status rather than assuming section 9 is current.
8. Inspect every proposed agent commit and branch status.
9. Re-run the smallest relevant test before merge, then the combined gate
   after merge.
10. Begin with the adversarial audit questions, especially where a merge will
    create a new authority seam.

Current task status when this document was created:

```text
Completed: caller-owned tracer
Completed: BSP cook/PVS/project defaults/diagnostics foundation
Completed: normal BSP Play lifecycle
Completed: Quake shared-BSP convergence branch checkpoint
Completed: tick-authoritative player/instance/equipment poses
Completed: synchronized orthographic BSP views
Completed: static world + mover + dynamic actor blocker composition
In progress: Draft/Release Play and exact budget UX
In progress: verified Cortex weapon content migration
In progress: reciprocal retained-pose combat
Pending: remaining TrenchBroom-like manipulation workflow
Pending: legacy grid boundary/retirement
Pending: Cortex budget correction and combined validation
Pending: reproducible final PSoXide/Quake pin and handoff
Pending: adversarial discrepancy audit and hardware battery
```

## 21. Evidence locations and Graphify memory

Existing project graph:

```text
/Users/ebonura/Desktop/repos/PSoXide/graphify-out/graph.json
```

Relevant saved graph queries:

```text
/Users/ebonura/Desktop/repos/PSoXide/graphify-out/memory/query_20260810_203932_how_should_editor_playtest_retain_one_player_and_i.md
/Users/ebonura/Desktop/repos/PSoXide/graphify-out/memory/query_20260810_212500_how_should_bsp_gameplay_compose_world_movers_and_dynamic_blockers.md
```

Use Graphify for orientation and impact analysis, then confirm important
claims against current source because the graph can lag new commits.

Temporary replay directories may disappear after reboot or cleanup:

```text
/tmp/psoxide-pose-replay.YCJ70b
/tmp/psoxide-integrated-blockers.MdZSqy
/tmp/psoxide-dynamic-bsp-final.RRvsSL
/tmp/psoxide-brush-final.3BxWOM
```

Copy final evidence into a durable, appropriately ignored or documented
location before calling the campaign complete. Do not commit large captures
without checking repository policy and owner preference.

## 22. Handoff maintenance rule

Update this file at every clean integration checkpoint with:

- new integration HEAD;
- merged agent commit and merge commit;
- exact test counts;
- exact cook/build/replay command;
- executable/content/display hashes;
- performance and budget deltas;
- newly discovered inconsistencies;
- changes to user-testable scope;
- active task status and next merge order.

If a later result contradicts this file, update the contradiction explicitly.
Do not silently delete the earlier claim. Record what changed and why so the
next worker can distinguish evolution from unreliable reporting.
