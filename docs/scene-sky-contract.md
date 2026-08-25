# Scene sky contract

PSoXide exposes one sky definition per scene. The World node owns the choice;
brush materials only decide where that sky may be seen.

## Authoring

The World node selects:

- projection: `Off`, `Panorama`, `QuakeLayered`, or `Cube`;
- visibility: `Always` or `ThroughSkySurfaces`;
- one source material for texture-backed projections.

A brush material exposes only `sky_aperture`. An aperture contributes geometry
to BSP visibility and clipping, but it does not select a texture, projection, or
animation. The cooker omits its ordinary face texture binding.

Old `layered_sky` and `directional_sky` material fields remain parse-only. On
load they migrate to a World projection/source plus `sky_aperture`, and are not
written back.

Far-vista geometry is deliberately separate. It is authored world scenery, not
another sky projection.

## Cooked and runtime boundary

`LevelSkyRecord` is the game-neutral contract. Its flags identify one
projection and the visibility policy; `texture_asset` identifies its one source.
BSP reports whether a visible aperture exists, but never chooses the sky.

`draw_scene_sky` owns the shared dispatch:

- procedural panorama uses the cached panorama packet path;
- Quake layered sky uses the shared view-ray layered kernel;
- cube sky uses the shared view-ray cube kernel.

All three texture-backed paths use `SkyTextureKind` and the same residency
manager. Projection kernels stay specialized because their packet layouts and
texture requirements differ; authoring, visibility, streaming, and VRAM
ownership do not.

The editor preview uses the same layered and cube packet kernels as the PS1
runtime, with a caller-supplied ordering-table slot. This prevents an editor-only
projection from becoming a second implementation.

## Adoption by quake-psx and hl-psx

The reusable boundary is the cooked/runtime contract, not PSoXide's editor data.
Each game can translate its existing map or BSP sky declaration into a
`LevelSkyRecord`, report visible apertures from its BSP traversal, and use the
shared dispatcher/residency owner. This can be adopted incrementally:

1. Share `SkyTextureKind` and VRAM upload/residency policy.
2. Share the layered/cube projection kernels and aperture result.
3. Translate the game's existing sky metadata into `LevelSkyRecord`.
4. Remove the old game-local dispatcher only after visual and performance A/B
   captures match on real content.

No game should change its sky art, packet density, ordering-table placement, or
visibility behavior merely to adopt the contract. Those remain explicit inputs
so sharing code cannot silently reduce visual quality or performance.
