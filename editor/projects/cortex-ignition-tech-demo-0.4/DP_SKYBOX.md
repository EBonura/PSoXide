# DP Fog City cube sky

The active v0.4 World uses `DP Fog City Cube Sky` (resource 21) in `Cube` mode
with `Always` visibility. This is a distant-background treatment derived from
the DP Complete mood: near-black industrial silhouettes, desaturated grey-green
fog, deep vertical architecture, and sparse cold-cyan glyph lights.

## Assets

- Editable ImageGen source: `source_assets/sky/dp_fog_city_equirect_v1.png`
  (1774 x 887, exact 2:1 equirectangular panorama).
- Runtime texture: `assets/textures/sky/dp_fog_city_cube_4bpp.psxt`
  (196,828 bytes).
- Native in-engine capture: `captures/dp_fog_city_in_engine.png` (320 x 240).
- Nearest-neighbour preview: `captures/dp_fog_city_in_engine_4x.png`
  (1280 x 960; no filtering or repainting).
- Deterministic route frames: `captures/skybox-route-gameplay-20260831/`.

The source was generated with built-in ImageGen in default mode, using only the
two approved fog-city screenshots as visual references. The original generated
master remains in Codex's generated-image store; the project copy is the
canonical editable source.

## Runtime format

The existing `psxed_project::sky_texture::cook_equirectangular_cube_sky`
converter samples the 2:1 panorama into six 256 x 256 faces. It writes those
faces side by side into one 1536 x 256 PSXT, quantized to 4bpp with six 16-colour
CLUT rows. This is the same format and conversion path used by v0.3's Null Choir
eclipse sky.

## Review scene

The previous 271 brushes were replaced by one 3072 x 256 x 3072 solid pedestal,
centred under Aletha's existing spawn at a top height of 953 world units. The
pedestal uses the DP BSP material that was beneath the original player spawn.
All non-brush project content remains available.

The pre-change project is recoverable from
`logs/project.ron.pre-dp-skybox.bak`. The open-world BSP cook intentionally
reports an exterior leak; that is expected for this temporary skybox-review
stage, and the cooker retains visibility through the opening.

## Generation prompt

> Create a production environment-art source for a retro PS1 BSP game skybox,
> based strictly on the two supplied reference screenshots. Output a very wide
> 2:1 equirectangular 360-degree panorama intended to be converted into a cube
> sky. Scene: an endless vertical industrial megacity lost in dense grey-green
> fog, with distant monumental stacked buildings, narrow machine towers,
> skeletal scaffolding, antenna spires, bridge fragments and enormous dark
> architectural silhouettes repeated all around the horizon. Preserve the
> reference mood: near-black charcoal machinery, muted desaturated sage/grey
> atmosphere, extremely deep fog layers, pixel-art / low-resolution
> late-1990s game texture character, restrained edge highlights, sparse tiny
> cyan-blue geometric glyph lights embedded in only a few distant structures.
> The skyline must occupy the middle and lower bands, with fog swallowing both
> the bottom abyss and the upper reaches. The image must work from any viewing
> direction: continuous horizon, balanced detail around the full width, no
> single central hero building, no foreground platform, no player, no weapon,
> no HUD, no readable text, no logo, no sun, no moon, no stars, no purple
> nebula, no glossy red/cyan sci-fi cathedral. Horizontally seamless left/right
> boundary; avoid large objects or cables crossing the seam. Vertically
> plausible for cube projection: upper pole is uniform dense fog with only
> faint tower tips; lower pole is dark foggy void. Crisp intentional pixel
> clusters and low-colour-value structure, not painterly blur, while
> atmospheric layers remain soft. The final should resemble the exact world
> behind the player in the provided screenshots, expanded into a complete
> 360-degree distant city.
