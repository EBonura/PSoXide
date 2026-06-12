# Plan: merge Material and Texture into one resource

Status: DELIVERED 2026-06-12 (all phases). Validation: cortex_ignition_v1
cook output byte-identical to the pre-merge baseline in all generated
files except `///` doc-comment label lines in level_manifest.cooked.rs;
psxed-project 280, psxed-ui 221, frontend 110 tests green; migration
round-trip test covers fold/convert/drop and reload stability.

## The question

The editor currently has two resource kinds for one mental object:

- `ResourceData::Texture { psxt_path }`: a cooked `.psxt` image on disk.
- `ResourceData::Material(MaterialResource)`: `texture: Option<ResourceId>`,
  `blend_mode`, `tint`, `face_sidedness` (scene_grid_types.rs:211).

Authoring anything paintable means importing a texture, then creating a
material that wraps it, then assigning the material. Two browser
sections, two names for the same thing, one extra indirection.

## Evidence from the real project (cortex_ignition_v1)

- 674 Texture resources, 659 materials with a texture.
- 658 distinct textures referenced by materials: exactly **one** texture
  is shared by two materials. Cardinality is 1:1 in practice.
- 16 Texture resources are referenced by nothing at all (import leftovers
  that never became materials), pure dead weight the split encourages.
- Direct texture-id consumers that bypass Material exist only in three
  schema spots: UI image nodes (`ui_types.rs` UiNodeKind::Image),
  far-vista ring/panels (`world_types.rs`), particle emitter masks
  (`resource_types.rs` ParticleResource). In cortex these are the only
  reason Texture is a resource kind at all.
- Model atlases already bypass both concepts: `ModelResource.texture_path`
  is a plain file path, not a resource id. Precedent: image files as
  assets, not resources.
- On PS1 there is no shader split to justify two objects: a texture page
  plus blend/tint bits IS the material.

Conclusion: the split costs daily authoring friction and buys almost
nothing. Merging is wise.

## Target model

One user-facing resource kind, keeping the name **Material**, which owns
its image:

```rust
pub struct MaterialResource {
    /// Path to the cooked .psxt image (was the Texture resource).
    pub psxt_path: String,
    pub blend_mode: PsxBlendMode,
    pub tint: [u8; 3],
    pub face_sidedness: MaterialFaceSidedness,
}
```

- `ResourceData::Texture` disappears from the user model (kept as a
  parse-only legacy variant during migration).
- Texture import creates a Material directly; imported art is paintable
  immediately.
- Two materials wanting the same image with different tints reference
  the same `psxt_path` (file-level sharing). The single real shared case
  in cortex shows how rare this is.
- Raw-image consumers (UI images, far vista, particle masks) reference a
  Material id and read only its image part; tint may even become useful
  there later.

## The one technical knot: dedupe must move from id to path

Today VRAM packing, cook world-face records (`world_cook.rs:369`), the
frontend `editor_textures` atlas slots, and per-room streaming residency
(`streaming.rs` `SceneResourceUse.textures`) all key texture pages by
the Texture **ResourceId**. Two materials sharing one texture share one
VRAM page because they share that id.

After the merge the shared key is the **resolved psxt path**. Path-keyed
dedupe must land before or with the schema change, or shared-image
materials would double their VRAM and streaming cost. Residency tracking
should key on texture pages (paths), not on materials.

## Migration (load-time, one-way)

All world grids, image/box props, and brush state reference **Material**
ids (653 of them in cortex); UI images, far vista, and particles
reference **Texture** ids. Both must survive:

1. On project load, when legacy Texture resources exist:
   - For each Material referencing a Texture: fold that texture's
     `psxt_path` into the material. Material ids do not change, so every
     grid/prop reference stays valid.
   - For each Texture id referenced directly (UI/vista/particles): remap
     the reference to the Material that wrapped that texture; if none
     exists, synthesize a Material reusing the Texture's own id so the
     reference stays valid without rewriting.
   - Unreferenced Textures (the 16): drop, report in the load log.
2. Saving writes only the new format. The legacy variant parses for a
   deprecation window, then gets deleted.
3. Validation gate: load + migrate + save + cook every project under
   `editor/projects/`, and byte-compare the cooked output (stream
   chunks, level manifest) against a pre-migration cook. The merge is an
   authoring-schema refactor only; cooked formats and the runtime are
   untouched, and identical cook output proves it.

## Phases (each shippable alone)

1. **Prep**: key VRAM/atlas/streaming dedupe by resolved psxt path
   instead of texture id. Behavior-neutral today (paths are unique per
   texture id). Add cooked-output-equality test fixtures.
2. **Schema + migration**: merged `MaterialResource` with `psxt_path`,
   load-time fold + id remap, legacy parse support, round-trip and
   cook-equality tests on cortex and the demo projects.
3. **Consumers**: switch UI image nodes, far vista, particle masks to
   material references (mechanical after phase 2's remap).
4. **Editor UI**: single browser section (texture filter instead of a
   separate kind), import dialog creates Materials, material inspector
   absorbs the texture preview/swap UI, delete Texture-only panels.
5. **Cleanup**: remove `ResourceData::Texture` and the legacy parse path.

## Risk notes

- Biggest real risk is the id remap for direct texture references; it
  must happen atomically inside load migration, covered by round-trip
  tests on the real project file.
- The frontend (`emu/crates/frontend/src/editor_textures.rs`) and the
  embedded-play cook both resolve textures; both move to path keying in
  phase 1 and need no further change later.
- Models, skeletons, animation, runtime, cooked formats: untouched.
- The "huge lift" is mostly breadth in editor UI surfaces (browser,
  pickers, inspectors, import dialog); the schema core is small.
