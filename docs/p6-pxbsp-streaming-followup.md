# P6: PXBSP microsection streaming (deferred follow-up)

Status: DEFERRED, not started, not on the finish-line critical path.
Written 2026-08-11 so the design survives the current campaign and a later
worker can pick it up without re-deriving it.

## 1. Why this document exists

The PXBSP runtime is **whole-map resident** today. That is a deliberate,
measured choice for the composed souls-like demonstration, not an
implementation gap that was overlooked. Two facts must not be blurred:

- Legacy grid-room streaming, which does exist and works, is NOT evidence
  that BSP streaming works. They are different pipelines.
- The PXBSP `StreamingIndex` lump is currently EMPTY, and streamed BSP
  textures are not implemented. A small cooked PXBSP file size is therefore
  never on its own proof that a level fits.

The finish-line rule that follows from this: the demonstration ships
whole-resident only if MEASURED budgets show safe headroom, and the
documentation says so plainly. If it does not fit, P6 becomes blocking and
the correct response is to stop and report, never to quietly delete content
or raise a capacity past what the hardware can hold.

## 2. What must be measured before claiming residency

Report all of these at the final head, from the build output rather than by
estimate, and state the PS1 limit each is measured against:

- final MIPS executable size;
- `.data` and `.bss`;
- stack reserve;
- every runtime arena capacity (packet arena, resident map budget, PVS row
  budget, collision scratch, audio buffers);
- cooked PXBSP size;
- texture bytes, both resident and VRAM-page footprint;
- total RAM and total VRAM, each with its remaining headroom.

A claim of the form "the PXBSP is only N bytes, so it fits" is not
acceptable evidence and must be rejected in review.

## 3. Seams that must stay intact

These exist now and are the anchor points a future implementation needs.
None of them may be deleted merely because they are currently unused:

- the PXBSP `StreamingIndex` lump (reserved, currently empty);
- the `ReadAt` abstraction that resident loading already goes through;
- the pack and residency boundaries in the cooker and the resident map;
- the fail-closed residency budget checks.

## 4. Intended architecture when P6 lands

**Resident core, pageable skin.** Collision hulls, the clip nodes, the PVS
rows, and the entity/gameplay tables stay resident: they are small, they are
touched every tick, and paging them would put disc latency inside the
movement and combat paths. Geometry payloads and textures become pageable.

**Microsections aligned to visibility, not to a grid.** The cooker should
partition drawable geometry into small chunks bounded by BSP leaf clusters
and, where the level provides them, by PVS boundaries and physical
chokepoints (doorways, corridors, stairwells). Aligning chunk boundaries
with the places the player's visibility set actually changes is what makes
prefetch predictable; a spatial grid over a BSP world does not.

**Chunk size.** An initial experimental target is 1 to 4 CD sectors per
geometry chunk, subject to measurement. Small chunks reduce peak residency
and make eviction cheap; too-small chunks waste sector overhead and multiply
index entries. This number must be tuned against real seek and read
measurements, not assumed.

**Shared textures are separate from geometry chunks.** A texture used by
several microsections must live in its own pack with its own residency and
reference accounting, otherwise a single shared texture forces its geometry
chunk to stay resident or gets duplicated across chunks.

## 5. Work items when P6 is picked up

1. Populate the `StreamingIndex` in the cooker: chunk table, per-chunk
   sector extents, texture dependencies, and the leaf-to-chunk mapping.
2. Resident core split: prove collision, PVS, and entity state remain fully
   resident and allocation-free while geometry and textures page.
3. Pageable geometry and texture loading through the existing `ReadAt`
   boundary, with fixed-capacity residency and a documented overflow policy.
4. Prefetch driven by the PVS row plus the player's motion, not by radius.
5. Eviction with hysteresis so a player standing in a doorway cannot thrash
   two chunks against each other.
6. Contiguous disc ordering: lay chunks out in expected traversal order so
   the common path is a short seek, and record the ordering in the pack so
   it is reproducible.
7. Telemetry: per-chunk load latency, residency high-water mark, prefetch
   hit and miss counts, eviction counts, and stalls attributable to paging.
8. Tests: fast-forward traversal (running a route faster than prefetch
   expects), backtracking (immediately reversing at a chunk boundary),
   and a worst-case visibility scene.
9. Hardware validation: paging behaviour is a CD-latency question, so
   emulator timing is indicative only and the console run is mandatory.

## 6. Documentation rule

Until P6 genuinely lands and is proven, the final documentation must say:

> Comicon BSP demo uses measured whole-map residency; P6 microsection
> streaming remains deferred.

Do not soften that sentence, and do not replace "measured" with an estimate.
