# Null Choir 64 x 64 texture set

All twelve source PNGs and cooked PSXT assets are exactly 64 x 64 pixels. They
are deliberately authored as single readable modules rather than 2 x 2 repeats,
then assigned to matching BSP detail so transitions are carried by both texture
and silhouette.

## Shared ImageGen direction

OpenAI ImageGen was given the Null Choir concept painting as the mood reference
and this shared direction for every material:

> One orthographic, front-facing dark industrial sci-fi material module, made to
> survive reduction to a 64 x 64 PlayStation texture. Chunky readable shapes,
> controlled charcoal metal, restrained oxidised red or cold cyan accents, no
> perspective, no lighting vignette, no text, no logo, no watermark, and no
> repeated 2 x 2 grid.

The four original materials were edited into single-module replacements. Eight
additional prompts specialized the same language for architectural context:

- `null_choir_bulkhead_v3` - interlocking wall plates and recessed seams.
- `null_choir_deck_v3` - grated walking deck with framed service channels.
- `null_choir_rib_v3` - oxidised-red structural rib face.
- `null_choir_core_v3` - cyan signal lattice and dark containment metal.
- `null_choir_wall_base_v3` - heavy lower plinth, vent band, and floor-contact lip.
- `null_choir_beam_face_v3` - nested layered beam face with inset red armour.
- `null_choir_beam_joint_v3` - reinforced knee/joint plate and large fasteners.
- `null_choir_deck_edge_v3` - curb, drain slot, and hazard edge transition.
- `null_choir_ceiling_vent_v3` - deep overhead louvre cassette.
- `null_choir_trench_liner_v3` - coolant wall plates with cyan lower glow.
- `null_choir_hazard_inset_v3` - framed oxidised-red warning insert.
- `null_choir_service_panel_v3` - recessed maintenance hatch and small indicators.

The deterministic project generator cooks these PNGs to 4bpp PSXT and its
focused test asserts every cooked header remains 64 x 64.
