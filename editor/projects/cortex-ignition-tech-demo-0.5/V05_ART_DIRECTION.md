# Cortex Ignition 0.5 art-direction experiment

Version 0.5 is a non-destructive copy of 0.4 for the Comic Con visual pass. The
level geometry, UVs, gameplay route, and existing face assignments are frozen.
Experiments should replace the source image and cooked PSXT behind an existing
material resource instead of repainting level faces.

## Protected signal language

These DP City materials define Cortex Ignition's readable machine language and
must remain recognisable. Their luminous cores are locked; their surrounding
housings may be rebuilt to belong to the ruined-plate family:

| Resource | Material | Current face uses | Direction |
| ---: | --- | ---: | --- |
| 163 | `DP City / Bright Green Signal` | 14 | Preserve the green glyph; replace the clean circuit-board housing with a chipped recessed socket. |
| 164 | `DP City / Bright Blue Signal` | 36 | Preserve the blue glyph; use the same recessed ruined housing language. |
| 162 | `DP City / Dense Teal Light Bay` | 86 | Preserve the 3-by-3 cyan panels; rebuild only the divider and outer plate. |

The accent silhouettes, colours, positions, and brightness hierarchy must not
be redesigned. The 0.5 pass changes only what the lights are mounted into.

## Locked sky

The existing directional sunset/fog city sky is approved and remains byte-for-
byte identical to 0.4. The sky resources, aperture setup, and background colour
are outside the texture experiment.

## Swap-in-place targets

These materials cover most of the environment and are the safest leverage
points for changing the mood without touching geometry or UV assignments:

| Resource | Material | Current face uses | 0.5 treatment |
| ---: | --- | ---: | --- |
| 160 | `DP City / Megastructure Wall` | 731 | Quieter near-black structure; fewer bright internal seams and broader modules. |
| 171 | `DP City / Guardrail (Cutout)` | 204 | Preserve the silhouette; suppress repetitive highlights. |
| 159 | `DP City / Deck Plate` | 72 | Dark walking plane with a restrained worn-metal edge. |
| 161 | `DP City / Platform Edge` | 69 | Retain navigation readability without competing with signals. |
| 168 | `DP City / Structural Beam` | 21 | Strong dark frame with sparse oxidised detail. |

Resources 165-167 (cables, hanging lattice, and ceiling undersides) have zero
face assignments in the current level and are out of scope for the Comic Con
pass.

## Visual contract

- Preserve the existing DP green, blue, and teal accents.
- Keep the orange/cyan UI and player readability unchanged.
- Use broad charcoal, blue-black, and muted grey-green structural values.
- Reserve bright cyan and green for signs, light bays, interactables, and route
  confirmation.
- Reduce circuit-like seams and equal-strength edge highlights on ordinary
  walls; favour larger recesses, black gaps, worn iron, and occasional oxidised
  trim.
- Do not change geometry, UVs, face assignments, or gameplay during this pass.
- Judge every candidate at native PS1 output size and in motion before
  propagating it.

## Fast review order

1. Rework resource 160, cook it to the existing PSXT path, and capture the most
   representative gameplay room.
2. If the wall establishes the intended mood, revise 159, 161, and 168 as one
   structural family.
3. Tune resource 171 only if its repeated cutout pattern remains too bright.
4. Verify that resources 162-164 now read as deliberate navigation accents.
5. Keep the 0.4 project as the untouched rollback and bake 0.5 only after the
   comparison is approved.

## First wall review

Three swap-in-place candidates and fixed-camera screenshots live under
`review/v05-wall-candidates/`. Candidate C was selected and promoted to the live
resource 160 slot. Its broad damaged plates recede behind the green and teal
signals; candidate B remains the useful more-Gothic extreme and candidate A
remains too close to the original circuitry at gameplay scale.

## Selected C family pass

The live 0.5 project now uses the C language for every high-impact material that
has actual face assignments: wall, guardrail, deck plate, platform edge,
structural beam, teal light bay, green signal, and blue signal. Geometry, UVs,
face assignments, light entities, and sky assets are unchanged. Fixed-camera
renders and the eight-texture native-resolution sheet live under
`review/v05-structural-family/`.
