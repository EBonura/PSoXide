# Sky system plan: directional environment + infinite ground

**Date:** 2026-08-24
**Branch:** `codex/cortex-skyboxes`, base `0a6881c3`
**Companion:** `docs/infinite-horizon-study-2026-08-23.md`
**Role of this document:** planner output. Nothing here is implemented.

---

## 1. Decision on the current work: already resolved, correctly

**This was settled while the plan was being written.** The work landed as
`12644fac feat(editor): expose Quake layered sky materials`.

That is the outcome I was going to recommend, and the commit is scoped the right
way:

- The message describes it as exposing an editor material, not as a skybox.
- It excluded the unrelated rustfmt reflows and the study-doc edits, which are
  still uncommitted and still belong to their own authors.
- It does not wire anything into the Cortex project cook.

Retaining it was right for a reason independent of the sunset goal: the Quake
layered sky was fully implemented in this codebase yet unreachable, with no UI
toggle and `layered_sky: false` on every material in the tree. Discarding the
checkbox would have re-lost that. The fixture also serves as regression cover
for the shared Quake kernel that quake-psx depends on.

Two standing conditions still apply, and they now apply to `12644fac`:

- It stays an **optional animated cloud material**. It is not the skybox and
  must not define the horizon (section 3).
- It stays out of the Cortex cook until the acceptance suite passes.

**One item worth revisiting:** the commit includes
`editor/crates/psxed-project/assets/sky/cortex_sunset_clouds_v2.png` at about
392 KiB, a generated PNG. That's a large binary for a generated artifact. Either
accept it as the generator's pinned input, or make the generator fully
parametric and drop the file in a follow-up. Not urgent, and **not worth
rewriting history for**, since this branch has concurrent authors.

## 2. Ownership map, verified

Audited before touching anything, because the tree has three authors in it.

| Path | Owner | Action |
|---|---|---|
| `docs/infinite-horizon-study-2026-08-23.md` (+87) | Me, this session. Sections 3.2 and 3.3 only | Keep, don't revert |
| `editor/.../src/ui_types.rs` (+8/-8) | Nobody's feature. Pure `cargo fmt` reflow of match arms | Leave alone |
| `editor/.../playtest/tests/ui_options.rs` (+4/-4) | Same rustfmt reflow | Leave alone |
| Everything else from the sky attempt | Committed as `12644fac` | Done, section 1 |

The sky attempt's files (`material_lab.rs`, `scene_grid_types.rs`, `tests.rs`,
both Cargo files, the fixture, the generator and the assets) are no longer
working-tree changes. They are in `12644fac`.

Note the study doc is already committed on this branch (in `2f969b46`); the
local diff is only my two added sections.

**Anyone picking this up:** re-run `git status` before acting. This branch had
three separate authors modifying it within a single hour, and the state above
was accurate at the time of writing, not necessarily now.

Also: earlier session work on `main` (emulator MMIO attribution, guest link-map
hook, `docs/phase1-profile-2026-08-23.md`) is not on this branch. It is not lost,
it lives on `main`. Don't go looking for it here.

---

## 3. Why the layered sky cannot be tuned into the reference look

This is a property of the mapping, not of the artwork, so no amount of atlas
iteration fixes it. `directional_texel` (`engine/crates/psx-bsp/src/sky.rs:61`)
does:

```
direction[2] *= 3            // scale the vertical axis
normalise by |direction|
u = direction[0] / |d| * k   // horizontal component only
v = direction[1] / |d| * k   // horizontal component only
```

The vertical component is consumed by the normalisation and then discarded. That
gives three defects, all visible in the captures:

1. **The mapping is 2:1.** `u,v` never see the sign of the vertical axis, so a
   ray 30 degrees above the horizon and one 30 degrees below at the same azimuth
   produce the *same texel*. That's the mirror symmetry across the lower band of
   `quake-layered-sunset-sky-only.png`.
2. **Unbounded stretch at the horizon.** As a ray approaches horizontal its
   horizontal components approach the unit circle, where UV changes vanishingly
   slowly with pitch. Screen rows near the horizon therefore sample nearly the
   same texel row. That's the vertical columns.
3. **Zenith convergence.** All up-rays tend to `(0,0)`, so the top swirls into a
   point.

Quake shipped this because sky was seen through small overhead apertures. It
never filled the screen and never reached the horizon, so defects 1 and 2 were
invisible. Our requirement is the exact opposite: full-screen, horizon-anchored,
composed.

**Conclusion: keep this renderer as an optional animated cloud overlay. It must
not define the horizon.**

---

## 4. Proposed architecture

Four separable pieces. Only the first two are the "skybox".

### 4.1 Directional cube environment (the skybox)

Replace the mapping, keep the mechanism. The existing lattice already provides
everything except the projection: a bounded screen-space grid, view-ray
generation, OT slot 2047, and aperture semantics. That infrastructure is sound.
Swap `directional_texel` for a cube-face lookup.

Per lattice vertex:

```
major = axis of max(|x|, |y|, |z|)      // selects one of 6 faces
u = other_axis_a / |major|              // one divide
v = other_axis_b / |major|              // one divide
```

Properties that matter here:

- **No pole singularity.** Every face is a well-conditioned gnomonic patch.
- **No folding.** The mapping is a bijection over directions.
- **Bounded distortion.** Worst case is the face corner, about 1.7x, versus the
  current mapping's unbounded horizon stretch.
- **Probably cheaper than what's there now.** The current path computes an
  `isqrt` per vertex. Cube lookup needs no square root, just a magnitude compare
  and two divides. Verify this rather than trusting it, but the arithmetic
  favours the replacement.
- **A fixed sun becomes trivial.** It's baked into a face and is automatically
  stable under yaw. The scrolling two-layer model can never do this, which is
  the single clearest reason the reference look was unreachable.

**Seam handling is the main implementation risk.** A lattice cell straddling a
face boundary would interpolate UVs across a discontinuity and produce a garbage
band. Standard fix: select the face from the *cell centre* ray, project all four
corners onto that face's plane, and let UVs run slightly outside the face,
backed by a few texels of border copied from the neighbouring face. Budget the
border explicitly; don't discover it late.

### 4.2 The 4bpp colour problem, which is the real difficulty

4bpp means **16 colours per CLUT**. A smooth sunset gradient in 16 colours is
the hardest constraint in this whole plan, harder than the geometry. Plan for it
up front:

- **Per-face CLUTs.** Six faces, six palettes, 96 colours total. Horizon faces
  spend their 16 on warm oranges; the zenith face spends its on purples.
- **Per-band CLUTs within a face.** The existing cyclorama already does exactly
  this (`SKY_PANORAMA_PALETTE_BANDS`, "a dedicated CLUT row per altitude range").
  Reuse the idea. A face split into 4 horizontal bands gets 64 effective colours
  on its own.
- **Dither at bake time.** Ordered dithering in the offline atlas generator is
  not optional for 16-colour gradients. Tune it so it reads as film grain at
  320x240 rather than as pattern.

Sizing: six 64x64 4bpp faces is 12.0 KiB of texels; with a 4-texel border,
15.2 KiB. Against roughly 1 MiB of VRAM with ~300 KiB of double-buffered
framebuffer, that's affordable with room to spare. Larger faces are negotiable
(see section 7.5); prove the colour approach at 64 first.

### 4.3 Background versus aperture

The requirement "must work when little or no opaque geometry is visible" is
incompatible with pure Quake aperture semantics, where sky draws only where sky
*brushes* exist. No sky brush, no sky.

**Recommendation:** draw the environment unconditionally at the far OT slot as a
camera-relative background, and let opaque world geometry paint over it via the
painter's algorithm. This decouples the sky from level authoring entirely and
satisfies the requirement directly.

Keep the aperture path available as a fill-rate optimisation for fully enclosed
interiors, behind a per-World setting. Don't delete it.

### 4.4 Screen partition with the ground, which is free performance

Worth designing in from the start rather than retrofitting. The sky only needs
to cover the screen **above** the horizon; the infinite ground disc covers
**below** it. Together they tile the screen exactly once with no mutual overdraw.

Given that today's profiling found GPU back-pressure is real and lands on
`FrameBuffer::begin_swap`, not paying for a full-screen sky *and* a full-screen
ground is worth having by construction.

### 4.5 Infinite ground

Unchanged from the study. Camera-locked polar mesh, rings spaced uniformly in
screen space via `d_k = FOCAL * h / y_k`, world-space UVs with centre snapping,
Gouraud fade into the sky's horizon colour, and `DepthPolicy::Fixed` slots
because the disc lives far beyond `FAR_Z = 1024`. See study sections 4 to 6.

The ground's rim colour must be sampled from the environment's horizon band, not
hand-tuned, or the two drift apart the moment the sky art changes.

---

## 5. Milestones

Each gated on captures before the next starts. Nothing touches the Cortex
project until milestone 6.

| # | Milestone | Gate |
|---|---|---|
| 0 | ~~Land the optional cloud material as its own commit~~ | **Done: `12644fac`** |
| 1 | Cube-face `directional_texel` replacement, flat debug colours per face | Six faces identifiable; no seam band; no fold; no pole |
| 2 | Offline cube atlas generator, 4bpp, per-face CLUTs, dithering | Gradient reads smooth at 320x240; sun present and fixed |
| 3 | Per-band CLUTs if milestone 2's colour count proves insufficient | Measured improvement, or dropped |
| 4 | Unconditional background mode at far OT slot | Renders with zero sky brushes; geometry occludes correctly |
| 5 | Infinite ground disc, joined to the sky horizon | Horizon stable under translation; no visible rim |
| 6 | Editor viewport parity | Same camera, editor vs runtime, composition agrees |
| 7 | Cortex integration | Full acceptance suite, section 6 |

Milestone 1 is the decision point for the whole approach. If cube-face selection
can't be made seam-free within the lattice, stop and re-plan rather than
papering over it.

---

## 6. Acceptance suite

From the handoff, made concrete. Every item is a capture unless stated.

**Directional coverage**
- Four cardinal yaws, plus the two diagonal yaws between them.
- Steep pitch up and steep pitch down.
- A slow 360 degree yaw sweep, checked as a sequence, not stills.

**Failure modes to rule out explicitly**
- No seams at cube face boundaries.
- No black holes or unwritten pixels.
- No vertical stretching near the horizon.
- No mirrored or repeated quadrants.
- No polar swirl at zenith or nadir.

**Structural**
- With representative level geometry, confirming full occlusion.
- With no opaque geometry visible at all.
- Horizon stability under pure camera *translation*, which is the test the
  previous attempt never had to pass.
- Editor and PS1-runtime capture from an identical camera, compared side by side.

**Budgets, recorded as numbers not adjectives**
- VRAM texels and CLUT count; confirm 4bpp throughout.
- Packet words and triangles per frame.
- RAM and arena headroom.
- Route cycles, plus an MMIO-stall capture to check the fill cost has not pushed
  the frame GPU-bound. See `docs/phase1-profile-2026-08-23.md` section 5.2 for
  the flag and the attribution command. **Take the baseline before milestone 1.**

**Process**
- Present captures for approval before modifying or cooking the Cortex project.

A still image cannot show horizon drift, seam shimmer under rotation, or dither
crawl. Any milestone judged only from stills is not judged.

---

## 7. Open decisions for you

1. **Cube versus alternatives.** Cube is my recommendation for the reasons in
   4.1. The alternative worth naming is a high-detail cylindrical horizon band
   plus a zenith cap, which spends resolution where the eye is but reintroduces
   a cap seam. I'd take the cube.
2. **Generated PNG in the repo** (section 1).
3. **Does the animated cloud overlay survive?** Section 4.1 keeps it available as
   a separate layer over the cube. It costs a second lattice pass. Worth it only
   if you actually want moving clouds over the static composition.
4. **Which project ships it first.** The study's open question stands: Cortex,
   quake-psx, or shared. The cube projection belongs in `psx-bsp` next to the
   existing kernel if it's to be shared.
5. **Face resolution.** 64x64 is the costed starting point (12.0 KiB, 15.2 KiB
   with a 4-texel border). 128x128 is 4x the texels: 48.0 KiB, or 54.2 KiB with
   borders. Both are comfortable against ~1 MiB of VRAM with ~300 KiB of
   double-buffered framebuffer, and 128 is materially better for a composed
   sunset. Decide after milestone 2 shows what 16 colours can actually do.

---

## 8. What I would not do

- Don't revive the cyclorama for this. The study already separates it, and it
  faceted and holed at the zenith.
- Don't try to fix the layered sky's projection in place. The defects are the
  mapping; see section 3.
- Don't bundle the optional cloud material with the skybox work in one commit.
- Don't cook Cortex before milestone 7.
