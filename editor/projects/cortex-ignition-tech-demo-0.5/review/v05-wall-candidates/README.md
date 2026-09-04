# 0.5 megastructure wall candidates

The four screenshots use the same clean editor-preview camera and differ only
in resource 160, `DP City / Megastructure Wall`. Geometry, UVs, lighting,
guardrails, the green/blue signs, and the teal light bays are unchanged.

The comparison sheet is ordered:

```text
baseline | candidate A
candidate B | candidate C
```

## Candidate intent

- **A — broad industrial plates:** fewer tiny seams and larger interlocking
  pieces, but still close to the existing futuristic language.
- **B — vertical techno-Gothic:** tall ribs, narrow recesses, and the strongest
  cathedral-like rhythm. It is intentionally the darkest and most linear.
- **C — ruined monolith:** broad damaged machine-stone plates, fewer repeated
  highlights, and the quietest background for the protected accents.

All candidates were produced with the built-in ImageGen edit workflow using
the original wall master as the edit target, the supplied DP Complete scene as
the mood reference, and the existing DP City contact sheet as the protected
material-family reference. The shared prompt contract required an orthographic,
four-edge-seamless, non-emissive wall master in black, charcoal, slate, muted
grey-green, and minimal brown corrosion; large shapes readable at 64 x 64;
and no signs, glyphs, cyan light bays, text, logos, or watermark.

The deterministic project preparation script reduced each master to 64 x 64,
sixteen colours, reconciled opposite edges, and verified seamlessness. Each PNG
was then cooked as an independent 4bpp PSXT under
`assets/textures/dp_city_kit/candidates/`.

Candidate C was approved and is now the active 0.5 wall material. The baseline,
A, and B assets remain here as rollback and comparison material.
