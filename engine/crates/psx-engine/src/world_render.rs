//! Surface material and lighting vocabulary shared by every world renderer.
//!
//! The grid room drawing helpers that used to live here are gone with the
//! grid world; what remains is the material, sample and lighting contract
//! the BSP renderer, props and actors shade through.

use psx_gpu::material::{TextureMaterial, TexturedGouraudPacketMaterial};

use crate::{RoomPoint, WorldVertex};

const ROOM_TEXTURE_UV_SIZE: u8 = 64;

const fn normalize_room_texture_uv_size(size: u8) -> u8 {
    if size == 0 || size > ROOM_TEXTURE_UV_SIZE {
        ROOM_TEXTURE_UV_SIZE
    } else {
        size
    }
}

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
                // speed_q8 (i16) * tick (u32) overflows i32 after ~18 minutes
                // of play, so the product is taken wide and only narrowed
                // after the wrap into byte UV space.
                // psx-numeric-allow-next-line: UV scroll accumulator, see above
                let hz = i64::from(hz.max(1));
                let resolve = |speed: i16, phase: u8, period: u8| {
                    // psx-numeric-allow-next-line: UV scroll accumulator
                    let travelled_q8 = i64::from(speed).saturating_mul(i64::from(tick)) / hz;
                    // psx-numeric-allow-next-line: UV scroll accumulator
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
