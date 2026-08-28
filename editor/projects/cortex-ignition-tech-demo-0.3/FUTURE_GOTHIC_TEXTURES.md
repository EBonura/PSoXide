# Future-Gothic Texture Pass

The v0.3 level currently uses a non-destructive, grittier future-Gothic pass
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

## Assets

Editable masters are in:

```text
source_assets/textures/future_gothic/
```

Runtime textures are in:

```text
assets/textures/future_gothic/
```

Each runtime texture is cooked at 64 x 64, 4bpp with a 16-colour CLUT. The
material tint is neutral `(128, 128, 128)`: brightness and colour should be
controlled by the albedo and scene lighting instead of exaggerated material
modulation.

To recook a texture after editing its master:

```sh
./target/debug/psxt-convert source.png output.psxt 64 64 4
```

The current masters deliberately lift the middle values enough to preserve
wear and plate boundaries under neutral lighting while leaving only the
deepest vents and slots black.
