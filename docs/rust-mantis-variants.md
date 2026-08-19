# Rust Mantis variants

One enemy, four silhouettes. The plan and, more usefully, what each axis
actually costs, because they are not remotely equal.

## The three axes

A variant differs along one or more of these, and the cost difference between
them decides the whole approach.

**Tint and scale are free.** The character record carries a
`material_override` with `tint_rgb`, and the model renderer carries
`visual_scale_q8` while the character carries `radius`/`height`. A recoloured,
resized enemy costs zero geometry, zero clips and zero disc. This is the axis
to lean on hardest.

**Equipment is nearly free.** Which weapon hangs off a socket is an Equipment
record on the entity, not a property of the model. Light versus heavy is two
records against one mesh. They also inherit the wireframe materialise effect,
since that is driven per equipment record.

**Mesh cuts cost a model each.** Every cut needs its own `.psxmdl`, and, the
part that bites, **its own copy of every clip**: cooked animations are pushed
without dedupe (`cook_entities.rs`, the `ModelAnimation` push), unlike textures
which dedupe on content. Two models sharing one rig and one set of gaits still
pay twice.

## Why the mesh cuts are cheap to make

The mesh is a single object (`robot`, 451 verts, 293 faces) but is built from
**54 disconnected islands**, and every vertex belongs wholly to one bone's
island. Measured on the claw: 58 vertices, 26 faces, and **zero faces
straddling the boundary**. So a limb comes off as whole islands with no torn
faces, no holes and no reweighting. `tools/`-side the cut is one Blender pass
(see the variant script in the campaign notes).

## The four variants

| # | variant | mesh | clips | other |
| --- | --- | --- | --- | --- |
| 1 | claw (original) | A, as imported | idle/walk/run + 2 claw attacks | hitbox on `LeftHand` |
| 2 | light weapon | B, claw removed | shares 1's set + weapon attacks | `left_hand_grip` socket, light Equipment record |
| 3 | heavy weapon | **B, same mesh as 2** | same as 2 | heavy Equipment record |
| 4 | crawler | C, legs removed | its own locomotion | different collision height, AI review |

Variants 2 and 3 share a mesh. The only difference between them is which
Equipment record the entity carries, so the second weapon costs one record.

Mesh A is 451 verts / 293 faces; mesh B is 393 / 267.

## Order of work

**1 first.** The mesh already exists, and it is the variant that proves the
attack path end to end: an attack clip bound to the character's action set, a
damage hitbox, and the enemy FSM actually striking. None of that exists on this
character yet, which is the real gap. Right now it has one hurtbox, no attack,
stagger or death clip, and no damage volume, so it can be hit and cannot hit
back.

**2 and 3 together**, because they are one mesh and one socket apart. The
weapon grips need measuring for this arm; the swords were fitted to Aletha's
hand and this is a different limb length. That is the same measure-and-tune
pass already done for her, not new machinery.

**4 last.** It needs its own locomotion (a crawl cannot borrow a biped's walk),
a different collision height, and a look at whether the enemy FSM's spacing and
circling still read on something that crawls.

## Decide before building more than two

Clip RAM is the ceiling and it is already close. Three 125-frame clips at 22
joints overflowed guest `.bss` by 33 KB, which is why the shipped gaits are
trimmed to 46, 30 and 28 frames. Four variants each carrying their own copy of
a shared set multiplies that by four for no visual gain.

Two ways out, and the first is small:

- **Dedupe cooked animations by content**, mirroring what the texture path
  already does. Byte-identical clips then collapse to one asset however many
  models reference them. This looks like a contained change and it is what
  makes a four-variant enemy affordable.
- Or accept the multiplier and keep the variant count down.

Worth settling before variant 4 exists, not after.
