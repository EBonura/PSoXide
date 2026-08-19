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

Format is 24 bytes per bone per frame: a full 3x3 `i16` matrix plus an `i16`
translation, per joint, per frame. Aletha's 26 joints cost 624 bytes a frame,
the mantis's 22 cost 528. Every cooked clip is
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

**A tighter affine.** `HMD8_AFFINE_BYTES = 20` against our 24, storing the
composed bone palette as packed Q1.11 rather than nine full `i16`. About 17%.

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

**2. Dedupe cooked animations by content.** Textures already dedupe on
`asset.bytes == bytes` twenty lines away in the same file; animations are
pushed unconditionally. Until that changes, every mesh variant re-pays for
every clip it shares: +90 KB for a variant sharing five clips, +162 KB sharing
nine. This is small and unblocks the variant plan.

**3. Trim the affine, 24 to 18 bytes.** The third row of an orthonormal matrix
is the cross product of the other two, so it need not be stored: six multiplies
at decode for a flat 25% off every clip, no visual change, no format
philosophy. That is a better first trim than adopting Q1.11 because it is
lossless. Going further to Q1.11 (20 bytes composed, or 8 with quaternions)
trades CPU for bytes, and this game is CPU-bound, so measure before choosing.

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
