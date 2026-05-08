//! Drawing helpers for cooked grid worlds.
//!
//! Walks a [`RoomRender`] and emits its floors / ceilings / walls
//! through [`WorldRenderPass::submit_textured_quad`]. Material slot
//! → runtime material is provided by the caller because the
//! current `.psxw` (VERSION 2) doesn't embed a material table.
//! See `docs/world-format-roadmap.md` for the future compact
//! format that will let this helper resolve materials itself.

use psx_gpu::{
    material::TextureMaterial,
    prim::{TriTextured, TriTexturedGouraud},
};

use crate::{
    render3d::{project_world_vertices_gte, CullMode, DepthPolicy},
    PrimitiveArena, RoomPoint, RoomRender, WorldCamera, WorldRenderPass, WorldSurfaceOptions,
    WorldVertex,
};

/// Which side(s) of a room face should render.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SurfaceSidedness {
    /// Authored/front winding only.
    Front,
    /// Opposite winding only.
    Back,
    /// No winding cull.
    Both,
}

/// Runtime material binding for cooked room geometry.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldRenderMaterial {
    /// GPU texture/material state.
    pub texture: TextureMaterial,
    /// Face-sidedness policy.
    pub sidedness: SurfaceSidedness,
    /// Texture-window width that maps the authored 64-texel face UV domain.
    pub texture_width: u8,
    /// Texture-window height that maps the authored 64-texel face UV domain.
    pub texture_height: u8,
}

impl WorldRenderMaterial {
    /// Build a front-sided material.
    pub const fn front(texture: TextureMaterial) -> Self {
        Self {
            texture,
            sidedness: SurfaceSidedness::Front,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Build a back-sided material.
    pub const fn back(texture: TextureMaterial) -> Self {
        Self {
            texture,
            sidedness: SurfaceSidedness::Back,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Build a double-sided material.
    pub const fn both(texture: TextureMaterial) -> Self {
        Self {
            texture,
            sidedness: SurfaceSidedness::Both,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Return a copy with the same texture state and sidedness but
    /// a different flat RGB tint.
    pub const fn with_tint(mut self, tint: (u8, u8, u8)) -> Self {
        self.texture = self.texture.with_tint(tint);
        self
    }

    /// Return a copy whose authored 64x64 face UVs are projected into
    /// the material's actual texture-window size.
    pub const fn with_texture_size(mut self, width: u8, height: u8) -> Self {
        self.texture_width = normalize_room_texture_uv_size(width);
        self.texture_height = normalize_room_texture_uv_size(height);
        self
    }
}

impl From<TextureMaterial> for WorldRenderMaterial {
    fn from(texture: TextureMaterial) -> Self {
        Self::front(texture)
    }
}

const fn wall_material(mut material: WorldRenderMaterial) -> WorldRenderMaterial {
    material.sidedness = match material.sidedness {
        SurfaceSidedness::Front => SurfaceSidedness::Back,
        SurfaceSidedness::Back => SurfaceSidedness::Front,
        SurfaceSidedness::Both => SurfaceSidedness::Both,
    };
    material
}

/// Kind of room surface currently being emitted.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorldSurfaceKind {
    /// Sector floor.
    Floor,
    /// Sector ceiling.
    Ceiling,
    /// Sector wall on a runtime cardinal edge.
    Wall {
        /// Runtime wall direction id.
        direction: u8,
    },
}

/// Per-surface data exposed to a room lighting/material pass.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldSurfaceSample {
    /// Surface kind.
    pub kind: WorldSurfaceKind,
    /// Sector X coordinate.
    pub sx: u16,
    /// Sector Z coordinate.
    pub sz: u16,
    /// Surface centre in the same room-local world coordinates as
    /// the emitted vertices.
    pub center: RoomPoint,
    /// Baked vertex RGB from `.psxw` static lighting, when the
    /// room carries it. Corner order matches emitted quad order and
    /// values are stored in the tuple form consumed by GPU packets.
    pub baked_vertex_rgb: Option<[(u8, u8, u8); 4]>,
    /// Surface ordinal inside the cooked sector. Floors and
    /// ceilings are always `0`; walls use their local wall-table
    /// index so baked lighting can distinguish stacked wall
    /// segments on the same edge.
    pub ordinal: u16,
}

/// Coarse grid visibility settings for room rendering.
///
/// This is intentionally cell-based rather than triangle-based: the
/// renderer can reject whole authored sectors before it walks their
/// floor/wall records. `radius_cells` bounds traversal around an
/// anchor such as the player, while the camera test rejects cells that
/// are outside the current view cone.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct GridVisibility {
    /// Runtime room-space anchor, usually the player root.
    pub anchor: RoomPoint,
    /// Maximum Chebyshev distance from `anchor` in grid cells.
    pub radius_cells: u16,
    /// Extra projected-pixel margin around the viewport. A non-zero
    /// margin avoids visible popping when a large cell straddles the
    /// frustum edge.
    pub screen_margin: i32,
}

impl GridVisibility {
    /// Build a conservative grid visibility window around an anchor.
    pub const fn around(anchor: RoomPoint, radius_cells: u16) -> Self {
        Self {
            anchor,
            radius_cells,
            screen_margin: 48,
        }
    }

    /// Return a copy with a different projected screen margin.
    pub const fn with_screen_margin(mut self, margin: i32) -> Self {
        self.screen_margin = margin;
        self
    }
}

/// Runtime counters from a grid-visible room draw.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridVisibilityStats {
    /// Non-empty cells considered inside the traversal radius.
    pub cells_considered: u16,
    /// Cells rejected by the coarse camera-space bounds test.
    pub cells_frustum_culled: u16,
    /// Cells that reached surface emission.
    pub cells_drawn: u16,
    /// Floor/ceiling/wall surfaces handed to the projection path.
    pub surfaces_considered: u16,
}

/// One precomputed grid cell selected by cooked visibility/PVS data.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridVisibleCell {
    /// Grid X coordinate inside the cooked room.
    pub x: u16,
    /// Grid Z coordinate inside the cooked room.
    pub z: u16,
    /// Minimum authored surface height in room-local engine units.
    pub min_y: i32,
    /// Maximum authored surface height in room-local engine units.
    pub max_y: i32,
}

impl GridVisibleCell {
    /// Empty placeholder for fixed runtime scratch arrays.
    pub const EMPTY: Self = Self {
        x: 0,
        z: 0,
        min_y: 0,
        max_y: 0,
    };

    /// Build one visible-cell draw record.
    pub const fn new(x: u16, z: u16, min_y: i32, max_y: i32) -> Self {
        Self { x, z, min_y, max_y }
    }
}

/// Predecoded room cell header used by the cached vertex-lit room
/// renderer.
///
/// The cache stores cells in flat `[x * depth + z]` order so a
/// cooked visible-cell reference can jump directly to its surface
/// range without asking the `.psxw` room view for its sector again.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedRoomCell {
    /// Grid X coordinate inside the cooked room.
    pub x: u16,
    /// Grid Z coordinate inside the cooked room.
    pub z: u16,
    /// Minimum authored surface height in room-local engine units.
    pub min_y: i32,
    /// Maximum authored surface height in room-local engine units.
    pub max_y: i32,
    /// Precomputed center used by the cached room frustum test.
    pub visibility_center: WorldVertex,
    /// Precomputed radius used by the cached room frustum test.
    pub visibility_radius: i32,
    /// First surface record for this cell inside the room surface cache.
    pub surface_first: u16,
    /// Number of cached floor/ceiling/wall surfaces in this cell.
    pub surface_count: u16,
}

impl CachedRoomCell {
    /// Empty placeholder for fixed runtime cache arrays.
    pub const EMPTY: Self = Self {
        x: 0,
        z: 0,
        min_y: 0,
        max_y: 0,
        visibility_center: WorldVertex::ZERO,
        visibility_radius: 0,
        surface_first: 0,
        surface_count: 0,
    };

    fn new(
        x: u16,
        z: u16,
        sector_size: i32,
        min_y: i32,
        max_y: i32,
        surface_first: u16,
        surface_count: u16,
    ) -> Self {
        let (visibility_center, visibility_radius) =
            cell_visibility_bounds(x, z, sector_size, min_y, max_y);
        Self {
            x,
            z,
            min_y,
            max_y,
            visibility_center,
            visibility_radius,
            surface_first,
            surface_count,
        }
    }
}

impl WorldSurfaceSample {
    /// Empty placeholder used by fixed runtime cache arrays.
    pub const EMPTY: Self = Self {
        kind: WorldSurfaceKind::Floor,
        sx: 0,
        sz: 0,
        center: RoomPoint::ZERO,
        baked_vertex_rgb: None,
        ordinal: 0,
    };
}

/// Predecoded vertex-lit room surface.
///
/// This stores the frame-invariant half of room drawing: material
/// slot, emitted vertex order, UV order, split id, and the surface
/// lighting sample. Per-frame work still applies camera projection,
/// culling, fog, and final ordering-table submission.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CachedRoomSurface {
    /// Local room material slot referenced by this surface.
    pub material_slot: u16,
    /// Emitted quad vertices in the same order the uncached path
    /// would submit them.
    pub vertices: [WorldVertex; 4],
    /// Indices into the cached room vertex stream. The indexed
    /// renderer uses these to project shared room corners once per
    /// frame instead of once per surface.
    pub vertex_indices: [u16; 4],
    /// Texture-page-relative UVs matching `vertices`.
    pub uvs: [(u8, u8); 4],
    /// Surface lighting/material sample.
    pub sample: WorldSurfaceSample,
    /// Authored diagonal split id for floors/ceilings.
    pub split: u8,
    /// Split-triangle index for floor/ceiling records, or `2`
    /// for a full quad surface such as a wall.
    pub triangle_index: u8,
}

impl CachedRoomSurface {
    /// Empty placeholder for fixed runtime cache arrays.
    pub const EMPTY: Self = Self {
        material_slot: 0,
        vertices: [WorldVertex::ZERO; 4],
        vertex_indices: [0; 4],
        uvs: [(0, 0); 4],
        sample: WorldSurfaceSample::EMPTY,
        split: SPLIT_NW_SE,
        triangle_index: WHOLE_QUAD_TRIANGLE_INDEX,
    };

    const fn new(
        material_slot: u16,
        vertices: [WorldVertex; 4],
        vertex_indices: [u16; 4],
        uvs: [(u8, u8); 4],
        sample: WorldSurfaceSample,
        split: u8,
        triangle_index: u8,
    ) -> Self {
        Self {
            material_slot,
            vertices,
            vertex_indices,
            uvs,
            sample,
            split,
            triangle_index,
        }
    }
}

/// Result from building a cached room surface stream.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CachedRoomSurfaceCacheStats {
    /// Number of cached cell headers written.
    pub cell_count: usize,
    /// Number of cached surface records written.
    pub surface_count: usize,
    /// Number of deduplicated cached world vertices written.
    pub vertex_count: usize,
    /// `true` when the caller-provided arrays were too small.
    pub overflow: bool,
}

/// Hook used by [`draw_room_lit`] to vary material tint per room
/// surface.
pub trait WorldSurfaceLighting {
    /// Shade one material for one room surface.
    fn shade(
        &self,
        sample: WorldSurfaceSample,
        material: WorldRenderMaterial,
    ) -> WorldRenderMaterial;

    /// Shade one vertex of one room surface. The default keeps
    /// legacy face-centre lighting behaviour; static-light passes can
    /// override this to feed textured Gouraud room packets.
    fn shade_vertex(
        &self,
        sample: WorldSurfaceSample,
        _vertex: RoomPoint,
        material: WorldRenderMaterial,
    ) -> (u8, u8, u8) {
        self.shade(sample, material).texture.tint()
    }

    /// Shade all four vertices of one emitted room quad. The
    /// default calls [`Self::shade_vertex`] for each vertex; baked
    /// static-light passes can override this for direct table lookup.
    fn shade_vertices(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        [
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[0]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[1]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[2]), material),
            self.shade_vertex(sample, RoomPoint::from_world_vertex(vertices[3]), material),
        ]
    }
}

/// No-op surface lighting.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct NoWorldSurfaceLighting;

impl WorldSurfaceLighting for NoWorldSurfaceLighting {
    fn shade(
        &self,
        _sample: WorldSurfaceSample,
        material: WorldRenderMaterial,
    ) -> WorldRenderMaterial {
        material
    }
}

/// Floor / ceiling split id for the standard NW→SE diagonal --
/// the value the cooker stamps when no rotation has been
/// authored. Mirrors `psxed_format::world::split::NORTH_WEST_SOUTH_EAST`.
/// Used by tests to spell the split id explicitly; runtime
/// emission falls through to this case for any non-`SPLIT_NE_SW`
/// id.
const SPLIT_NW_SE: u8 = psx_asset::WORLD_SPLIT_NORTH_WEST_SOUTH_EAST;
/// Alternate split id (NE→SW diagonal). Mirrors
/// `psxed_format::world::split::NORTH_EAST_SOUTH_WEST`.
const SPLIT_NE_SW: u8 = psx_asset::WORLD_SPLIT_NORTH_EAST_SOUTH_WEST;
const WHOLE_QUAD_TRIANGLE_INDEX: u8 = psx_asset::world_topology::WHOLE_QUAD_TRIANGLE_INDEX;
const ROOM_TEXTURE_UV_SIZE: u8 = 64;

/// Texture-page-relative tile size used by legacy v1 helper tests.
#[cfg(test)]
const TILE_UV: u8 = 64;

/// Direction id for the north edge.
///
/// Mirrors `psxed_format::world::direction::NORTH` -- kept inline
/// so `psx-engine` doesn't need a direct `psxed-format` dep
/// (it already reaches the format via `psx-asset`, but adding
/// the direct dep just for four byte constants is overkill).
const DIR_NORTH: u8 = 0;
const DIR_EAST: u8 = 1;
const DIR_SOUTH: u8 = 2;
const DIR_WEST: u8 = 3;
const DIR_NORTH_WEST_SOUTH_EAST: u8 = 4;
const DIR_NORTH_EAST_SOUTH_WEST: u8 = 5;

#[cfg(test)]
const WALL_UVS: [(u8, u8); 4] = [(0, TILE_UV), (TILE_UV, TILE_UV), (TILE_UV, 0), (0, 0)];

/// Walk every populated sector of `room`, emitting one textured
/// quad per floor / ceiling face plus one per wall.
///
/// `materials` is indexed by the slot ids returned from
/// [`SectorRender::floor_material`], [`SectorRender::ceiling_material`]
/// and [`WallRender::material`]. A face whose slot points past the
/// table is dropped silently -- friendlier than a panic while the
/// author is mid-iteration with partially-assigned materials.
///
/// Cells are corner-rooted at world `(0, 0)`: cell `(sx, sz)`
/// occupies `x ∈ [sx*S, (sx+1)*S]`, `z ∈ [sz*S, (sz+1)*S]`.
/// Position the camera target at the room's centre -- typically
/// `(W*S/2, 0, D*S/2)` -- so the orbit lands on the geometry.
///
/// `options` carries the depth band + range. Per-material
/// [`SurfaceSidedness`] selects front-only, back-only, or
/// double-sided emission; front-sided faces use [`CullMode::Back`].
///
/// # Quad corner conventions
///
/// All four-corner inputs to [`WorldRenderPass::submit_textured_quad`]
/// are emitted in perimeter order. The renderer splits along the
/// `0`–`2` diagonal (see `TEXTURED_QUAD_TRIANGLES` in `render3d.rs`),
/// so corner positions and UVs must agree on what `0`, `1`, `2`,
/// `3` mean.
///
/// * **Floors / ceilings** -- records store `[NW, NE, SE, SW]`.
///   Floors keep that top-facing winding; ceilings flip to the
///   inward underside winding. UVs are transformed with the vertices.
/// * **Walls** -- runtime records store `[bottom-left, bottom-right,
///   top-right, top-left]` for an owning cell edge. That physical corner
///   order makes the wall back side face the owning cell/interior. Wall
///   emission swaps Front/Back material intent so authors can use a
///   front-sided material for the common one-sided interior wall case.
///
/// [`SectorRender::floor_material`]: crate::SectorRender::floor_material
/// [`SectorRender::ceiling_material`]: crate::SectorRender::ceiling_material
/// [`WallRender::material`]: crate::WallRender::material
pub fn draw_room<const OT: usize>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    draw_room_lit(
        room,
        materials,
        &NoWorldSurfaceLighting,
        camera,
        options,
        triangles,
        world,
    );
}

/// Draw a room while giving the caller one material-shading hook per
/// emitted floor, ceiling, and wall surface.
#[allow(clippy::too_many_arguments)]
pub fn draw_room_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    for sx in 0..room.width() {
        for sz in 0..room.depth() {
            let Some(sector) = room.sector(sx, sz) else {
                continue;
            };
            let _ = draw_sector_lit(
                room, sx, sz, sector, materials, lighting, camera, options, triangles, world,
            );
        }
    }
}

/// Draw a room through a coarse grid visibility pass.
///
/// Traversal is ring-ordered from farthest to nearest around
/// `visibility.anchor`, which gives bucketed ordering a stable coarse
/// back-to-front submission order before the PS1 ordering table handles
/// per-triangle depth buckets.
#[allow(clippy::too_many_arguments)]
pub fn draw_room_lit_grid_visible<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visibility: GridVisibility,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let width = room.width();
    let depth = room.depth();
    if width == 0 || depth == 0 {
        return stats;
    }

    let sector_size = room.sector_size().max(1);
    let anchor_x = grid_cell_for_world(visibility.anchor.x, sector_size).clamp(0, width as i32 - 1);
    let anchor_z = grid_cell_for_world(visibility.anchor.z, sector_size).clamp(0, depth as i32 - 1);
    let radius = visibility.radius_cells as i32;
    let min_x = (anchor_x - radius).max(0) as u16;
    let max_x = (anchor_x + radius).min(width as i32 - 1) as u16;
    let min_z = (anchor_z - radius).max(0) as u16;
    let max_z = (anchor_z + radius).min(depth as i32 - 1) as u16;

    let max_ring_x = (anchor_x - min_x as i32).max(max_x as i32 - anchor_x);
    let max_ring_z = (anchor_z - min_z as i32).max(max_z as i32 - anchor_z);
    let mut ring = max_ring_x.max(max_ring_z);
    loop {
        let mut sx = min_x;
        while sx <= max_x {
            let mut sz = min_z;
            while sz <= max_z {
                let dx = ((sx as i32) - anchor_x).abs();
                let dz = ((sz as i32) - anchor_z).abs();
                if dx.max(dz) == ring {
                    if let Some(sector) = room.sector(sx, sz) {
                        stats.cells_considered = stats.cells_considered.saturating_add(1);
                        let (min_y, max_y) = sector_y_bounds(room, sector);
                        if !cell_visible_to_camera(
                            camera,
                            options,
                            sx,
                            sz,
                            sector_size,
                            min_y,
                            max_y,
                            visibility.screen_margin,
                        ) {
                            stats.cells_frustum_culled =
                                stats.cells_frustum_culled.saturating_add(1);
                        } else {
                            stats.cells_drawn = stats.cells_drawn.saturating_add(1);
                            stats.surfaces_considered =
                                stats.surfaces_considered.saturating_add(draw_sector_lit(
                                    room, sx, sz, sector, materials, lighting, camera, options,
                                    triangles, world,
                                ));
                        }
                    }
                }
                if sz == max_z {
                    break;
                }
                sz += 1;
            }
            if sx == max_x {
                break;
            }
            sx += 1;
        }
        if ring == 0 {
            break;
        }
        ring -= 1;
    }

    stats
}

/// Draw a room using one textured Gouraud triangle per emitted
/// triangle. The lighting hook is evaluated at every surface corner,
/// which gives static point lights a smooth per-vertex falloff while
/// preserving authored texture windows/UV tiling.
#[allow(clippy::too_many_arguments)]
pub fn draw_room_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    for sx in 0..room.width() {
        for sz in 0..room.depth() {
            let Some(sector) = room.sector(sx, sz) else {
                continue;
            };
            let _ = draw_sector_vertex_lit(
                room, sx, sz, sector, materials, lighting, camera, options, triangles, world,
            );
        }
    }
}

/// Draw a vertex-lit room through the same coarse grid visibility pass
/// used by [`draw_room_lit_grid_visible`].
#[allow(clippy::too_many_arguments)]
pub fn draw_room_vertex_lit_grid_visible<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visibility: GridVisibility,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let width = room.width();
    let depth = room.depth();
    if width == 0 || depth == 0 {
        return stats;
    }

    let sector_size = room.sector_size().max(1);
    let anchor_x = grid_cell_for_world(visibility.anchor.x, sector_size).clamp(0, width as i32 - 1);
    let anchor_z = grid_cell_for_world(visibility.anchor.z, sector_size).clamp(0, depth as i32 - 1);
    let radius = visibility.radius_cells as i32;
    let min_x = (anchor_x - radius).max(0) as u16;
    let max_x = (anchor_x + radius).min(width as i32 - 1) as u16;
    let min_z = (anchor_z - radius).max(0) as u16;
    let max_z = (anchor_z + radius).min(depth as i32 - 1) as u16;

    let max_ring_x = (anchor_x - min_x as i32).max(max_x as i32 - anchor_x);
    let max_ring_z = (anchor_z - min_z as i32).max(max_z as i32 - anchor_z);
    let mut ring = max_ring_x.max(max_ring_z);
    loop {
        let mut sx = min_x;
        while sx <= max_x {
            let mut sz = min_z;
            while sz <= max_z {
                let dx = ((sx as i32) - anchor_x).abs();
                let dz = ((sz as i32) - anchor_z).abs();
                if dx.max(dz) == ring {
                    if let Some(sector) = room.sector(sx, sz) {
                        stats.cells_considered = stats.cells_considered.saturating_add(1);
                        let (min_y, max_y) = sector_y_bounds(room, sector);
                        if !cell_visible_to_camera(
                            camera,
                            options,
                            sx,
                            sz,
                            sector_size,
                            min_y,
                            max_y,
                            visibility.screen_margin,
                        ) {
                            stats.cells_frustum_culled =
                                stats.cells_frustum_culled.saturating_add(1);
                        } else {
                            stats.cells_drawn = stats.cells_drawn.saturating_add(1);
                            stats.surfaces_considered =
                                stats
                                    .surfaces_considered
                                    .saturating_add(draw_sector_vertex_lit(
                                        room, sx, sz, sector, materials, lighting, camera, options,
                                        triangles, world,
                                    ));
                        }
                    }
                }
                if sz == max_z {
                    break;
                }
                sz += 1;
            }
            if sx == max_x {
                break;
            }
            sx += 1;
        }
        if ring == 0 {
            break;
        }
        ring -= 1;
    }

    stats
}

/// Draw a vertex-lit room from a cooked far-to-near visible-cell
/// list. This avoids rebuilding the same ring traversal and cell
/// bounds every frame; the caller supplies PVS/portal-filtered cells
/// generated by the editor cook.
#[allow(clippy::too_many_arguments)]
pub fn draw_room_vertex_lit_visible_cells<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let sector_size = room.sector_size().max(1);
    for cell in cells {
        let Some(sector) = room.sector(cell.x, cell.z) else {
            continue;
        };
        stats.cells_considered = stats.cells_considered.saturating_add(1);
        if !cell_visible_to_camera(
            camera,
            options,
            cell.x,
            cell.z,
            sector_size.max(1),
            cell.min_y,
            cell.max_y,
            screen_margin,
        ) {
            stats.cells_frustum_culled = stats.cells_frustum_culled.saturating_add(1);
            continue;
        }
        stats.cells_drawn = stats.cells_drawn.saturating_add(1);
        stats.surfaces_considered =
            stats
                .surfaces_considered
                .saturating_add(draw_sector_vertex_lit(
                    room, cell.x, cell.z, sector, materials, lighting, camera, options, triangles,
                    world,
                ));
    }
    stats
}

/// Predecode all renderable floor, ceiling, and wall surfaces in a
/// room into caller-owned fixed arrays.
///
/// Cell headers are written in flat `[x * depth + z]` order for the
/// whole room grid. Surface records are only written for populated
/// sectors that reference a material slot and have valid geometry.
/// If either output slice is too small, `overflow` is set and callers
/// should fall back to the uncached room renderer for that room.
pub fn cache_room_vertex_lit_surfaces(
    room: RoomRender<'_, '_>,
    materials: &[WorldRenderMaterial],
    cells_out: &mut [CachedRoomCell],
    vertices_out: &mut [WorldVertex],
    surfaces_out: &mut [CachedRoomSurface],
) -> CachedRoomSurfaceCacheStats {
    let width = room.width();
    let depth = room.depth();
    let cell_count = (width as usize).saturating_mul(depth as usize);
    if cell_count > cells_out.len() || cell_count > u16::MAX as usize {
        return CachedRoomSurfaceCacheStats {
            cell_count: 0,
            surface_count: 0,
            vertex_count: 0,
            overflow: true,
        };
    }

    let sector_size = room.sector_size();
    let mut vertex_count = 0usize;
    let mut surface_count = 0usize;
    let mut sx = 0u16;
    while sx < width {
        let mut sz = 0u16;
        while sz < depth {
            let cell_index = sx as usize * depth as usize + sz as usize;
            let surface_first = surface_count;
            cells_out[cell_index] =
                CachedRoomCell::new(sx, sz, sector_size, 0, 0, surface_first as u16, 0);

            let Some(sector) = room.sector(sx, sz) else {
                sz += 1;
                continue;
            };

            if sector.has_floor() {
                let heights = sector.floor_heights();
                let split = sector.floor_split();
                if let Some((slot, uvs)) = merged_floor_surface(sector) {
                    let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                    let Some(vertex_indices) =
                        cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                    else {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    };
                    let sample = WorldSurfaceSample {
                        kind: WorldSurfaceKind::Floor,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                        ordinal: 0,
                    };
                    if !cache_room_surface(
                        surfaces_out,
                        &mut surface_count,
                        CachedRoomSurface::new(
                            slot,
                            vertices,
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        ),
                    ) {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    }
                } else {
                    for triangle_index in 0..2 {
                        if !sector.floor_triangle_present(triangle_index) {
                            continue;
                        }
                        let Some(slot) = sector.floor_triangle_material(triangle_index) else {
                            continue;
                        };
                        let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Floor,
                            sx,
                            sz,
                            center: horizontal_triangle_center(
                                sx,
                                sz,
                                sector_size,
                                heights,
                                split,
                                triangle_index,
                            ),
                            baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                            ordinal: triangle_index as u16,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                slot,
                                vertices,
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.floor_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
            }

            if sector.has_ceiling() {
                let heights = sector.ceiling_heights();
                let split = sector.ceiling_split();
                if let Some((slot, uvs)) = merged_ceiling_surface(sector) {
                    let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                    let Some(vertex_indices) =
                        cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                    else {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    };
                    let sample = WorldSurfaceSample {
                        kind: WorldSurfaceKind::Ceiling,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                        ordinal: 0,
                    };
                    if !cache_room_surface(
                        surfaces_out,
                        &mut surface_count,
                        CachedRoomSurface::new(
                            slot,
                            vertices,
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        ),
                    ) {
                        return CachedRoomSurfaceCacheStats {
                            cell_count,
                            surface_count,
                            vertex_count,
                            overflow: true,
                        };
                    }
                } else {
                    for triangle_index in 0..2 {
                        if !sector.ceiling_triangle_present(triangle_index) {
                            continue;
                        }
                        let Some(slot) = sector.ceiling_triangle_material(triangle_index) else {
                            continue;
                        };
                        let vertices = horizontal_vertices(sx, sz, sector_size, heights);
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Ceiling,
                            sx,
                            sz,
                            center: horizontal_triangle_center(
                                sx,
                                sz,
                                sector_size,
                                heights,
                                split,
                                triangle_index,
                            ),
                            baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                            ordinal: triangle_index as u16,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                slot,
                                vertices,
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.ceiling_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
            }

            let mut i = 0;
            while i < sector.wall_count() {
                if let Some(wall) = room.sector_wall(sector, i) {
                    if let Some(vertices) =
                        wall_vertices(sx, sz, sector_size, wall.direction(), wall.heights())
                    {
                        let Some(vertex_indices) =
                            cache_room_vertices(vertices_out, &mut vertex_count, vertices)
                        else {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        };
                        let (split, triangle_index) = wall_shape_triangle(wall.shape())
                            .unwrap_or((SPLIT_NW_SE, WHOLE_QUAD_TRIANGLE_INDEX));
                        let sample = WorldSurfaceSample {
                            kind: WorldSurfaceKind::Wall {
                                direction: wall.direction(),
                            },
                            sx,
                            sz,
                            center: wall_shape_center(vertices, wall.shape()),
                            baked_vertex_rgb: baked_vertex_rgb(room.wall_light(sector, i)),
                            ordinal: i,
                        };
                        if !cache_room_surface(
                            surfaces_out,
                            &mut surface_count,
                            CachedRoomSurface::new(
                                wall.material(),
                                vertices,
                                vertex_indices,
                                cached_material_uvs(materials, wall.material(), wall.uvs()),
                                sample,
                                split,
                                triangle_index,
                            ),
                        ) {
                            return CachedRoomSurfaceCacheStats {
                                cell_count,
                                surface_count,
                                vertex_count,
                                overflow: true,
                            };
                        }
                    }
                }
                i += 1;
            }

            let surface_len = surface_count.saturating_sub(surface_first);
            if surface_len > u16::MAX as usize || surface_first > u16::MAX as usize {
                return CachedRoomSurfaceCacheStats {
                    cell_count,
                    surface_count,
                    vertex_count,
                    overflow: true,
                };
            }
            if surface_len > 0 {
                let (min_y, max_y) = sector_y_bounds(room, sector);
                cells_out[cell_index] = CachedRoomCell::new(
                    sx,
                    sz,
                    sector_size,
                    min_y,
                    max_y,
                    surface_first as u16,
                    surface_len as u16,
                );
            }

            sz += 1;
        }
        sx += 1;
    }

    CachedRoomSurfaceCacheStats {
        cell_count,
        surface_count,
        vertex_count,
        overflow: false,
    }
}

/// Draw a vertex-lit room from cached surface records and a cooked
/// visible-cell list.
///
/// This is the hot-path counterpart to
/// [`cache_room_vertex_lit_surfaces`]. It preserves the same
/// projection, culling, lighting/fog, and material sidedness behavior
/// as [`draw_room_vertex_lit_visible_cells`] while avoiding per-frame
/// room-sector decode work.
#[allow(clippy::too_many_arguments)]
pub fn draw_cached_room_vertex_lit_visible_cells<const OT: usize, L: WorldSurfaceLighting>(
    cached_cells: &[CachedRoomCell],
    cached_surfaces: &[CachedRoomSurface],
    room_depth: u16,
    _sector_size: i32,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visible_cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let depth = room_depth as usize;
    if depth == 0 {
        return stats;
    }

    for visible in visible_cells {
        let cell_index = visible.x as usize * depth + visible.z as usize;
        let Some(cell) = cached_cells.get(cell_index).copied() else {
            continue;
        };
        if cell.surface_count == 0 || cell.x != visible.x || cell.z != visible.z {
            continue;
        }

        stats.cells_considered = stats.cells_considered.saturating_add(1);
        if !cell_visibility_visible_to_camera(
            camera,
            options,
            cell.visibility_center,
            cell.visibility_radius,
            screen_margin,
        ) {
            stats.cells_frustum_culled = stats.cells_frustum_culled.saturating_add(1);
            continue;
        }

        stats.cells_drawn = stats.cells_drawn.saturating_add(1);
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .saturating_add(draw_cached_room_surface(
                        cached_surfaces[i],
                        materials,
                        lighting,
                        camera,
                        options,
                        triangles,
                        world,
                    ));
            i += 1;
        }
    }
    stats
}

/// Draw a cached vertex-lit room using a deduplicated cached vertex
/// stream. The projected scratch slices must be at least as long as
/// `cached_vertices`.
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed_cached_room_vertex_lit_visible_cells<
    const OT: usize,
    L: WorldSurfaceLighting,
>(
    cached_cells: &[CachedRoomCell],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_vertices: &mut [crate::render3d::ProjectedVertex],
    projected_valid: &mut [bool],
    room_depth: u16,
    _sector_size: i32,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    visible_cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    let depth = room_depth as usize;
    if depth == 0
        || projected_vertices.len() < cached_vertices.len()
        || projected_valid.len() < cached_vertices.len()
    {
        return stats;
    }

    project_world_vertices_gte(*camera, cached_vertices, projected_vertices, projected_valid);

    for visible in visible_cells {
        let cell_index = visible.x as usize * depth + visible.z as usize;
        let Some(cell) = cached_cells.get(cell_index).copied() else {
            continue;
        };
        if cell.surface_count == 0 || cell.x != visible.x || cell.z != visible.z {
            continue;
        }

        stats.cells_considered = stats.cells_considered.saturating_add(1);
        if !cell_visibility_visible_to_camera(
            camera,
            options,
            cell.visibility_center,
            cell.visibility_radius,
            screen_margin,
        ) {
            stats.cells_frustum_culled = stats.cells_frustum_culled.saturating_add(1);
            continue;
        }

        stats.cells_drawn = stats.cells_drawn.saturating_add(1);
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut i = first;
        while i < end {
            stats.surfaces_considered =
                stats
                    .surfaces_considered
                    .saturating_add(draw_indexed_cached_room_surface(
                        cached_surfaces[i],
                        projected_vertices,
                        projected_valid,
                        materials,
                        lighting,
                        options,
                        triangles,
                        world,
                    ));
            i += 1;
        }
    }
    stats
}

fn cache_room_surface(
    surfaces_out: &mut [CachedRoomSurface],
    surface_count: &mut usize,
    surface: CachedRoomSurface,
) -> bool {
    if *surface_count >= surfaces_out.len() || *surface_count >= u16::MAX as usize {
        return false;
    }
    surfaces_out[*surface_count] = surface;
    *surface_count += 1;
    true
}

fn cache_room_vertices(
    vertices_out: &mut [WorldVertex],
    vertex_count: &mut usize,
    vertices: [WorldVertex; 4],
) -> Option<[u16; 4]> {
    Some([
        cache_room_vertex(vertices_out, vertex_count, vertices[0])?,
        cache_room_vertex(vertices_out, vertex_count, vertices[1])?,
        cache_room_vertex(vertices_out, vertex_count, vertices[2])?,
        cache_room_vertex(vertices_out, vertex_count, vertices[3])?,
    ])
}

fn cache_room_vertex(
    vertices_out: &mut [WorldVertex],
    vertex_count: &mut usize,
    vertex: WorldVertex,
) -> Option<u16> {
    let mut i = 0usize;
    while i < *vertex_count {
        if vertices_out[i] == vertex {
            return u16::try_from(i).ok();
        }
        i += 1;
    }

    if *vertex_count >= vertices_out.len() || *vertex_count >= u16::MAX as usize {
        return None;
    }
    let index = *vertex_count;
    vertices_out[index] = vertex;
    *vertex_count += 1;
    u16::try_from(index).ok()
}

fn cached_material_uvs(
    materials: &[WorldRenderMaterial],
    slot: u16,
    uvs: [(u8, u8); 4],
) -> [(u8, u8); 4] {
    match materials.get(slot as usize) {
        Some(&material) => material_uvs(material, uvs),
        None => uvs,
    }
}

fn baked_vertex_rgb(rgb: Option<[[u8; 3]; 4]>) -> Option<[(u8, u8, u8); 4]> {
    rgb.map(|rgb| {
        [
            (rgb[0][0], rgb[0][1], rgb[0][2]),
            (rgb[1][0], rgb[1][1], rgb[1][2]),
            (rgb[2][0], rgb[2][1], rgb[2][2]),
            (rgb[3][0], rgb[3][1], rgb[3][2]),
        ]
    })
}

fn merged_floor_surface(sector: crate::SectorRender) -> Option<(u16, [(u8, u8); 4])> {
    merge_horizontal_triangle_surface(
        [
            sector.floor_triangle_material(0),
            sector.floor_triangle_material(1),
        ],
        [sector.floor_triangle_uvs(0), sector.floor_triangle_uvs(1)],
    )
}

fn merged_ceiling_surface(sector: crate::SectorRender) -> Option<(u16, [(u8, u8); 4])> {
    merge_horizontal_triangle_surface(
        [
            sector.ceiling_triangle_material(0),
            sector.ceiling_triangle_material(1),
        ],
        [
            sector.ceiling_triangle_uvs(0),
            sector.ceiling_triangle_uvs(1),
        ],
    )
}

fn merge_horizontal_triangle_surface(
    materials: [Option<u16>; 2],
    uvs: [[(u8, u8); 4]; 2],
) -> Option<(u16, [(u8, u8); 4])> {
    let slot = materials[0]?;
    if materials[1]? != slot || uvs[0] != uvs[1] {
        return None;
    }
    Some((slot, uvs[0]))
}

#[allow(clippy::too_many_arguments)]
fn draw_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: CachedRoomSurface,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> u16 {
    let Some(&material) = materials.get(surface.material_slot as usize) else {
        return 0;
    };
    let material = cached_uv_material(material);
    let colors = vertex_lighting_colors(lighting, surface.sample, material, surface.vertices);
    match surface.sample.kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                submit_split_triangle_vertex_lit(
                    camera,
                    options.with_depth_policy(DepthPolicy::Farthest),
                    CullMode::Back,
                    material,
                    surface.vertices,
                    surface.uvs,
                    colors,
                    surface.split,
                    surface.triangle_index as usize,
                    matches!(surface.sample.kind, WorldSurfaceKind::Ceiling),
                    triangles,
                    world,
                )
            } else {
                let (vertices, uvs, colors) =
                    if matches!(surface.sample.kind, WorldSurfaceKind::Ceiling) {
                        (
                            reverse_quad_winding(surface.vertices),
                            reverse_quad_winding(surface.uvs),
                            reverse_quad_winding(colors),
                        )
                    } else {
                        (surface.vertices, surface.uvs, colors)
                    };
                submit_split_quad_vertex_lit(
                    camera,
                    options.with_depth_policy(DepthPolicy::Farthest),
                    CullMode::Back,
                    material,
                    vertices,
                    uvs,
                    colors,
                    surface.split,
                    triangles,
                    world,
                )
            }
        }
        WorldSurfaceKind::Wall { .. } => {
            let material = wall_material(material);
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                submit_split_triangle_vertex_lit(
                    camera,
                    options,
                    CullMode::Back,
                    material,
                    surface.vertices,
                    surface.uvs,
                    colors,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                    triangles,
                    world,
                )
            } else {
                submit_quad_vertex_lit(
                    camera,
                    options,
                    CullMode::Back,
                    material,
                    surface.vertices,
                    surface.uvs,
                    colors,
                    triangles,
                    world,
                )
            }
        }
    }
    1
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn draw_indexed_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: CachedRoomSurface,
    projected_vertices: &[crate::render3d::ProjectedVertex],
    projected_valid: &[bool],
    materials: &[WorldRenderMaterial],
    lighting: &L,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> u16 {
    let Some(&material) = materials.get(surface.material_slot as usize) else {
        return 0;
    };
    let material = cached_uv_material(material);
    let ids = surface.vertex_indices;
    let Some(projected) = indexed_projected_quad(projected_vertices, projected_valid, ids) else {
        return 0;
    };
    let colors = vertex_lighting_colors(lighting, surface.sample, material, surface.vertices);
    match surface.sample.kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                submit_projected_split_triangle_vertex_lit_cached_uvs(
                    projected,
                    surface.uvs,
                    colors,
                    material,
                    options.with_depth_policy(DepthPolicy::Farthest),
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    matches!(surface.sample.kind, WorldSurfaceKind::Ceiling),
                    triangles,
                    world,
                )
            } else {
                let (projected, uvs, colors) =
                    if matches!(surface.sample.kind, WorldSurfaceKind::Ceiling) {
                        (
                            reverse_quad_winding(projected),
                            reverse_quad_winding(surface.uvs),
                            reverse_quad_winding(colors),
                        )
                    } else {
                        (projected, surface.uvs, colors)
                    };
                submit_sided_projected_gouraud_quad_cached_uvs(
                    world,
                    triangles,
                    projected,
                    uvs,
                    colors,
                    material,
                    options.with_depth_policy(DepthPolicy::Farthest),
                    CullMode::Back,
                    split_triangles_runtime(surface.split),
                );
            }
        }
        WorldSurfaceKind::Wall { .. } => {
            let material = wall_material(material);
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                submit_projected_split_triangle_vertex_lit_cached_uvs(
                    projected,
                    surface.uvs,
                    colors,
                    material,
                    options,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                    triangles,
                    world,
                )
            } else {
                submit_sided_projected_gouraud_quad_cached_uvs(
                    world,
                    triangles,
                    projected,
                    surface.uvs,
                    colors,
                    material,
                    options,
                    CullMode::Back,
                    SPLIT_NW_SE_TRIANGLES,
                );
            }
        }
    }
    1
}

#[inline(always)]
fn indexed_projected_quad(
    projected_vertices: &[crate::render3d::ProjectedVertex],
    projected_valid: &[bool],
    ids: [u16; 4],
) -> Option<[crate::render3d::ProjectedVertex; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let limit = projected_vertices.len().min(projected_valid.len());
    let max_index = a.max(b).max(c).max(d);
    if max_index >= limit {
        return None;
    }
    if !projected_valid[a] || !projected_valid[b] || !projected_valid[c] || !projected_valid[d] {
        return None;
    }
    Some([
        projected_vertices[a],
        projected_vertices[b],
        projected_vertices[c],
        projected_vertices[d],
    ])
}

const fn cached_uv_material(mut material: WorldRenderMaterial) -> WorldRenderMaterial {
    material.texture_width = ROOM_TEXTURE_UV_SIZE;
    material.texture_height = ROOM_TEXTURE_UV_SIZE;
    material
}

#[allow(clippy::too_many_arguments)]
fn draw_sector_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    sx: u16,
    sz: u16,
    sector: crate::SectorRender,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> u16 {
    let sector_size = room.sector_size();
    let mut surfaces = 0u16;

    if sector.has_floor() {
        let heights = sector.floor_heights();
        let split = sector.floor_split();
        if let Some((slot, uvs)) = merged_floor_surface(sector) {
            if let Some(&base_material) = materials.get(slot as usize) {
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Floor,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                        ordinal: 0,
                    },
                    base_material,
                );
                surfaces = surfaces.saturating_add(1);
                emit_floor(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    uvs,
                    material,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        } else {
            for triangle_index in 0..2 {
                if !sector.floor_triangle_present(triangle_index) {
                    continue;
                }
                let Some(slot) = sector.floor_triangle_material(triangle_index) else {
                    continue;
                };
                let Some(&base_material) = materials.get(slot as usize) else {
                    continue;
                };
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Floor,
                        sx,
                        sz,
                        center: horizontal_triangle_center(
                            sx,
                            sz,
                            sector_size,
                            heights,
                            split,
                            triangle_index,
                        ),
                        baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                        ordinal: triangle_index as u16,
                    },
                    base_material,
                );
                surfaces = surfaces.saturating_add(1);
                emit_floor_triangle(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    triangle_index,
                    sector.floor_triangle_uvs(triangle_index),
                    material,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
    }

    if sector.has_ceiling() {
        let heights = sector.ceiling_heights();
        let split = sector.ceiling_split();
        if let Some((slot, uvs)) = merged_ceiling_surface(sector) {
            if let Some(&base_material) = materials.get(slot as usize) {
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Ceiling,
                        sx,
                        sz,
                        center: horizontal_face_center(sx, sz, sector_size, heights),
                        baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                        ordinal: 0,
                    },
                    base_material,
                );
                surfaces = surfaces.saturating_add(1);
                emit_ceiling(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    uvs,
                    material,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        } else {
            for triangle_index in 0..2 {
                if !sector.ceiling_triangle_present(triangle_index) {
                    continue;
                }
                let Some(slot) = sector.ceiling_triangle_material(triangle_index) else {
                    continue;
                };
                let Some(&base_material) = materials.get(slot as usize) else {
                    continue;
                };
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Ceiling,
                        sx,
                        sz,
                        center: horizontal_triangle_center(
                            sx,
                            sz,
                            sector_size,
                            heights,
                            split,
                            triangle_index,
                        ),
                        baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                        ordinal: triangle_index as u16,
                    },
                    base_material,
                );
                surfaces = surfaces.saturating_add(1);
                emit_ceiling_triangle(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    triangle_index,
                    sector.ceiling_triangle_uvs(triangle_index),
                    material,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
    }

    let mut i = 0;
    while i < sector.wall_count() {
        if let Some(wall) = room.sector_wall(sector, i) {
            if let Some(&base_material) = materials.get(wall.material() as usize) {
                let Some(center) = wall_face_center(
                    sx,
                    sz,
                    sector_size,
                    wall.direction(),
                    wall.heights(),
                    wall.shape(),
                ) else {
                    i += 1;
                    continue;
                };
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Wall {
                            direction: wall.direction(),
                        },
                        sx,
                        sz,
                        center,
                        baked_vertex_rgb: baked_vertex_rgb(room.wall_light(sector, i)),
                        ordinal: i,
                    },
                    base_material,
                );
                surfaces = surfaces.saturating_add(1);
                emit_wall(
                    sx,
                    sz,
                    sector_size,
                    wall.direction(),
                    wall.shape(),
                    wall.heights(),
                    wall.uvs(),
                    material,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
        i += 1;
    }

    surfaces
}

#[allow(clippy::too_many_arguments)]
fn draw_sector_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    room: RoomRender<'_, '_>,
    sx: u16,
    sz: u16,
    sector: crate::SectorRender,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> u16 {
    let sector_size = room.sector_size();
    let mut surfaces = 0u16;

    if sector.has_floor() {
        let heights = sector.floor_heights();
        let split = sector.floor_split();
        if let Some((slot, uvs)) = merged_floor_surface(sector) {
            if let Some(&material) = materials.get(slot as usize) {
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Floor,
                    sx,
                    sz,
                    center: horizontal_face_center(sx, sz, sector_size, heights),
                    baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                    ordinal: 0,
                };
                surfaces = surfaces.saturating_add(1);
                emit_floor_vertex_lit(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    uvs,
                    material,
                    sample,
                    lighting,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        } else {
            for triangle_index in 0..2 {
                if !sector.floor_triangle_present(triangle_index) {
                    continue;
                }
                let Some(slot) = sector.floor_triangle_material(triangle_index) else {
                    continue;
                };
                let Some(&material) = materials.get(slot as usize) else {
                    continue;
                };
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Floor,
                    sx,
                    sz,
                    center: horizontal_triangle_center(
                        sx,
                        sz,
                        sector_size,
                        heights,
                        split,
                        triangle_index,
                    ),
                    baked_vertex_rgb: baked_vertex_rgb(room.floor_light(sx, sz)),
                    ordinal: triangle_index as u16,
                };
                surfaces = surfaces.saturating_add(1);
                emit_floor_triangle_vertex_lit(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    triangle_index,
                    sector.floor_triangle_uvs(triangle_index),
                    material,
                    sample,
                    lighting,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
    }

    if sector.has_ceiling() {
        let heights = sector.ceiling_heights();
        let split = sector.ceiling_split();
        if let Some((slot, uvs)) = merged_ceiling_surface(sector) {
            if let Some(&material) = materials.get(slot as usize) {
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Ceiling,
                    sx,
                    sz,
                    center: horizontal_face_center(sx, sz, sector_size, heights),
                    baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                    ordinal: 0,
                };
                surfaces = surfaces.saturating_add(1);
                emit_ceiling_vertex_lit(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    uvs,
                    material,
                    sample,
                    lighting,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        } else {
            for triangle_index in 0..2 {
                if !sector.ceiling_triangle_present(triangle_index) {
                    continue;
                }
                let Some(slot) = sector.ceiling_triangle_material(triangle_index) else {
                    continue;
                };
                let Some(&material) = materials.get(slot as usize) else {
                    continue;
                };
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Ceiling,
                    sx,
                    sz,
                    center: horizontal_triangle_center(
                        sx,
                        sz,
                        sector_size,
                        heights,
                        split,
                        triangle_index,
                    ),
                    baked_vertex_rgb: baked_vertex_rgb(room.ceiling_light(sx, sz)),
                    ordinal: triangle_index as u16,
                };
                surfaces = surfaces.saturating_add(1);
                emit_ceiling_triangle_vertex_lit(
                    sx,
                    sz,
                    sector_size,
                    heights,
                    split,
                    triangle_index,
                    sector.ceiling_triangle_uvs(triangle_index),
                    material,
                    sample,
                    lighting,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
    }

    let mut i = 0;
    while i < sector.wall_count() {
        if let Some(wall) = room.sector_wall(sector, i) {
            if let Some(&material) = materials.get(wall.material() as usize) {
                let Some(center) = wall_face_center(
                    sx,
                    sz,
                    sector_size,
                    wall.direction(),
                    wall.heights(),
                    wall.shape(),
                ) else {
                    i += 1;
                    continue;
                };
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Wall {
                        direction: wall.direction(),
                    },
                    sx,
                    sz,
                    center,
                    baked_vertex_rgb: baked_vertex_rgb(room.wall_light(sector, i)),
                    ordinal: i,
                };
                surfaces = surfaces.saturating_add(1);
                emit_wall_vertex_lit(
                    sx,
                    sz,
                    sector_size,
                    wall.direction(),
                    wall.shape(),
                    wall.heights(),
                    wall.uvs(),
                    material,
                    sample,
                    lighting,
                    camera,
                    options,
                    triangles,
                    world,
                );
            }
        }
        i += 1;
    }

    surfaces
}

fn grid_cell_for_world(value: i32, sector_size: i32) -> i32 {
    if value >= 0 {
        value / sector_size
    } else {
        (value - sector_size + 1) / sector_size
    }
}

fn sector_y_bounds(room: RoomRender<'_, '_>, sector: crate::SectorRender) -> (i32, i32) {
    let mut min_y = i32::MAX;
    let mut max_y = i32::MIN;
    let mut any = false;

    if sector.has_floor() {
        include_heights(&mut min_y, &mut max_y, &mut any, sector.floor_heights());
    }
    if sector.has_ceiling() {
        include_heights(&mut min_y, &mut max_y, &mut any, sector.ceiling_heights());
    }

    let mut i = 0;
    while i < sector.wall_count() {
        if let Some(wall) = room.sector_wall(sector, i) {
            include_heights(&mut min_y, &mut max_y, &mut any, wall.heights());
        }
        i += 1;
    }

    if any {
        (min_y, max_y)
    } else {
        (0, room.sector_size())
    }
}

fn include_heights(min_y: &mut i32, max_y: &mut i32, any: &mut bool, heights: [i32; 4]) {
    let mut i = 0;
    while i < heights.len() {
        *min_y = (*min_y).min(heights[i]);
        *max_y = (*max_y).max(heights[i]);
        *any = true;
        i += 1;
    }
}

fn cell_visible_to_camera(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    sx: u16,
    sz: u16,
    sector_size: i32,
    min_y: i32,
    max_y: i32,
    screen_margin: i32,
) -> bool {
    let (center, radius) = cell_visibility_bounds(sx, sz, sector_size, min_y, max_y);
    cell_visibility_visible_to_camera(camera, options, center, radius, screen_margin)
}

#[inline(always)]
fn cell_visibility_bounds(
    sx: u16,
    sz: u16,
    sector_size: i32,
    min_y: i32,
    max_y: i32,
) -> (WorldVertex, i32) {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let center = WorldVertex::new((x0 + x1) / 2, (min_y + max_y) / 2, (z0 + z1) / 2);
    let half_height = ((max_y - min_y).abs() / 2).max(sector_size / 2);
    let radius = sector_size.saturating_add(half_height);
    (center, radius)
}

#[inline(always)]
fn cell_visibility_visible_to_camera(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    center: WorldVertex,
    radius: i32,
    screen_margin: i32,
) -> bool {
    let view = camera.view_vertex(center);
    let near = camera.projection.near_z.max(1);
    let far = options.depth_range.far().max(near);
    if view.z < near.saturating_sub(radius) || view.z > far.saturating_add(radius) {
        return false;
    }

    let z = view.z.max(near);
    let focal = camera.projection.focal_length.max(1);
    let half_w = (camera.projection.screen_x as i32)
        .saturating_add(screen_margin)
        .max(1);
    let half_h = (camera.projection.screen_y as i32)
        .saturating_add(screen_margin)
        .max(1);
    let projected_x = view.x.abs().saturating_sub(radius).saturating_mul(focal);
    let projected_y = view.y.abs().saturating_sub(radius).saturating_mul(focal);
    projected_x <= half_w.saturating_mul(z) && projected_y <= half_h.saturating_mul(z)
}

/// Emit one floor quad. Cooked corners are `[NW, NE, SE, SW]`,
/// which already faces upward into playable space.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn emit_floor<const OT: usize>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let verts = [
        WorldVertex::new(x0, heights[0], z0),
        WorldVertex::new(x1, heights[1], z0),
        WorldVertex::new(x1, heights[2], z1),
        WorldVertex::new(x0, heights[3], z1),
    ];
    submit_split_quad(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        uvs,
        split,
        triangles,
        world,
    );
}

/// Emit one ceiling quad. Cooked corners are `[NW, NE, SE, SW]`;
/// runtime flips them so front-sided ceilings face the room
/// interior/underside.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn emit_ceiling<const OT: usize>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let verts = reverse_quad_winding([
        WorldVertex::new(x0, heights[0], z0),
        WorldVertex::new(x1, heights[1], z0),
        WorldVertex::new(x1, heights[2], z1),
        WorldVertex::new(x0, heights[3], z1),
    ]);
    submit_split_quad(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        reverse_quad_winding(uvs),
        split,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_floor_triangle<const OT: usize>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    submit_split_triangle(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        horizontal_vertices(sx, sz, sector_size, heights),
        uvs,
        split,
        triangle_index,
        false,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_ceiling_triangle<const OT: usize>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    submit_split_triangle(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        horizontal_vertices(sx, sz, sector_size, heights),
        uvs,
        split,
        triangle_index,
        true,
        triangles,
        world,
    );
}

/// Emit one wall quad. Wall heights `[BL, BR, TR, TL]` map onto
/// the cell's edge endpoints by direction.
#[allow(clippy::too_many_arguments)]
fn emit_wall<const OT: usize>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    shape: u16,
    heights: [i32; 4],
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(verts) = wall_vertices(sx, sz, sector_size, direction, heights) else {
        return;
    };
    let material = wall_material(material);
    if let Some((split, triangle_index)) = wall_shape_triangle(shape) {
        submit_split_triangle(
            camera,
            options,
            CullMode::Back,
            material,
            verts,
            uvs,
            split,
            triangle_index as usize,
            false,
            triangles,
            world,
        );
        return;
    }
    submit_quad(
        camera,
        options,
        CullMode::Back,
        material,
        verts,
        uvs,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn emit_floor_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let verts = [
        WorldVertex::new(x0, heights[0], z0),
        WorldVertex::new(x1, heights[1], z0),
        WorldVertex::new(x1, heights[2], z1),
        WorldVertex::new(x0, heights[3], z1),
    ];
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_quad_vertex_lit(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        uvs,
        colors,
        split,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn emit_ceiling_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let verts = reverse_quad_winding([
        WorldVertex::new(x0, heights[0], z0),
        WorldVertex::new(x1, heights[1], z0),
        WorldVertex::new(x1, heights[2], z1),
        WorldVertex::new(x0, heights[3], z1),
    ]);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_quad_vertex_lit(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        reverse_quad_winding(uvs),
        colors,
        split,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_floor_triangle_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let verts = horizontal_vertices(sx, sz, sector_size, heights);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_triangle_vertex_lit(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        uvs,
        colors,
        split,
        triangle_index,
        false,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_ceiling_triangle_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let verts = horizontal_vertices(sx, sz, sector_size, heights);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_triangle_vertex_lit(
        camera,
        options.with_depth_policy(DepthPolicy::Farthest),
        CullMode::Back,
        material,
        verts,
        uvs,
        colors,
        split,
        triangle_index,
        true,
        triangles,
        world,
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_wall_vertex_lit<const OT: usize, L: WorldSurfaceLighting>(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    shape: u16,
    heights: [i32; 4],
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(verts) = wall_vertices(sx, sz, sector_size, direction, heights) else {
        return;
    };
    let material = wall_material(material);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    if let Some((split, triangle_index)) = wall_shape_triangle(shape) {
        submit_split_triangle_vertex_lit(
            camera,
            options,
            CullMode::Back,
            material,
            verts,
            uvs,
            colors,
            split,
            triangle_index as usize,
            false,
            triangles,
            world,
        );
        return;
    }
    submit_quad_vertex_lit(
        camera,
        options,
        CullMode::Back,
        material,
        verts,
        uvs,
        colors,
        triangles,
        world,
    );
}

fn vertex_lighting_colors<L: WorldSurfaceLighting>(
    lighting: &L,
    sample: WorldSurfaceSample,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
) -> [(u8, u8, u8); 4] {
    lighting.shade_vertices(sample, verts, material)
}

/// Project + submit one textured quad along the standard
/// `submit_textured_quad` 0–2 diagonal.
#[allow(clippy::too_many_arguments)]
fn submit_quad<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(projected) = camera.project_world_quad(verts) else {
        return;
    };
    submit_sided_projected_quad(world, triangles, projected, uvs, material, options, cull);
}

/// Project + submit a split-aware textured quad. `split == 0`
/// keeps the standard NW→SE diagonal; `split == 1` flips to
/// NE→SW. UVs are kept in the same `[NW, NE, SE, SW]` slot
/// order as the input verts, so the texture orientation
/// doesn't change with the diagonal -- only the triangulation
/// boundary moves.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
fn submit_split_quad<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    split: u8,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    if split != SPLIT_NE_SW {
        // Standard split shares the existing helper -- same
        // triangulation `submit_textured_quad` always used.
        submit_quad(
            camera, options, cull, material, verts, uvs, triangles, world,
        );
        return;
    }
    let Some(mut projected) = camera.project_world_quad(verts) else {
        return;
    };
    let mut uvs = uvs;
    if material.sidedness == SurfaceSidedness::Back {
        projected = reverse_quad_winding(projected);
        uvs = reverse_quad_winding(uvs);
    }
    uvs = material_uvs(material, uvs);
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, cull))
        .with_material_layer(material.texture);
    let [(a, b, c), (d, e, f)] = SPLIT_NE_SW_TRIANGLES;
    let stats = world.submit_textured_triangle(
        triangles,
        [projected[a], projected[b], projected[c]],
        [uvs[a], uvs[b], uvs[c]],
        material.texture,
        opts,
    );
    if stats.primitive_overflow || stats.command_overflow {
        return;
    }
    let _ = world.submit_textured_triangle(
        triangles,
        [projected[d], projected[e], projected[f]],
        [uvs[d], uvs[e], uvs[f]],
        material.texture,
        opts,
    );
}

#[allow(clippy::too_many_arguments)]
fn submit_split_triangle<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(projected) = camera.project_world_quad(verts) else {
        return;
    };
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, cull))
        .with_material_layer(material.texture);
    let uvs = material_uvs(material, uvs);
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    let (a, b, c) = tri;
    let _ = world.submit_textured_triangle(
        triangles,
        [projected[a], projected[b], projected[c]],
        [uvs[a], uvs[b], uvs[c]],
        material.texture,
        opts,
    );
}

#[allow(clippy::too_many_arguments)]
fn submit_quad_vertex_lit<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(projected) = camera.project_world_quad(verts) else {
        return;
    };
    submit_sided_projected_gouraud_quad(
        world,
        triangles,
        projected,
        uvs,
        colors,
        material,
        options,
        cull,
        SPLIT_NW_SE_TRIANGLES,
    );
}

#[allow(clippy::too_many_arguments)]
fn submit_split_quad_vertex_lit<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    split: u8,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(projected) = camera.project_world_quad(verts) else {
        return;
    };
    let split_triangles = if split == SPLIT_NE_SW {
        SPLIT_NE_SW_TRIANGLES
    } else {
        SPLIT_NW_SE_TRIANGLES
    };
    submit_sided_projected_gouraud_quad(
        world,
        triangles,
        projected,
        uvs,
        colors,
        material,
        options,
        cull,
        split_triangles,
    );
}

#[allow(clippy::too_many_arguments)]
fn submit_split_triangle_vertex_lit<const OT: usize>(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    cull: CullMode,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(projected) = camera.project_world_quad(verts) else {
        return;
    };
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, cull))
        .with_material_layer(material.texture);
    let uvs = material_uvs(material, uvs);
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    let (a, b, c) = tri;
    let _ = world.submit_textured_gouraud_triangle(
        triangles,
        [projected[a], projected[b], projected[c]],
        [uvs[a], uvs[b], uvs[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
    );
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_projected_split_triangle_vertex_lit_cached_uvs<const OT: usize>(
    projected: [crate::render3d::ProjectedVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    cull: CullMode,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, cull))
        .with_material_layer(material.texture);
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    let (a, b, c) = tri;
    let _ = world.submit_textured_gouraud_triangle(
        triangles,
        [projected[a], projected[b], projected[c]],
        [uvs[a], uvs[b], uvs[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
    );
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_sided_projected_gouraud_quad_cached_uvs<const OT: usize>(
    world: &mut WorldRenderPass<'_, '_, OT>,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    verts: [crate::render3d::ProjectedVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    base_cull: CullMode,
    split_triangles: [(usize, usize, usize); 2],
) {
    let (verts, uvs, colors) = match material.sidedness {
        SurfaceSidedness::Back => (
            reverse_quad_winding(verts),
            reverse_quad_winding(uvs),
            reverse_quad_winding(colors),
        ),
        SurfaceSidedness::Front | SurfaceSidedness::Both => (verts, uvs, colors),
    };
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, base_cull))
        .with_material_layer(material.texture);
    let [(a, b, c), (d, e, f)] = split_triangles;
    let stats = world.submit_textured_gouraud_triangle(
        triangles,
        [verts[a], verts[b], verts[c]],
        [uvs[a], uvs[b], uvs[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
    );
    if stats.primitive_overflow || stats.command_overflow {
        return;
    }
    let _ = world.submit_textured_gouraud_triangle(
        triangles,
        [verts[d], verts[e], verts[f]],
        [uvs[d], uvs[e], uvs[f]],
        [colors[d], colors[e], colors[f]],
        material.texture,
        opts,
    );
}

#[allow(clippy::too_many_arguments)]
fn submit_sided_projected_gouraud_quad<const OT: usize>(
    world: &mut WorldRenderPass<'_, '_, OT>,
    triangles: &mut PrimitiveArena<'_, TriTexturedGouraud>,
    verts: [crate::render3d::ProjectedVertex; 4],
    uvs: [(u8, u8); 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    base_cull: CullMode,
    split_triangles: [(usize, usize, usize); 2],
) {
    let (verts, uvs, colors) = match material.sidedness {
        SurfaceSidedness::Back => (
            reverse_quad_winding(verts),
            reverse_quad_winding(uvs),
            reverse_quad_winding(colors),
        ),
        SurfaceSidedness::Front | SurfaceSidedness::Both => (verts, uvs, colors),
    };
    let uvs = material_uvs(material, uvs);
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, base_cull))
        .with_material_layer(material.texture);
    let [(a, b, c), (d, e, f)] = split_triangles;
    let stats = world.submit_textured_gouraud_triangle(
        triangles,
        [verts[a], verts[b], verts[c]],
        [uvs[a], uvs[b], uvs[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
    );
    if stats.primitive_overflow || stats.command_overflow {
        return;
    }
    let _ = world.submit_textured_gouraud_triangle(
        triangles,
        [verts[d], verts[e], verts[f]],
        [uvs[d], uvs[e], uvs[f]],
        [colors[d], colors[e], colors[f]],
        material.texture,
        opts,
    );
}

fn submit_sided_projected_quad<const OT: usize>(
    world: &mut WorldRenderPass<'_, '_, OT>,
    triangles: &mut PrimitiveArena<'_, TriTextured>,
    verts: [crate::render3d::ProjectedVertex; 4],
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    base_cull: CullMode,
) {
    let (verts, uvs) = match material.sidedness {
        SurfaceSidedness::Back => (reverse_quad_winding(verts), reverse_quad_winding(uvs)),
        SurfaceSidedness::Front | SurfaceSidedness::Both => (verts, uvs),
    };
    let uvs = material_uvs(material, uvs);
    let opts = options
        .with_cull_mode(cull_for_sidedness(material.sidedness, base_cull))
        .with_material_layer(material.texture);
    let _ = world.submit_textured_quad(triangles, verts, uvs, material.texture, opts);
}

const fn cull_for_sidedness(sidedness: SurfaceSidedness, base: CullMode) -> CullMode {
    match sidedness {
        SurfaceSidedness::Both => CullMode::None,
        SurfaceSidedness::Front | SurfaceSidedness::Back => base,
    }
}

const fn normalize_room_texture_uv_size(size: u8) -> u8 {
    if size == 0 || size > ROOM_TEXTURE_UV_SIZE {
        ROOM_TEXTURE_UV_SIZE
    } else {
        size
    }
}

fn material_uvs(material: WorldRenderMaterial, uvs: [(u8, u8); 4]) -> [(u8, u8); 4] {
    let width = normalize_room_texture_uv_size(material.texture_width);
    let height = normalize_room_texture_uv_size(material.texture_height);
    if width == ROOM_TEXTURE_UV_SIZE && height == ROOM_TEXTURE_UV_SIZE {
        return uvs;
    }
    [
        scale_material_uv(uvs[0], width, height),
        scale_material_uv(uvs[1], width, height),
        scale_material_uv(uvs[2], width, height),
        scale_material_uv(uvs[3], width, height),
    ]
}

fn scale_material_uv((u, v): (u8, u8), width: u8, height: u8) -> (u8, u8) {
    (
        scale_material_uv_component(u, width),
        scale_material_uv_component(v, height),
    )
}

fn scale_material_uv_component(value: u8, size: u8) -> u8 {
    let scaled = (u16::from(value) * u16::from(size)) / u16::from(ROOM_TEXTURE_UV_SIZE);
    scaled.min(u16::from(u8::MAX)) as u8
}

/// Triangle index pairs used when a sector authors the
/// alternate (NE→SW) diagonal. The source topology lives in the
/// cooked world contract; this tuple form just matches the local
/// renderer call sites.
const SPLIT_NE_SW_TRIANGLES: [(usize, usize, usize); 2] =
    tuple_triangles(psx_asset::world_topology::HORIZONTAL_NE_SW_TRIANGLES);

/// Triangle index pairs used by the standard NW→SE diagonal.
const SPLIT_NW_SE_TRIANGLES: [(usize, usize, usize); 2] =
    tuple_triangles(psx_asset::world_topology::HORIZONTAL_NW_SE_TRIANGLES);

const fn tuple_triangles(triangles: [[usize; 3]; 2]) -> [(usize, usize, usize); 2] {
    [
        (triangles[0][0], triangles[0][1], triangles[0][2]),
        (triangles[1][0], triangles[1][1], triangles[1][2]),
    ]
}

/// Resolve the per-split triangulation. Default split (0) and
/// every unrecognised id fall back to the NW-SE diagonal so a
/// future split id never silently empties the room.
const fn split_triangles_runtime(split: u8) -> [(usize, usize, usize); 2] {
    if split == SPLIT_NE_SW {
        SPLIT_NE_SW_TRIANGLES
    } else {
        SPLIT_NW_SE_TRIANGLES
    }
}

/// Test-facing alias for the runtime triangulation table.
#[cfg(test)]
const fn split_triangles(split: u8) -> [(usize, usize, usize); 2] {
    split_triangles_runtime(split)
}

/// World-space bounds of a sector cell rooted at world `(0, 0)`.
/// Returns `(x0, x1, z0, z1)` so individual quads can pick the
/// corners they need by index.
const fn cell_bounds(sx: u16, sz: u16, sector_size: i32) -> (i32, i32, i32, i32) {
    let x0 = (sx as i32) * sector_size;
    let x1 = ((sx as i32) + 1) * sector_size;
    let z0 = (sz as i32) * sector_size;
    let z1 = ((sz as i32) + 1) * sector_size;
    (x0, x1, z0, z1)
}

fn horizontal_vertices(sx: u16, sz: u16, sector_size: i32, heights: [i32; 4]) -> [WorldVertex; 4] {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    [
        WorldVertex::new(x0, heights[0], z0),
        WorldVertex::new(x1, heights[1], z0),
        WorldVertex::new(x1, heights[2], z1),
        WorldVertex::new(x0, heights[3], z1),
    ]
}

#[allow(dead_code)]
fn horizontal_face_center(sx: u16, sz: u16, sector_size: i32, heights: [i32; 4]) -> RoomPoint {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let cy = average4_i32(heights[0], heights[1], heights[2], heights[3]);
    RoomPoint::new((x0 + x1) / 2, cy, (z0 + z1) / 2)
}

fn horizontal_triangle_center(
    sx: u16,
    sz: u16,
    sector_size: i32,
    heights: [i32; 4],
    split: u8,
    triangle_index: usize,
) -> RoomPoint {
    let verts = horizontal_vertices(sx, sz, sector_size, heights);
    let (a, b, c) = split_triangles_runtime(split)[triangle_index.min(1)];
    RoomPoint::new(
        (verts[a].x + verts[b].x + verts[c].x) / 3,
        (verts[a].y + verts[b].y + verts[c].y) / 3,
        (verts[a].z + verts[b].z + verts[c].z) / 3,
    )
}

fn wall_face_center(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    heights: [i32; 4],
    shape: u16,
) -> Option<RoomPoint> {
    let verts = wall_vertices(sx, sz, sector_size, direction, heights)?;
    Some(wall_shape_center(verts, shape))
}

fn wall_shape_center(verts: [WorldVertex; 4], shape: u16) -> RoomPoint {
    if let Some((split, triangle_index)) = wall_shape_triangle(shape) {
        let (a, b, c) = split_triangles_runtime(split)[triangle_index as usize];
        return RoomPoint::new(
            (verts[a].x + verts[b].x + verts[c].x) / 3,
            (verts[a].y + verts[b].y + verts[c].y) / 3,
            (verts[a].z + verts[b].z + verts[c].z) / 3,
        );
    }
    RoomPoint::new(
        average4_i32(verts[0].x, verts[1].x, verts[2].x, verts[3].x),
        average4_i32(verts[0].y, verts[1].y, verts[2].y, verts[3].y),
        average4_i32(verts[0].z, verts[1].z, verts[2].z, verts[3].z),
    )
}

fn average4_i32(a: i32, b: i32, c: i32, d: i32) -> i32 {
    a.saturating_add(b).saturating_add(c).saturating_add(d) / 4
}

const fn wall_shape_triangle(shape: u16) -> Option<(u8, u8)> {
    match psx_asset::world_topology::wall_shape_triangle(shape) {
        Some((split, triangle_index)) => Some((split, triangle_index)),
        None => None,
    }
}

fn wall_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    direction: u8,
    heights: [i32; 4],
) -> Option<[WorldVertex; 4]> {
    let (x0, x1, z0, z1) = cell_bounds(sx, sz, sector_size);
    let bl_br_tr_tl = match direction {
        DIR_NORTH => [
            WorldVertex::new(x0, heights[0], z0),
            WorldVertex::new(x1, heights[1], z0),
            WorldVertex::new(x1, heights[2], z0),
            WorldVertex::new(x0, heights[3], z0),
        ],
        DIR_EAST => [
            WorldVertex::new(x1, heights[0], z0),
            WorldVertex::new(x1, heights[1], z1),
            WorldVertex::new(x1, heights[2], z1),
            WorldVertex::new(x1, heights[3], z0),
        ],
        DIR_SOUTH => [
            WorldVertex::new(x1, heights[0], z1),
            WorldVertex::new(x0, heights[1], z1),
            WorldVertex::new(x0, heights[2], z1),
            WorldVertex::new(x1, heights[3], z1),
        ],
        DIR_WEST => [
            WorldVertex::new(x0, heights[0], z1),
            WorldVertex::new(x0, heights[1], z0),
            WorldVertex::new(x0, heights[2], z0),
            WorldVertex::new(x0, heights[3], z1),
        ],
        DIR_NORTH_WEST_SOUTH_EAST => [
            WorldVertex::new(x0, heights[0], z0),
            WorldVertex::new(x1, heights[1], z1),
            WorldVertex::new(x1, heights[2], z1),
            WorldVertex::new(x0, heights[3], z0),
        ],
        DIR_NORTH_EAST_SOUTH_WEST => [
            WorldVertex::new(x1, heights[0], z0),
            WorldVertex::new(x0, heights[1], z1),
            WorldVertex::new(x0, heights[2], z1),
            WorldVertex::new(x1, heights[3], z0),
        ],
        _ => return None,
    };
    Some(bl_br_tr_tl)
}

#[cfg(test)]
fn wall_uvs() -> [(u8, u8); 4] {
    WALL_UVS
}

fn reverse_quad_winding<T: Copy>(corners: [T; 4]) -> [T; 4] {
    [corners[0], corners[3], corners[2], corners[1]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Angle;
    use crate::{ProjectedVertex, WorldProjection, Q12};

    /// Helper: the two indices both triangles in `[t0, t1]`
    /// share form the diagonal of the split. Returned sorted
    /// so test assertions are stable.
    fn diagonal(triangles: [(usize, usize, usize); 2]) -> [usize; 2] {
        let [t0, t1] = triangles;
        let a = [t0.0, t0.1, t0.2];
        let b = [t1.0, t1.1, t1.2];
        let mut shared = [usize::MAX; 2];
        let mut n = 0;
        for &i in &a {
            if b.contains(&i) && n < 2 {
                shared[n] = i;
                n += 1;
            }
        }
        shared.sort();
        shared
    }

    #[test]
    fn split_zero_uses_nw_se_diagonal() {
        // Standard split -- both triangles meet at corners 0
        // and 2, which is the diagonal `submit_textured_quad`
        // has always used.
        let triangles = split_triangles(SPLIT_NW_SE);
        assert_eq!(triangles[0], (0, 1, 2));
        assert_eq!(triangles[1], (0, 2, 3));
        assert_eq!(diagonal(triangles), [0, 2]);
    }

    #[test]
    fn split_one_uses_ne_sw_diagonal() {
        // Alternate split -- the two triangles share corners
        // 1 (NE) and 3 (SW), which is the perpendicular
        // diagonal. This is the case the prior renderer got
        // wrong: it used the NW→SE diagonal regardless of
        // the cooked / collision split id.
        let triangles = split_triangles(SPLIT_NE_SW);
        assert_eq!(triangles[0], (0, 1, 3));
        assert_eq!(triangles[1], (1, 2, 3));
        assert_eq!(diagonal(triangles), [1, 3]);
    }

    #[test]
    fn unknown_split_id_falls_back_to_nw_se() {
        // Future split-ids (e.g. quad subdivision) shouldn't
        // empty the room -- fall through to the standard
        // diagonal so the user sees something while the
        // schema catches up.
        for unknown in [2u8, 3, 9, 200] {
            assert_eq!(split_triangles(unknown), SPLIT_NW_SE_TRIANGLES);
        }
    }

    #[test]
    fn merge_horizontal_triangle_surface_combines_matching_triangles() {
        let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
        assert_eq!(
            merge_horizontal_triangle_surface([Some(3), Some(3)], [uvs, uvs]),
            Some((3, uvs))
        );
    }

    #[test]
    fn merge_horizontal_triangle_surface_preserves_real_splits() {
        let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
        let shifted_uvs = [(0, 0), (32, 0), (32, TILE_UV), (0, TILE_UV)];

        assert_eq!(
            merge_horizontal_triangle_surface([Some(3), Some(4)], [uvs, uvs]),
            None
        );
        assert_eq!(
            merge_horizontal_triangle_surface([Some(3), Some(3)], [uvs, shifted_uvs]),
            None
        );
        assert_eq!(
            merge_horizontal_triangle_surface([Some(3), None], [uvs, uvs]),
            None
        );
    }

    #[test]
    fn each_split_covers_every_corner() {
        // Sanity: every triangulation must reference all four
        // corners across its two triangles, otherwise the quad
        // has a hole.
        for split in [SPLIT_NW_SE, SPLIT_NE_SW] {
            let [t0, t1] = split_triangles(split);
            let mut seen = [false; 4];
            for i in [t0.0, t0.1, t0.2, t1.0, t1.1, t1.2] {
                seen[i] = true;
            }
            assert!(seen.iter().all(|&v| v), "split {split} misses a corner");
        }
    }

    #[test]
    fn cardinal_wall_backs_face_their_owning_cell() {
        let projection = WorldProjection::new(160, 120, 200, 16);
        let y = 512;
        let center = WorldVertex::new(512, y, 512);
        let cases = [
            (
                DIR_NORTH,
                WorldCamera::from_basis(
                    projection,
                    center,
                    Q12::ZERO,
                    Q12::ONE,
                    Q12::ZERO,
                    Q12::ONE,
                ),
            ),
            (
                DIR_EAST,
                WorldCamera::from_basis(
                    projection,
                    center,
                    Q12::NEG_ONE,
                    Q12::ZERO,
                    Q12::ZERO,
                    Q12::ONE,
                ),
            ),
            (
                DIR_SOUTH,
                WorldCamera::from_basis(
                    projection,
                    center,
                    Q12::ZERO,
                    Q12::NEG_ONE,
                    Q12::ZERO,
                    Q12::ONE,
                ),
            ),
            (
                DIR_WEST,
                WorldCamera::from_basis(
                    projection,
                    center,
                    Q12::ONE,
                    Q12::ZERO,
                    Q12::ZERO,
                    Q12::ONE,
                ),
            ),
        ];

        for (direction, camera) in cases {
            let verts =
                wall_vertices(0, 0, 1024, direction, [0, 0, 1024, 1024]).expect("cardinal wall");
            let projected = camera
                .project_world_quad(verts)
                .expect("wall projects from owning cell");
            for (a, b, c) in SPLIT_NW_SE_TRIANGLES {
                assert!(
                    projected_triangle_area(projected[a], projected[b], projected[c]) < 0,
                    "direction {direction} wall back side should face owning cell"
                );
            }
        }
    }

    #[test]
    fn diagonal_wall_vertices_use_runtime_corner_convention() {
        let nw_se = wall_vertices(0, 0, 1024, DIR_NORTH_WEST_SOUTH_EAST, [10, 20, 30, 40])
            .expect("nw-se diagonal wall");
        assert_eq!(nw_se[0], WorldVertex::new(0, 10, 0));
        assert_eq!(nw_se[1], WorldVertex::new(1024, 20, 1024));
        assert_eq!(nw_se[2], WorldVertex::new(1024, 30, 1024));
        assert_eq!(nw_se[3], WorldVertex::new(0, 40, 0));

        let ne_sw = wall_vertices(0, 0, 1024, DIR_NORTH_EAST_SOUTH_WEST, [50, 60, 70, 80])
            .expect("ne-sw diagonal wall");
        assert_eq!(ne_sw[0], WorldVertex::new(1024, 50, 0));
        assert_eq!(ne_sw[1], WorldVertex::new(0, 60, 1024));
        assert_eq!(ne_sw[2], WorldVertex::new(0, 70, 1024));
        assert_eq!(ne_sw[3], WorldVertex::new(1024, 80, 0));
    }

    #[test]
    fn floors_face_playable_interior() {
        let projection = WorldProjection::new(160, 120, 200, 16);
        let camera = WorldCamera::orbit_yaw(
            projection,
            WorldVertex::new(512, 0, 512),
            1100,
            2048,
            Angle::ZERO,
        );
        let verts = [
            WorldVertex::new(0, 0, 0),
            WorldVertex::new(1024, 0, 0),
            WorldVertex::new(1024, 0, 1024),
            WorldVertex::new(0, 0, 1024),
        ];
        let projected = camera
            .project_world_quad(verts)
            .expect("floor projects from playable camera");

        for (a, b, c) in SPLIT_NW_SE_TRIANGLES {
            let area = projected_triangle_area(projected[a], projected[b], projected[c]);
            assert!(
                area > 0,
                "floor should not be culled from above: area={area} projected={projected:?}"
            );
        }
    }

    #[test]
    fn wall_uvs_follow_physical_wall_corner_order() {
        assert_eq!(
            wall_uvs(),
            [(0, TILE_UV), (TILE_UV, TILE_UV), (TILE_UV, 0), (0, 0)]
        );
    }

    #[test]
    fn wall_material_swaps_front_and_back_only() {
        let texture = TextureMaterial::opaque(0, 0, (128, 128, 128));
        assert_eq!(
            wall_material(WorldRenderMaterial::front(texture)).sidedness,
            SurfaceSidedness::Back
        );
        assert_eq!(
            wall_material(WorldRenderMaterial::back(texture)).sidedness,
            SurfaceSidedness::Front
        );
        assert_eq!(
            wall_material(WorldRenderMaterial::both(texture)).sidedness,
            SurfaceSidedness::Both
        );
    }

    #[test]
    fn material_texture_size_projects_default_uvs_once() {
        let material = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)))
            .with_texture_size(32, 32);
        assert_eq!(
            material_uvs(
                material,
                [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)]
            ),
            [(0, 0), (32, 0), (32, 32), (0, 32)]
        );
    }

    #[test]
    fn material_texture_size_preserves_authored_repeat_count() {
        let material = WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128)))
            .with_texture_size(32, 64);
        assert_eq!(
            material_uvs(material, [(0, 0), (128, 0), (128, TILE_UV), (0, TILE_UV)]),
            [(0, 0), (64, 0), (64, TILE_UV), (0, TILE_UV)]
        );
    }

    #[test]
    fn floor_depth_uses_farthest_projected_corner() {
        const ZERO: TriTextured = TriTextured::new(
            [(0, 0), (0, 0), (0, 0)],
            [(0, 0), (0, 0), (0, 0)],
            0,
            0,
            (0, 0, 0),
        );
        let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
        let mut ot = crate::OtFrame::begin(&mut ot_storage);
        let mut triangle_storage = [const { ZERO }; 4];
        let mut triangles = PrimitiveArena::new(&mut triangle_storage);
        let mut commands = [crate::WorldTriCommand::EMPTY; 4];
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

        let projection = WorldProjection::new(160, 120, 200, 16);
        let camera = WorldCamera::orbit_yaw(
            projection,
            WorldVertex::new(512, 0, 512),
            1100,
            2048,
            Angle::ZERO,
        );
        let options =
            WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096))
                .with_textured_triangle_splitting(false);
        emit_floor(
            0,
            0,
            1024,
            [0, 0, 0, 0],
            SPLIT_NW_SE,
            [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)],
            WorldRenderMaterial::front(TextureMaterial::opaque(0, 0, (128, 128, 128))),
            &camera,
            options,
            &mut triangles,
            &mut pass,
        );
        assert_eq!(pass.command_len(), 2);
        drop(pass);

        let projected = camera
            .project_world_quad([
                WorldVertex::new(0, 0, 0),
                WorldVertex::new(1024, 0, 0),
                WorldVertex::new(1024, 0, 1024),
                WorldVertex::new(0, 0, 1024),
            ])
            .expect("floor projects from playable camera");
        let [(a, b, c), (d, e, f)] = SPLIT_NW_SE_TRIANGLES;
        assert_eq!(
            commands[0].depth_raw(),
            max3(projected[a].sz, projected[b].sz, projected[c].sz)
        );
        assert_eq!(
            commands[1].depth_raw(),
            max3(projected[d].sz, projected[e].sz, projected[f].sz)
        );
    }

    #[test]
    fn cached_full_ceiling_faces_playable_interior() {
        const ZERO: TriTexturedGouraud = TriTexturedGouraud::new(
            [(0, 0), (0, 0), (0, 0)],
            [(0, 0), (0, 0), (0, 0)],
            [(0, 0, 0), (0, 0, 0), (0, 0, 0)],
            0,
            0,
        );
        let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
        let mut ot = crate::OtFrame::begin(&mut ot_storage);
        let mut triangle_storage = [const { ZERO }; 4];
        let mut triangles = PrimitiveArena::new(&mut triangle_storage);
        let mut commands = [crate::WorldTriCommand::EMPTY; 4];
        let mut pass = WorldRenderPass::new(&mut ot, &mut commands);

        let projection = WorldProjection::new(160, 120, 200, 16);
        let camera = WorldCamera::orbit_yaw(
            projection,
            WorldVertex::new(512, 1024, 512),
            0,
            2048,
            Angle::ZERO,
        );
        let options =
            WorldSurfaceOptions::new(crate::DepthBand::whole(), crate::DepthRange::new(0, 4096))
                .with_textured_triangle_splitting(false);
        let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
        let vertices = horizontal_vertices(0, 0, 1024, [1024, 1024, 1024, 1024]);
        let surface = CachedRoomSurface::new(
            0,
            vertices,
            [0, 1, 2, 3],
            uvs,
            WorldSurfaceSample {
                kind: WorldSurfaceKind::Ceiling,
                sx: 0,
                sz: 0,
                center: horizontal_face_center(0, 0, 1024, [1024, 1024, 1024, 1024]),
                baked_vertex_rgb: None,
                ordinal: 0,
            },
            SPLIT_NW_SE,
            WHOLE_QUAD_TRIANGLE_INDEX,
        );

        assert_eq!(
            draw_cached_room_surface(
                surface,
                &[WorldRenderMaterial::front(TextureMaterial::opaque(
                    0,
                    0,
                    (128, 128, 128)
                ))],
                &NoWorldSurfaceLighting,
                &camera,
                options,
                &mut triangles,
                &mut pass,
            ),
            1
        );
        assert_eq!(pass.command_len(), 2);
    }

    #[test]
    fn horizontal_face_center_uses_cell_midpoint_and_average_height() {
        assert_eq!(
            horizontal_face_center(2, 3, 1024, [0, 512, 1024, 512]),
            RoomPoint::new(2560, 512, 3584)
        );
    }

    #[test]
    fn wall_face_center_uses_emitted_runtime_wall_geometry() {
        assert_eq!(
            wall_face_center(
                0,
                0,
                1024,
                DIR_EAST,
                [0, 0, 1024, 1024],
                psx_asset::WORLD_WALL_SHAPE_QUAD
            ),
            Some(RoomPoint::new(1024, 512, 512))
        );
        assert_eq!(
            wall_face_center(
                0,
                0,
                1024,
                DIR_NORTH,
                [0, 0, 1024, 1024],
                psx_asset::WORLD_WALL_SHAPE_QUAD
            ),
            Some(RoomPoint::new(512, 512, 0))
        );
        assert_eq!(
            wall_face_center(
                0,
                0,
                1024,
                DIR_NORTH,
                [0, 0, 1024, 1024],
                psx_asset::WORLD_WALL_SHAPE_DROP_TOP_RIGHT
            ),
            Some(RoomPoint::new(341, 341, 0))
        );
    }

    fn projected_triangle_area(a: ProjectedVertex, b: ProjectedVertex, c: ProjectedVertex) -> i32 {
        let ax = (b.sx as i32) - (a.sx as i32);
        let ay = (b.sy as i32) - (a.sy as i32);
        let bx = (c.sx as i32) - (a.sx as i32);
        let by = (c.sy as i32) - (a.sy as i32);
        ax * by - ay * bx
    }

    const fn max3(a: i32, b: i32, c: i32) -> i32 {
        let ab = if a > b { a } else { b };
        if ab > c {
            ab
        } else {
            c
        }
    }
}
