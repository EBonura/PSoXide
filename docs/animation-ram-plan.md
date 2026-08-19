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

**1. Residency (structural, biggest win).** Stop marking model clips
`PersistentGameplay`. Scope them the way room textures already are, so a scene
holds the clips its characters can reach. PSoXide already has the streaming
machinery, the resource-set keys, and the per-state enter/exit hooks; this is
about classification rather than new mechanism. Expect the win to scale with
how many characters exist rather than being a fixed percentage, which is
exactly what a boss plus four mantis variants needs.

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

**4. Audit the peak at build time.** Port hl-psx's habit: compute resident
animation per scene, compare against a declared cap, fail the cook. Cheap, and
it turns "the linker refused" into "this clip is 12 KB over".

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

One real design fork to settle first, because it changes the shape of the
answer. HMD8 is a MERGED container (`HMRG`) that carries poses inside the model
blob, and hl-psx gets residency by cooking a per-map VARIANT of the model
holding only that map's reachable clips. PSoXide instead keeps every clip as
its own stream asset. Cooking scene variants is simpler and is why hl-psx's
peak is auditable at build time; per-clip streaming is more dynamic and suits
rooms that come and go. Whichever we pick, the format has to agree with it, so
pick before porting.

Suggested sequence:

1. Extract the core (packed affine, bone ranges, interpolation) into
   `psx-asset` as the shared type, HL-specific sections gated by the flags that
   already exist.
2. Decide residency: per-scene cooked variants (hl-psx's answer) or per-clip
   streaming. This determines whether poses stay merged into the model blob.
3. Move PSoXide's cook onto it, keeping `.psxanim` readable until the games
   have migrated.
4. Point hl-psx at the SDK type and delete its private copy, which is the
   check that the extraction actually generalised.

## Why this direction

hl-psx wrote HMD8 privately in `game/src/model.rs` rather than using
`psx_asset`'s `.psxanim`, which says the SDK format did not do what the harder
consumer needed. The fix is to adopt the better format, not to export the
weaker one: promoting today's 24-byte always-resident `.psxanim` into a shared
crate would hand hl-psx our problem.
