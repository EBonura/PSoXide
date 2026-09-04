# Future-Gothic Texture Pass

The v0.4 level currently uses a non-destructive, grittier future-Gothic pass
for its six most common architectural surfaces. The intent is aged industrial
construction with Gothic proportions and detail language: heavy ribs, lancet
recesses, iron frames, worn plate, corrosion, chipped paint, and restrained
emissive accents. The finish stays realistic and low-saturation rather than
ornamental or cartoon-like.

## Material mapping

| Previous material | New material | Faces changed |
| --- | --- | ---: |
| `Layered Beam` (14) | `Future Gothic / Layered Beam` (115) | 420 |
| `Bulkhead V1 Draft` (3) and `Bulkhead V3` (1) | `Future Gothic / Bulkhead` (116) | 275 |
| `Deck` (4) | `Future Gothic / Deck Grating` (117) | 124 |
| `Rib` (7) | `Future Gothic / Emergency Rib` (118) | 83 |
| `Service Panel` (20) | `Future Gothic / Service Panel` (119) | 82 |
| `Wall Plinth` (13) | `Future Gothic / Wall Plinth` (120) | 87 |

The previous resources remain in `project.ron` and can still be painted from
the material browser. Only the existing face assignments were redirected.
Five isolated accent faces retain their original Signal Core or Aletha Crystal
materials deliberately; every repeatedly used architectural face now belongs
to this pass.

## Additional authoring materials

| Material | Resource | Intended use |
| --- | ---: | --- |
| `Future Gothic / Exterior Rock` | 121 | Outdoor cliffs, cave walls, rocky ground, and exposed foundations |
| `Future Gothic / Computer Terminal` | 122 | Animated front face; alternates between two CRT readouts |
| `Future Gothic / Computer Terminal Left` | 123 | Left side of a brush-built terminal |
| `Future Gothic / Computer Terminal Right` | 124 | Right side of a brush-built terminal |
| `Future Gothic / Computer Terminal Back` | 125 | Rear service panel and cable connections |
| `Future Gothic / Computer Terminal Top` | 126 | Vented upper casing |
| `Future Gothic / Computer Terminal Bottom` | 127 | Reinforced underside or base |
| `Future Gothic / Computer Terminal Readout A` | 128 | First selectable source material for the animated front |
| `Future Gothic / Computer Terminal Readout B` | 129 | Second selectable source material for the animated front |

These materials are registered and ready to paint but are not assigned to
existing faces. Exterior Rock uses a very weak, rough reflection response. The
six terminal materials form a complete box-prop kit and keep a modest
reflection response for iron, vents, connectors, and screen glass. The front
cycles between the two readout materials every 24 ticks; its metal casing is
identical between them. All materials use neutral tint so scene lighting
remains authoritative.

## Assets

Editable masters are in:

```text
source_assets/textures/future_gothic/
```

Runtime textures are in:

```text
assets/textures/future_gothic/
```

Each authored texture is cooked at 64 x 64, 4bpp with a 16-colour CLUT. When a
material cycles between two source materials, the cooker verifies that their
dimensions match and jointly packs their resolved images into a temporary
128 x 64 runtime atlas with one shared 16-colour CLUT. Authors never edit or
select that atlas. The material tint is neutral `(128, 128, 128)`: brightness
and colour should be controlled by the albedo and scene lighting instead of
exaggerated material modulation.

To recook a texture after editing its master:

```sh
./target/debug/psxt-convert source.png output.psxt 64 64 4
```

To author or retime this in the editor, select the material and open either
the right-hand Inspector or Material Lab. Under `Animation`, choose `Cycle
Materials`, select Material A and Material B, then set `Ticks per frame` and
the starting material. The preview resolves those materials exactly as the
cooker does. Both sources retain the face's existing UVs; atlas layout and
shared-palette generation remain cook-time implementation details.

The current masters deliberately lift the middle values enough to preserve
wear and plate boundaries under neutral lighting while leaving only the
deepest vents and slots black.
