# GTE camera/projection kit (design note)

Status: design pending a decision; no code change yet. Written 2026-07 during
the downstream audit.

## The duplication

Three games grew first-person/3D projection scaffolding on top of
`psx-gte::scene`, each a different flavor of the same needs:

- voxide (~260 lines): `Camera { pos, yaw, pitch }`, `gte_load_camera`
  (rotation build + screen offset + projection plane), the
  camera-relative-chunk-origin pattern (`gte_begin_chunk`, documented
  cross-chunk-crack rationale), `project_quad_gte` (RTPT + RTPS with an SZ3
  near-band software fallback), `project_world_clamped` scalar path, a 1/z
  reciprocal table, and the `quad_exploded` span guard (itself borrowed from
  hl-psx).
- hl-psx `render.rs` (271 lines): near-plane + guard-band Sutherland-Hodgman
  clip with software reprojection over `scene::Projected`; header says it is
  the second copy, ported from oot-psx `room.rs`.
- gh-psx `highway.rs` `Builder::quad3d` (~30 lines): project 4 corners via
  `scene::project_vertex`, reject behind-camera/off-screen, arena-push,
  depth-insert. The minimal immediate-mode glue.

## Why this is NOT a mechanical extraction

- The GTE-not-CPU rule: any shared helper must keep work on the GTE; the
  house memory records that scalar "wins" in the emulator can be silicon
  losses. Every extraction must be re-verified on hardware (HWB-011 lineage:
  an MTC2 commit slip in MVMVA corrupted geometry on console while the
  emulator looked fine).
- The three flavors disagree on clipping strategy (software S-H clip vs SZ3
  near-band fallback vs reject-only), and the right one depends on the
  game's geometry (rooms vs voxel chunks vs a fretboard). A shared kit
  should offer the strategies, not pick one.
- voxide's kit assumes camera-relative chunk origins (i16 range management);
  hl-psx's assumes world-space rooms pre-translated. The camera-load helper
  is shareable; the origin policy is not.

## Proposed shape (when taken up)

Extend `psx-gte::scene` with, in order of confidence:

1. `CameraQ12 { pos: Vec3I32, yaw: u16, pitch: u16 }` +
   `load_camera(&CameraQ12, screen_offset, proj_plane)`: pure register
   setup, identical across all three games. Lowest risk, highest reuse.
2. `project_quad(...) -> Option<[Projected; 4]>` with a
   `NearPolicy::{Reject, Sz3Fallback}` knob (voxide + gh-psx flavors).
3. The near-plane + guard-band clipper as a separate, documented module
   (hl-psx/oot flavor); it is bigger and only room-streaming games want it.

Each step migrates its source game in the same change and gates on that
game's existing visual checks (voxide `make smoke`, hl-psx
`psoxide-map-smoke`), plus one hardware session before the pattern is
declared safe (emulator GTE parity is good but the rule is silicon first).

## Trigger

Take this up when the sibling-checkout games are building against a main
that has it, i.e. after the emu branch merges; or when the next 3D project
starts and would otherwise grow flavor number four.
