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
    prim::{QuadTexturedGouraud, TriTextured, TriTexturedGouraud},
};
use psx_level::{
    LevelCachedRoomCellRecord, LevelCachedRoomSurfaceRecord, LevelCachedRoomVertexRecord,
};

#[cfg(feature = "room-surface-profile")]
use crate::render3d::TexturedGouraudSubmitMicroProfile;

use crate::{
    render3d::{
        project_world_vertex_indices_gte, project_world_vertices_gte, CullMode, DepthPolicy,
        LoadedWorldCameraGte, PreparedTriangleDepth, ProjectedVertex,
        AdaptiveSubdivisionKindMask, AdaptiveSubdivisionProfile, ViewVertex,
    },
    PrimitiveSink, RoomPoint, RoomRender, RoomSurfaceSink, WorldCamera, WorldRenderPass,
    WorldSurfaceOptions, WorldVertex,
};

mod cache_build;
mod indexed_cache;
mod room_draw;

pub use cache_build::cache_room_vertex_lit_surfaces;
use indexed_cache::{cached_surface_center, RoomSurfaceMicroProfile};
#[cfg(test)]
use indexed_cache::{
    cached_surface_uses_triangle_depth, encoded_warmed_room_quad_backface_culled,
    adaptive_warmed_quad_requires_dynamic_submit,
};
pub use indexed_cache::{
    draw_indexed_cached_room_vertex_lit_all_cells,
    draw_indexed_cached_room_vertex_lit_visible_cells, prewarm_indexed_cached_room_quads,
};
pub use room_draw::{
    draw_room, draw_room_lit, draw_room_lit_grid_visible, draw_room_vertex_lit,
    draw_room_vertex_lit_grid_visible, draw_room_vertex_lit_visible_cells,
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

/// Runtime animation for a room material's single resident texture pass.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum WorldMaterialAnimation {
    /// No per-frame UV work; eligible for immutable prebuilt packets.
    #[default]
    Static,
    /// Signed Q8 texels-per-second UV motion.
    UvScroll {
        /// Horizontal Q8 texels per second.
        speed_u_q8: i16,
        /// Vertical Q8 texels per second.
        speed_v_q8: i16,
        /// Initial horizontal texel offset.
        phase_u: u8,
        /// Initial vertical texel offset.
        phase_v: u8,
    },
    /// Row-major frames packed into the material's existing 4bpp texture.
    Flipbook {
        /// Number of frame columns in the atlas.
        columns: u8,
        /// Number of active row-major frames.
        frame_count: u8,
        /// Simulation ticks each frame remains selected.
        ticks_per_frame: u8,
        /// Initial frame index.
        phase: u8,
    },
}

impl WorldMaterialAnimation {
    /// Whether this material must resolve UVs at draw time.
    pub const fn is_animated(self) -> bool {
        !matches!(self, Self::Static)
    }

    /// Material-local UV offset for the supplied gameplay clock.
    ///
    /// Scroll motion wraps at the resident texture-window dimensions rather
    /// than at 256. This keeps every vertex on the same side of the byte-UV
    /// rollover while GP0(E2) repeats the texture on the far edge.
    pub fn uv_offset(self, tick: u32, hz: u16, frame_width: u8, frame_height: u8) -> (u8, u8) {
        match self {
            Self::Static => (0, 0),
            Self::UvScroll {
                speed_u_q8,
                speed_v_q8,
                phase_u,
                phase_v,
            } => {
                let hz = i64::from(hz.max(1));
                let resolve = |speed: i16, phase: u8, period: u8| {
                    let travelled_q8 = i64::from(speed).saturating_mul(i64::from(tick)) / hz;
                    (travelled_q8 / 256 + i64::from(phase)).rem_euclid(i64::from(period.max(1)))
                        as u8
                };
                (
                    resolve(speed_u_q8, phase_u, frame_width),
                    resolve(speed_v_q8, phase_v, frame_height),
                )
            }
            Self::Flipbook {
                columns,
                frame_count,
                ticks_per_frame,
                phase,
            } => {
                let columns = columns.max(1);
                let frame_count = frame_count.max(1);
                let frame = ((tick / u32::from(ticks_per_frame.max(1))) + u32::from(phase))
                    % u32::from(frame_count);
                (
                    (frame as u8 % columns).wrapping_mul(frame_width),
                    (frame as u8 / columns).wrapping_mul(frame_height),
                )
            }
        }
    }
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
    /// Optional UV animation evaluated from the gameplay clock.
    pub animation: WorldMaterialAnimation,
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
            animation: WorldMaterialAnimation::Static,
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
            animation: WorldMaterialAnimation::Static,
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
            animation: WorldMaterialAnimation::Static,
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

    /// Return a copy with runtime UV animation.
    pub const fn with_animation(mut self, animation: WorldMaterialAnimation) -> Self {
        self.animation = animation;
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
    // Whether the cooker proved the owning cell is the only walkable side; see
    // `CACHED_SURFACE_WALL_FACES_OWNER`.
    faces_owner: bool,
) -> WorldRenderMaterial {
    // Cardinal wall windings make the owning cell's interior the back side, so
    // backface culling used to delete a boundary wall for anyone standing in
    // the cell that owns it -- which, for a wall that bounds the playable area,
    // is the only place a player can stand. That is the cortex_v1 report:
    // approach the corridor's west wall and it disappears once you enter its
    // owning cell, with no counter recording the loss because a backface cull
    // is a legitimate outcome.
    //
    // A sector wall is solid geometry, not a one-sided cut, so both of its
    // faces are real surfaces. Diagonal walls already opted out of the
    // Front/Back distinction for exactly this reason; cardinals now match them.
    // `wall_material` still selects the authored per-side texture, so the two
    // faces keep their own appearance.
    //
    // Both faces is correct but not free: it doubles the wall's raster work and
    // costs +9.09% of the render stage. When the cooker can prove from grid
    // adjacency that the neighbour behind the wall is absent or non-walkable,
    // the owning cell is the ONLY side a player can reach, so skipping the
    // front/back swap puts the visible face where the player is and one face
    // suffices. Anything the cooker cannot prove -- including walls walkable
    // from both sides -- keeps the conservative answer above.
    // A diagonal cuts through a cell rather than bounding it, so it has no
    // single neighbour to prove empty and is reachable from both sides.
    let cardinal = !matches!(
        direction,
        DIR_NORTH_WEST_SOUTH_EAST | DIR_NORTH_EAST_SOUTH_WEST
    );
    if cardinal && faces_owner {
        // Keep the same swap the both-faces path uses -- that is the
        // orientation that renders correctly -- and only drop the second face.
        // Returning the UNSWAPPED material here instead was the 2026-07-25
        // regression: every wall the rule fired on drew its far side, which
        // reads in the editor as inverted normals.
        return wall_material(material);
    }
    material = wall_material(material);
    material.sidedness = SurfaceSidedness::Both;
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

/// Conservative union of every portal frustum that reaches one room.
///
/// A room can be reached through more than one portal path. Keeping the
/// component-wise union prevents a later cell pass from treating the first
/// doorway as the room's only aperture and deleting geometry visible through
/// another path.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PortalCellWindow {
    /// Inclusive left tangent in Q12 view-space units.
    pub left_tan_q12: i32,
    /// Inclusive right tangent in Q12 view-space units.
    pub right_tan_q12: i32,
    /// Inclusive lower tangent in Q12 view-space units.
    pub min_y_tan_q12: i32,
    /// Inclusive upper tangent in Q12 view-space units.
    pub max_y_tan_q12: i32,
}

impl PortalCellWindow {
    /// Construct one clipped tangent window.
    pub const fn new(
        left_tan_q12: i32,
        right_tan_q12: i32,
        min_y_tan_q12: i32,
        max_y_tan_q12: i32,
    ) -> Self {
        Self {
            left_tan_q12,
            right_tan_q12,
            min_y_tan_q12,
            max_y_tan_q12,
        }
    }

    /// Return the conservative component-wise union of two portal paths.
    pub const fn union(self, other: Self) -> Self {
        Self {
            left_tan_q12: if self.left_tan_q12 < other.left_tan_q12 {
                self.left_tan_q12
            } else {
                other.left_tan_q12
            },
            right_tan_q12: if self.right_tan_q12 > other.right_tan_q12 {
                self.right_tan_q12
            } else {
                other.right_tan_q12
            },
            min_y_tan_q12: if self.min_y_tan_q12 < other.min_y_tan_q12 {
                self.min_y_tan_q12
            } else {
                other.min_y_tan_q12
            },
            max_y_tan_q12: if self.max_y_tan_q12 > other.max_y_tan_q12 {
                self.max_y_tan_q12
            } else {
                other.max_y_tan_q12
            },
        }
    }
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
    /// Whether this wall's playable side is the cell that owns it.
    ///
    /// See [`CACHED_SURFACE_WALL_FACES_OWNER`].
    pub const fn wall_faces_owner(&self) -> bool {
        self.kind_flags & CACHED_SURFACE_WALL_FACES_OWNER != 0
    }

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
    fn with_wall_faces_owner(mut self, faces_owner: bool) -> Self {
        if faces_owner {
            self.kind_flags |= CACHED_SURFACE_WALL_FACES_OWNER;
        }
        self
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
/// A cardinal wall whose playable side is the cell that OWNS it.
///
/// Cardinal wall windings put the owning cell's interior on the back face, so a
/// wall is visible from its neighbour by default. When that neighbour is absent
/// or non-walkable the owning cell is the only side a player can stand on, and
/// without this the wall is culled from the one place it can be seen. The cooker
/// sets this from grid adjacency; the renderer flips such a wall and keeps it
/// single-sided, instead of paying for both faces everywhere.
const CACHED_SURFACE_WALL_FACES_OWNER: u8 = 0b0000_0100;
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
            self.shade_vertex(sample, vertices[0], material),
            self.shade_vertex(sample, vertices[1], material),
            self.shade_vertex(sample, vertices[2], material),
            self.shade_vertex(sample, vertices[3], material),
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

    /// Shade a prewarmed static packet from baked RGB without reconstructing
    /// its immutable material.
    ///
    /// The default declines this path. Project-specialized lighting adapters
    /// can opt in when their result depends only on baked RGB and prepared
    /// vertex depths.
    fn shade_prewarmed_baked_vertices(
        &self,
        _sample: WorldSurfaceSample,
        _depths: Option<[i32; 4]>,
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
    let half = sector_size >> 1;
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
                surfaces = surfaces.wrapping_add(1);
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
    // Use the tight conservative sphere around the cell AABB. The previous
    // `sector_size + half_height` L1 bound was much larger than the geometry:
    // a flat square cell used 1.5x the sector size instead of sqrt(0.5) times
    // it. That admitted large runs of fully off-screen cells, only to reject
    // their surfaces later in the hottest room-rendering loop.
    //
    // Ceil each half extent because integer midpoint truncation can leave the
    // farther edge one unit away for odd/negative ranges, then ceil the square
    // root so the replacement never under-bounds a corner.
    let half_x = (x1.saturating_sub(x0).abs().saturating_add(1) >> 1) as u64;
    let half_y = (max_y.saturating_sub(min_y).abs().saturating_add(1) >> 1) as u64;
    let half_z = (z1.saturating_sub(z0).abs().saturating_add(1) >> 1) as u64;
    let radius_squared = half_x
        .saturating_mul(half_x)
        .saturating_add(half_y.saturating_mul(half_y))
        .saturating_add(half_z.saturating_mul(half_z));
    let radius_floor = radius_squared.isqrt();
    let radius = radius_floor
        .saturating_add(u64::from(
            radius_floor.saturating_mul(radius_floor) != radius_squared,
        ))
        .min(i32::MAX as u64) as i32;
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
    CellFrustum::new(camera, options, screen_margin).sphere_visible(view, radius)
}

/// Per-draw precomputed constants for the per-cell sphere visibility tests.
///
/// The cell-select loops run one test per candidate cell per frame; deriving
/// the clamped near/far/focal/screen extents once per draw call instead of
/// per cell, and using exact widening 32x32->64 products (one MULT each on
/// MIPS) instead of saturating 32-bit products, keeps the per-cell cost to
/// the compares themselves. The widening products differ from the old
/// saturating ones only where a product would have clamped at i32::MAX,
/// which PVS-range-bounded cell centers cannot reach.
#[derive(Copy, Clone)]
pub(crate) struct CellFrustum {
    near: i32,
    far: i32,
    focal: i32,
    half_w: i32,
    half_h: i32,
    sphere_x_support: i32,
    sphere_y_support: i32,
    lateral_i32_limit: u32,
    view_abs: [[i32; 3]; 3],
}

#[inline(always)]
fn conservative_plane_sphere_support(a: i32, b: i32) -> i32 {
    let larger = a.max(b).max(0);
    let smaller = a.min(b).max(0);
    // `larger + smaller / 2` is an upper bound for
    // `sqrt(larger^2 + smaller^2)` when `smaller <= larger`. Round the half
    // upward so integer truncation cannot turn the bound into an underestimate.
    larger.saturating_add(smaller.saturating_add(1) >> 1)
}

#[cold]
#[inline(never)]
fn cell_aabb_extent_wide(row: [i32; 3], half_x: i32, half_y: i32, half_z: i32) -> i32 {
    let q12 = (row[0] as i64 * half_x as i64)
        .saturating_add(row[1] as i64 * half_y as i64)
        .saturating_add(row[2] as i64 * half_z as i64)
        .saturating_add(4095)
        >> 12;
    (q12.min(i32::MAX as i64) as i32).saturating_add(2)
}

#[cold]
#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn cell_aabb_lateral_visible_wide(
    view: ViewVertex,
    z: i32,
    extent_x: i32,
    extent_y: i32,
    extent_z: i32,
    focal: i32,
    half_w: i32,
    half_h: i32,
) -> bool {
    let px = view.x.abs() as i64 * focal as i64;
    let py = view.y.abs() as i64 * focal as i64;
    let x_limit =
        half_w as i64 * z as i64 + focal as i64 * extent_x as i64 + half_w as i64 * extent_z as i64;
    let y_limit =
        half_h as i64 * z as i64 + focal as i64 * extent_y as i64 + half_h as i64 * extent_z as i64;
    px <= x_limit && py <= y_limit
}

impl CellFrustum {
    #[inline(always)]
    fn cell_aabb_view_extents(self, half_x: i32, half_y: i32, half_z: i32) -> [i32; 3] {
        let half_x = half_x.max(0);
        let half_y = half_y.max(0);
        let half_z = half_z.max(0);
        let extent_fast = half_x <= 174_000 && half_y <= 174_000 && half_z <= 174_000;
        if extent_fast {
            let extent = |row: [i32; 3]| {
                ((row[0] * half_x + row[1] * half_y + row[2] * half_z + 4095) >> 12)
                    .saturating_add(2)
            };
            [
                extent(self.view_abs[0]),
                extent(self.view_abs[1]),
                extent(self.view_abs[2]),
            ]
        } else {
            [
                cell_aabb_extent_wide(self.view_abs[0], half_x, half_y, half_z),
                cell_aabb_extent_wide(self.view_abs[1], half_x, half_y, half_z),
                cell_aabb_extent_wide(self.view_abs[2], half_x, half_y, half_z),
            ]
        }
    }

    #[inline(always)]
    pub(crate) fn new(
        camera: &WorldCamera,
        options: WorldSurfaceOptions,
        screen_margin: i32,
    ) -> Self {
        let near = camera.projection.near_z.max(1);
        let focal = camera.projection.focal_length.max(1);
        let half_w = (camera.projection.screen_x as i32)
            .saturating_add(screen_margin)
            .max(1);
        let half_h = (camera.projection.screen_y as i32)
            .saturating_add(screen_margin)
            .max(1);
        let sy_sp = camera.sin_yaw.mul_q12(camera.sin_pitch).raw();
        let cy_sp = camera.cos_yaw.mul_q12(camera.sin_pitch).raw();
        let sy_cp = camera.sin_yaw.mul_q12(camera.cos_pitch).raw();
        let cy_cp = camera.cos_yaw.mul_q12(camera.cos_pitch).raw();
        let lateral_max_coefficient = focal.max(half_w).max(half_h) as u32;
        Self {
            near,
            far: options.depth_range.far().max(near),
            focal,
            half_w,
            half_h,
            // For side plane `focal*x - half_w*z = 0`, a sphere extends
            // `radius * sqrt(focal^2 + half_w^2)` along the plane normal.
            // A tight integer upper bound for the plane-normal length is
            // evaluated once per draw. The old `radius * focal` term
            // under-bounded spheres near the screen edge and only appeared
            // safe because cell radii were themselves heavily inflated.
            sphere_x_support: conservative_plane_sphere_support(focal, half_w),
            sphere_y_support: conservative_plane_sphere_support(focal, half_h),
            // Three non-negative products are summed for each lateral AABB
            // plane. Keeping every input below this limit proves that both
            // plane sums fit in i32, while unusual authored coordinates fall
            // back to the exact widened implementation.
            lateral_i32_limit: (i32::MAX as u32 / 3) / lateral_max_coefficient,
            view_abs: [
                [camera.cos_yaw.raw().abs(), 0, camera.sin_yaw.raw().abs()],
                [sy_sp.abs(), camera.cos_pitch.raw().abs(), cy_sp.abs()],
                [sy_cp.abs(), camera.sin_pitch.raw().abs(), cy_cp.abs()],
            ],
        }
    }

    /// Near/far band plus lateral extent test for a view-space sphere.
    #[inline(always)]
    pub(crate) fn sphere_visible(self, view: ViewVertex, radius: i32) -> bool {
        if view.z < self.near.saturating_sub(radius) || view.z > self.far.saturating_add(radius) {
            return false;
        }
        self.sphere_lateral(view, radius)
    }

    /// Lateral plus near test with NO far plane (the all-cells root-room
    /// cull; see `cell_visibility_view_in_lateral_frustum`).
    #[inline(always)]
    pub(crate) fn sphere_visible_no_far(self, view: ViewVertex, radius: i32) -> bool {
        if view.z < self.near.saturating_sub(radius) {
            return false;
        }
        self.sphere_lateral(view, radius)
    }

    /// Near/far and lateral test for a world-axis-aligned cell box.
    ///
    /// The box is converted to a conservative view-space AABB. Two units of
    /// slack cover the separate Q12 truncations used for the transformed
    /// centre and corners, so this remains a broad-phase rejection only.
    #[inline(always)]
    pub(crate) fn cell_aabb_visible(
        self,
        view: ViewVertex,
        half_x: i32,
        half_y: i32,
        half_z: i32,
    ) -> bool {
        // Each absolute Q12 basis coefficient is <= 4096. With all three
        // half-extents <= 174,000, their worst-case sum plus rounding is below
        // i32::MAX, so the common PS1-sized cell can avoid widened saturating
        // arithmetic. Unusually large authored cells retain the old path.
        let [extent_x, extent_y, extent_z] = self.cell_aabb_view_extents(half_x, half_y, half_z);
        if view.z < self.near.saturating_sub(extent_z) || view.z > self.far.saturating_add(extent_z)
        {
            return false;
        }

        let z = view.z.max(self.near);
        let lateral_max_value = view
            .x
            .unsigned_abs()
            .max(view.y.unsigned_abs())
            .max(z as u32)
            .max(extent_x as u32)
            .max(extent_y as u32)
            .max(extent_z as u32);
        if lateral_max_value <= self.lateral_i32_limit {
            let px = view.x.abs() * self.focal;
            let py = view.y.abs() * self.focal;
            let x_limit = self.half_w * z + self.focal * extent_x + self.half_w * extent_z;
            let y_limit = self.half_h * z + self.focal * extent_y + self.half_h * extent_z;
            px <= x_limit && py <= y_limit
        } else {
            cell_aabb_lateral_visible_wide(
                view,
                z,
                extent_x,
                extent_y,
                extent_z,
                self.focal,
                self.half_w,
                self.half_h,
            )
        }
    }

    /// Conservative AABB test against a clipped portal tangent window.
    #[inline(always)]
    pub(crate) fn cell_aabb_intersects_portal_window(
        self,
        view: ViewVertex,
        half_x: i32,
        half_y: i32,
        half_z: i32,
        window: PortalCellWindow,
    ) -> bool {
        let [extent_x, extent_y, extent_z] = self.cell_aabb_view_extents(half_x, half_y, half_z);
        let x_support = |edge: i32| {
            4096i32
                .saturating_mul(extent_x)
                .saturating_add(edge.saturating_abs().saturating_mul(extent_z))
        };
        let y_support = |edge: i32| {
            4096i32
                .saturating_mul(extent_y)
                .saturating_add(edge.saturating_abs().saturating_mul(extent_z))
        };
        view.x
            .saturating_mul(4096)
            .saturating_sub(window.left_tan_q12.saturating_mul(view.z))
            >= -x_support(window.left_tan_q12)
            && window
                .right_tan_q12
                .saturating_mul(view.z)
                .saturating_sub(view.x.saturating_mul(4096))
                >= -x_support(window.right_tan_q12)
            && view
                .y
                .saturating_mul(4096)
                .saturating_sub(window.min_y_tan_q12.saturating_mul(view.z))
                >= -y_support(window.min_y_tan_q12)
            && window
                .max_y_tan_q12
                .saturating_mul(view.z)
                .saturating_sub(view.y.saturating_mul(4096))
                >= -y_support(window.max_y_tan_q12)
    }

    #[inline(always)]
    fn sphere_lateral(self, view: ViewVertex, radius: i32) -> bool {
        let z = view.z.max(self.near);
        let radius = radius.max(0);
        // psx-numeric-allow-next-line: frustum plane products widen to the native MULT result
        let px = view.x.abs() as i64 * self.focal as i64;
        // psx-numeric-allow-next-line: frustum plane products widen to the native MULT result
        let py = view.y.abs() as i64 * self.focal as i64;
        // psx-numeric-allow-next-line: screen and conservative sphere-support products share a widened sum
        let x_limit = self.half_w as i64 * z as i64 + radius as i64 * self.sphere_x_support as i64;
        // psx-numeric-allow-next-line: screen and conservative sphere-support products share a widened sum
        let y_limit = self.half_h as i64 * z as i64 + radius as i64 * self.sphere_y_support as i64;
        px <= x_limit && py <= y_limit
    }
}

/// Lateral + near frustum test for a cell visibility sphere, with NO far plane.
/// The all-cells path uses this only for the camera/root room; neighbouring
/// active rooms skip coarse cell culling so portal-edge geometry is not dropped.
#[allow(dead_code)]
fn cell_visibility_view_in_lateral_frustum(
    camera: &WorldCamera,
    view: ViewVertex,
    radius: i32,
    screen_margin: i32,
) -> bool {
    let near = camera.projection.near_z.max(1);
    let lateral = CellFrustum {
        near,
        far: i32::MAX,
        focal: camera.projection.focal_length.max(1),
        half_w: (camera.projection.screen_x as i32)
            .saturating_add(screen_margin)
            .max(1),
        half_h: (camera.projection.screen_y as i32)
            .saturating_add(screen_margin)
            .max(1),
        sphere_x_support: conservative_plane_sphere_support(
            camera.projection.focal_length.max(1),
            (camera.projection.screen_x as i32)
                .saturating_add(screen_margin)
                .max(1),
        ),
        sphere_y_support: conservative_plane_sphere_support(
            camera.projection.focal_length.max(1),
            (camera.projection.screen_y as i32)
                .saturating_add(screen_margin)
                .max(1),
        ),
        lateral_i32_limit: 0,
        view_abs: [[0; 3]; 3],
    };
    lateral.sphere_visible_no_far(view, radius)
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
    let material = wall_material_for_direction(material, direction, false);
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
    let material = wall_material_for_direction(material, direction, false);
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
    uvs = animated_material_uvs(material, options, uvs);
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
    let uvs = animated_material_uvs(material, options, uvs);
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
    let uvs = animated_material_uvs(material, options, uvs);
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
    triangles: &mut impl RoomSurfaceSink,
    verts: [crate::render3d::ProjectedVertex; 4],
    uv_words: [u16; 4],
    colors: [(u8, u8, u8); 4],
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    prepared_depth: Option<PreparedTriangleDepth>,
    _base_cull: CullMode,
    split: u8,
    prebuilt: Option<(&mut QuadTexturedGouraud, &mut u8)>,
    prebuilt_colors_static: bool,
    prebuilt_ready_value: u8,
    profile: &mut RoomSurfaceMicroProfile,
) {
    let opts = options.with_material_layer(material.texture);
    // The hardware quad (GP0 3Ch) rasterizes as tri(q0,q1,q2)+tri(q1,q2,q3),
    // i.e. the 1-2 diagonal. Reorder the four corners so that diagonal
    // lands on the engine's split diagonal, then one quad packet is
    // pixel-identical to the two leaves (proved bit-exact in the emulator
    // GPU tests). Unknown split ids use the standard NW-SE fallback, matching
    // `split_triangles_runtime`.
    if let Some(prepared_depth) = prepared_depth {
        let (quad_verts, quad_uv_words, quad_colors) = (
            quad_packet_order(verts, split),
            quad_packet_order(uv_words, split),
            quad_packet_order(colors, split),
        );
        let _ = world.submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth(
            triangles,
            prebuilt,
            prebuilt_colors_static,
            prebuilt_ready_value,
            quad_verts,
            quad_uv_words,
            quad_colors,
            material.texture,
            &opts,
            prepared_depth,
        );
        #[cfg(not(feature = "room-surface-profile"))]
        let _ = profile;
        return;
    }
    let (verts, uv_words, colors) = match material.sidedness {
        SurfaceSidedness::Back => (
            reverse_quad_winding(verts),
            reverse_quad_winding(uv_words),
            reverse_quad_winding(colors),
        ),
        SurfaceSidedness::Front | SurfaceSidedness::Both => (verts, uv_words, colors),
    };
    let split_triangles = split_triangles_runtime(split);
    let [(a, b, c), (d, e, f)] = split_triangles;
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
    let uvs = animated_material_uvs(material, options, uvs);
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
    let uvs = animated_material_uvs(material, options, uvs);
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

fn animated_material_uvs(
    material: WorldRenderMaterial,
    options: WorldSurfaceOptions,
    uvs: [(u8, u8); 4],
) -> [(u8, u8); 4] {
    let uvs = material_uvs(material, uvs);
    let (offset_u, offset_v) = material.animation.uv_offset(
        options.material_animation_tick,
        options.material_animation_hz,
        material.texture_width,
        material.texture_height,
    );
    if offset_u == 0 && offset_v == 0 {
        return uvs;
    }
    uvs.map(|(u, v)| (u.wrapping_add(offset_u), v.wrapping_add(offset_v)))
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

/// Corner order for one hardware quad packet.
///
/// GP0's quad rasterizes tri(q0,q1,q2) + tri(q1,q2,q3), so this puts the
/// authored split diagonal on the shared q1-q2 edge.
///
/// The order deliberately does NOT depend on `SurfaceSidedness`. Reversing the
/// four corners for a `Back` face gives the same two triangles with reversed
/// winding, but it also SWAPS WHICH ONE IS SUBMITTED FIRST, and
/// `submit_textured_gouraud_quad_prescreened_uv_words_prepared_depth` falls
/// back to emitting them as separate triangles that compute their own depth
/// whenever the quad needs splitting. On a wall seen at a grazing angle those
/// two depths straddle anything standing in front of it, so the swap flipped
/// the tie and the wall painted over the player. The winding is not needed
/// here: the software backface cull has already run and the GPU does not cull.
#[inline(always)]
fn quad_packet_order<T: Copy>(values: [T; 4], split: u8) -> [T; 4] {
    if split == SPLIT_NE_SW {
        [values[0], values[1], values[3], values[2]]
    } else {
        [values[1], values[0], values[2], values[3]]
    }
}

fn reverse_quad_winding<T: Copy>(corners: [T; 4]) -> [T; 4] {
    [corners[0], corners[3], corners[2], corners[1]]
}

#[cfg(test)]
mod tests;
