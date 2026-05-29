//! Drawing helpers for cooked grid worlds.
//!
//! Walks a [`RoomRender`] and emits its floors / ceilings / walls
//! through [`WorldRenderPass::submit_textured_quad`]. Material slot
//! → runtime material is provided by the caller because the
//! current `.psxw` (VERSION 2) doesn't embed a material table.
//! See `docs/world-format-roadmap.md` for the future compact
//! format that will let this helper resolve materials itself.

use psx_gpu::{
    material::{TextureMaterial, TexturedGouraudPacketMaterial},
    prim::{TriTextured, TriTexturedGouraud},
};
use psx_level::{
    LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord, LevelCachedRoomVertexRecord,
};

#[cfg(feature = "room-surface-profile")]
use crate::render3d::TexturedGouraudSubmitMicroProfile;

use crate::{
    render3d::{
        project_world_vertex_indices_gte, CullMode, DepthPolicy, LoadedWorldCameraGte,
        PreparedTriangleDepth, ProjectedVertex, ViewVertex,
    },
    PrimitiveSink, RoomPoint, RoomRender, WorldCamera, WorldRenderPass, WorldSurfaceOptions,
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
    /// Prepacked textured-Gouraud packet state derived from `texture`.
    pub gouraud_packet: TexturedGouraudPacketMaterial,
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
            gouraud_packet: texture.textured_gouraud_packet_material(),
            sidedness: SurfaceSidedness::Front,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Build a back-sided material.
    pub const fn back(texture: TextureMaterial) -> Self {
        Self {
            texture,
            gouraud_packet: texture.textured_gouraud_packet_material(),
            sidedness: SurfaceSidedness::Back,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Build a double-sided material.
    pub const fn both(texture: TextureMaterial) -> Self {
        Self {
            texture,
            gouraud_packet: texture.textured_gouraud_packet_material(),
            sidedness: SurfaceSidedness::Both,
            texture_width: ROOM_TEXTURE_UV_SIZE,
            texture_height: ROOM_TEXTURE_UV_SIZE,
        }
    }

    /// Return a copy with the same texture state and sidedness but
    /// a different flat RGB tint.
    pub const fn with_tint(mut self, tint: (u8, u8, u8)) -> Self {
        self.texture = self.texture.with_tint(tint);
        self.gouraud_packet = self.texture.textured_gouraud_packet_material();
        self
    }

    /// Return a copy whose authored 64x64 face UVs are projected into
    /// the material's actual texture-window size.
    pub const fn with_texture_size(mut self, width: u8, height: u8) -> Self {
        self.texture_width = normalize_room_texture_uv_size(width);
        self.texture_height = normalize_room_texture_uv_size(height);
        self
    }

    /// Build a material descriptor for room-cache generation when
    /// only the texture-window dimensions matter.
    pub const fn cache_only(texture_width: u8, texture_height: u8) -> Self {
        Self::front(TextureMaterial::opaque(0, 0, (0x80, 0x80, 0x80)))
            .with_texture_size(texture_width, texture_height)
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

const fn wall_material_for_direction(
    mut material: WorldRenderMaterial,
    direction: u8,
) -> WorldRenderMaterial {
    // Cardinal wall windings make the owning cell's interior the back side.
    // Diagonal walls are freestanding cuts through a cell and are always used
    // from both sides, so they ignore the authored Front/Back distinction.
    match direction {
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST => {
            material.sidedness = SurfaceSidedness::Both;
            material
        }
        _ => wall_material(material),
    }
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
    /// Unique cached room vertices projected for the drawn cells.
    pub projected_vertices: u16,
    /// Floor/ceiling/wall surfaces handed to the projection path.
    pub surfaces_considered: u16,
}

/// Depth-key policy for indexed cached room drawing.
///
/// The PS1 has no z-buffer, so these modes trade stability, speed, and
/// overlap correctness for room geometry emitted from the cached
/// cell/surface path.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum CachedRoomDepthMode {
    /// Current fast path: each accepted cell provides one fixed depth key.
    #[default]
    FixedCell,
    /// Every cached surface computes its ordering-table key from its
    /// projected triangle vertices.
    PerTriangle,
    /// Keep fixed-cell depth for stable flat geometry, but use
    /// per-triangle depth for sloped or high-depth-span horizontal
    /// surfaces such as stair ramps.
    Hybrid,
    /// Like [`Self::Hybrid`], but also depth-sorts vertical surfaces
    /// with a large projected depth span. This is meant for testing
    /// ramp-vs-wall conflicts without paying full per-triangle cost.
    HybridWalls,
}

/// Runtime subdivision scope for cached room geometry.
///
/// The projected edge threshold still comes from
/// [`WorldSurfaceOptions::textured_split_max_edge`]. This enum decides
/// which cached room surfaces are allowed to spend that budget.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum CachedRoomSubdivisionMode {
    /// Current behavior: every submitted cached surface can subdivide
    /// when it exceeds the projected edge threshold.
    #[default]
    All,
    /// Only surfaces using per-triangle depth get visual subdivision.
    DepthSorted,
    /// Only slope/depth-risky surfaces get visual subdivision.
    Risky,
}

#[cfg(feature = "room-surface-profile")]
#[derive(Copy, Clone, Debug, Default)]
struct RoomSurfaceMicroProfile {
    submit_detail: TexturedGouraudSubmitMicroProfile,
    material_cycles: u32,
    projected_cycles: u32,
    screen_cycles: u32,
    kind_cycles: u32,
    backface_cycles: u32,
    lighting_cycles: u32,
    submit_cycles: u32,
    profiled: u32,
    material_misses: u32,
    projected_rejects: u32,
    screen_culled: u32,
    backface_culled: u32,
    floors: u32,
    ceilings: u32,
    walls: u32,
    whole_quads: u32,
    split_tris: u32,
    lighting_rejects: u32,
}

#[cfg(not(feature = "room-surface-profile"))]
#[derive(Copy, Clone, Debug, Default)]
struct RoomSurfaceMicroProfile;

impl RoomSurfaceMicroProfile {
    #[inline(always)]
    fn new() -> Self {
        #[cfg(feature = "room-surface-profile")]
        {
            Self::default()
        }
        #[cfg(not(feature = "room-surface-profile"))]
        {
            Self
        }
    }

    #[inline(always)]
    fn cycle() -> u32 {
        #[cfg(feature = "room-surface-profile")]
        {
            crate::telemetry::cycle_counter()
        }
        #[cfg(not(feature = "room-surface-profile"))]
        {
            0
        }
    }

    #[inline(always)]
    fn elapsed(start: u32) -> u32 {
        Self::cycle().wrapping_sub(start)
    }

    #[inline(always)]
    fn add_material(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.material_cycles = self.material_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_projected(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.projected_cycles = self.projected_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_screen(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.screen_cycles = self.screen_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_kind(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.kind_cycles = self.kind_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_backface(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.backface_cycles = self.backface_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_lighting(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.lighting_cycles = self.lighting_cycles.saturating_add(_cycles);
        }
    }

    #[inline(always)]
    fn add_submit(&mut self, _cycles: u32) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.submit_cycles = self.submit_cycles.saturating_add(_cycles);
        }
    }

    #[cfg(feature = "room-surface-profile")]
    #[inline(always)]
    fn submit_profile(&mut self) -> &mut TexturedGouraudSubmitMicroProfile {
        &mut self.submit_detail
    }

    #[inline(always)]
    fn count_profiled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.profiled = self.profiled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_material_miss(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.material_misses = self.material_misses.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_projected_reject(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.projected_rejects = self.projected_rejects.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_screen_culled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.screen_culled = self.screen_culled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_backface_culled(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.backface_culled = self.backface_culled.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_lighting_reject(&mut self) {
        #[cfg(feature = "room-surface-profile")]
        {
            self.lighting_rejects = self.lighting_rejects.saturating_add(1);
        }
    }

    #[inline(always)]
    fn count_kind(&mut self, _kind: WorldSurfaceKind) {
        #[cfg(feature = "room-surface-profile")]
        {
            match _kind {
                WorldSurfaceKind::Floor => self.floors = self.floors.saturating_add(1),
                WorldSurfaceKind::Ceiling => self.ceilings = self.ceilings.saturating_add(1),
                WorldSurfaceKind::Wall { .. } => self.walls = self.walls.saturating_add(1),
            }
        }
    }

    #[inline(always)]
    fn count_shape(&mut self, _triangle_index: u8) {
        #[cfg(feature = "room-surface-profile")]
        {
            if _triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                self.split_tris = self.split_tris.saturating_add(1);
            } else {
                self.whole_quads = self.whole_quads.saturating_add(1);
            }
        }
    }

    #[inline(always)]
    fn emit(self) {
        #[cfg(feature = "room-surface-profile")]
        {
            use crate::telemetry;
            telemetry::counter(
                telemetry::counter::ROOM_SURF_MATERIAL_CYCLES,
                self.material_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_PROJECTED_CYCLES,
                self.projected_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SCREEN_CYCLES,
                self.screen_cycles,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_KIND_CYCLES, self.kind_cycles);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_BACKFACE_CYCLES,
                self.backface_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_LIGHTING_CYCLES,
                self.lighting_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SUBMIT_CYCLES,
                self.submit_cycles,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_PROFILED, self.profiled);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_MATERIAL_MISSES,
                self.material_misses,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_PROJECTED_REJECTS,
                self.projected_rejects,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_SCREEN_CULLED,
                self.screen_culled,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SURF_BACKFACE_CULLED,
                self.backface_culled,
            );
            telemetry::counter(telemetry::counter::ROOM_SURF_FLOORS, self.floors);
            telemetry::counter(telemetry::counter::ROOM_SURF_CEILINGS, self.ceilings);
            telemetry::counter(telemetry::counter::ROOM_SURF_WALLS, self.walls);
            telemetry::counter(telemetry::counter::ROOM_SURF_WHOLE_QUADS, self.whole_quads);
            telemetry::counter(telemetry::counter::ROOM_SURF_SPLIT_TRIS, self.split_tris);
            telemetry::counter(
                telemetry::counter::ROOM_SURF_LIGHTING_REJECTS,
                self.lighting_rejects,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_HW_SAFE_TEST_CYCLES,
                self.submit_detail.hw_safe_test_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PACKET_FILL_CYCLES,
                self.submit_detail.packet_fill_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PRIMITIVE_PUSH_CYCLES,
                self.submit_detail.primitive_push_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_DEPTH_CYCLES,
                self.submit_detail.depth_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_COMMAND_CYCLES,
                self.submit_detail.command_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_FALLBACK_CYCLES,
                self.submit_detail.fallback_cycles,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_HW_SAFE_CALLS,
                self.submit_detail.hw_safe_calls,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_FALLBACK_CALLS,
                self.submit_detail.fallback_calls,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_COMMAND_OVERFLOWS,
                self.submit_detail.command_overflows,
            );
            telemetry::counter(
                telemetry::counter::ROOM_SUBMIT_PRIMITIVE_OVERFLOWS,
                self.submit_detail.primitive_overflows,
            );
        }
    }
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
    /// Room-local index into the generated cached-cell slice. Older
    /// callers can leave this as `u16::MAX` and use the coordinate
    /// fallback.
    pub cache_cell_index: u16,
    /// Optional camera-space depth hint. Negative sentinel values
    /// encode whether the renderer still needs to run the camera
    /// cull. This lives in the struct's natural tail padding.
    pub camera_depth: i16,
}

impl GridVisibleCell {
    /// Sentinel used when no direct generated cache-cell index is known.
    pub const CACHE_CELL_INDEX_UNKNOWN: u16 = u16::MAX;
    /// Sentinel used when no precomputed camera depth is known.
    pub const CAMERA_DEPTH_UNKNOWN: i16 = i16::MIN;
    /// Sentinel used when the caller has already camera-culled this
    /// cell, but exact `i32` depth still needs to be computed.
    pub const CAMERA_DEPTH_PRECULLED: i16 = i16::MIN + 1;

    /// Empty placeholder for fixed runtime scratch arrays.
    pub const EMPTY: Self = Self {
        x: 0,
        z: 0,
        min_y: 0,
        max_y: 0,
        cache_cell_index: Self::CACHE_CELL_INDEX_UNKNOWN,
        camera_depth: Self::CAMERA_DEPTH_UNKNOWN,
    };

    /// Build one visible-cell draw record.
    pub const fn new(x: u16, z: u16, min_y: i32, max_y: i32) -> Self {
        Self {
            x,
            z,
            min_y,
            max_y,
            cache_cell_index: Self::CACHE_CELL_INDEX_UNKNOWN,
            camera_depth: Self::CAMERA_DEPTH_UNKNOWN,
        }
    }

    /// Build one visible-cell draw record with a precomputed
    /// room-cache cell index.
    pub const fn with_cache_cell_index(
        x: u16,
        z: u16,
        min_y: i32,
        max_y: i32,
        cache_cell_index: u16,
    ) -> Self {
        Self {
            x,
            z,
            min_y,
            max_y,
            cache_cell_index,
            camera_depth: Self::CAMERA_DEPTH_UNKNOWN,
        }
    }

    /// Return a copy carrying a caller-provided camera-space depth
    /// hint or cull-state sentinel.
    pub const fn with_camera_depth(mut self, camera_depth: i16) -> Self {
        self.camera_depth = camera_depth;
        self
    }
}

/// Predecoded room cell header used by the cached vertex-lit room
/// renderer.
///
/// The cache stores only populated cells, sorted by `(x, z)`, so
/// empty room-grid space does not consume active runtime cache. A
/// cooked visible-cell reference finds its surface range with a small
/// binary search over this compact table.
#[repr(C)]
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
    pub visibility_center: [i32; 3],
    /// Precomputed radius used by the cached room frustum test.
    pub visibility_radius: i32,
    /// First surface record for this cell inside the room surface cache.
    pub surface_first: u16,
    /// Number of cached floor/ceiling/wall surfaces in this cell.
    pub surface_count: u16,
    /// First room-local cached vertex index for this cell.
    pub vertex_first: u16,
    /// Number of unique cached vertices referenced by this cell.
    pub vertex_count: u16,
}

impl CachedRoomCell {
    /// Empty placeholder for fixed runtime cache arrays.
    pub const EMPTY: Self = Self {
        x: 0,
        z: 0,
        min_y: 0,
        max_y: 0,
        visibility_center: [0; 3],
        visibility_radius: 0,
        surface_first: 0,
        surface_count: 0,
        vertex_first: 0,
        vertex_count: 0,
    };

    fn new(
        x: u16,
        z: u16,
        sector_size: i32,
        min_y: i32,
        max_y: i32,
        surface_first: u16,
        surface_count: u16,
        vertex_first: u16,
        vertex_count: u16,
    ) -> Self {
        let (visibility_center, visibility_radius) =
            cell_visibility_bounds(x, z, sector_size, min_y, max_y);
        Self {
            x,
            z,
            min_y,
            max_y,
            visibility_center: visibility_center.to_array(),
            visibility_radius,
            surface_first,
            surface_count,
            vertex_first,
            vertex_count,
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
/// slot, cached vertex indices, UV order, split id, and the surface
/// lighting sample. Per-frame work still applies camera projection,
/// culling, fog, and final ordering-table submission.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CachedRoomSurface {
    /// Local room material slot referenced by this surface.
    pub material_slot: u16,
    /// Indices into the cached room vertex stream. The indexed
    /// renderer uses these to project shared room corners once per
    /// frame instead of once per surface.
    pub vertex_indices: [u16; 4],
    /// Sector X coordinate for the reconstructed lighting sample.
    pub sample_sx: u16,
    /// Sector Z coordinate for the reconstructed lighting sample.
    pub sample_sz: u16,
    /// Surface ordinal for the reconstructed lighting sample.
    pub sample_ordinal: u16,
    /// Packed low 16 bits of each packet UV word: `u | v << 8`.
    pub uv_words: [u16; 4],
    /// Cached baked RGB values. Valid when `kind_flags` carries
    /// [`CACHED_SURFACE_HAS_BAKED_RGB`].
    pub baked_vertex_rgb: [(u8, u8, u8); 4],
    /// Packed surface kind plus cached render flags.
    pub kind_flags: u8,
    /// Runtime wall direction when this is a wall surface.
    pub wall_direction: u8,
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
        vertex_indices: [0; 4],
        sample_sx: 0,
        sample_sz: 0,
        sample_ordinal: 0,
        uv_words: [0; 4],
        baked_vertex_rgb: [(0, 0, 0); 4],
        kind_flags: CACHED_SURFACE_KIND_FLOOR,
        wall_direction: 0,
        split: SPLIT_NW_SE,
        triangle_index: WHOLE_QUAD_TRIANGLE_INDEX,
    };

    const fn new(
        material_slot: u16,
        vertex_indices: [u16; 4],
        uvs: [(u8, u8); 4],
        sample: WorldSurfaceSample,
        split: u8,
        triangle_index: u8,
    ) -> Self {
        let (kind, wall_direction) = cached_surface_kind_code(sample.kind);
        let mut kind_flags = kind;
        let mut baked_vertex_rgb = [(0, 0, 0); 4];
        if let Some(rgb) = sample.baked_vertex_rgb {
            baked_vertex_rgb = rgb;
            kind_flags |= CACHED_SURFACE_HAS_BAKED_RGB;
        }
        Self {
            material_slot,
            vertex_indices,
            sample_sx: sample.sx,
            sample_sz: sample.sz,
            sample_ordinal: sample.ordinal,
            uv_words: cached_surface_uv_words(uvs),
            baked_vertex_rgb,
            kind_flags,
            wall_direction,
            split,
            triangle_index,
        }
    }

    fn sample_with_center(
        self,
        vertices: [WorldVertex; 4],
        include_center: bool,
    ) -> WorldSurfaceSample {
        WorldSurfaceSample {
            kind: cached_surface_kind(self.kind_flags, self.wall_direction),
            sx: self.sample_sx,
            sz: self.sample_sz,
            center: if include_center {
                cached_surface_center(vertices, self.split, self.triangle_index)
            } else {
                RoomPoint::ZERO
            },
            baked_vertex_rgb: if self.kind_flags & CACHED_SURFACE_HAS_BAKED_RGB != 0 {
                Some(self.baked_vertex_rgb)
            } else {
                None
            },
            ordinal: self.sample_ordinal,
        }
    }

    fn sample_without_center(self) -> WorldSurfaceSample {
        WorldSurfaceSample {
            kind: cached_surface_kind(self.kind_flags, self.wall_direction),
            sx: self.sample_sx,
            sz: self.sample_sz,
            center: RoomPoint::ZERO,
            baked_vertex_rgb: if self.kind_flags & CACHED_SURFACE_HAS_BAKED_RGB != 0 {
                Some(self.baked_vertex_rgb)
            } else {
                None
            },
            ordinal: self.sample_ordinal,
        }
    }

    const fn has_baked_rgb(self) -> bool {
        self.kind_flags & CACHED_SURFACE_HAS_BAKED_RGB != 0
    }

    #[inline(always)]
    fn with_horizontal_non_flat(mut self, non_flat: bool) -> Self {
        if non_flat {
            self.kind_flags |= CACHED_SURFACE_HORIZONTAL_NON_FLAT;
        }
        self
    }

    #[cfg(test)]
    const fn uvs(self) -> [(u8, u8); 4] {
        [
            cached_surface_uv_pair(self.uv_words[0]),
            cached_surface_uv_pair(self.uv_words[1]),
            cached_surface_uv_pair(self.uv_words[2]),
            cached_surface_uv_pair(self.uv_words[3]),
        ]
    }
}

const fn cached_surface_uv_words(uvs: [(u8, u8); 4]) -> [u16; 4] {
    [
        cached_surface_uv_word(uvs[0]),
        cached_surface_uv_word(uvs[1]),
        cached_surface_uv_word(uvs[2]),
        cached_surface_uv_word(uvs[3]),
    ]
}

const fn cached_surface_uv_word(uv: (u8, u8)) -> u16 {
    (uv.0 as u16) | ((uv.1 as u16) << 8)
}

#[cfg(test)]
const fn cached_surface_uv_pair(word: u16) -> (u8, u8) {
    (word as u8, (word >> 8) as u8)
}

const CACHED_SURFACE_KIND_MASK: u8 = 0b0000_0011;
const CACHED_SURFACE_KIND_FLOOR: u8 = 0;
const CACHED_SURFACE_KIND_CEILING: u8 = 1;
const CACHED_SURFACE_KIND_WALL: u8 = 2;
const CACHED_SURFACE_HORIZONTAL_NON_FLAT: u8 = 0b0100_0000;
const CACHED_SURFACE_HAS_BAKED_RGB: u8 = 0b1000_0000;

const _: () = assert!(
    core::mem::size_of::<LevelCachedRoomCellRecord>() == core::mem::size_of::<CachedRoomCell>()
);
const _: () = assert!(
    core::mem::align_of::<LevelCachedRoomCellRecord>() == core::mem::align_of::<CachedRoomCell>()
);
const _: () = assert!(
    core::mem::size_of::<LevelCachedRoomVertexRecord>() == core::mem::size_of::<WorldVertex>()
);
const _: () = assert!(
    core::mem::align_of::<LevelCachedRoomVertexRecord>() == core::mem::align_of::<WorldVertex>()
);
const _: () = assert!(
    core::mem::size_of::<LevelCachedRoomSurfaceRecord>()
        == core::mem::size_of::<CachedRoomSurface>()
);
const _: () = assert!(
    core::mem::align_of::<LevelCachedRoomSurfaceRecord>()
        == core::mem::align_of::<CachedRoomSurface>()
);

/// View generated level cache cell records as renderer cache cells.
///
/// `psx-level` owns the manifest schema while `psx-engine` owns the
/// renderer types. The two record layouts are asserted above so cooked
/// manifests can be drawn without copying room-cache payloads into a
/// mutable runtime arena.
pub fn cached_room_cells_from_level_records(
    records: &[LevelCachedRoomCellRecord],
) -> &[CachedRoomCell] {
    // SAFETY: The record and renderer structs are `repr(C)`, contain
    // the same field types in the same order, and the const assertions
    // above pin size/alignment equality.
    unsafe { core::slice::from_raw_parts(records.as_ptr().cast::<CachedRoomCell>(), records.len()) }
}

/// View generated level cache vertex records as renderer vertices.
pub fn cached_room_vertices_from_level_records(
    records: &[LevelCachedRoomVertexRecord],
) -> &[WorldVertex] {
    // SAFETY: See `cached_room_cells_from_level_records`.
    unsafe { core::slice::from_raw_parts(records.as_ptr().cast::<WorldVertex>(), records.len()) }
}

/// View generated level cache surface records as renderer surfaces.
pub fn cached_room_surfaces_from_level_records(
    records: &[LevelCachedRoomSurfaceRecord],
) -> &[CachedRoomSurface] {
    // SAFETY: See `cached_room_cells_from_level_records`.
    unsafe {
        core::slice::from_raw_parts(records.as_ptr().cast::<CachedRoomSurface>(), records.len())
    }
}

const fn cached_surface_kind_code(kind: WorldSurfaceKind) -> (u8, u8) {
    match kind {
        WorldSurfaceKind::Floor => (CACHED_SURFACE_KIND_FLOOR, 0),
        WorldSurfaceKind::Ceiling => (CACHED_SURFACE_KIND_CEILING, 0),
        WorldSurfaceKind::Wall { direction } => (CACHED_SURFACE_KIND_WALL, direction),
    }
}

const fn cached_surface_kind(kind_flags: u8, wall_direction: u8) -> WorldSurfaceKind {
    match kind_flags & CACHED_SURFACE_KIND_MASK {
        CACHED_SURFACE_KIND_CEILING => WorldSurfaceKind::Ceiling,
        CACHED_SURFACE_KIND_WALL => WorldSurfaceKind::Wall {
            direction: wall_direction,
        },
        _ => WorldSurfaceKind::Floor,
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

    /// Shade all four vertices when the caller already has camera-space
    /// depths for fog. The default preserves the older vertex-only path.
    fn shade_vertices_with_depths(
        &self,
        sample: WorldSurfaceSample,
        vertices: [WorldVertex; 4],
        _depths: [i32; 4],
        material: WorldRenderMaterial,
    ) -> [(u8, u8, u8); 4] {
        self.shade_vertices(sample, vertices, material)
    }

    /// Fast path for cached surfaces that already carry baked vertex RGB.
    ///
    /// Returning `Some` lets indexed cached renderers skip reconstructing
    /// the source world quad when the lighting implementation can shade
    /// directly from baked RGB plus optional prepared depth values.
    fn shade_cached_baked_vertices(
        &self,
        _sample: WorldSurfaceSample,
        _depths: Option<[i32; 4]>,
        _material: WorldRenderMaterial,
    ) -> Option<[(u8, u8, u8); 4]> {
        None
    }

    /// Whether cached surfaces with baked RGB can be submitted with
    /// those colors directly. Static no-fog room lighting can return
    /// `true` because the cooker has already applied material tint and
    /// authored lights.
    fn uses_direct_baked_vertex_rgb(&self) -> bool {
        false
    }

    /// Convert a projected camera-space depth into the value cached
    /// for [`Self::shade_vertices_with_depths`]. The default keeps
    /// raw depth; fog implementations can precompute a blend factor.
    fn prepare_vertex_depth(&self, depth: i32) -> i32 {
        depth
    }

    /// Whether this lighting pass needs the cached camera-space
    /// depth values supplied to [`Self::shade_vertices_with_depths`].
    fn uses_vertex_depths(&self) -> bool {
        true
    }

    /// Whether cached renderers must reconstruct the exact surface
    /// center before calling lighting hooks. Implementations that
    /// shade only from baked RGB or emitted vertices can return
    /// `false` and skip that arithmetic in the room hot path.
    fn needs_surface_sample_center(&self, _sample_has_baked_rgb: bool) -> bool {
        true
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

const fn horizontal_depth_policy() -> DepthPolicy {
    DepthPolicy::Farthest
}

const HORIZONTAL_DEPTH_BIAS: i32 = 512;
const HYBRID_HORIZONTAL_DEPTH_SPAN: i32 = 768;

const fn horizontal_depth_options(options: WorldSurfaceOptions) -> WorldSurfaceOptions {
    let options = match options.depth_policy {
        DepthPolicy::Fixed(_) => options,
        _ => options.with_depth_policy(horizontal_depth_policy()),
    };
    options.with_depth_bias(options.depth_bias.saturating_add(HORIZONTAL_DEPTH_BIAS))
}

fn tile_depth_options(
    options: WorldSurfaceOptions,
    camera: &WorldCamera,
    cell: GridVisibleCell,
    sector_size: i32,
) -> WorldSurfaceOptions {
    options.with_depth_policy(DepthPolicy::Fixed(tile_camera_depth(
        camera,
        cell,
        sector_size,
    )))
}

#[inline(always)]
fn tile_depth_options_from_depth(options: WorldSurfaceOptions, depth: i32) -> WorldSurfaceOptions {
    options.with_depth_policy(DepthPolicy::Fixed(depth))
}

#[inline(always)]
fn triangle_depth_options(options: WorldSurfaceOptions) -> WorldSurfaceOptions {
    options.with_depth_policy(DepthPolicy::Average)
}

fn tile_camera_depth(camera: &WorldCamera, cell: GridVisibleCell, sector_size: i32) -> i32 {
    let sector_size = sector_size.max(1);
    let half = sector_size / 2;
    let center = WorldVertex::new(
        (cell.x as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
        cell.min_y.saturating_add(cell.max_y) / 2,
        (cell.z as i32)
            .saturating_mul(sector_size)
            .saturating_add(half),
    );
    camera.view_vertex(center).z
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct CachedRoomSubmitDepths {
    vertical: Option<PreparedTriangleDepth>,
    horizontal: Option<PreparedTriangleDepth>,
}

impl CachedRoomSubmitDepths {
    #[inline(always)]
    fn from_cell_options<const OT: usize>(options: WorldSurfaceOptions) -> Self {
        Self {
            vertical: PreparedTriangleDepth::from_fixed_options::<OT>(options),
            horizontal: PreparedTriangleDepth::from_fixed_options::<OT>(horizontal_depth_options(
                options,
            )),
        }
    }
}

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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
                            let cell_options = tile_depth_options(
                                options,
                                camera,
                                GridVisibleCell::new(sx, sz, min_y, max_y),
                                sector_size,
                            );
                            stats.surfaces_considered =
                                stats.surfaces_considered.saturating_add(draw_sector_lit(
                                    room,
                                    sx,
                                    sz,
                                    sector,
                                    materials,
                                    lighting,
                                    camera,
                                    cell_options,
                                    triangles,
                                    world,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
                            let cell_options = tile_depth_options(
                                options,
                                camera,
                                GridVisibleCell::new(sx, sz, min_y, max_y),
                                sector_size,
                            );
                            stats.surfaces_considered =
                                stats
                                    .surfaces_considered
                                    .saturating_add(draw_sector_vertex_lit(
                                        room,
                                        sx,
                                        sz,
                                        sector,
                                        materials,
                                        lighting,
                                        camera,
                                        cell_options,
                                        triangles,
                                        world,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
        let cell_options = tile_depth_options(options, camera, *cell, sector_size);
        stats.surfaces_considered =
            stats
                .surfaces_considered
                .saturating_add(draw_sector_vertex_lit(
                    room,
                    cell.x,
                    cell.z,
                    sector,
                    materials,
                    lighting,
                    camera,
                    cell_options,
                    triangles,
                    world,
                ));
    }
    stats
}

/// Predecode all renderable floor, ceiling, and wall surfaces in a
/// room into caller-owned fixed arrays.
///
/// Cell headers are written in `(x, z)` order only for populated cells.
/// Surface records are only written for populated sectors that
/// reference a material slot and have valid geometry.
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

    let sector_size = room.sector_size();
    let mut cell_count = 0usize;
    let mut vertex_count = 0usize;
    let mut surface_count = 0usize;
    let mut sx = 0u16;
    while sx < width {
        let mut sz = 0u16;
        while sz < depth {
            let surface_first = surface_count;

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
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        )
                        .with_horizontal_non_flat(horizontal_heights_non_flat4(heights)),
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
                        let triangle_heights = sector.floor_triangle_heights(triangle_index);
                        let vertices = horizontal_triangle_vertices(
                            sx,
                            sz,
                            sector_size,
                            split,
                            triangle_index,
                            triangle_heights,
                            heights,
                        );
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
                                triangle_heights_to_quad(
                                    heights,
                                    split,
                                    triangle_index,
                                    triangle_heights,
                                ),
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
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.floor_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            )
                            .with_horizontal_non_flat(
                                horizontal_heights_non_flat3(triangle_heights),
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
                            vertex_indices,
                            cached_material_uvs(materials, slot, uvs),
                            sample,
                            split,
                            WHOLE_QUAD_TRIANGLE_INDEX,
                        )
                        .with_horizontal_non_flat(horizontal_heights_non_flat4(heights)),
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
                        let triangle_heights = sector.ceiling_triangle_heights(triangle_index);
                        let vertices = horizontal_triangle_vertices(
                            sx,
                            sz,
                            sector_size,
                            split,
                            triangle_index,
                            triangle_heights,
                            heights,
                        );
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
                                triangle_heights_to_quad(
                                    heights,
                                    split,
                                    triangle_index,
                                    triangle_heights,
                                ),
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
                                vertex_indices,
                                cached_material_uvs(
                                    materials,
                                    slot,
                                    sector.ceiling_triangle_uvs(triangle_index),
                                ),
                                sample,
                                split,
                                triangle_index as u8,
                            )
                            .with_horizontal_non_flat(
                                horizontal_heights_non_flat3(triangle_heights),
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
            if surface_len > u16::MAX as usize
                || surface_first > u16::MAX as usize
                || cell_count > u16::MAX as usize
            {
                return CachedRoomSurfaceCacheStats {
                    cell_count,
                    surface_count,
                    vertex_count,
                    overflow: true,
                };
            }
            if surface_len > 0 {
                if cell_count >= cells_out.len() {
                    return CachedRoomSurfaceCacheStats {
                        cell_count,
                        surface_count,
                        vertex_count,
                        overflow: true,
                    };
                }
                let (min_y, max_y) = sector_y_bounds(room, sector);
                cells_out[cell_count] = CachedRoomCell::new(
                    sx,
                    sz,
                    sector_size,
                    min_y,
                    max_y,
                    surface_first as u16,
                    surface_len as u16,
                    0,
                    0,
                );
                cell_count += 1;
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

/// Draw a cached vertex-lit room using a deduplicated cached vertex
/// stream. The projected scratch slices must be at least as long as
/// `cached_vertices`.
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed_cached_room_vertex_lit_visible_cells<
    const OT: usize,
    L: WorldSurfaceLighting,
>(
    cached_cells: &[CachedRoomCell],
    cached_cell_vertices: &[u16],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_indices: &mut [u16],
    projected_vertices: &mut [crate::render3d::ProjectedVertex],
    projected_ready: &mut [bool],
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    _room_depth: u16,
    _sector_size: i32,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    visible_cells: &[GridVisibleCell],
    screen_margin: i32,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_ready.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < visible_cells.len()
        || accepted_cell_depths.len() < visible_cells.len()
    {
        return stats;
    }
    if visible_cells.is_empty() {
        return stats;
    }

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;
    let mut accepted_depths_need_sort = false;
    // Per-cell depth/cull transforms run on the GTE (MVMVA) via the loaded
    // camera instead of redoing the camera rotation in CPU fixed-point for
    // every candidate cell; matches the rounding of the GTE vertex projection.
    let loaded_camera = LoadedWorldCameraGte::load(*camera);

    for visible in visible_cells.iter().copied() {
        let Some(cell_index) = cached_room_cell_index_for_visible(cached_cells, visible) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index).copied() else {
            continue;
        };

        stats.cells_considered = stats.cells_considered.saturating_add(1);
        let cell_depth = if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_PRECULLED {
            let visibility_center = WorldVertex::new(
                cell.visibility_center[0],
                cell.visibility_center[1],
                cell.visibility_center[2],
            );
            loaded_camera.view_vertex(visibility_center).z
        } else if visible.camera_depth == GridVisibleCell::CAMERA_DEPTH_UNKNOWN {
            let visibility_center = WorldVertex::new(
                cell.visibility_center[0],
                cell.visibility_center[1],
                cell.visibility_center[2],
            );
            let visibility_view = loaded_camera.view_vertex(visibility_center);
            if !cell_visibility_view_visible_to_camera(
                camera,
                options,
                visibility_view,
                cell.visibility_radius,
                screen_margin,
            ) {
                stats.cells_frustum_culled = stats.cells_frustum_culled.saturating_add(1);
                continue;
            }
            accepted_depths_need_sort = true;
            visibility_view.z
        } else {
            visible.camera_depth as i32
        };

        stats.cells_drawn = stats.cells_drawn.saturating_add(1);
        accepted_cell_indices[accepted_cell_count] = cell_index as u16;
        accepted_cell_depths[accepted_cell_count] = cell_depth;
        accepted_cell_count += 1;
        projected_index_count = collect_cached_cell_vertex_indices(
            cell,
            cached_cell_vertices,
            cached_surfaces,
            projected_ready,
            projected_indices,
            projected_index_count,
        );
    }
    if accepted_depths_need_sort {
        sort_cached_room_cell_indices_by_depth(
            &mut accepted_cell_indices[..accepted_cell_count],
            &mut accepted_cell_depths[..accepted_cell_count],
        );
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_CELL_SELECT);

    let projected_indices = &projected_indices[..projected_index_count];
    stats.projected_vertices = projected_index_count.min(u16::MAX as usize) as u16;
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_PROJECT);
    project_world_vertex_indices_gte(
        *camera,
        cached_vertices,
        projected_indices,
        projected_vertices,
    );
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_PROJECT);
    if use_vertex_depths {
        crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_DEPTH_PREP);
        for raw_index in projected_indices {
            let index = *raw_index as usize;
            projected_depths[index] = lighting.prepare_vertex_depth(projected_vertices[index].sz);
        }
        crate::telemetry::stage_end(crate::telemetry::stage::ROOM_DEPTH_PREP);
    }

    let mut surface_profile = RoomSurfaceMicroProfile::new();
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    for accepted_index in 0..accepted_cell_count {
        let Some(&cell_index) = accepted_cell_indices.get(accepted_index) else {
            continue;
        };
        let Some(&cell_depth) = accepted_cell_depths.get(accepted_index) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
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
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        screen_bounds,
                        materials,
                        lighting,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    for raw_index in projected_indices {
        if let Some(ready) = projected_ready.get_mut(*raw_index as usize) {
            *ready = false;
        }
    }
    stats
}

/// Draw every populated cell from a cached vertex-lit room.
///
/// This bypasses cooked visible-cell/PVS filtering after the caller has
/// already selected an active chunk. Cells are still depth-sorted for the
/// ordering-table painter path, and surfaces still run the usual projection,
/// screen, near-plane, and backface checks.
#[allow(clippy::too_many_arguments)]
pub fn draw_indexed_cached_room_vertex_lit_all_cells<const OT: usize, L: WorldSurfaceLighting>(
    cached_cells: &[CachedRoomCell],
    cached_cell_vertices: &[u16],
    cached_vertices: &[WorldVertex],
    cached_surfaces: &[CachedRoomSurface],
    projected_indices: &mut [u16],
    projected_vertices: &mut [ProjectedVertex],
    projected_ready: &mut [bool],
    projected_depths: &mut [i32],
    accepted_cell_indices: &mut [u16],
    accepted_cell_depths: &mut [i32],
    materials: &[WorldRenderMaterial],
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    screen_margin: i32,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) -> GridVisibilityStats {
    let mut stats = GridVisibilityStats::default();
    if projected_indices.len() < cached_vertices.len()
        || projected_vertices.len() < cached_vertices.len()
        || projected_ready.len() < cached_vertices.len()
        || projected_depths.len() < cached_vertices.len()
        || accepted_cell_indices.len() < cached_cells.len()
        || accepted_cell_depths.len() < cached_cells.len()
    {
        return stats;
    }
    if cached_cells.is_empty() || cached_surfaces.is_empty() {
        return stats;
    }

    let use_vertex_depths = lighting.uses_vertex_depths();
    let use_direct_baked_rgb = lighting.uses_direct_baked_vertex_rgb();
    let screen_bounds = projected_screen_bounds(camera, screen_margin);
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_CELL_SELECT);
    let mut projected_index_count = 0usize;
    let mut accepted_cell_count = 0usize;

    for (cell_index, cell) in cached_cells.iter().copied().enumerate() {
        if cell.surface_count == 0 || cell_index > u16::MAX as usize {
            continue;
        }
        stats.cells_considered = stats.cells_considered.saturating_add(1);
        let visibility_center = WorldVertex::new(
            cell.visibility_center[0],
            cell.visibility_center[1],
            cell.visibility_center[2],
        );
        let cell_depth = camera.view_vertex(visibility_center).z;
        stats.cells_drawn = stats.cells_drawn.saturating_add(1);
        accepted_cell_indices[accepted_cell_count] = cell_index as u16;
        accepted_cell_depths[accepted_cell_count] = cell_depth;
        accepted_cell_count += 1;
    }
    sort_cached_room_cell_indices_by_depth(
        &mut accepted_cell_indices[..accepted_cell_count],
        &mut accepted_cell_depths[..accepted_cell_count],
    );
    for &cell_index in &accepted_cell_indices[..accepted_cell_count] {
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        projected_index_count = collect_cached_cell_vertex_indices(
            cell,
            cached_cell_vertices,
            cached_surfaces,
            projected_ready,
            projected_indices,
            projected_index_count,
        );
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_CELL_SELECT);

    let projected_indices = &projected_indices[..projected_index_count];
    stats.projected_vertices = projected_index_count.min(u16::MAX as usize) as u16;
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_PROJECT);
    project_world_vertex_indices_gte(
        *camera,
        cached_vertices,
        projected_indices,
        projected_vertices,
    );
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_PROJECT);
    if use_vertex_depths {
        crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_DEPTH_PREP);
        for raw_index in projected_indices {
            let index = *raw_index as usize;
            projected_depths[index] = lighting.prepare_vertex_depth(projected_vertices[index].sz);
        }
        crate::telemetry::stage_end(crate::telemetry::stage::ROOM_DEPTH_PREP);
    }

    let mut surface_profile = RoomSurfaceMicroProfile::new();
    crate::telemetry::stage_begin(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    for accepted_index in 0..accepted_cell_count {
        let Some(&cell_index) = accepted_cell_indices.get(accepted_index) else {
            continue;
        };
        let Some(&cell_depth) = accepted_cell_depths.get(accepted_index) else {
            continue;
        };
        let Some(cell) = cached_cells.get(cell_index as usize).copied() else {
            continue;
        };
        let cell_options = tile_depth_options_from_depth(options, cell_depth);
        let submit_depths = CachedRoomSubmitDepths::from_cell_options::<OT>(cell_options);
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
                        cached_vertices,
                        projected_vertices,
                        projected_depths,
                        use_vertex_depths,
                        use_direct_baked_rgb,
                        screen_bounds,
                        materials,
                        lighting,
                        cell_options,
                        submit_depths,
                        depth_mode,
                        subdivision_mode,
                        triangles,
                        world,
                        &mut surface_profile,
                    ));
            i += 1;
        }
    }
    crate::telemetry::stage_end(crate::telemetry::stage::ROOM_SURFACE_DRAW);
    surface_profile.emit();
    for raw_index in projected_indices {
        if let Some(ready) = projected_ready.get_mut(*raw_index as usize) {
            *ready = false;
        }
    }
    stats
}

fn sort_cached_room_cell_indices_by_depth(indices: &mut [u16], depths: &mut [i32]) {
    if indices.len() > depths.len() {
        return;
    }
    let mut gap = indices.len() / 2;
    while gap > 0 {
        let mut i = gap;
        while i < indices.len() {
            let index = indices[i];
            let depth = depths[i];
            let mut j = i;
            while j >= gap && depths[j - gap] < depth {
                indices[j] = indices[j - gap];
                depths[j] = depths[j - gap];
                j -= gap;
            }
            indices[j] = index;
            depths[j] = depth;
            i += 1;
        }
        gap /= 2;
    }
}

fn cached_room_cell_index(cells: &[CachedRoomCell], x: u16, z: u16) -> Option<usize> {
    let key = cached_room_cell_key(x, z);
    let mut low = 0usize;
    let mut high = cells.len();
    while low < high {
        let mid = (low + high) / 2;
        let cell = cells[mid];
        let cell_key = cached_room_cell_key(cell.x, cell.z);
        if cell_key < key {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    let cell = cells.get(low).copied()?;
    (cached_room_cell_key(cell.x, cell.z) == key && cell.surface_count != 0).then_some(low)
}

#[inline(always)]
fn cached_room_cell_index_for_visible(
    cells: &[CachedRoomCell],
    visible: GridVisibleCell,
) -> Option<usize> {
    if visible.cache_cell_index != GridVisibleCell::CACHE_CELL_INDEX_UNKNOWN {
        let index = visible.cache_cell_index as usize;
        let cell = *cells.get(index)?;
        if cell.x == visible.x && cell.z == visible.z && cell.surface_count != 0 {
            return Some(index);
        }
    }
    cached_room_cell_index(cells, visible.x, visible.z)
}

fn collect_cached_cell_vertex_indices(
    cell: CachedRoomCell,
    cached_cell_vertices: &[u16],
    cached_surfaces: &[CachedRoomSurface],
    projected_ready: &mut [bool],
    projected_indices: &mut [u16],
    mut projected_index_count: usize,
) -> usize {
    if cell.vertex_count == 0 {
        let first = cell.surface_first as usize;
        let end = first
            .saturating_add(cell.surface_count as usize)
            .min(cached_surfaces.len());
        let mut surface_index = first;
        while surface_index < end {
            for raw_index in cached_surfaces[surface_index].vertex_indices {
                projected_index_count = push_unique_projected_index(
                    raw_index,
                    projected_ready,
                    projected_indices,
                    projected_index_count,
                );
            }
            surface_index += 1;
        }
        return projected_index_count;
    }
    let first = cell.vertex_first as usize;
    let end = first
        .saturating_add(cell.vertex_count as usize)
        .min(cached_cell_vertices.len());
    let mut i = first;
    while i < end {
        projected_index_count = push_unique_projected_index(
            cached_cell_vertices[i],
            projected_ready,
            projected_indices,
            projected_index_count,
        );
        i += 1;
    }
    projected_index_count
}

fn push_unique_projected_index(
    raw_index: u16,
    projected_ready: &mut [bool],
    projected_indices: &mut [u16],
    projected_index_count: usize,
) -> usize {
    let vertex_index = raw_index as usize;
    if vertex_index < projected_ready.len()
        && !projected_ready[vertex_index]
        && projected_index_count < projected_indices.len()
    {
        projected_ready[vertex_index] = true;
        projected_indices[projected_index_count] = raw_index;
        projected_index_count + 1
    } else {
        projected_index_count
    }
}

const fn cached_room_cell_key(x: u16, z: u16) -> u32 {
    ((x as u32) << 16) | z as u32
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
    let mut i = *vertex_count;
    while i > 0 {
        i -= 1;
        if vertices_out[i] == vertex {
            return u16::try_from(i).ok();
        }
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
        [
            sector.floor_triangle_heights(0),
            sector.floor_triangle_heights(1),
        ],
        sector.floor_heights(),
        sector.floor_split(),
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
        [
            sector.ceiling_triangle_heights(0),
            sector.ceiling_triangle_heights(1),
        ],
        sector.ceiling_heights(),
        sector.ceiling_split(),
    )
}

fn merge_horizontal_triangle_surface(
    materials: [Option<u16>; 2],
    uvs: [[(u8, u8); 4]; 2],
    heights: [[i32; 3]; 2],
    face_heights: [i32; 4],
    split: u8,
) -> Option<(u16, [(u8, u8); 4])> {
    let slot = materials[0]?;
    if materials[1]? != slot
        || uvs[0] != uvs[1]
        || heights[0] != triangle_heights_from_quad(face_heights, split, 0)
        || heights[1] != triangle_heights_from_quad(face_heights, split, 1)
    {
        return None;
    }
    Some((slot, uvs[0]))
}

fn triangle_heights_from_quad(heights: [i32; 4], split: u8, triangle_index: usize) -> [i32; 3] {
    let (a, b, c) = split_triangles_runtime(split)[triangle_index.min(1)];
    [heights[a], heights[b], heights[c]]
}

fn triangle_heights_to_quad(
    mut fallback: [i32; 4],
    split: u8,
    triangle_index: usize,
    heights: [i32; 3],
) -> [i32; 4] {
    let (a, b, c) = split_triangles_runtime(split)[triangle_index.min(1)];
    fallback[a] = heights[0];
    fallback[b] = heights[1];
    fallback[c] = heights[2];
    fallback
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn draw_indexed_cached_room_surface<const OT: usize, L: WorldSurfaceLighting>(
    surface: CachedRoomSurface,
    cached_vertices: &[WorldVertex],
    projected_vertices: &[ProjectedVertex],
    projected_depths: &[i32],
    use_vertex_depths: bool,
    use_direct_baked_rgb: bool,
    screen_bounds: ProjectedScreenBounds,
    materials: &[WorldRenderMaterial],
    lighting: &L,
    options: WorldSurfaceOptions,
    submit_depths: CachedRoomSubmitDepths,
    depth_mode: CachedRoomDepthMode,
    subdivision_mode: CachedRoomSubdivisionMode,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
    profile: &mut RoomSurfaceMicroProfile,
) -> u16 {
    profile.count_profiled();
    profile.count_shape(surface.triangle_index);
    let projected_start = RoomSurfaceMicroProfile::cycle();
    let ids = surface.vertex_indices;
    let Some(projected) = indexed_projected_quad(projected_vertices, ids) else {
        profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
        profile.count_projected_reject();
        return 0;
    };
    profile.add_projected(RoomSurfaceMicroProfile::elapsed(projected_start));
    let screen_start = RoomSurfaceMicroProfile::cycle();
    if projected_quad_outside_screen(projected, screen_bounds) {
        profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
        profile.count_screen_culled();
        return 1;
    }
    profile.add_screen(RoomSurfaceMicroProfile::elapsed(screen_start));
    let kind_start = RoomSurfaceMicroProfile::cycle();
    let kind = cached_surface_kind(surface.kind_flags, surface.wall_direction);
    profile.add_kind(RoomSurfaceMicroProfile::elapsed(kind_start));
    profile.count_kind(kind);
    let material_start = RoomSurfaceMicroProfile::cycle();
    let Some(&material) = materials.get(surface.material_slot as usize) else {
        profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
        profile.count_material_miss();
        return 0;
    };
    let material = cached_uv_material(material);
    profile.add_material(RoomSurfaceMicroProfile::elapsed(material_start));
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            let is_ceiling = matches!(kind, WorldSurfaceKind::Ceiling);
            let use_triangle_depth =
                cached_surface_uses_triangle_depth(depth_mode, kind, surface, projected);
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (horizontal_depth_options(options), submit_depths.horizontal)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                use_triangle_depth,
                kind,
                surface,
                projected,
            );
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_split_triangle_backface_culled(
                    projected,
                    material,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    is_ceiling,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    projected_depths,
                    ids,
                    use_vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    surface.uv_words,
                    colors,
                    material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    is_ceiling,
                    triangles,
                    world,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            } else {
                let projected_for_cull = if is_ceiling {
                    reverse_quad_winding(projected)
                } else {
                    projected
                };
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_quad_backface_culled(
                    projected_for_cull,
                    material,
                    CullMode::Back,
                    split_triangles_runtime(surface.split),
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    projected_depths,
                    ids,
                    use_vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let (projected, uv_words, colors) = if is_ceiling {
                    (
                        reverse_quad_winding(projected),
                        reverse_quad_winding(surface.uv_words),
                        reverse_quad_winding(colors),
                    )
                } else {
                    (projected, surface.uv_words, colors)
                };
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_sided_projected_gouraud_quad_cached_uv_words(
                    world,
                    triangles,
                    projected,
                    uv_words,
                    colors,
                    material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    split_triangles_runtime(surface.split),
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
        WorldSurfaceKind::Wall { direction } => {
            let wall_material = wall_material_for_direction(material, direction);
            let use_triangle_depth =
                cached_surface_uses_triangle_depth(depth_mode, kind, surface, projected);
            let (surface_options, prepared_depth) = if use_triangle_depth {
                (triangle_depth_options(options), None)
            } else {
                (options, submit_depths.vertical)
            };
            let surface_options = cached_surface_subdivision_options(
                surface_options,
                subdivision_mode,
                use_triangle_depth,
                kind,
                surface,
                projected,
            );
            if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_split_triangle_backface_culled(
                    projected,
                    wall_material,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    projected_depths,
                    ids,
                    use_vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_projected_split_triangle_vertex_lit_cached_uv_words(
                    projected,
                    surface.uv_words,
                    colors,
                    wall_material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    surface.split,
                    surface.triangle_index as usize,
                    false,
                    triangles,
                    world,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            } else {
                let backface_start = RoomSurfaceMicroProfile::cycle();
                let backface_culled = projected_quad_backface_culled(
                    projected,
                    wall_material,
                    CullMode::Back,
                    SPLIT_NW_SE_TRIANGLES,
                );
                profile.add_backface(RoomSurfaceMicroProfile::elapsed(backface_start));
                if backface_culled {
                    profile.count_backface_culled();
                    return 1;
                }
                let lighting_start = RoomSurfaceMicroProfile::cycle();
                let Some(colors) = indexed_vertex_lighting_colors(
                    lighting,
                    surface,
                    material,
                    cached_vertices,
                    projected_depths,
                    ids,
                    use_vertex_depths,
                    use_direct_baked_rgb,
                ) else {
                    profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                    profile.count_lighting_reject();
                    return 0;
                };
                profile.add_lighting(RoomSurfaceMicroProfile::elapsed(lighting_start));
                let submit_start = RoomSurfaceMicroProfile::cycle();
                submit_sided_projected_gouraud_quad_cached_uv_words(
                    world,
                    triangles,
                    projected,
                    surface.uv_words,
                    colors,
                    wall_material,
                    surface_options,
                    prepared_depth,
                    CullMode::Back,
                    SPLIT_NW_SE_TRIANGLES,
                    profile,
                );
                profile.add_submit(RoomSurfaceMicroProfile::elapsed(submit_start));
            }
        }
    }
    1
}

#[inline(always)]
fn indexed_world_quad(vertices: &[WorldVertex], ids: [u16; 4]) -> Option<[WorldVertex; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= vertices.len() {
        return None;
    }
    // SAFETY: `max_index < vertices.len()` proves every id is in range.
    unsafe {
        Some([
            *vertices.get_unchecked(a),
            *vertices.get_unchecked(b),
            *vertices.get_unchecked(c),
            *vertices.get_unchecked(d),
        ])
    }
}

fn cached_surface_uses_triangle_depth(
    mode: CachedRoomDepthMode,
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    match mode {
        CachedRoomDepthMode::FixedCell => false,
        CachedRoomDepthMode::PerTriangle => true,
        CachedRoomDepthMode::Hybrid => match kind {
            WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
                cached_horizontal_surface_is_risky(surface, projected)
            }
            WorldSurfaceKind::Wall { .. } => false,
        },
        CachedRoomDepthMode::HybridWalls => cached_surface_is_risky(kind, surface, projected),
    }
}

fn cached_surface_subdivision_options(
    options: WorldSurfaceOptions,
    mode: CachedRoomSubdivisionMode,
    use_triangle_depth: bool,
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> WorldSurfaceOptions {
    let allow_visual_subdivision = match mode {
        CachedRoomSubdivisionMode::All => true,
        CachedRoomSubdivisionMode::DepthSorted => use_triangle_depth,
        CachedRoomSubdivisionMode::Risky => cached_surface_is_risky(kind, surface, projected),
    };
    if allow_visual_subdivision {
        options
    } else {
        options.with_textured_triangle_max_edge(0)
    }
}

fn cached_surface_is_risky(
    kind: WorldSurfaceKind,
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    match kind {
        WorldSurfaceKind::Floor | WorldSurfaceKind::Ceiling => {
            cached_horizontal_surface_is_risky(surface, projected)
        }
        WorldSurfaceKind::Wall { .. } => {
            cached_surface_projected_depth_span(surface, projected) >= HYBRID_HORIZONTAL_DEPTH_SPAN
        }
    }
}

fn cached_horizontal_surface_is_risky(
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> bool {
    if surface.kind_flags & CACHED_SURFACE_HORIZONTAL_NON_FLAT != 0 {
        return true;
    }
    cached_surface_projected_depth_span(surface, projected) >= HYBRID_HORIZONTAL_DEPTH_SPAN
}

fn cached_surface_projected_depth_span(
    surface: CachedRoomSurface,
    projected: [ProjectedVertex; 4],
) -> i32 {
    if surface.triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        let (a, b, c) = split_triangles_runtime(surface.split)[surface.triangle_index as usize];
        let min_z = projected[a].sz.min(projected[b].sz).min(projected[c].sz);
        let max_z = projected[a].sz.max(projected[b].sz).max(projected[c].sz);
        return max_z.saturating_sub(min_z);
    }
    let min_z = projected[0]
        .sz
        .min(projected[1].sz)
        .min(projected[2].sz)
        .min(projected[3].sz);
    let max_z = projected[0]
        .sz
        .max(projected[1].sz)
        .max(projected[2].sz)
        .max(projected[3].sz);
    max_z.saturating_sub(min_z)
}

fn cached_surface_center(vertices: [WorldVertex; 4], split: u8, triangle_index: u8) -> RoomPoint {
    if triangle_index < WHOLE_QUAD_TRIANGLE_INDEX {
        let (a, b, c) = split_triangles_runtime(split)[triangle_index as usize];
        return RoomPoint::new(
            (vertices[a].x + vertices[b].x + vertices[c].x) / 3,
            (vertices[a].y + vertices[b].y + vertices[c].y) / 3,
            (vertices[a].z + vertices[b].z + vertices[c].z) / 3,
        );
    }
    RoomPoint::new(
        average4_i32(vertices[0].x, vertices[1].x, vertices[2].x, vertices[3].x),
        average4_i32(vertices[0].y, vertices[1].y, vertices[2].y, vertices[3].y),
        average4_i32(vertices[0].z, vertices[1].z, vertices[2].z, vertices[3].z),
    )
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ProjectedScreenBounds {
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
}

fn projected_screen_bounds(camera: &WorldCamera, margin: i32) -> ProjectedScreenBounds {
    let margin = margin.max(0);
    ProjectedScreenBounds {
        left: -margin,
        right: (camera.projection.screen_x as i32)
            .saturating_mul(2)
            .saturating_add(margin),
        top: -margin,
        bottom: (camera.projection.screen_y as i32)
            .saturating_mul(2)
            .saturating_add(margin),
    }
}

#[inline(always)]
fn projected_quad_outside_screen(
    projected: [ProjectedVertex; 4],
    bounds: ProjectedScreenBounds,
) -> bool {
    let min_x = projected[0]
        .sx
        .min(projected[1].sx)
        .min(projected[2].sx)
        .min(projected[3].sx) as i32;
    let max_x = projected[0]
        .sx
        .max(projected[1].sx)
        .max(projected[2].sx)
        .max(projected[3].sx) as i32;
    let min_y = projected[0]
        .sy
        .min(projected[1].sy)
        .min(projected[2].sy)
        .min(projected[3].sy) as i32;
    let max_y = projected[0]
        .sy
        .max(projected[1].sy)
        .max(projected[2].sy)
        .max(projected[3].sy) as i32;
    max_x < bounds.left || min_x > bounds.right || max_y < bounds.top || min_y > bounds.bottom
}

#[inline(always)]
fn indexed_projected_quad(
    projected_vertices: &[ProjectedVertex],
    ids: [u16; 4],
) -> Option<[ProjectedVertex; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= projected_vertices.len() {
        return None;
    }
    // SAFETY: `max_index < projected_vertices.len()` proves every id is in range.
    let projected = unsafe {
        [
            *projected_vertices.get_unchecked(a),
            *projected_vertices.get_unchecked(b),
            *projected_vertices.get_unchecked(c),
            *projected_vertices.get_unchecked(d),
        ]
    };
    if !projected[0].is_valid()
        || !projected[1].is_valid()
        || !projected[2].is_valid()
        || !projected[3].is_valid()
    {
        return None;
    }
    Some(projected)
}

#[inline(always)]
fn indexed_quad_depths(depths: &[i32], ids: [u16; 4]) -> Option<[i32; 4]> {
    let a = ids[0] as usize;
    let b = ids[1] as usize;
    let c = ids[2] as usize;
    let d = ids[3] as usize;
    let max_index = a.max(b).max(c).max(d);
    if max_index >= depths.len() {
        return None;
    }
    // SAFETY: `max_index < depths.len()` proves every id is in range.
    unsafe {
        Some([
            *depths.get_unchecked(a),
            *depths.get_unchecked(b),
            *depths.get_unchecked(c),
            *depths.get_unchecked(d),
        ])
    }
}

#[inline(always)]
fn indexed_vertex_lighting_colors<L: WorldSurfaceLighting>(
    lighting: &L,
    surface: CachedRoomSurface,
    material: WorldRenderMaterial,
    cached_vertices: &[WorldVertex],
    depths: &[i32],
    ids: [u16; 4],
    use_vertex_depths: bool,
    use_direct_baked_rgb: bool,
) -> Option<[(u8, u8, u8); 4]> {
    if use_direct_baked_rgb && surface.has_baked_rgb() {
        return Some(surface.baked_vertex_rgb);
    }
    if surface.has_baked_rgb() {
        let prepared_depths = if use_vertex_depths {
            Some(indexed_quad_depths(depths, ids)?)
        } else {
            None
        };
        let sample = surface.sample_without_center();
        if let Some(colors) =
            lighting.shade_cached_baked_vertices(sample, prepared_depths, material)
        {
            return Some(colors);
        }
    }

    let vertices = indexed_world_quad(cached_vertices, ids)?;
    let sample = surface.sample_with_center(
        vertices,
        lighting.needs_surface_sample_center(surface.has_baked_rgb()),
    );
    if use_vertex_depths {
        let depths = indexed_quad_depths(depths, ids)?;
        return Some(vertex_lighting_colors_with_depths(
            lighting, sample, material, vertices, depths,
        ));
    }
    Some(vertex_lighting_colors(lighting, sample, material, vertices))
}

#[inline(always)]
fn projected_split_triangle_backface_culled(
    projected: [ProjectedVertex; 4],
    material: WorldRenderMaterial,
    base_cull: CullMode,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
) -> bool {
    if cull_for_sidedness(material.sidedness, base_cull) != CullMode::Back {
        return false;
    }
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    projected_quad_triangle_back_facing(projected, tri)
}

#[inline(always)]
fn projected_quad_backface_culled(
    projected: [ProjectedVertex; 4],
    material: WorldRenderMaterial,
    base_cull: CullMode,
    split_triangles: [(usize, usize, usize); 2],
) -> bool {
    if cull_for_sidedness(material.sidedness, base_cull) != CullMode::Back {
        return false;
    }
    let projected = if material.sidedness == SurfaceSidedness::Back {
        reverse_quad_winding(projected)
    } else {
        projected
    };
    let [(a, b, c), (d, e, f)] = split_triangles;
    projected_quad_triangle_back_facing(projected, (a, b, c))
        && projected_quad_triangle_back_facing(projected, (d, e, f))
}

#[inline(always)]
fn projected_quad_triangle_back_facing(
    projected: [ProjectedVertex; 4],
    tri: (usize, usize, usize),
) -> bool {
    let (a, b, c) = tri;
    projected_triangle_back_facing([projected[a], projected[b], projected[c]])
}

#[inline(always)]
fn projected_triangle_back_facing(verts: [ProjectedVertex; 3]) -> bool {
    psx_gte::scene::screen_triangle_back_facing([
        (verts[0].sx, verts[0].sy),
        (verts[1].sx, verts[1].sy),
        (verts[2].sx, verts[2].sy),
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
                let triangle_heights = sector.floor_triangle_heights(triangle_index);
                let triangle_quad_heights =
                    triangle_heights_to_quad(heights, split, triangle_index, triangle_heights);
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Floor,
                        sx,
                        sz,
                        center: horizontal_triangle_center(
                            sx,
                            sz,
                            sector_size,
                            triangle_quad_heights,
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
                    triangle_heights,
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
                let triangle_heights = sector.ceiling_triangle_heights(triangle_index);
                let triangle_quad_heights =
                    triangle_heights_to_quad(heights, split, triangle_index, triangle_heights);
                let material = lighting.shade(
                    WorldSurfaceSample {
                        kind: WorldSurfaceKind::Ceiling,
                        sx,
                        sz,
                        center: horizontal_triangle_center(
                            sx,
                            sz,
                            sector_size,
                            triangle_quad_heights,
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
                    triangle_heights,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
                let triangle_heights = sector.floor_triangle_heights(triangle_index);
                let triangle_quad_heights =
                    triangle_heights_to_quad(heights, split, triangle_index, triangle_heights);
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Floor,
                    sx,
                    sz,
                    center: horizontal_triangle_center(
                        sx,
                        sz,
                        sector_size,
                        triangle_quad_heights,
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
                    triangle_heights,
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
                let triangle_heights = sector.ceiling_triangle_heights(triangle_index);
                let triangle_quad_heights =
                    triangle_heights_to_quad(heights, split, triangle_index, triangle_heights);
                let sample = WorldSurfaceSample {
                    kind: WorldSurfaceKind::Ceiling,
                    sx,
                    sz,
                    center: horizontal_triangle_center(
                        sx,
                        sz,
                        sector_size,
                        triangle_quad_heights,
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
                    triangle_heights,
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
    cell_visibility_view_visible_to_camera(camera, options, view, radius, screen_margin)
}

#[inline(always)]
fn cell_visibility_view_visible_to_camera(
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    view: ViewVertex,
    radius: i32,
    screen_margin: i32,
) -> bool {
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
        horizontal_depth_options(options),
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
        horizontal_depth_options(options),
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
    heights: [i32; 3],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    submit_split_triangle(
        camera,
        horizontal_depth_options(options),
        CullMode::Back,
        material,
        horizontal_triangle_vertices(sx, sz, sector_size, split, triangle_index, heights, [0; 4]),
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
    heights: [i32; 3],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    submit_split_triangle(
        camera,
        horizontal_depth_options(options),
        CullMode::Back,
        material,
        horizontal_triangle_vertices(sx, sz, sector_size, split, triangle_index, heights, [0; 4]),
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(verts) = wall_vertices(sx, sz, sector_size, direction, heights) else {
        return;
    };
    let material = wall_material_for_direction(material, direction);
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
        horizontal_depth_options(options),
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
        horizontal_depth_options(options),
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
    heights: [i32; 3],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let verts =
        horizontal_triangle_vertices(sx, sz, sector_size, split, triangle_index, heights, [0; 4]);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_triangle_vertex_lit(
        camera,
        horizontal_depth_options(options),
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
    heights: [i32; 3],
    split: u8,
    triangle_index: usize,
    uvs: [(u8, u8); 4],
    material: WorldRenderMaterial,
    sample: WorldSurfaceSample,
    lighting: &L,
    camera: &WorldCamera,
    options: WorldSurfaceOptions,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let verts =
        horizontal_triangle_vertices(sx, sz, sector_size, split, triangle_index, heights, [0; 4]);
    let colors = vertex_lighting_colors(lighting, sample, material, verts);
    submit_split_triangle_vertex_lit(
        camera,
        horizontal_depth_options(options),
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
) {
    let Some(verts) = wall_vertices(sx, sz, sector_size, direction, heights) else {
        return;
    };
    let material = wall_material_for_direction(material, direction);
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

fn vertex_lighting_colors_with_depths<L: WorldSurfaceLighting>(
    lighting: &L,
    sample: WorldSurfaceSample,
    material: WorldRenderMaterial,
    verts: [WorldVertex; 4],
    depths: [i32; 4],
) -> [(u8, u8, u8); 4] {
    lighting.shade_vertices_with_depths(sample, verts, depths, material)
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
fn submit_projected_split_triangle_vertex_lit_cached_uv_words<const OT: usize>(
    projected: [crate::render3d::ProjectedVertex; 4],
    uv_words: [u16; 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    prepared_depth: Option<PreparedTriangleDepth>,
    _cull: CullMode,
    split: u8,
    triangle_index: usize,
    reverse_front: bool,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    world: &mut WorldRenderPass<'_, '_, OT>,
    profile: &mut RoomSurfaceMicroProfile,
) {
    let opts = options.with_material_layer(material.texture);
    let mut tri = split_triangles_runtime(split)[triangle_index.min(1)];
    if reverse_front ^ (material.sidedness == SurfaceSidedness::Back) {
        tri = (tri.0, tri.2, tri.1);
    }
    let (a, b, c) = tri;
    let tri_verts = [projected[a], projected[b], projected[c]];
    let tri_uv_words = [uv_words[a], uv_words[b], uv_words[c]];
    let tri_colors = [colors[a], colors[b], colors[c]];
    if let Some(prepared_depth) = prepared_depth {
        #[cfg(feature = "room-surface-profile")]
        let _ = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth_profiled(
            triangles,
            tri_verts,
            tri_uv_words,
            tri_colors,
            material.gouraud_packet,
            opts,
            prepared_depth,
            profile.submit_profile(),
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let _ = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth(
            triangles,
            tri_verts,
            tri_uv_words,
            tri_colors,
            material.gouraud_packet,
            opts,
            prepared_depth,
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let _ = profile;
        return;
    }
    #[cfg(feature = "room-surface-profile")]
    let _ = world.submit_textured_gouraud_triangle_prescreened_uv_words_profiled(
        triangles,
        tri_verts,
        tri_uv_words,
        tri_colors,
        material.texture,
        opts,
        profile.submit_profile(),
    );
    #[cfg(not(feature = "room-surface-profile"))]
    let _ = world.submit_textured_gouraud_triangle_prescreened_uv_words(
        triangles,
        tri_verts,
        tri_uv_words,
        tri_colors,
        material.texture,
        opts,
    );
    #[cfg(not(feature = "room-surface-profile"))]
    let _ = profile;
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn submit_sided_projected_gouraud_quad_cached_uv_words<const OT: usize>(
    world: &mut WorldRenderPass<'_, '_, OT>,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
    verts: [crate::render3d::ProjectedVertex; 4],
    uv_words: [u16; 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    prepared_depth: Option<PreparedTriangleDepth>,
    _base_cull: CullMode,
    split_triangles: [(usize, usize, usize); 2],
    profile: &mut RoomSurfaceMicroProfile,
) {
    let (verts, uv_words, colors) = match material.sidedness {
        SurfaceSidedness::Back => (
            reverse_quad_winding(verts),
            reverse_quad_winding(uv_words),
            reverse_quad_winding(colors),
        ),
        SurfaceSidedness::Front | SurfaceSidedness::Both => (verts, uv_words, colors),
    };
    let opts = options.with_material_layer(material.texture);
    let [(a, b, c), (d, e, f)] = split_triangles;
    if let Some(prepared_depth) = prepared_depth {
        #[cfg(feature = "room-surface-profile")]
        let stats = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth_profiled(
            triangles,
            [verts[a], verts[b], verts[c]],
            [uv_words[a], uv_words[b], uv_words[c]],
            [colors[a], colors[b], colors[c]],
            material.gouraud_packet,
            opts,
            prepared_depth,
            profile.submit_profile(),
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let stats = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth(
            triangles,
            [verts[a], verts[b], verts[c]],
            [uv_words[a], uv_words[b], uv_words[c]],
            [colors[a], colors[b], colors[c]],
            material.gouraud_packet,
            opts,
            prepared_depth,
        );
        if stats.primitive_overflow || stats.command_overflow {
            return;
        }
        #[cfg(feature = "room-surface-profile")]
        let _ = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth_profiled(
            triangles,
            [verts[d], verts[e], verts[f]],
            [uv_words[d], uv_words[e], uv_words[f]],
            [colors[d], colors[e], colors[f]],
            material.gouraud_packet,
            opts,
            prepared_depth,
            profile.submit_profile(),
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let _ = world.submit_textured_gouraud_triangle_leaf_uv_words_prepared_depth(
            triangles,
            [verts[d], verts[e], verts[f]],
            [uv_words[d], uv_words[e], uv_words[f]],
            [colors[d], colors[e], colors[f]],
            material.gouraud_packet,
            opts,
            prepared_depth,
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let _ = profile;
        return;
    }
    #[cfg(feature = "room-surface-profile")]
    let stats = world.submit_textured_gouraud_triangle_prescreened_uv_words_profiled(
        triangles,
        [verts[a], verts[b], verts[c]],
        [uv_words[a], uv_words[b], uv_words[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
        profile.submit_profile(),
    );
    #[cfg(not(feature = "room-surface-profile"))]
    let stats = world.submit_textured_gouraud_triangle_prescreened_uv_words(
        triangles,
        [verts[a], verts[b], verts[c]],
        [uv_words[a], uv_words[b], uv_words[c]],
        [colors[a], colors[b], colors[c]],
        material.texture,
        opts,
    );
    if stats.primitive_overflow || stats.command_overflow {
        return;
    }
    #[cfg(feature = "room-surface-profile")]
    let _ = world.submit_textured_gouraud_triangle_prescreened_uv_words_profiled(
        triangles,
        [verts[d], verts[e], verts[f]],
        [uv_words[d], uv_words[e], uv_words[f]],
        [colors[d], colors[e], colors[f]],
        material.texture,
        opts,
        profile.submit_profile(),
    );
    #[cfg(not(feature = "room-surface-profile"))]
    let _ = world.submit_textured_gouraud_triangle_prescreened_uv_words(
        triangles,
        [verts[d], verts[e], verts[f]],
        [uv_words[d], uv_words[e], uv_words[f]],
        [colors[d], colors[e], colors[f]],
        material.texture,
        opts,
    );
    #[cfg(not(feature = "room-surface-profile"))]
    let _ = profile;
}

#[allow(clippy::too_many_arguments)]
fn submit_sided_projected_gouraud_quad<const OT: usize>(
    world: &mut WorldRenderPass<'_, '_, OT>,
    triangles: &mut impl PrimitiveSink<TriTexturedGouraud>,
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
    triangles: &mut impl PrimitiveSink<TriTextured>,
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

#[inline(always)]
fn horizontal_heights_non_flat4(heights: [i32; 4]) -> bool {
    heights[0] != heights[1] || heights[0] != heights[2] || heights[0] != heights[3]
}

#[inline(always)]
fn horizontal_heights_non_flat3(heights: [i32; 3]) -> bool {
    heights[0] != heights[1] || heights[0] != heights[2]
}

fn horizontal_triangle_vertices(
    sx: u16,
    sz: u16,
    sector_size: i32,
    split: u8,
    triangle_index: usize,
    triangle_heights: [i32; 3],
    face_heights: [i32; 4],
) -> [WorldVertex; 4] {
    horizontal_vertices(
        sx,
        sz,
        sector_size,
        triangle_heights_to_quad(face_heights, split, triangle_index, triangle_heights),
    )
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
    use crate::PrimitiveArena;
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
        let face_heights = [0, 0, 0, 0];
        let heights = [
            triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 0),
            triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 1),
        ];
        assert_eq!(
            merge_horizontal_triangle_surface(
                [Some(3), Some(3)],
                [uvs, uvs],
                heights,
                face_heights,
                SPLIT_NW_SE,
            ),
            Some((3, uvs))
        );
    }

    #[test]
    fn merge_horizontal_triangle_surface_preserves_real_splits() {
        let uvs = [(0, 0), (TILE_UV, 0), (TILE_UV, TILE_UV), (0, TILE_UV)];
        let shifted_uvs = [(0, 0), (32, 0), (32, TILE_UV), (0, TILE_UV)];
        let face_heights = [0, 0, 0, 0];
        let heights = [
            triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 0),
            triangle_heights_from_quad(face_heights, SPLIT_NW_SE, 1),
        ];

        assert_eq!(
            merge_horizontal_triangle_surface(
                [Some(3), Some(4)],
                [uvs, uvs],
                heights,
                face_heights,
                SPLIT_NW_SE,
            ),
            None
        );
        assert_eq!(
            merge_horizontal_triangle_surface(
                [Some(3), Some(3)],
                [uvs, shifted_uvs],
                heights,
                face_heights,
                SPLIT_NW_SE,
            ),
            None
        );
        assert_eq!(
            merge_horizontal_triangle_surface(
                [Some(3), None],
                [uvs, uvs],
                heights,
                face_heights,
                SPLIT_NW_SE,
            ),
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
    fn diagonal_wall_materials_are_forced_double_sided() {
        let texture = TextureMaterial::opaque(0, 0, (128, 128, 128));
        assert_eq!(
            wall_material_for_direction(WorldRenderMaterial::front(texture), DIR_NORTH).sidedness,
            SurfaceSidedness::Back
        );
        assert_eq!(
            wall_material_for_direction(
                WorldRenderMaterial::front(texture),
                DIR_NORTH_WEST_SOUTH_EAST
            )
            .sidedness,
            SurfaceSidedness::Both
        );
        assert_eq!(
            wall_material_for_direction(
                WorldRenderMaterial::back(texture),
                DIR_NORTH_EAST_SOUTH_WEST
            )
            .sidedness,
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
    fn generated_cache_records_reconstruct_cached_samples() {
        let vertices = [
            WorldVertex::new(0, 10, 0),
            WorldVertex::new(1024, 20, 0),
            WorldVertex::new(1024, 30, 1024),
            WorldVertex::new(0, 40, 1024),
        ];
        let vertex_records = [
            LevelCachedRoomVertexRecord {
                x: vertices[0].x,
                y: vertices[0].y,
                z: vertices[0].z,
            },
            LevelCachedRoomVertexRecord {
                x: vertices[1].x,
                y: vertices[1].y,
                z: vertices[1].z,
            },
            LevelCachedRoomVertexRecord {
                x: vertices[2].x,
                y: vertices[2].y,
                z: vertices[2].z,
            },
            LevelCachedRoomVertexRecord {
                x: vertices[3].x,
                y: vertices[3].y,
                z: vertices[3].z,
            },
        ];
        assert_eq!(
            cached_room_vertices_from_level_records(&vertex_records),
            &vertices
        );

        let cell_records = [LevelCachedRoomCellRecord {
            x: 3,
            z: 4,
            min_y: 10,
            max_y: 40,
            visibility_center: [512, 25, 512],
            visibility_radius: 1040,
            surface_first: 7,
            surface_count: 1,
            vertex_first: 2,
            vertex_count: 4,
        }];
        let cells = cached_room_cells_from_level_records(&cell_records);
        assert_eq!(cells[0].x, 3);
        assert_eq!(cells[0].visibility_center, [512, 25, 512]);
        assert_eq!(cells[0].surface_first, 7);
        assert_eq!(cells[0].vertex_first, 2);

        let baked = [(1, 2, 3), (4, 5, 6), (7, 8, 9), (10, 11, 12)];
        let surface = CachedRoomSurface::new(
            5,
            [0, 1, 2, 3],
            [(0, 0), (32, 0), (32, 64), (0, 64)],
            WorldSurfaceSample {
                kind: WorldSurfaceKind::Wall {
                    direction: DIR_EAST,
                },
                sx: 3,
                sz: 4,
                center: RoomPoint::ZERO,
                baked_vertex_rgb: Some(baked),
                ordinal: 9,
            },
            SPLIT_NE_SW,
            1,
        );
        let surface_records = [LevelCachedRoomSurfaceRecord {
            material_slot: surface.material_slot,
            vertex_indices: surface.vertex_indices,
            sample_sx: surface.sample_sx,
            sample_sz: surface.sample_sz,
            sample_ordinal: surface.sample_ordinal,
            uv_words: surface.uv_words,
            baked_vertex_rgb: surface.baked_vertex_rgb,
            kind_flags: surface.kind_flags,
            wall_direction: surface.wall_direction,
            split: surface.split,
            triangle_index: surface.triangle_index,
        }];
        let surfaces = cached_room_surfaces_from_level_records(&surface_records);
        assert_eq!(surfaces[0], surface);
        assert_eq!(surfaces[0].uvs(), [(0, 0), (32, 0), (32, 64), (0, 64)]);
        let sample = surfaces[0].sample_with_center(vertices, true);
        assert_eq!(
            sample.kind,
            WorldSurfaceKind::Wall {
                direction: DIR_EAST
            }
        );
        assert_eq!(sample.sx, 3);
        assert_eq!(sample.sz, 4);
        assert_eq!(sample.ordinal, 9);
        assert_eq!(sample.baked_vertex_rgb, Some(baked));
        assert_eq!(
            sample.center,
            cached_surface_center(vertices, SPLIT_NE_SW, 1)
        );
    }

    #[test]
    fn floor_depth_uses_farthest_projected_depth() {
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
            max3(projected[a].sz, projected[b].sz, projected[c].sz) + HORIZONTAL_DEPTH_BIAS
        );
        assert_eq!(
            commands[1].depth_raw(),
            max3(projected[d].sz, projected[e].sz, projected[f].sz) + HORIZONTAL_DEPTH_BIAS
        );
    }

    #[test]
    fn cached_full_ceiling_faces_playable_interior() {
        let mut ot_storage = psx_gpu::ot::OrderingTable::<8>::new();
        let mut ot = crate::OtFrame::begin(&mut ot_storage);
        let mut packet_scratch = crate::PrimitivePacketScratch::<4>::ZERO;
        let mut triangles = crate::PrimitivePacketArena::new(&mut packet_scratch);
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
        let cells = [CachedRoomCell::new(0, 0, 1024, 1024, 1024, 0, 1, 0, 4)];
        let surface = CachedRoomSurface::new(
            0,
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
        let surfaces = [surface];
        let visible_cells = [GridVisibleCell::new(0, 0, 1024, 1024)];
        let cell_vertices = [0u16, 1, 2, 3];
        let mut projected_indices = [0u16; 4];
        let mut projected = [ProjectedVertex::new(0, 0, 0); 4];
        let mut projected_ready = [false; 4];
        let mut projected_depths = [0; 4];
        let mut accepted_cell_indices = [0u16; 1];
        let mut accepted_cell_depths = [0; 1];

        let stats = draw_indexed_cached_room_vertex_lit_visible_cells(
            &cells,
            &cell_vertices,
            &vertices,
            &surfaces,
            &mut projected_indices,
            &mut projected,
            &mut projected_ready,
            &mut projected_depths,
            &mut accepted_cell_indices,
            &mut accepted_cell_depths,
            1,
            1024,
            &[WorldRenderMaterial::front(TextureMaterial::opaque(
                0,
                0,
                (128, 128, 128),
            ))],
            &NoWorldSurfaceLighting,
            &camera,
            options,
            CachedRoomDepthMode::FixedCell,
            CachedRoomSubdivisionMode::All,
            &visible_cells,
            0,
            &mut triangles,
            &mut pass,
        );
        assert_eq!(stats.surfaces_considered, 1);
        assert_eq!(projected_ready, [false; 4]);
        assert_eq!(pass.command_len(), 2);
        drop(pass);

        let expected_depth =
            tile_camera_depth(&camera, visible_cells[0], 1024) + HORIZONTAL_DEPTH_BIAS;
        assert_eq!(commands[0].depth_raw(), expected_depth);
        assert_eq!(commands[1].depth_raw(), expected_depth);
    }

    #[test]
    fn hybrid_depth_uses_triangle_depth_for_sloped_horizontal_surfaces() {
        let projected = [
            ProjectedVertex::new(0, 0, 1024),
            ProjectedVertex::new(64, 0, 1056),
            ProjectedVertex::new(64, 64, 1088),
            ProjectedVertex::new(0, 64, 1040),
        ];
        let surface = CachedRoomSurface::new(
            0,
            [0, 1, 2, 3],
            [(0, 0), (64, 0), (64, 64), (0, 64)],
            WorldSurfaceSample {
                kind: WorldSurfaceKind::Floor,
                sx: 0,
                sz: 0,
                center: RoomPoint::ZERO,
                baked_vertex_rgb: None,
                ordinal: 0,
            },
            SPLIT_NW_SE,
            WHOLE_QUAD_TRIANGLE_INDEX,
        );
        let sloped_surface = surface.with_horizontal_non_flat(true);

        assert!(!cached_surface_uses_triangle_depth(
            CachedRoomDepthMode::Hybrid,
            WorldSurfaceKind::Floor,
            surface,
            projected,
        ));
        assert!(cached_surface_uses_triangle_depth(
            CachedRoomDepthMode::Hybrid,
            WorldSurfaceKind::Floor,
            sloped_surface,
            projected,
        ));
        assert!(cached_surface_uses_triangle_depth(
            CachedRoomDepthMode::PerTriangle,
            WorldSurfaceKind::Wall {
                direction: DIR_EAST,
            },
            surface,
            projected,
        ));
        assert!(!cached_surface_uses_triangle_depth(
            CachedRoomDepthMode::Hybrid,
            WorldSurfaceKind::Wall {
                direction: DIR_EAST,
            },
            surface,
            [
                ProjectedVertex::new(0, 0, 1024),
                ProjectedVertex::new(64, 0, 2048),
                ProjectedVertex::new(64, 64, 2112),
                ProjectedVertex::new(0, 64, 1088),
            ],
        ));
        assert!(cached_surface_uses_triangle_depth(
            CachedRoomDepthMode::HybridWalls,
            WorldSurfaceKind::Wall {
                direction: DIR_EAST,
            },
            surface,
            [
                ProjectedVertex::new(0, 0, 1024),
                ProjectedVertex::new(64, 0, 2048),
                ProjectedVertex::new(64, 64, 2112),
                ProjectedVertex::new(0, 64, 1088),
            ],
        ));
    }

    #[test]
    fn horizontal_face_center_uses_cell_midpoint_and_average_height() {
        assert_eq!(
            horizontal_face_center(2, 3, 1024, [0, 512, 1024, 512]),
            RoomPoint::new(2560, 512, 3584)
        );
    }

    #[test]
    fn grid_visible_cell_camera_depth_fits_existing_padding() {
        assert_eq!(core::mem::size_of::<GridVisibleCell>(), 16);
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
