# Capsule hitboxes / damage system (design)

> Status: design. Branch `capsule-hitboxes`. No runtime code yet.
> 32-bit only: all collision math is i32/u32 fixed-point, no float, no 64-bit
> (see `docs/`). Engine is CPU-bound, so every test is squared-distance,
> branch-early, and reuses transforms the renderer already computes.

## Goal

Attach collision volumes to skeleton joints so the damage system can answer
"did attack volume X overlap hurt volume Y this frame, and where?". Volumes
follow the baked animation (they ride the joints), are authored per model in
the editor, cooked into the model bundle, and tested at runtime against a tiny
per-frame budget.

We use **capsules** (a segment `A..B` plus a radius `r`, a.k.a. swept sphere).
Decision is final; this doc is about making them cheap.

### Why capsules, not cubes (recap)

For *joint-attached, animated* volumes the cost ranking on PS1 is
`sphere < capsule < OBB < mesh`:
- AABB is cheapest but cannot rotate with a limb -> useless on a swinging arm.
- OBB rotates but needs the joint's 3x3 orientation + a Separating-Axis test
  (up to 15 projections, many muls) -> the priciest practical option on a
  multiply-bound CPU.
- A capsule rotates "for free": store two endpoints, transform them by the
  joint matrix the renderer already produces, and the test is
  point/segment-to-segment **squared** distance vs `(r1+r2)^2` -- no
  orientation matrix, no SAT, no `sqrt`.

So capsules are both cheaper than cubes here *and* fit limbs better.

## Representation

```
HurtCapsule {
    joint: u16,         // joint index in the cooked .psxmdl skeleton
    a: [i16; 3],        // segment endpoint A, joint-local units (like a vertex)
    b: [i16; 3],        // segment endpoint B, joint-local
    radius: u16,        // local units
    group: u8,          // bitmask: which layer (head/torso/limb/weapon...)
    flags: u8,          // enabled, attack-vs-hurt, damage-multiplier index...
}
```

Endpoints live in **joint-local space** exactly like a cooked vertex, so they
inherit the same model-local quantization and `local_to_world_q12` scale -- no
new coordinate convention. This mirrors `AttachmentSocket`
(`resource_types.rs:745`: `joint` + local `translation`), which is the existing
per-joint authoring precedent; a capsule is "a socket with a second point and a
radius".

A degenerate capsule (`a == b`) is a **sphere** -- free fallback for the head
or a fist, tested by the same code with the segment collapsed to a point.

## Attachment / per-frame update

The renderer already computes a `JointWorldTransform` per joint per frame
(`render3d.rs:526`, world space, before the camera view -- that is the one we
want, not `JointViewTransform` which is camera-space). To place a capsule:

```
world_a = joint_world[capsule.joint] * capsule.a
world_b = joint_world[capsule.joint] * capsule.b
```

Two point transforms per capsule, reusing transforms we already pay for. No
per-capsule matrix decomposition. With ~5-10 capsules per character that is a
few dozen GTE/`RTPS`-class transforms per frame -- negligible.

Only update capsules that can participate this frame (an attack's active
frames, or a character that is on-screen / in range).

## Efficiency rules

1. **Squared distances only.** Compare `d2 <= (r_sum)^2`; never take `sqrt`.
2. **Broad-phase bounding sphere first.** Each capsule has a cheap bounding
   sphere (`center = midpoint(a,b)`, `radius = halflen + r`). Reject pairs with
   a sphere-sphere squared-distance test before the segment-segment math. Most
   pairs die here in a handful of muls.
3. **Fixed-point overflow is the real constraint.** Squaring world coordinates
   overflows i32. Do every test on **relative vectors in a bounded local
   space**: subtract one endpoint first so components stay within
   `reach + capsule_span` (small), then square. Pick a Q-format with headroom
   (candidate: positions in a hit-unit ~1/16 of a world unit so `d2` fits i32
   for any plausible reach). This must be nailed down before coding -- a wrong
   shift here silently clamps like the head-bounds bug did.
4. **Tiny budget.** A few attack volumes vs a few hurt volumes per frame =
   low tens of segment tests worst case. Even full segment-segment is free at
   this scale; no spatial structure needed yet.
5. **GTE optional.** The dot products map to GTE ops if a test ever gets hot,
   but it will not at this volume -- keep it scalar first, measure, only then
   move to the GTE (and per `keep-work-on-gte-not-cpu`, never the reverse).

## The math (fixed-point)

- **Point vs capsule** (and capsule-vs-sphere): clamp the point's projection
  onto segment `A..B` to `[0,1]` (param `t = clamp(dot(P-A, B-A)/|B-A|^2, 0,1)`,
  done with a divide guarded for the `a==b` sphere case), nearest point
  `Q = A + t*(B-A)`, hit if `|P-Q|^2 <= (r + r_p)^2`.
- **Capsule vs capsule**: closest distance between segments `A..B` and `C..D`
  (clamped-parameter closest-point-on-two-segments), hit if
  `d2 <= (r1 + r2)^2`. One guarded divide; the rest is dot products and
  clamps.
- All in i32 fixed-point; the one division per test uses the engine's existing
  guarded `muldiv`-style helper, not float.

## Cooked format

Extend the model bundle (it already carries per-joint data + an attachment
table). Add a capsule table: `capsule_count: u16` in the model header (reuse a
reserved field) + `CapsuleRecord` entries after the part/vertex tables. The
runtime reads them once at load into a fixed pool, like parts. Keep authoring
in `ModelResource` (a `hurt_capsules: Vec<HurtCapsule>` beside `attachments`),
cooked alongside the model.

## Runtime query + damage flow

```
HitQuery: attacker capsules (active) x target capsules (enabled, group mask)
  -> Vec<HitResult { attacker_group, target_group, approx_point }>
```

The damage layer consumes hits: pick the highest-priority target group
(head > torso > limb for multipliers), apply damage once per swing (track an
already-hit set per attack so one swing does not multi-hit), spawn the hit
reaction. The collision layer stays pure geometry; damage/iframes/reactions sit
above it.

Primary interaction to confirm with the user: **player weapon -> enemy hurt
capsules** is the first target; enemy attack -> player and projectile -> either
come after.

## Authoring (editor)

Per-joint capsule editor in the model inspector (reuse the attachment-socket UI
pattern): pick a joint, drag the two endpoints + radius in the preview, assign
group/flags. The animation viewer already renders the skeleton overlay -- draw
the capsules there (wireframe segment + radius rings) so authors see them move
with the animation, including a "show on attack frames" toggle.

## Phases

1. **Format + author + draw.** `HurtCapsule` in `ModelResource`, cooked
   `CapsuleRecord` table, editor authoring + overlay in the animation viewer.
   No queries yet -- just see capsules ride the bones.
2. **Geometry core.** Fixed-point point/segment + segment/segment tests +
   broad-phase sphere reject, in a small `psx-*` crate with unit tests
   (overflow cases first).
3. **Runtime query + damage.** Per-frame active-set, group masks, hit results,
   one-hit-per-swing, hook into a damage/reaction stub.
4. **Tune.** Benchmark on the emulator cycle model; only GTE-offload if a test
   shows up in the per-vblank chart.

## Open decisions (need answers before phase 2)

- Hit-unit / Q-format and overflow bound (the #3 above) -- pick concrete shifts.
- Capsule budget per character (cap at e.g. 8?).
- Primary hit interaction order (player->enemy first?).
- Does the player also need hurt capsules, or is the player a simpler
  cylinder (the runtime already treats actors as vertical cylinders for
  movement collision -- maybe reuse that for player hurt)?
