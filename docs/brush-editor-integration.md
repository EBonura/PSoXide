# Brush editing in psxed: integration design

Status: design drafted 2026-08-10, branch `quake-bsp-world`. Companion to
docs/quake-bsp-migration-plan.md (P4). Brush editing is an independent
implementation built from the public Quake `.map` format semantics and
standard computational-geometry algorithms; it integrates with psxed's
existing interaction substrate rather than replacing it.

## Design principle

Brushes are strictly Quake-map-shaped: every face plane is defined by
three integer grid points, all vertices land on (or snap to) integer
coordinates, and brushes are convex by construction. Integer 3-point
planes make plane predicates exact (cross products and dots in wide
integers), which removes most robustness hazards float-plane editors
fight. Host-side solving may use f64 internally; stored data is integer,
consistent with the repo's integer-first discipline (the runtime numeric
guard applies to the guest only).

## What gets built

- **Brush kernel** in `psxed-project/src/brush.rs`: `BrushFace` (three
  integer points defining the plane, material, UV state) and `Brush`
  (face set). Plane derivation from the 3 points, plane-set to
  face-polygon solving (a base winding on each plane clipped by all other
  planes, Sutherland-Hodgman), degenerate-face dropping, validity checks,
  bounds and vertex dedup.
- **Builder primitives**: axis-aligned cuboid first (create tool +
  brush prefabs), wedge/cylinder recipes later.
- **Create tool**: drag a rect in the 2D view (or on a 3D face plane)
  plus a height, producing a cuboid brush.
- **Extrude (face drag)**: translate a face plane along its normal and
  re-solve; generalizes the existing BoxProp face-handle drag
  (`NodeGizmoHandle::BoxFace`) to arbitrary normals.
- **Clip tool**: up to three points define a plane; split the brush into
  front/back; keep either or both.
- **Vertex/edge tools**: topology-aware vertex moves. Built LAST; this is
  the hardest piece and the initial release is complete without it
  (clip + extrude cover most shaping).
- **Face UVs**: paraxial (dominant-axis) projection as the default, with
  Valve-220-style per-face U/V axes + offset/rotation/scale as the stored
  form, and texture lock on move/rotate. psxed's numeric UV inspector and
  material paint/eyedropper remain the interaction surface.

## What psxed already provides (survey 2026-08-10)

Reused as-is: camera rig, ray-picking entry points, gizmo
drawing/hit-testing, grid + quantum snapping, marquee and structured
selection, Face/Edge/Vertex selection modes with welded/detached
semantics, prefab floating-placement UX (rotate/flip/nudge/commit),
snapshot undo (`history.rs`), the material system, `editor_preview`
packet rendering with `overlays.rs` outlines, and the headless
ViewportHarness, so every brush tool lands with pointer-level tests.

Picking is a linear ray-vs-convex-brush test (nearest front-facing plane
hit lying inside the face polygon); a BVH waits until real maps demand
it. `NodeKind::Brush` joins the scene model; brush ops ride the existing
snapshot undo unchanged.

Prerequisite refactor: viewport tool dispatch is a hard-coded if/else in
`workspace/viewport.rs`; brush/clip/vertex tools need the planned
`trait Tool { hover/begin/update/end/draw }` FSM (migration plan P4).
`enum Interaction` already models in-flight strokes and stays.

Ortho views are phased: the existing top-down 2D pane learns brush
create/drag first; front/side panes and any docking come after the 3D
tools work.

## Integration order (each step lands green on its own)

1. Tool-FSM refactor of the existing tools (no behavior change; existing
   psxed-ui tests + ViewportHarness stay green).
2. Brush kernel + unit tests (cuboid, wedge, clip split, degenerate
   rejection, integer snap round-trips).
3. `NodeKind::Brush` + editor_preview rendering + picking + selection
   wiring: brushes visible and selectable, no editing tools yet.
4. Create tool + extrude. At this point simple rooms can be boxed out
   end to end.
5. Clip tool; UV axes + texture lock; vertex tools last.
6. Play stopgap before the P5 compiler exists: cook brushes to the
   existing free-surface record path (the `PlaytestBoxPropSurface`
   pattern) with per-brush AABB collision, so brush maps render and are
   roughly walkable in embedded Play. The real BSP/vis/light compile and
   clipnode collision replace this at P5, with collision arriving via the
   quake-core crate from the quake-psx all-Rust port.

## Open items

- Brush worlds stay single-room (no grid streaming/visibility) until
  P5/P6 land; fine for authoring-tool development.
- Face-UV texture-window interaction with the windowed classic-affine
  work in flight on the parallel branch; reconcile when it lands on main.
