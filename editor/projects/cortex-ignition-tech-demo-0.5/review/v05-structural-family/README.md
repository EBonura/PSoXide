# Selected C structural family

This review contains the first coherent 0.5 family pass. It changes only DP
City materials that have actual face assignments in the level. The skybox and
the zero-use cable, lattice, and ceiling textures are deliberately excluded.

## Used-texture sheet

`used-textures.png` is ordered left-to-right:

```text
deck plate | C wall | platform edge | teal light bay
green sign | blue sign | structural beam | guardrail
```

Every tile shown is the final 64 x 64, sixteen-colour source used to cook the
live 4bpp PSXT. Opposite edges were reconciled and verified by `build_kit.py`.
The guardrail retains the original alpha silhouette.

## Scene comparisons

- `wall-c-only.png`: C wall selected; the remaining materials are still the
  copied 0.4 versions.
- `full-family-camera-a.png`: the complete family from the identical camera.
- `wall-only-vs-family.png`: wall-only on the left, complete family on the
  right.
- `full-family-camera-b.png`: a second clean gameplay-area angle emphasizing
  the guardrail and green/cyan light sources.

## Image-generation contract

The masters were edited with the built-in ImageGen workflow. The selected C
wall was the material-family reference and the supplied DP Complete scene was
the mood reference. Prompts required broad near-black machine-stone plates,
blackened iron, muted grey-green wear, minimal corrosion, low-frequency forms
that survive 64 x 64 reduction, no extra glyphs or emissive details, and exact
preservation of each texture's UV role.

The light-source prompts additionally locked the green and blue glyph shapes
and the cyan 3-by-3 panel layout while replacing only their clean circuit-board
housings with chipped recessed sockets. The guardrail prompt locked its open
cutout silhouette and the original alpha mask was reapplied after generation.

## Scope proof

The live replacements keep the existing material IDs, paths, UVs, and face
assignments. Current face coverage is: wall 731, guardrail 204, teal light bay
86, deck plate 72, platform edge 69, blue sign 36, structural beam 21, and
green sign 14. All three sky PSXTs compare byte-for-byte with 0.4.
