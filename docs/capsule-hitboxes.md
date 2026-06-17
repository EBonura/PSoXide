# Capsule hitboxes / damage system (design)

> Status: design. Branch `capsule-hitboxes`. No runtime code yet.
> Hard constraints: **32-bit only** — all collision math is i32/u32 fixed-point,
> no float, no 64-bit (`docs/`). The engine is CPU-bound, so every test is a
> squared-distance compare, branch-early, and reuses transforms the renderer
> already computes. Cited facts below are from the engine source; line numbers
> are indicative, names are stable.

## 1. Goal & scope

Attach collision volumes to skeleton joints so the damage layer can answer "did
attack volume X overlap hurt volume Y this frame, and where?". Volumes follow
the baked animation (they ride joints), are mostly *derived* (not authored) from
the shared rig, and are tested against a tiny per-frame budget.

We use **capsules** (segment `A..B` + radius `r`, a.k.a. swept sphere). Decision
is final; this doc is about making them correct and cheap in fixed-point.

First milestone (what this doc commits to building first): **player hurt
volumes + the claw attack volume, placed on the bones and drawn** — no hit
query yet. Everything else (queries, damage, reactions, enemies, projectiles)
builds on that and reuses the same primitive.

### Why capsules, not cubes (settled)

For *joint-attached, animated* volumes the practical cost order is
`sphere < capsule < OBB < mesh`:
- **AABB** is cheapest but can't rotate — useless on a swinging limb.
- **OBB** rotates but needs the joint's 3×3 orientation + a Separating-Axis
  test (up to 15 projections) — the priciest option on a multiply-bound CPU.
- **Capsule** rotates for free: store two endpoints, transform them by the
  joint matrix the renderer already produces, and the test is squared distance
  to a segment — no orientation matrix, no SAT, **no `sqrt`**.

Capsules are therefore both cheaper than cubes here *and* fit limbs better.

## 2. Shared humanoid rig: one template for every character

Every model rides the same Mixamo humanoid skeleton (same joint count, names,
order). So hurt volumes are **not authored per model** — there is one shared
**humanoid hurt template** keyed by canonical joint, reused by the player and
every enemy. This is where the code sharing lives:

- **One placement routine** `place(joint_world, capsule) -> (world_a, world_b)`.
- **One test** (Section 5).
- **One template**, auto-derived from the rig.

**Auto-derivation.** For a bone from joint `J` to its child `C`, the default
capsule is `A = J origin (0,0,0)`, `B = C's joint-local offset`, `radius =
fraction × bone_length`. Bone offsets come straight from the skeleton, so the
same template fits any rig-compatible model with zero authoring. Per-model
override is a single `radius_scale` (Q8), with optional per-region tweaks.

### 2a. Player hurt template (shared body capsules)

~11 volumes, each on the named joint, segment running down the bone to its
child. Default `radius` proposal: a fraction of the bone length so it
auto-scales per model (torso fatter, limbs thinner).

| Region        | Joint             | Segment        | Group | radius (×bone len) |
| ---           | ---               | ---            | ---   | ---                |
| Head          | Head              | sphere (`a==b`)| Head  | fixed ~0.12 unit   |
| Torso         | Spine1            | Hips .. Neck   | Torso | 0.45               |
| Upper arm L/R | Left/RightArm     | Arm .. ForeArm | Limb  | 0.30               |
| Forearm L/R   | Left/RightForeArm | ForeArm .. Hand| Limb  | 0.28               |
| Thigh L/R     | Left/RightUpLeg   | UpLeg .. Leg   | Limb  | 0.35               |
| Shin L/R      | Left/RightLeg     | Leg .. Foot    | Limb  | 0.30               |

Groups drive damage multipliers / reactions: `Head`, `Torso`, `Limb`. The
player is a humanoid character, so the player hurt set **is** this template — no
special-casing. (See Section 11 for the player-cylinder alternative.)

### 2b. Claw / weapon attack volume

The weapon is **built into the model mesh** (the Rust Mantis claws, skinned to
the hand/forearm joints) — not a separate equipped weapon. So the attack volume
is a capsule **authored on the hand joint, extending out along the claw to its
tip**. The claw tip is not a bone, so this one is authored per model (it cannot
be derived like the hurt template); it reuses the same primitive, flagged
`role = Attack`, active only on an attack's active frames.

- Built-in claws (current): per claw, on `Left/RightHand`, `a = hand origin`,
  `b = claw-tip offset`, `radius = claw thickness`.
- Future equipped weapon: same primitive authored on the weapon model, placed
  by its hand socket — no new machinery.

**Hurt body capsules are shared/derived; attack capsules are authored per
model** (the weapon/claw shape is not a bone). Same geometry, placement, and
test for all of them.

## 3. Data model

One primitive for hurt and attack, distinguished by `role` + active timing:

```
Capsule {
    joint:  u16,     // joint index in the cooked .psxmdl skeleton
    a:      [i16; 3],// endpoint A, joint-local units (same space as a vertex)
    b:      [i16; 3],// endpoint B, joint-local (a == b -> sphere)
    radius: u16,     // joint-local units
    role:   u8,      // Hurt | Attack
    group:  u8,      // Head / Torso / Limb / Weapon
    flags:  u8,      // enabled, ...
}
```

Endpoints live in **joint-local model units, exactly like a cooked
`ModelVertex.position`** (`psx-asset`, `Vec3I16`), so they inherit the same
quantization and `local_to_world_q12` scale — no new coordinate convention, and
they transform through the identical vertex path. This mirrors
`AttachmentSocket` (`resource_types.rs`: `joint` + local `translation`), the
existing per-joint authoring precedent — a capsule is "a socket with a second
point + radius".

## 4. Coordinate & transform integration (concrete)

- **World space** is `WorldVertex` = `RoomPoint` = `i32 × 3` (`transform.rs`).
  `1.0` unit = `0x1000` (4096) in the stored i32; range ≈ `±524,288` units.
- **Model-local** vertices/endpoints are `Vec3I16` (±8.0-ish units locally).
- **`JointWorldTransform`** (`render3d.rs`): `rotation: Mat3I16` in **1.3.12**
  (`0x1000` = 1.0), `translation: WorldVertex` (i32, world units). It already
  bakes `local_to_world_q12` and the instance rotation
  (`compute_joint_world_transform`). Use this one (world space), **not**
  `JointViewTransform` (camera space).
- **Placing an endpoint** is the existing vertex transform:
  `world.x = (row0 · a) >> 12 + translation.x`, etc., where `row · a` is the
  `dot_world_q12` pattern (`render3d.rs`): i16 row × i16 local, `>> 12`,
  saturating. Two endpoints per capsule → 6 MACs + 3 adds each. With ≤ ~12
  hurt + a few attack capsules per active character that is a few dozen
  transforms/frame — negligible, and reuses transforms the skin pass already
  has in hand.

## 5. Fixed-point collision math (the core)

### 5a. Primitive: point-vs-capsule (i32-safe)

Nearest point on segment `A..B` to point `P`, then squared-distance compare:

```
d   = B - A                       // capsule axis (world i32, small: bone-scale)
len2 = d·d                        // |axis|^2
t   = clamp( div_q12_i32(P_minus_A · d, len2), 0, 4096 )   // Q12 in [0,1]
Q   = A + mul_q12_i32(t, d)       // nearest point on segment
hit = sq(Q.x-P.x)+sq(Q.y-P.y)+sq(Q.z-P.z) <= sq(radius + r_p)
```

- `div_q12_i32` (`engine/fixed.rs`) is the **guarded divide**: zero denominator
  returns 0, no 64-bit, Q12 result. For a **sphere** (`a==b`) `len2 == 0` →
  `t = 0` → `Q = A` → it degrades to a pure point-point test. Free fallback.
- `sq()` is `square_i32_saturating` (`psx-math/int32.rs`): exact while
  `|x| <= 46,340`, saturates to `i32::MAX` beyond. Combined with
  `saturating_add`, the squared distance is **exact when the points are close**
  (≤ ~11 units apart — the only case that can be a hit) and **saturates to
  "definitely no hit" when far**. This is precisely the engine's existing
  pattern in `character_motor.rs::cylinder_overlaps`.
- Magnitudes stay in i32: for close pairs `P_minus_A` is small and the axis is
  bone-scale (≤ a few units = ≤ ~12k raw), so `P_minus_A · d` ≈ 450M worst case
  — comfortably inside i32. (Far pairs are rejected in broad-phase first, 5c.)

### 5b. Capsule-vs-capsule: sample, don't solve (and why)

True segment-segment closest distance needs the denominator
`|d1|^2 · |d2|^2 - (d1·d2)^2`. At our magnitudes `|d|^2 ≈ 4.5e8`, so that
product is ≈ `2e17` — **far past i32**, and pre-shifting to fit costs ~7 bits of
precision and is exactly the kind of fixed-point footgun that produced the
head-bounds clamp bug. We do **not** solve segment-segment.

Instead, **sample the (thin) attack capsule as N points** (endpoints + interior,
N = 3..5) and run point-vs-capsule (5a) against each hurt capsule, taking the
min. Every operation is i32-safe, it's cheaper than the full solve, and for hit
detection the sampling error (< segment_len / 2N) is well under a capsule
radius. The attacker is always the thin volume (a claw/blade), so sampling the
attacker against the fat hurt capsule is the accurate direction.

### 5c. Broad-phase (cull before the precise test)

Each capsule has a bounding sphere: `center = (A+B)>>1`, `radius = (|A-B|>>1) +
r` (the half-length uses `isqrt_i32`, `psx-math`, computed once per placement,
no per-test sqrt). Reject a pair with a sphere-sphere squared-distance test
(`sq(dx)+sq(dy)+sq(dz) <= sq(r1+r2)`) before 5a/5b. Far pairs die here in ~10
ops via the saturating compare. At our volume no spatial structure is needed.

### 5d. Determinism & edge cases

- All math is saturating i32 → fully deterministic across runs/platforms (no
  float, no UB).
- `a == b` → sphere (5a handles via `len2 == 0`).
- Degenerate radius 0 → still valid (segment/point distance).
- `div_q12_i32` zero-guard covers zero-length axis; `t` is always clamped to
  `[0, 4096]` so `Q` stays on the segment.
- No `sqrt` on the hot path; the only `isqrt` is once-per-placement for the
  broad-phase radius.

### 5e. GTE: not used (justified)

The GTE exposes no bare dot-product; `MVMVA/SQR/NCLIP/RTPS` are graphics ops,
and loading GTE registers to do a single 3-vector dot would cost more than the
scalar MACs for our handful of tests. Collision stays **scalar i32**, following
the `character_motor.rs` precedent. (Per `keep-work-on-gte-not-cpu`, that rule
targets the per-vertex render hot path, not a few dozen collision tests/frame.)

## 6. Performance budget (concrete)

NTSC vblank = **564,398 cycles** (`bus/timing.rs`), at the flat **2 cycles /
instruction** model (`cpu/timing.rs`, `BIAS = 2`) ≈ **282k instructions/vblank**.
A point-vs-capsule test is ~30-50 instructions; a sampled capsule-vs-capsule
(N=4) ~150-250; placement ~20/capsule. Worst realistic frame — one attacker's
few attack capsules × a target's ~11 hurt capsules, after broad-phase culls most
— is low hundreds of precise tests = a few thousand instructions ≈ **< 1% of a
vblank**. Collision will not appear in the per-vblank chart; no offload needed.

## 7. Cooked format

The model bundle already carries per-joint tables (joints, parts, vertices,
materials) + an attachment table. Add a capsule table:
- Header: `capsule_count: u16` (reuse a reserved header field) +
  `radius_scale_q8: u16` per-model override.
- Body: `capsule_count × CapsuleRecord`, fixed layout
  `joint:u16, a:i16×3, b:i16×3, radius:u16, role:u8, group:u8, flags:u8`
  (16 bytes; pad to alignment), after the existing part/vertex tables.
The runtime reads them once into a fixed pool at load, like parts. The shared
hurt template is **derived at cook time from the skeleton** and emitted into the
same table (so the runtime path is uniform); per-model authoring only adds
attack capsules + overrides, keeping cooked data tiny.

## 8. Runtime query + damage flow

```
HitQuery: active attacker capsules (role=Attack, in active frames)
        × enabled target capsules (role=Hurt, group mask)
  -> Vec<HitResult { attacker, target_group, approx_point }>
```

The **damage layer sits above** the pure-geometry collision layer and owns:
- group priority for multipliers (`Head > Torso > Limb`),
- **one hit per swing** (an attack carries an already-hit target set so a single
  swing can't multi-hit across frames),
- i-frames / hitstun, and spawning the hit reaction.

Geometry returns overlaps; it never knows about HP. First interaction wired:
**player claw → enemy hurt template**; enemy-attack → player and projectile →
either reuse the same query with swapped sets.

## 9. Authoring (editor)

Most volumes are derived, so authoring is light:
- Model inspector: a `radius_scale` slider + per-region radius tweaks; an
  **attack-capsule editor** (pick joint, drag `a`/`b`/radius) for the claws,
  reusing the attachment-socket UI pattern.
- The animation viewer already draws the skeleton overlay; draw capsules there
  (segment + end rings) so authors watch them ride the animation, with a "show
  on active frames" toggle for attack volumes. This is also the phase-1
  deliverable's acceptance check.

## 10. Phases

1. **Template + player hurt + claw attack, drawn.** Derive the humanoid template
   from the rig; emit the capsule table; place on the player via
   `JointWorldTransform`; author the two claw attack capsules; draw both as
   overlays in the animation viewer. **No queries** — just see them ride bones.
2. **Geometry core.** `sq`/`div_q12_i32`-based point-vs-capsule, sampled
   capsule-vs-capsule, broad-phase sphere — in a small `psx-*` crate, **unit
   tests first** (overflow saturation, `a==b`, far-saturates-to-no-hit, known
   overlaps).
3. **Query + damage.** Active-set, group masks, hit results, one-hit-per-swing,
   into a damage/reaction stub. Wire player→enemy.
4. **Tune.** Confirm cost on the emulator cycle model; widen to enemy→player and
   projectiles.

## 11. Risks & mitigations

- **Fixed-point overflow** (the head-bounds-bug class). Mitigation: relative
  vectors only; `square_i32_saturating` + saturating adds (exact-when-close,
  saturate-when-far); **no segment-segment solve**; unit tests target the
  saturation boundary (`|x|` near 46,340) and zero-length axes first.
- **Saturation correctness**: a far pair saturating to `i32::MAX` must read as
  "no hit" — guaranteed because it always exceeds `sq(r_sum)`. Tested explicitly.
- **Active-frame timing**: attack volumes must be live only on the right frames;
  drive from the animation/attack state, not a fixed window — a phase-3 concern,
  flagged so it isn't designed out now.
- **Template-vs-mesh mismatch**: derived capsules approximate the mesh; the
  per-region radius fractions are the tuning knob, validated visually in phase 1.

## 12. Decisions & open questions

Resolved:
- Capsules, not cubes (Section 1).
- One shared derived humanoid hurt template; attack capsules authored per model.
- Built-in claws → attack capsule on the hand joint to the claw tip.
- Point-segment + sampling, **not** segment-segment (i32 overflow, 5b).
- Overflow handled by the `square_i32_saturating` + relative-vector pattern, not
  a bespoke hit-unit space.

Open:
- **Player hurt: template vs movement cylinder.** The runtime already maintains a
  `CharacterCollisionCylinder` (position/radius/height, `character_motor.rs`) for
  movement; reusing it for a *coarse* player hurt volume is nearly free and i32-
  proven. The template gives per-limb/locational damage and parity with enemies.
  Default plan: **template for parity**, with the cylinder as a cheaper coarse
  fallback if a character doesn't need locational hits.
- Final per-region radius fractions (phase-1 visual tuning).
- Capsule budget cap per character (proposed 16).
- Sample count N for attack capsules (proposed 4; tune in phase 2).
