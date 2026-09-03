//! Quake's view-direction layered-sky projection.
//!
//! Sky brush polygons define only the aperture through which the sky is seen.
//! Their authored surface UVs must not make the sky appear attached to nearby
//! geometry. This module is kept renderer-neutral so PSoXide and Quake-PSX can
//! use one integer projection and one seam-safe packet UV policy.
//!
//! Ported from `quake-core/src/sky.rs` at Quake-PSX revision
//! `e32f6f66cff1759954f224846ce0b326c3d55d30` (GPL-2, same authorship).

use psx_math::int32::isqrt_i32;

use psx_engine::ClassicAffineSubmit;
use psx_gpu::material::TextureWindow;
use psx_gpu::prim::{ClassicTriTextured, QuadTextured};
use psx_gte::math::Mat3I16;

const MATERIAL_TICKS_PER_SECOND: u32 = 60;
const SKY_BACKGROUND_CYCLE_SECONDS: u32 = 16;
const SKY_FOREGROUND_CYCLE_SECONDS: u32 = 8;
const SKY_COLUMNS: usize = 10;
const SKY_ROWS: usize = 12;
const SKY_CELLS: usize = SKY_COLUMNS * SKY_ROWS;
const SKY_OT_SLOT: u32 = 2047;
const SKY_QUAD_WORDS: usize = 10;
const SKY_TRI_WORDS: usize = 8;
const SKY_WINDOW_PACKET_WORDS: usize = 2;
const SKY_WINDOW_PACKET_COUNT: usize = 3;
const CUBE_SKY_WINDOW_PACKET_COUNT: usize = 1;
const CUBE_SKY_COLUMNS: usize = 12;
const CUBE_SKY_ROWS: usize = 9;
const CUBE_SKY_PACKET_BUDGET_WORDS: usize = 2304;
pub const CUBE_SKY_FACE_WIDTH: u16 = 256;
pub const CUBE_SKY_FACE_HEIGHT: u16 = 256;
pub const CUBE_SKY_ATLAS_SIZE: [u16; 2] = [CUBE_SKY_FACE_WIDTH * 6, CUBE_SKY_FACE_HEIGHT];
pub const CUBE_SKY_FACE_COUNT: u16 = 6;
pub const CUBE_SKY_CLUT_ENTRIES_PER_FACE: u16 = 16;
pub const CUBE_SKY_CLUT_ENTRIES: u16 = CUBE_SKY_FACE_COUNT * CUBE_SKY_CLUT_ENTRIES_PER_FACE;

/// Packet words required by the constant-cost two-layer screen lattice.
pub const VIEW_RAY_SKY_PACKET_WORDS: usize =
    SKY_CELLS * 2 * SKY_QUAD_WORDS + SKY_WINDOW_PACKET_COUNT * SKY_WINDOW_PACKET_WORDS;

/// Packet words required by the single-pass directional cube environment.
pub const VIEW_RAY_CUBE_SKY_PACKET_WORDS: usize =
    CUBE_SKY_PACKET_BUDGET_WORDS + CUBE_SKY_WINDOW_PACKET_COUNT * SKY_WINDOW_PACKET_WORDS;

/// One face of a camera-relative cube environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CubeFace {
    PositiveX = 0,
    NegativeX = 1,
    PositiveY = 2,
    NegativeY = 3,
    PositiveZ = 4,
    NegativeZ = 5,
}

impl CubeFace {
    const ALL: [Self; 6] = [
        Self::PositiveX,
        Self::NegativeX,
        Self::PositiveY,
        Self::NegativeY,
        Self::PositiveZ,
        Self::NegativeZ,
    ];

    /// One full 4bpp page per 256x256 face.
    #[inline]
    pub const fn texture_page_offset(self) -> u16 {
        self as u16
    }

    /// Page-local vertical origin of this face.
    #[inline]
    pub const fn page_v_origin(self) -> u8 {
        0
    }
}

/// Rotation-keyed texel lattice of the layered sky (see
/// [`submit_view_ray_layered_sky_to_slot`]).
#[derive(Clone, Copy, PartialEq, Eq)]
struct LayeredSkySampleKey {
    rotation: [[i16; 3]; 3],
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    width: u8,
}

struct LayeredSkySampleCache {
    key: LayeredSkySampleKey,
    valid: bool,
    samples: [[[i32; 2]; SKY_COLUMNS + 1]; SKY_ROWS + 1],
}

static mut LAYERED_SKY_SAMPLE_CACHE: LayeredSkySampleCache = LayeredSkySampleCache {
    key: LayeredSkySampleKey {
        rotation: [[0; 3]; 3],
        screen_size: [0; 2],
        screen_center: [0; 2],
        projection: 0,
        width: 0,
    },
    valid: false,
    samples: [[[0; 2]; SKY_COLUMNS + 1]; SKY_ROWS + 1],
};

/// One staged GP0(E2) selector for a complete sky layer.
#[repr(C, align(4))]
struct SkyWindowPacket {
    tag: u32,
    command: u32,
}

impl SkyWindowPacket {
    const fn new(command: u32, ot_slot: u16) -> Self {
        Self {
            tag: (1 << 24) | ot_slot as u32,
            command,
        }
    }
}

const _: () = assert!(
    core::mem::size_of::<SkyWindowPacket>()
        == SKY_WINDOW_PACKET_WORDS * core::mem::size_of::<u32>()
);
const _: () =
    assert!(core::mem::size_of::<QuadTextured>() == SKY_QUAD_WORDS * core::mem::size_of::<u32>());

/// Return signed material-relative texel coordinates for a Quake sky ray.
///
/// Keeping this signed until a small raster cell is emitted is important.
/// Casting a whole dome corner to `u8` can cross the byte seam and make the
/// PS1 interpolate through most of the texture between adjacent vertices.
pub fn directional_texel(mut direction: [i32; 3], layer_width: u8) -> [i32; 2] {
    direction[2] = direction[2].saturating_mul(3);

    // Keep the squared length inside i32 without changing the direction.
    while direction[0]
        .unsigned_abs()
        .max(direction[1].unsigned_abs())
        .max(direction[2].unsigned_abs())
        > 16_000
    {
        direction[0] >>= 1;
        direction[1] >>= 1;
        direction[2] >>= 1;
    }

    let length_squared = direction[0]
        .saturating_mul(direction[0])
        .saturating_add(direction[1].saturating_mul(direction[1]))
        .saturating_add(direction[2].saturating_mul(direction[2]));
    let length = isqrt_i32(length_squared).max(1);
    let denominator = length * 128;
    let project = |component: i32| {
        // Original Quake uses `6 * 63 / length` against a 128-texel layer.
        // Preserve that projection while scaling it to the selected sky mip.
        let numerator = component * 378 * i32::from(layer_width);
        numerator / denominator
    };

    [project(direction[0]), project(direction[1])]
}

/// Recover a world-space viewing ray from one screen coordinate.
///
/// `world_to_view_q12` is the rotation already loaded for the frame. Its
/// transpose is sufficient because Quake's coordinate conversion has a
/// uniform scale, which disappears during sky normalisation.
pub fn screen_view_ray(
    screen: [i16; 2],
    center: [i16; 2],
    projection: i16,
    world_to_view_q12: [[i16; 3]; 3],
) -> [i32; 3] {
    let camera = [
        (i32::from(screen[0]) - i32::from(center[0])).clamp(-2048, 2048),
        (i32::from(screen[1]) - i32::from(center[1])).clamp(-2048, 2048),
        i32::from(projection).clamp(1, 2048),
    ];
    let mut world = [0i32; 3];
    for axis in 0..3 {
        let value = camera[0] * i32::from(world_to_view_q12[0][axis])
            + camera[1] * i32::from(world_to_view_q12[1][axis])
            + camera[2] * i32::from(world_to_view_q12[2][axis]);
        world[axis] = value >> 12;
    }
    world
}

/// Select the cube face whose plane is most perpendicular to `direction`.
pub fn cube_face(direction: [i32; 3]) -> CubeFace {
    let ax = direction[0].unsigned_abs();
    let ay = direction[1].unsigned_abs();
    let az = direction[2].unsigned_abs();
    if ax >= ay && ax >= az {
        if direction[0] >= 0 {
            CubeFace::PositiveX
        } else {
            CubeFace::NegativeX
        }
    } else if ay >= az {
        if direction[1] >= 0 {
            CubeFace::PositiveY
        } else {
            CubeFace::NegativeY
        }
    } else if direction[2] >= 0 {
        CubeFace::PositiveZ
    } else {
        CubeFace::NegativeZ
    }
}

/// Project a direction onto one selected cube face in signed Q12 UV space.
///
/// Cells select a face from their centre ray and project all four corners to
/// that same plane. This keeps affine interpolation local at face boundaries.
pub fn cube_face_uv_q12(direction: [i32; 3], face: CubeFace) -> [i32; 2] {
    let (u, v, denominator) = match face {
        CubeFace::PositiveX => (-direction[2], -direction[1], direction[0]),
        CubeFace::NegativeX => (direction[2], -direction[1], -direction[0]),
        CubeFace::PositiveY => (direction[0], direction[2], direction[1]),
        CubeFace::NegativeY => (direction[0], -direction[2], -direction[1]),
        CubeFace::PositiveZ => (direction[0], -direction[1], direction[2]),
        CubeFace::NegativeZ => (-direction[0], -direction[1], -direction[2]),
    };
    let denominator = denominator.unsigned_abs().max(1) as i32;
    [
        u.saturating_mul(4096) / denominator,
        v.saturating_mul(4096) / denominator,
    ]
}

/// Convert signed face-local Q12 coordinates to the source atlas.
///
/// The source is 1536x256: six adjacent full 256x256 4bpp pages. Runtime
/// packet UVs remain page-local and select the face's page separately.
pub fn cube_atlas_uv(direction: [i32; 3], face: CubeFace) -> [u16; 2] {
    let local = cube_face_uv_q12(direction, face);
    let u = ((local[0].saturating_add(4096) * i32::from(CUBE_SKY_FACE_WIDTH - 1)) >> 13)
        .clamp(0, i32::from(CUBE_SKY_FACE_WIDTH - 1)) as u16;
    let v = ((local[1].saturating_add(4096) * i32::from(CUBE_SKY_FACE_HEIGHT - 1)) >> 13)
        .clamp(0, i32::from(CUBE_SKY_FACE_HEIGHT - 1)) as u16;
    [
        face.texture_page_offset() * CUBE_SKY_FACE_WIDTH + u,
        u16::from(face.page_v_origin()) + v,
    ]
}

#[derive(Clone, Copy, Default)]
struct CubeSkyVertex {
    screen_q12: [i32; 2],
    ray: [i32; 3],
}

/// The four edge-plane distances of one cube face for a ray, in plane order.
/// Each face is its signed major axis against the two others; the four
/// sums are exactly the per-plane table in [`cube_face_plane_distance`].
#[inline(always)]
const fn cube_face_plane_distances(ray: [i32; 3], face: CubeFace) -> [i32; 4] {
    let [x, y, z] = ray;
    let (major, a, b) = match face {
        CubeFace::PositiveX => (x, y, z),
        CubeFace::NegativeX => (-x, y, z),
        CubeFace::PositiveY => (y, x, z),
        CubeFace::NegativeY => (-y, x, z),
        CubeFace::PositiveZ => (z, x, y),
        CubeFace::NegativeZ => (-z, x, y),
    };
    [major - a, major + a, major - b, major + b]
}

#[inline]
const fn cube_face_plane_distance(ray: [i32; 3], face: CubeFace, plane: usize) -> i32 {
    let [x, y, z] = ray;
    match (face, plane) {
        (CubeFace::PositiveX, 0) => x - y,
        (CubeFace::PositiveX, 1) => x + y,
        (CubeFace::PositiveX, 2) => x - z,
        (CubeFace::PositiveX, _) => x + z,
        (CubeFace::NegativeX, 0) => -x - y,
        (CubeFace::NegativeX, 1) => -x + y,
        (CubeFace::NegativeX, 2) => -x - z,
        (CubeFace::NegativeX, _) => -x + z,
        (CubeFace::PositiveY, 0) => y - x,
        (CubeFace::PositiveY, 1) => y + x,
        (CubeFace::PositiveY, 2) => y - z,
        (CubeFace::PositiveY, _) => y + z,
        (CubeFace::NegativeY, 0) => -y - x,
        (CubeFace::NegativeY, 1) => -y + x,
        (CubeFace::NegativeY, 2) => -y - z,
        (CubeFace::NegativeY, _) => -y + z,
        (CubeFace::PositiveZ, 0) => z - x,
        (CubeFace::PositiveZ, 1) => z + x,
        (CubeFace::PositiveZ, 2) => z - y,
        (CubeFace::PositiveZ, _) => z + y,
        (CubeFace::NegativeZ, 0) => -z - x,
        (CubeFace::NegativeZ, 1) => -z + x,
        (CubeFace::NegativeZ, 2) => -z - y,
        (CubeFace::NegativeZ, _) => -z + y,
    }
}

#[inline]
fn interpolate_cube_sky_vertex(
    from: CubeSkyVertex,
    to: CubeSkyVertex,
    from_distance: i32,
    to_distance: i32,
) -> CubeSkyVertex {
    let denominator = from_distance - to_distance;
    let t_q12 = if denominator == 0 {
        0
    } else {
        (from_distance << 12) / denominator
    }
    .clamp(0, 4096);
    let interpolate = |from: i32, to: i32| from + ((to - from) * t_q12 >> 12);
    CubeSkyVertex {
        screen_q12: [
            interpolate(from.screen_q12[0], to.screen_q12[0]),
            interpolate(from.screen_q12[1], to.screen_q12[1]),
        ],
        ray: [
            interpolate(from.ray[0], to.ray[0]),
            interpolate(from.ray[1], to.ray[1]),
            interpolate(from.ray[2], to.ray[2]),
        ],
    }
}

/// Index of the `face_a` edge plane that separates it from the adjacent
/// `face_b`, or `None` when the two faces are opposite (no shared edge).
///
/// `face_b`'s axis direction lies outside exactly one of `face_a`'s four
/// planes when the faces share an edge, and outside all four when they are
/// opposite.
#[inline(always)]
fn cube_face_shared_edge_plane(face_a: CubeFace, face_b: CubeFace) -> Option<usize> {
    let axis = match face_b {
        CubeFace::PositiveX => [4096, 0, 0],
        CubeFace::NegativeX => [-4096, 0, 0],
        CubeFace::PositiveY => [0, 4096, 0],
        CubeFace::NegativeY => [0, -4096, 0],
        CubeFace::PositiveZ => [0, 0, 4096],
        CubeFace::NegativeZ => [0, 0, -4096],
    };
    let distances = cube_face_plane_distances(axis, face_a);
    let mut plane = None;
    let mut index = 0usize;
    while index < 4 {
        if distances[index] < 0 {
            if plane.is_some() {
                return None;
            }
            plane = Some(index);
        }
        index += 1;
    }
    plane
}

/// Split one lattice cell whose corners lie on exactly two adjacent cube
/// faces along the edge they share, in a single pass.
///
/// The six-face clipper reaches the same two polygons by clipping the cell
/// against each face's four planes in turn, of which only the shared edge
/// plane ever cuts: a convex cell whose vertices all sit inside a face is
/// inside that face. The one plane clipped here is that edge plane, with
/// the same distance function and the same interpolation, so the vertices
/// are the ones the six-face path produces. When an edge point classifies
/// to a third face the cell reaches past the shared edge (a cube corner is
/// near) and the caller falls back to the full clipper; the split is exact
/// whenever it returns `Some`.
///
/// Corner samples reuse the texel each corner already resolved on its own
/// face; only the two edge points project afresh, once per side.
#[inline(always)]
fn split_cube_sky_cell_on_edge(
    corners: &[CubeSkyGridVertex; 4],
    face_a: CubeFace,
    face_b: CubeFace,
    samples_a: &mut [((i16, i16), u16); 6],
    samples_b: &mut [((i16, i16), u16); 6],
) -> Option<(usize, usize)> {
    let plane = cube_face_shared_edge_plane(face_a, face_b)?;
    let distances = [
        cube_face_plane_distance(corners[0].vertex.ray, face_a, plane),
        cube_face_plane_distance(corners[1].vertex.ray, face_a, plane),
        cube_face_plane_distance(corners[2].vertex.ray, face_a, plane),
        cube_face_plane_distance(corners[3].vertex.ray, face_a, plane),
    ];
    // A corner classified to the other face by an exact tie (`cube_face`
    // breaks ties toward X, then Y) can still land on this side of the
    // plane; it projects afresh onto the side's face like an edge point.
    let corner_sample = |corner: CubeSkyGridVertex, face: CubeFace| {
        if corner.face != face {
            return cube_sky_packet_vertex(corner.vertex, face);
        }
        let screen = (
            ((corner.vertex.screen_q12[0] + 2048) >> 12).clamp(0, i32::from(i16::MAX)) as i16,
            ((corner.vertex.screen_q12[1] + 2048) >> 12).clamp(0, i32::from(i16::MAX)) as i16,
        );
        (screen, corner.uv)
    };
    let mut count_a = 0usize;
    let mut count_b = 0usize;
    let mut previous = corners[3];
    let mut previous_distance = distances[3];
    let mut index = 0usize;
    while index < 4 {
        let current = corners[index];
        let current_distance = distances[index];
        if (current_distance >= 0) != (previous_distance >= 0) {
            let edge = interpolate_cube_sky_vertex(
                previous.vertex,
                current.vertex,
                previous_distance,
                current_distance,
            );
            let edge_face = cube_face(edge.ray);
            if edge_face != face_a && edge_face != face_b {
                return None;
            }
            samples_a[count_a] = cube_sky_packet_vertex(edge, face_a);
            count_a += 1;
            samples_b[count_b] = cube_sky_packet_vertex(edge, face_b);
            count_b += 1;
        }
        if current_distance >= 0 {
            samples_a[count_a] = corner_sample(current, face_a);
            count_a += 1;
        } else {
            samples_b[count_b] = corner_sample(current, face_b);
            count_b += 1;
        }
        previous = current;
        previous_distance = current_distance;
        index += 1;
    }
    Some((count_a, count_b))
}

/// Clip one lattice cell to `face`. The polygon lands in `polygon` and its
/// vertex count is returned; `scratch` is the other Sutherland-Hodgman
/// buffer. Both are the caller's, sized once per frame: zeroing two 160-byte
/// arrays, copying the cell in and copying every pass back cost the mixed
/// cells 5% of a Cortex frame in `memset` and `memcpy`, for six faces per
/// cell of which a cell can touch at most three. A face every corner lies
/// outside of on one plane is rejected before any buffer is written, and
/// the four passes alternate between the two buffers so the result is in
/// `polygon` without a copy. The arithmetic per vertex is unchanged.
#[inline(always)]
fn clip_cube_sky_cell(
    cell: &[CubeSkyVertex; 4],
    face: CubeFace,
    polygon: &mut [CubeSkyVertex; 8],
    scratch: &mut [CubeSkyVertex; 8],
) -> usize {
    let corner_distances = [
        cube_face_plane_distances(cell[0].ray, face),
        cube_face_plane_distances(cell[1].ray, face),
        cube_face_plane_distances(cell[2].ray, face),
        cube_face_plane_distances(cell[3].ray, face),
    ];
    let mut plane = 0usize;
    while plane < 4 {
        if corner_distances
            .iter()
            .all(|distances| distances[plane] < 0)
        {
            return 0;
        }
        plane += 1;
    }

    #[inline(always)]
    fn clip_pass(
        input: &[CubeSkyVertex],
        input_distances: impl Fn(usize) -> i32,
        output: &mut [CubeSkyVertex; 8],
    ) -> usize {
        let input_count = input.len();
        let mut output_count = 0usize;
        let mut previous = input[input_count - 1];
        let mut previous_distance = input_distances(input_count - 1);
        let mut previous_inside = previous_distance >= 0;
        for (index, current) in input.iter().copied().enumerate() {
            let current_distance = input_distances(index);
            let current_inside = current_distance >= 0;
            if current_inside != previous_inside {
                debug_assert!(output_count < output.len());
                output[output_count] = interpolate_cube_sky_vertex(
                    previous,
                    current,
                    previous_distance,
                    current_distance,
                );
                output_count += 1;
            }
            if current_inside {
                debug_assert!(output_count < output.len());
                output[output_count] = current;
                output_count += 1;
            }
            previous = current;
            previous_distance = current_distance;
            previous_inside = current_inside;
        }
        output_count
    }

    // Pass 0 reads the cell and its precomputed distances; passes 1..4
    // alternate scratch -> polygon -> scratch -> polygon.
    let count = clip_pass(&cell[..], |index| corner_distances[index][0], scratch);
    if count == 0 {
        return 0;
    }
    let count = clip_pass(
        &scratch[..count],
        |index| cube_face_plane_distance(scratch[index].ray, face, 1),
        polygon,
    );
    if count == 0 {
        return 0;
    }
    let count = clip_pass(
        &polygon[..count],
        |index| cube_face_plane_distance(polygon[index].ray, face, 2),
        scratch,
    );
    if count == 0 {
        return 0;
    }
    clip_pass(
        &scratch[..count],
        |index| cube_face_plane_distance(scratch[index].ray, face, 3),
        polygon,
    )
}

#[inline]
fn cube_sky_packet_vertex(vertex: CubeSkyVertex, face: CubeFace) -> ((i16, i16), u16) {
    let screen = (
        ((vertex.screen_q12[0] + 2048) >> 12).clamp(0, i32::from(i16::MAX)) as i16,
        ((vertex.screen_q12[1] + 2048) >> 12).clamp(0, i32::from(i16::MAX)) as i16,
    );
    // Packet UVs are page-local, so the atlas origin `cube_atlas_uv` adds is
    // taken straight back out by the caller's `% 256`. Both halves are exact
    // no-ops at this width -- `texture_page_offset` scales by the 256-wide
    // face and `page_v_origin` is 0 -- so read the face projection directly
    // and skip the round trip. `page_local_uv_matches_the_atlas_round_trip`
    // pins the two forms together.
    let [u, v] = cube_face_page_local_uv(vertex.ray, face);
    (screen, u16::from(u) | (u16::from(v) << 8))
}

/// Page-local 8-bit texel for a ray on one cube face.
///
/// This is [`cube_atlas_uv`] with the atlas origin left off: the clamp already
/// bounds both axes to `0..=255`, which is exactly the range the packet's `u8`
/// fields hold.
#[inline]
fn cube_face_page_local_uv(direction: [i32; 3], face: CubeFace) -> [u8; 2] {
    let local = cube_face_uv_q12(direction, face);
    [
        ((local[0].saturating_add(4096) * i32::from(CUBE_SKY_FACE_WIDTH - 1)) >> 13)
            .clamp(0, i32::from(CUBE_SKY_FACE_WIDTH - 1)) as u8,
        ((local[1].saturating_add(4096) * i32::from(CUBE_SKY_FACE_HEIGHT - 1)) >> 13)
            .clamp(0, i32::from(CUBE_SKY_FACE_HEIGHT - 1)) as u8,
    ]
}

/// One grid corner of the sky lattice, with the two per-corner results the
/// cell loop would otherwise recompute.
///
/// Every interior corner is shared by four cells, and the row carry already
/// keeps each corner alive for two rows, so `cube_face` and
/// `cube_sky_packet_vertex` used to run 432 times a frame for 130 distinct
/// corners. Resolving both at the corner makes that 130. A cell whose four
/// corners agree on a face wants exactly the sample each corner already holds
/// -- the uniform branch projects every corner onto its own face -- so the
/// cached value is the same one the old code computed.
#[derive(Clone, Copy)]
struct CubeSkyGridVertex {
    vertex: CubeSkyVertex,
    face: CubeFace,
    uv: u16,
}

impl Default for CubeSkyGridVertex {
    fn default() -> Self {
        Self {
            vertex: CubeSkyVertex::default(),
            face: CubeFace::PositiveX,
            uv: 0,
        }
    }
}

/// Select the contiguous 16-colour CLUT allocated for one cube face.
#[inline]
pub const fn cube_face_clut(base_clut: u16, face: CubeFace) -> u16 {
    base_clut.wrapping_add(face as u16)
}

/// Convert a PSoXide Y-up world vector back to Quake's Z-up sky basis.
///
/// The brush importer maps `(quake_x, quake_y, quake_z)` to
/// `(world_x, world_y, world_z) = (quake_x, quake_z, -quake_y)`.
#[inline]
pub const fn quake_direction_from_y_up(direction: [i32; 3]) -> [i32; 3] {
    [direction[0], -direction[2], direction[1]]
}

/// Rebase four signed sky samples into packet UV bytes without changing the
/// local projection gradient.
///
/// The texture window repeats every `period` texels, but that does not make
/// the shortest wrapped delta the correct affine gradient. Keep the original
/// signed deltas and translate the whole packet by complete periods until all
/// four coordinates fit in GP0's byte UVs.
pub fn packet_quad_uv(
    samples: [[i32; 2]; 4],
    atlas: [u8; 2],
    period: [u8; 2],
    scroll: [u8; 2],
) -> [[u8; 2]; 4] {
    let mut output = [[0u8; 2]; 4];
    for axis in 0..2 {
        let period = i32::from(period[axis]).max(1);
        let scroll = i32::from(scroll[axis]);
        let sample_anchor = samples[0][axis];
        let anchor = sample_anchor + i32::from(atlas[axis]) + scroll;
        let mut values = [0i32; 4];
        values[0] = anchor;
        for index in 1..4 {
            values[index] = anchor + samples[index][axis] - sample_anchor;
        }

        let mut minimum = values[0];
        let mut maximum = values[0];
        for value in &values[1..] {
            minimum = minimum.min(*value);
            maximum = maximum.max(*value);
        }
        while minimum < 0 {
            for value in &mut values {
                *value += period;
            }
            minimum += period;
            maximum += period;
        }
        while maximum > 255 {
            for value in &mut values {
                *value -= period;
            }
            minimum -= period;
            maximum -= period;
        }
        debug_assert!(minimum >= 0 && maximum <= 255);
        for index in 0..4 {
            output[index][axis] = values[index] as u8;
        }
    }
    output
}

/// Return material-relative UV bytes for a layered Quake sky vertex.
///
/// This helper is retained for parity tests and tools. The runtime background
/// uses [`screen_view_ray`] so packet cost does not grow with sky-face count.
pub fn directional_uv(
    vertex_units: [i16; 3],
    camera_origin_q12: [i32; 3],
    layer_width: u8,
) -> [u8; 2] {
    let direction = [
        i32::from(vertex_units[0]).saturating_sub(camera_origin_q12[0] >> 12),
        i32::from(vertex_units[1]).saturating_sub(camera_origin_q12[1] >> 12),
        i32::from(vertex_units[2]).saturating_sub(camera_origin_q12[2] >> 12),
    ];
    let projected = directional_texel(direction, layer_width);
    [projected[0] as u8, projected[1] as u8]
}

/// Draw Quake's two sky layers as a bounded view-ray background.
///
/// Visible sky brushes select this material but emit no geometry. The caller
/// appends the returned tagged packets after world geometry so prepend-only OT
/// insertion executes the sky first, behind opaque world surfaces.
///
/// `layer_size` describes one of the two adjacent square atlas halves.
/// `screen_size` and `screen_center` use framebuffer pixel coordinates.
///
/// # Safety
///
/// `output` must have room for [`VIEW_RAY_SKY_PACKET_WORDS`] writable `u32`
/// values and must be aligned for the packet structs written here.
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_view_ray_layered_sky(
    texture_page: u16,
    clut: u16,
    atlas_origin: [u8; 2],
    layer_size: [u8; 2],
    view_rotation: Mat3I16,
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    material_tick: u32,
    output: *mut u32,
) -> ClassicAffineSubmit {
    unsafe {
        submit_view_ray_layered_sky_to_slot(
            texture_page,
            clut,
            atlas_origin,
            layer_size,
            view_rotation,
            screen_size,
            screen_center,
            projection,
            material_tick,
            SKY_OT_SLOT as u16,
            output,
        )
    }
}

/// Draw Quake's layered sky into a caller-selected ordering-table slot.
///
/// This is the adapter used by host editor previews whose ordering table is
/// deeper than the 2048-slot hardware/runtime table. The projection and packet
/// stream are otherwise byte-for-byte identical to
/// [`submit_view_ray_layered_sky`].
///
/// # Safety
///
/// Same output storage contract as [`submit_view_ray_layered_sky`].
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_view_ray_layered_sky_to_slot(
    texture_page: u16,
    clut: u16,
    atlas_origin: [u8; 2],
    layer_size: [u8; 2],
    view_rotation: Mat3I16,
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    material_tick: u32,
    ot_slot: u16,
    output: *mut u32,
) -> ClassicAffineSubmit {
    let width = layer_size[0].clamp(8, 128);
    let height = layer_size[1].clamp(8, 128);
    debug_assert!(width.is_power_of_two());
    debug_assert!(height.is_power_of_two());
    debug_assert!(atlas_origin[0].is_multiple_of(width));
    debug_assert!(atlas_origin[1].is_multiple_of(height));
    debug_assert!(u16::from(atlas_origin[0]) + u16::from(width) * 2 <= 256);
    debug_assert!(u16::from(atlas_origin[1]) + u16::from(height) <= 256);
    let foreground_window =
        TextureWindow::power_of_two_tile(atlas_origin[0], atlas_origin[1], width, height);
    let background_origin = [atlas_origin[0].wrapping_add(width), atlas_origin[1]];
    let background_window =
        TextureWindow::power_of_two_tile(background_origin[0], background_origin[1], width, height);
    // `tick * width / period` modulo 256, without the 64-bit product: split
    // the tick into whole periods and a remainder; only the low byte of the
    // quotient is kept, so the whole-period term may wrap freely.
    let scroll = |cycle_seconds: u32| {
        let period = (MATERIAL_TICKS_PER_SECOND * cycle_seconds).max(1);
        let whole = (material_tick / period).wrapping_mul(u32::from(width));
        let part = (material_tick % period) * u32::from(width) / period;
        (whole.wrapping_add(part) & 0xff) as u8
    };
    let foreground_scroll = [
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
        scroll(SKY_FOREGROUND_CYCLE_SECONDS),
    ];
    let background_scroll = [
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
        scroll(SKY_BACKGROUND_CYCLE_SECONDS),
    ];

    let screen_width = screen_size[0].max(1);
    let screen_height = screen_size[1].max(1);
    // The texel lattice depends on the view rotation and the screen
    // geometry alone; the scroll is added per cell below. A frame that keeps
    // the previous rotation reuses the lattice instead of paying a square
    // root and two divides per corner (143 corners at the default lattice).
    // The guest is single-threaded and the host tests run this kernel from
    // one thread at a time, so a plain static carries the cache.
    let key = LayeredSkySampleKey {
        rotation: view_rotation.m,
        screen_size: [screen_width, screen_height],
        screen_center,
        projection,
        width,
    };
    // SAFETY: single-threaded access, see above.
    let cache = unsafe { &mut *core::ptr::addr_of_mut!(LAYERED_SKY_SAMPLE_CACHE) };
    if !cache.valid || cache.key != key {
        for (row, sample_row) in cache.samples.iter_mut().enumerate() {
            let y = (row * screen_height as usize / SKY_ROWS) as i16;
            for (column, sample) in sample_row.iter_mut().enumerate() {
                let x = (column * screen_width as usize / SKY_COLUMNS) as i16;
                let world_ray =
                    screen_view_ray([x, y], screen_center, projection.max(1), view_rotation.m);
                *sample = directional_texel(quake_direction_from_y_up(world_ray), width);
            }
        }
        cache.key = key;
        cache.valid = true;
    }
    let samples = &cache.samples;

    let mut next = output;
    // The tagged stream is linked by prepending packets. Stage the reset first
    // so it executes after both sky layers and before ordinary world geometry.
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(TextureWindow::NONE.word(), ot_slot));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }

    let mut emit_layer = |atlas: [u8; 2], window: TextureWindow, scroll: [u8; 2]| {
        for row in 0..SKY_ROWS {
            let y0 = (row * screen_height as usize / SKY_ROWS) as i16;
            let y1 = ((row + 1) * screen_height as usize / SKY_ROWS) as i16;
            for column in 0..SKY_COLUMNS {
                let x0 = (column * screen_width as usize / SKY_COLUMNS) as i16;
                let x1 = ((column + 1) * screen_width as usize / SKY_COLUMNS) as i16;
                let vertices = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
                let cell_samples = [
                    samples[row][column],
                    samples[row][column + 1],
                    samples[row + 1][column],
                    samples[row + 1][column + 1],
                ];
                let uv = packet_quad_uv(cell_samples, atlas, [width, height], scroll)
                    .map(|[u, v]| (u, v));
                unsafe {
                    let mut quad =
                        QuadTextured::new(vertices, uv, clut, texture_page, (0x80, 0x80, 0x80));
                    quad.tag = ((QuadTextured::WORDS as u32) << 24) | u32::from(ot_slot);
                    next.cast::<QuadTextured>().write(quad);
                    next = next.add(SKY_QUAD_WORDS);
                }
            }
        }
        unsafe {
            next.cast::<SkyWindowPacket>()
                .write(SkyWindowPacket::new(window.word(), ot_slot));
            next = next.add(SKY_WINDOW_PACKET_WORDS);
        }
    };

    // Foreground is staged first, then background. Equal-slot OT prepending
    // reverses their execution so the opaque background draws first and the
    // masked foreground overlays it.
    emit_layer(atlas_origin, foreground_window, foreground_scroll);
    emit_layer(background_origin, background_window, background_scroll);

    ClassicAffineSubmit {
        next_packet: next,
        packets: (SKY_CELLS * 2 + SKY_WINDOW_PACKET_COUNT) as u32,
        hardware_triangles: (SKY_CELLS * 4) as u32,
    }
}

/// Draw a camera-centred six-face cube environment.
///
/// A screen lattice is clipped against the six exact cube-face dominance
/// regions (`+X >= abs(Y), abs(Z)`, and so on). Each resulting polygon uses
/// only one face and adjacent faces share the same clipped screen edge. This
/// avoids both a visible geometric cube and the old centre-ray approximation,
/// while keeping every emitted triangle inside a small affine screen cell.
///
/// # Safety
///
/// `output` must have room for [`VIEW_RAY_CUBE_SKY_PACKET_WORDS`] writable
/// `u32` values and be aligned for the packet structs written here.
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_view_ray_cube_sky(
    texture_page: u16,
    clut: u16,
    view_rotation: Mat3I16,
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    output: *mut u32,
) -> ClassicAffineSubmit {
    unsafe {
        submit_view_ray_cube_sky_to_slot(
            texture_page,
            clut,
            view_rotation,
            screen_size,
            screen_center,
            projection,
            SKY_OT_SLOT as u16,
            output,
        )
    }
}

/// Draw the directional cube environment into a caller-selected ordering-table
/// slot. Host previews use this to keep the exact runtime projection behind a
/// deeper editor geometry table.
///
/// # Safety
///
/// Same output storage contract as [`submit_view_ray_cube_sky`].
#[allow(clippy::too_many_arguments)]
pub unsafe fn submit_view_ray_cube_sky_to_slot(
    texture_page: u16,
    clut: u16,
    view_rotation: Mat3I16,
    screen_size: [i16; 2],
    screen_center: [i16; 2],
    projection: i16,
    ot_slot: u16,
    output: *mut u32,
) -> ClassicAffineSubmit {
    let screen_width = screen_size[0].max(1);
    let screen_height = screen_size[1].max(1);
    let mut next = output;
    let mut packets = 0u32;
    let mut hardware_triangles = 0u32;
    let mut packet_words = 0usize;
    // The lattice's screen positions do not depend on the view, so they are
    // resolved once instead of per corner. They are also exactly the screen
    // coordinates `cube_sky_packet_vertex` would return for a grid corner --
    // `((x << 12) + 2048) >> 12 == x` for the non-negative x a column can
    // produce -- so the uniform branch reads them straight from here and the
    // corner cache carries only the texel.
    let mut column_x = [0i16; CUBE_SKY_COLUMNS + 1];
    for (column, x) in column_x.iter_mut().enumerate() {
        *x = (column * screen_width as usize / CUBE_SKY_COLUMNS) as i16;
    }
    let mut row_y = [0i16; CUBE_SKY_ROWS + 1];
    for (row, y) in row_y.iter_mut().enumerate() {
        *y = (row * screen_height as usize / CUBE_SKY_ROWS) as i16;
    }
    // `screen_view_ray` is a 3x3 multiply against a point that only ever moves
    // along the lattice, and the three terms of that sum depend on the column,
    // the row and nothing at all respectively. Splitting them turns 9 multiplies
    // per corner (1,170 a frame) into 39 + 30 + 3 for the whole lattice, leaving
    // three adds and a shift per corner. The sum is formed left to right in the
    // same order and width as the original, so it is the same i32 result --
    // `the_separated_lattice_ray_matches_screen_view_ray` pins that.
    let rotation = view_rotation.m;
    let projection_term = {
        let z = i32::from(projection.max(1)).clamp(1, 2048);
        [
            z * i32::from(rotation[2][0]),
            z * i32::from(rotation[2][1]),
            z * i32::from(rotation[2][2]),
        ]
    };
    let mut column_term = [[0i32; 3]; CUBE_SKY_COLUMNS + 1];
    for (column, term) in column_term.iter_mut().enumerate() {
        let x = (i32::from(column_x[column]) - i32::from(screen_center[0])).clamp(-2048, 2048);
        *term = [
            x * i32::from(rotation[0][0]),
            x * i32::from(rotation[0][1]),
            x * i32::from(rotation[0][2]),
        ];
    }
    let mut row_term = [[0i32; 3]; CUBE_SKY_ROWS + 1];
    for (row, term) in row_term.iter_mut().enumerate() {
        let y = (i32::from(row_y[row]) - i32::from(screen_center[1])).clamp(-2048, 2048);
        *term = [
            y * i32::from(rotation[1][0]),
            y * i32::from(rotation[1][1]),
            y * i32::from(rotation[1][2]),
        ];
    }
    let vertex_at = |column: usize, row: usize| {
        let screen = [column_x[column], row_y[row]];
        let (cx, ry) = (column_term[column], row_term[row]);
        let vertex = CubeSkyVertex {
            screen_q12: [i32::from(screen[0]) << 12, i32::from(screen[1]) << 12],
            ray: [
                (cx[0] + ry[0] + projection_term[0]) >> 12,
                (cx[1] + ry[1] + projection_term[1]) >> 12,
                (cx[2] + ry[2] + projection_term[2]) >> 12,
            ],
        };
        let face = cube_face(vertex.ray);
        let [u, v] = cube_face_page_local_uv(vertex.ray, face);
        CubeSkyGridVertex {
            vertex,
            face,
            uv: u16::from(u) | (u16::from(v) << 8),
        }
    };
    // Two row buffers indexed by parity rather than one buffer plus a
    // `top = bottom` copy: the corner cache made each row 364 bytes, and
    // copying it twelve times a frame showed up as `memcpy`.
    let mut grid = [[CubeSkyGridVertex::default(); CUBE_SKY_COLUMNS + 1]; 2];
    let mut polygon = [CubeSkyVertex::default(); 8];
    let mut scratch = [CubeSkyVertex::default(); 8];
    let mut samples = [((0i16, 0i16), 0u16); 8];
    for (column, vertex) in grid[0].iter_mut().enumerate() {
        *vertex = vertex_at(column, 0);
    }
    'rows: for row in 0..CUBE_SKY_ROWS {
        let top_row = row & 1;
        let bottom_row = (row + 1) & 1;
        // Indexed rather than `iter_mut().enumerate()`: the iterator form is
        // tidier but links 880 more bytes of .text, which crosses a section
        // alignment boundary and costs 2 KiB of guest RAM for +0.02 fps.
        #[allow(clippy::needless_range_loop)]
        for column in 0..=CUBE_SKY_COLUMNS {
            grid[bottom_row][column] = vertex_at(column, row + 1);
        }
        for column in 0..CUBE_SKY_COLUMNS {
            let corners = [
                grid[top_row][column],
                grid[top_row][column + 1],
                grid[bottom_row][column + 1],
                grid[bottom_row][column],
            ];
            if corners[1..]
                .iter()
                .all(|corner| corner.face == corners[0].face)
            {
                if packet_words + SKY_QUAD_WORDS > CUBE_SKY_PACKET_BUDGET_WORDS {
                    debug_assert!(false, "directional sky packet envelope exhausted");
                    break 'rows;
                }
                let face = corners[0].face;
                let (x0, x1) = (column_x[column], column_x[column + 1]);
                let (y0, y1) = (row_y[row], row_y[row + 1]);
                let vertices = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
                let uv = [corners[0].uv, corners[1].uv, corners[3].uv, corners[2].uv]
                    .map(|texel| ((texel & 0xff) as u8, (texel >> 8) as u8));
                unsafe {
                    let mut quad = QuadTextured::new(
                        vertices,
                        uv,
                        cube_face_clut(clut, face),
                        texture_page.wrapping_add(face.texture_page_offset()),
                        (0x80, 0x80, 0x80),
                    );
                    quad.tag = ((QuadTextured::WORDS as u32) << 24) | u32::from(ot_slot);
                    next.cast::<QuadTextured>().write(quad);
                    next = next.add(SKY_QUAD_WORDS);
                }
                packet_words += SKY_QUAD_WORDS;
                packets = packets.wrapping_add(1);
                hardware_triangles = hardware_triangles.wrapping_add(2);
                continue;
            }
            // The common mixed cell straddles one cube edge: split it on that
            // edge in one pass and fan each side. Three-face cells (a cube
            // corner inside the cell) and cells the split cannot prove exact
            // take the six-face clipper below.
            let face_a = corners[0].face;
            let mut face_b = None;
            let mut third_face = false;
            for corner in &corners[1..] {
                if corner.face != face_a {
                    match face_b {
                        None => face_b = Some(corner.face),
                        Some(face) if face != corner.face => third_face = true,
                        Some(_) => {}
                    }
                }
            }
            if let (Some(face_b), false) = (face_b, third_face) {
                let mut samples_a = [((0i16, 0i16), 0u16); 6];
                let mut samples_b = [((0i16, 0i16), 0u16); 6];
                if let Some((count_a, count_b)) = split_cube_sky_cell_on_edge(
                    &corners,
                    face_a,
                    face_b,
                    &mut samples_a,
                    &mut samples_b,
                ) {
                    for (face, samples, count) in [
                        (face_a, &samples_a, count_a),
                        (face_b, &samples_b, count_b),
                    ] {
                        if count < 3 {
                            continue;
                        }
                        for index in 1..count - 1 {
                            if packet_words + SKY_TRI_WORDS > CUBE_SKY_PACKET_BUDGET_WORDS {
                                debug_assert!(false, "directional sky packet envelope exhausted");
                                break 'rows;
                            }
                            let vertices = [samples[0].0, samples[index].0, samples[index + 1].0];
                            let uv_words = [samples[0].1, samples[index].1, samples[index + 1].1];
                            unsafe {
                                next.cast::<ClassicTriTextured>().write(
                                    ClassicTriTextured::with_staged_slot(
                                        vertices,
                                        uv_words,
                                        0x0080_8080,
                                        cube_face_clut(clut, face),
                                        texture_page.wrapping_add(face.texture_page_offset()),
                                        ot_slot,
                                    ),
                                );
                                next = next.add(SKY_TRI_WORDS);
                            }
                            packet_words += SKY_TRI_WORDS;
                            packets = packets.wrapping_add(1);
                            hardware_triangles = hardware_triangles.wrapping_add(1);
                        }
                    }
                    continue;
                }
            }
            // Only the mixed branch needs the bare vertices, and it is the rare
            // one: materialising them for every cell cost 7% of the stage.
            let cell = corners.map(|corner| corner.vertex);
            for face in CubeFace::ALL {
                let count = clip_cube_sky_cell(&cell, face, &mut polygon, &mut scratch);
                if count < 3 {
                    continue;
                }
                for (sample, vertex) in samples[..count]
                    .iter_mut()
                    .zip(polygon[..count].iter().copied())
                {
                    *sample = cube_sky_packet_vertex(vertex, face);
                }
                for index in 1..count - 1 {
                    if packet_words + SKY_TRI_WORDS > CUBE_SKY_PACKET_BUDGET_WORDS {
                        debug_assert!(false, "directional sky packet envelope exhausted");
                        break 'rows;
                    }
                    let vertices = [samples[0].0, samples[index].0, samples[index + 1].0];
                    let uv_words = [samples[0].1, samples[index].1, samples[index + 1].1];
                    unsafe {
                        next.cast::<ClassicTriTextured>().write(
                            ClassicTriTextured::with_staged_slot(
                                vertices,
                                uv_words,
                                0x0080_8080,
                                cube_face_clut(clut, face),
                                texture_page.wrapping_add(face.texture_page_offset()),
                                ot_slot,
                            ),
                        );
                        next = next.add(SKY_TRI_WORDS);
                    }
                    packet_words += SKY_TRI_WORDS;
                    packets = packets.wrapping_add(1);
                    hardware_triangles = hardware_triangles.wrapping_add(1);
                }
            }
        }
    }
    // Linked packets are prepended to one OT slot, so the last staged packet
    // executes first and clears any texture window inherited from a prior draw.
    unsafe {
        next.cast::<SkyWindowPacket>()
            .write(SkyWindowPacket::new(TextureWindow::NONE.word(), ot_slot));
        next = next.add(SKY_WINDOW_PACKET_WORDS);
    }

    ClassicAffineSubmit {
        next_packet: next,
        packets: packets + CUBE_SKY_WINDOW_PACKET_COUNT as u32,
        hardware_triangles,
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::{
        cube_atlas_uv, cube_face, cube_face_clut, cube_face_uv_q12, directional_texel,
        directional_uv, packet_quad_uv, quake_direction_from_y_up, screen_view_ray,
        submit_view_ray_cube_sky, submit_view_ray_cube_sky_to_slot, submit_view_ray_layered_sky,
        CubeFace, CUBE_SKY_ATLAS_SIZE, VIEW_RAY_CUBE_SKY_PACKET_WORDS, VIEW_RAY_SKY_PACKET_WORDS,
    };
    use psx_gte::math::Mat3I16;

    #[test]
    fn edge_split_matches_the_six_face_clip_on_two_face_cells() {
        // For every lattice cell whose corners lie on exactly two faces and
        // whose split is accepted, each face's vertex set must equal what
        // the six-face clipper produces for that face, and no third face
        // may receive a polygon from the clipper.
        use super::{
            clip_cube_sky_cell, cube_face_page_local_uv, cube_sky_packet_vertex,
            split_cube_sky_cell_on_edge, CubeSkyGridVertex, CubeSkyVertex, CUBE_SKY_COLUMNS,
            CUBE_SKY_ROWS,
        };
        use alloc::vec::Vec;
        let (width, height, cx, cy, projection) = (320i16, 240i16, 160i16, 120i16, 320i16);
        let mut checked = 0usize;
        let mut fallbacks = 0usize;
        let mut exact = 0usize;
        let mut near = 0usize;
        for rx in (0..256u32).step_by(8) {
            for ry in (0..256u32).step_by(4) {
                let rotation = Mat3I16::rotate_xyz(rx as u16, ry as u16, 0).m;
                let corner = |column: usize, row: usize| {
                    let sx = (column * width as usize / CUBE_SKY_COLUMNS) as i16;
                    let sy = (row * height as usize / CUBE_SKY_ROWS) as i16;
                    let ray = screen_view_ray([sx, sy], [cx, cy], projection, rotation);
                    let vertex = CubeSkyVertex {
                        screen_q12: [i32::from(sx) << 12, i32::from(sy) << 12],
                        ray,
                    };
                    let face = cube_face(ray);
                    let [u, v] = cube_face_page_local_uv(ray, face);
                    CubeSkyGridVertex {
                        vertex,
                        face,
                        uv: u16::from(u) | (u16::from(v) << 8),
                    }
                };
                for row in 0..CUBE_SKY_ROWS {
                    for column in 0..CUBE_SKY_COLUMNS {
                        let corners = [
                            corner(column, row),
                            corner(column + 1, row),
                            corner(column + 1, row + 1),
                            corner(column, row + 1),
                        ];
                        let face_a = corners[0].face;
                        let mut face_b = None;
                        let mut third = false;
                        for c in &corners[1..] {
                            if c.face != face_a {
                                match face_b {
                                    None => face_b = Some(c.face),
                                    Some(f) if f != c.face => third = true,
                                    Some(_) => {}
                                }
                            }
                        }
                        let Some(face_b) = face_b else { continue };
                        if third {
                            continue;
                        }
                        let mut samples_a = [((0i16, 0i16), 0u16); 6];
                        let mut samples_b = [((0i16, 0i16), 0u16); 6];
                        let Some((count_a, count_b)) = split_cube_sky_cell_on_edge(
                            &corners,
                            face_a,
                            face_b,
                            &mut samples_a,
                            &mut samples_b,
                        ) else {
                            fallbacks += 1;
                            continue;
                        };
                        let cell = corners.map(|c| c.vertex);
                        let mut polygon = [CubeSkyVertex::default(); 8];
                        let mut scratch = [CubeSkyVertex::default(); 8];
                        // A polygon of zero screen area rasterizes nothing, so
                        // either side may drop it: a cell edge lying exactly on
                        // the cube edge plane gives the clipper a collinear sliver.
                        let area = |points: &[((i16, i16), u16)]| -> i64 {
                            let n = points.len();
                            (0..n)
                                .map(|i| {
                                    let (a, b) = (points[i].0, points[(i + 1) % n].0);
                                    i64::from(a.0) * i64::from(b.1) - i64::from(b.0) * i64::from(a.1)
                                })
                                .sum()
                        };
                        // Squared distance from a point to a polygon's
                        // boundary is at most four pixels.
                        let within = |point: (i16, i16), polygon: &[((i16, i16), u16)]| {
                            let n = polygon.len();
                            (0..n).any(|i| {
                                let (a, b) = (polygon[i].0, polygon[(i + 1) % n].0);
                                let (px, py) = (f64::from(point.0), f64::from(point.1));
                                let (ax, ay) = (f64::from(a.0), f64::from(a.1));
                                let (bx, by) = (f64::from(b.0), f64::from(b.1));
                                let (dx, dy) = (bx - ax, by - ay);
                                let len2 = dx * dx + dy * dy;
                                let t = if len2 == 0.0 {
                                    0.0
                                } else {
                                    (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
                                };
                                let (cx, cy) = (ax + t * dx - px, ay + t * dy - py);
                                cx * cx + cy * cy <= 4.0
                            })
                        };
                        let got_a: Vec<_> = samples_a[..count_a].to_vec();
                        let got_b: Vec<_> = samples_b[..count_b].to_vec();
                        for (face, samples, count) in
                            [(face_a, &samples_a, count_a), (face_b, &samples_b, count_b)]
                        {
                            let n = clip_cube_sky_cell(&cell, face, &mut polygon, &mut scratch);
                            let mut expect: Vec<_> = polygon[..n]
                                .iter()
                                .map(|v| cube_sky_packet_vertex(*v, face))
                                .collect();
                            let mut got: Vec<_> = samples[..count].to_vec();
                            let (expect_area, got_area) = (area(&expect), area(&got));
                            if expect_area == 0 {
                                expect.clear();
                            }
                            if got_area == 0 {
                                got.clear();
                            }
                            // The clipper repeats a vertex that sits exactly
                            // on the plane (its interpolation lands on the
                            // vertex itself); compare as vertex sets.
                            expect.sort();
                            expect.dedup();
                            got.sort();
                            got.dedup();
                            if got == expect {
                                exact += 1;
                                continue;
                            }
                            // The clipper interpolates rays along already
                            // rounded polygon edges, so after two cuts its
                            // vertices drift by about a pixel; a corner cut
                            // then shows up as a pixel-wide wedge along an
                            // edge. The split cuts the exact corners once.
                            // Accept when every vertex of either polygon is
                            // within two pixels of the other's boundary.
                            let expect_order: Vec<_> = polygon[..n]
                                .iter()
                                .map(|v| cube_sky_packet_vertex(*v, face))
                                .collect();
                            let got_order: Vec<_> = samples[..count].to_vec();
                            assert!(
                                got_order.iter().all(|g| within(g.0, &expect_order))
                                    && expect_order.iter().all(|e| within(e.0, &got_order)),
                                "rx {rx} ry {ry} cell {column},{row} face {face:?}: {got:?} ({got_area}) vs {expect:?} ({expect_area})"
                            );
                            near += 1;
                        }
                        for face in CubeFace::ALL {
                            if face == face_a || face == face_b {
                                continue;
                            }
                            let n = clip_cube_sky_cell(&cell, face, &mut polygon, &mut scratch);
                            let drawn: Vec<_> = polygon[..n]
                                .iter()
                                .map(|v| cube_sky_packet_vertex(*v, face))
                                .collect();
                            // The same drift can hand the clipper a pixel-wide
                            // wedge on a third face along one of the split's
                            // edges; anything further from them is a real miss.
                            assert!(
                                n < 3
                                    || area(&drawn) == 0
                                    || drawn
                                        .iter()
                                        .all(|v| within(v.0, &got_a) || within(v.0, &got_b)),
                                "rx {rx} ry {ry} cell {column},{row}: six-face clip drew {face:?} {drawn:?}"
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1000, "checked {checked}");
        assert!(fallbacks < checked / 20, "fallbacks {fallbacks} of {checked}");
        assert!(near < exact / 20, "near {near} exact {exact} fallbacks {fallbacks}");
    }

    #[test]
    fn the_separated_lattice_ray_matches_screen_view_ray() {
        // The cube sky no longer calls `screen_view_ray` per corner; it adds a
        // per-column, a per-row and a constant term. Require the two to agree
        // exactly over every lattice corner, for a spread of rotations, screen
        // sizes and centres -- including centres that push a corner past the
        // +-2048 clamp, which is the one place the split could disagree.
        for (rx, ry, rz) in [
            (0u16, 0u16, 0u16),
            (512, 1024, 2048),
            (3000, 100, 60000),
            (16384, 32768, 49152),
            (65535, 65535, 65535),
        ] {
            let rotation = Mat3I16::rotate_xyz(rx, ry, rz).m;
            for (width, height, cx, cy, projection) in [
                (320i16, 240i16, 160i16, 120i16, 320i16),
                (320, 240, 0, 0, 1),
                (640, 480, 320, 240, 2048),
                (320, 240, -4000, 5000, 512),
                (1, 1, 160, 120, 300),
            ] {
                let projection_z = i32::from(projection.max(1)).clamp(1, 2048);
                let constant = [
                    projection_z * i32::from(rotation[2][0]),
                    projection_z * i32::from(rotation[2][1]),
                    projection_z * i32::from(rotation[2][2]),
                ];
                for column in 0..=super::CUBE_SKY_COLUMNS {
                    let sx = (column * width.max(1) as usize / super::CUBE_SKY_COLUMNS) as i16;
                    let x = (i32::from(sx) - i32::from(cx)).clamp(-2048, 2048);
                    let column_term = [
                        x * i32::from(rotation[0][0]),
                        x * i32::from(rotation[0][1]),
                        x * i32::from(rotation[0][2]),
                    ];
                    for row in 0..=super::CUBE_SKY_ROWS {
                        let sy = (row * height.max(1) as usize / super::CUBE_SKY_ROWS) as i16;
                        let y = (i32::from(sy) - i32::from(cy)).clamp(-2048, 2048);
                        let row_term = [
                            y * i32::from(rotation[1][0]),
                            y * i32::from(rotation[1][1]),
                            y * i32::from(rotation[1][2]),
                        ];
                        let split = [
                            (column_term[0] + row_term[0] + constant[0]) >> 12,
                            (column_term[1] + row_term[1] + constant[1]) >> 12,
                            (column_term[2] + row_term[2] + constant[2]) >> 12,
                        ];
                        assert_eq!(
                            split,
                            screen_view_ray([sx, sy], [cx, cy], projection.max(1), rotation),
                            "corner ({column},{row}) centre ({cx},{cy})"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_grid_corner_keeps_its_screen_position_through_the_packet_form() {
        // The uniform branch now reads a cell's screen corners from the
        // lattice tables instead of from `cube_sky_packet_vertex`. That is only
        // exact because a grid corner's Q12 position round-trips: the +2048
        // rounding term is below one whole unit and a column position is never
        // negative. Pin it over every screen width the sky can be asked for.
        for x in 0i32..=1024 {
            let q12 = x << 12;
            assert_eq!(
                ((q12 + 2048) >> 12).clamp(0, i32::from(i16::MAX)) as i16,
                x as i16,
                "grid corner {x}"
            );
        }
    }

    #[test]
    fn page_local_uv_matches_the_atlas_round_trip() {
        // The packet path stopped going through `cube_atlas_uv` only because
        // the atlas origin it adds is taken straight back out by `% 256`.
        // Sweep every face over a spread of rays, including the axis, edge and
        // corner directions where the projection saturates, and require the
        // two forms to agree exactly.
        // `screen_view_ray` clamps the camera vector to +-2048 and the
        // projection to 1..=2048, so a runtime ray component cannot exceed
        // 3 * 2048 * 4096 >> 12 = 6144. Sweep well past that, but stay inside
        // the range the arithmetic is defined on.
        let mut rays = alloc::vec![
            [1, 0, 0],
            [-1, 0, 0],
            [0, 1, 0],
            [0, -1, 0],
            [0, 0, 1],
            [0, 0, -1],
            [1, 1, 1],
            [-1, 1, -1],
            [1, -1, 1],
            [-1, -1, -1],
            [0, 0, 0],
            [6144, 6144, 6144],
            [-6144, 6143, -6144],
            [6144, 1, 6144],
            [16384, 16384, 16383],
            [-16384, -16383, -16384],
        ];
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 8) % 32_769) as i32 - 16_384
        };
        for _ in 0..40_000 {
            rays.push([next(), next(), next()]);
        }
        // Only a ray's own face is exercised. Projecting a ray onto a face it
        // does not belong to divides by a non-dominant axis, and the resulting
        // Q12 coordinate can exceed the range `cube_atlas_uv`'s `* 255` step
        // holds -- a pre-existing debug-only overflow in that function, which
        // release builds wrap and no caller reaches: the uniform branch uses
        // each corner's own face, and clipped vertices lie inside the face
        // they were clipped to.
        for ray in rays {
            let face = cube_face(ray);
            let [atlas_u, atlas_v] = cube_atlas_uv(ray, face);
            let expected = [
                (atlas_u % super::CUBE_SKY_FACE_WIDTH) as u8,
                (atlas_v % 256) as u8,
            ];
            assert_eq!(
                super::cube_face_page_local_uv(ray, face),
                expected,
                "ray {ray:?} face {face:?}"
            );
        }
    }

    #[test]
    fn a_uniform_cell_samples_every_corner_on_its_own_face() {
        // The grid-corner cache is only sound because the uniform branch
        // projects each corner onto the face that corner already selected.
        // Assert that identity directly: where all four corners agree,
        // sampling with the shared face equals sampling with each corner's own.
        let rotation = Mat3I16::rotate_xyz(0, 0, 0);
        for x in -160i32..160 {
            for y in [-120i32, -40, 40, 119] {
                let ray = screen_view_ray([x as i16, y as i16], [160, 120], 320, rotation.m);
                let own = cube_face(ray);
                let vertex = super::CubeSkyVertex {
                    screen_q12: [x << 12, y << 12],
                    ray,
                };
                assert_eq!(
                    super::cube_sky_packet_vertex(vertex, own),
                    super::cube_sky_packet_vertex(vertex, cube_face(vertex.ray)),
                );
            }
        }
    }

    #[test]
    fn distance_does_not_create_sky_parallax() {
        assert_eq!(
            directional_uv([100, 0, 0], [0, 0, 0], 64),
            directional_uv([200, 0, 0], [0, 0, 0], 64)
        );
    }

    #[test]
    fn translating_camera_and_aperture_together_keeps_the_sky_fixed() {
        let original = directional_uv([100, -40, 25], [0, 0, 0], 64);
        let translated =
            directional_uv([1_100, -540, 225], [1_000 << 12, -500 << 12, 200 << 12], 64);
        assert_eq!(translated, original);
    }

    #[test]
    fn quake_basis_center_ray_points_forward() {
        let basis = [[0, -0x3000, 0], [0, 0, -0x3000], [0x3000, 0, 0]];
        assert_eq!(
            screen_view_ray([160, 120], [160, 120], 160, basis),
            [480, 0, 0]
        );
        assert_eq!(directional_texel([480, 0, 0], 64), [189, 0]);
    }

    #[test]
    fn y_up_vectors_convert_back_to_the_imported_quake_basis() {
        assert_eq!(quake_direction_from_y_up([5, 7, -11]), [5, 11, 7]);
    }

    #[test]
    fn cube_projection_distinguishes_all_six_directions() {
        assert_eq!(cube_face([1, 0, 0]), CubeFace::PositiveX);
        assert_eq!(cube_face([-1, 0, 0]), CubeFace::NegativeX);
        assert_eq!(cube_face([0, 1, 0]), CubeFace::PositiveY);
        assert_eq!(cube_face([0, -1, 0]), CubeFace::NegativeY);
        assert_eq!(cube_face([0, 0, 1]), CubeFace::PositiveZ);
        assert_eq!(cube_face([0, 0, -1]), CubeFace::NegativeZ);
        assert_ne!(
            cube_atlas_uv([0, 1, 1], CubeFace::PositiveY),
            cube_atlas_uv([0, -1, 1], CubeFace::NegativeY)
        );
    }

    #[test]
    fn cube_face_centres_map_inside_six_distinct_tiles() {
        let samples = [
            ([1, 0, 0], CubeFace::PositiveX),
            ([-1, 0, 0], CubeFace::NegativeX),
            ([0, 1, 0], CubeFace::PositiveY),
            ([0, -1, 0], CubeFace::NegativeY),
            ([0, 0, 1], CubeFace::PositiveZ),
            ([0, 0, -1], CubeFace::NegativeZ),
        ];
        let mut centres = [[0u16; 2]; 6];
        for (index, (direction, face)) in samples.into_iter().enumerate() {
            assert_eq!(cube_face_uv_q12(direction, face), [0, 0]);
            let uv = cube_atlas_uv(direction, face);
            assert!(uv[0] < CUBE_SKY_ATLAS_SIZE[0]);
            assert!(uv[1] < CUBE_SKY_ATLAS_SIZE[1]);
            centres[index] = uv;
        }
        for left in 0..centres.len() {
            for right in left + 1..centres.len() {
                assert_ne!(centres[left], centres[right]);
            }
        }
    }

    #[test]
    fn cube_faces_select_contiguous_palette_slots() {
        let base = 0x7a40;
        for (index, face) in [
            CubeFace::PositiveX,
            CubeFace::NegativeX,
            CubeFace::PositiveY,
            CubeFace::NegativeY,
            CubeFace::PositiveZ,
            CubeFace::NegativeZ,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(cube_face_clut(base, face), base + index as u16);
        }
    }

    #[test]
    fn pitched_view_basis_reaches_zenith_and_nadir_faces() {
        let cases = [
            (
                [[0x1000, 0, 0], [0, 0, -0x1000], [0, 0x1000, 0]],
                CubeFace::PositiveY,
            ),
            (
                [[0x1000, 0, 0], [0, 0, 0x1000], [0, -0x1000, 0]],
                CubeFace::NegativeY,
            ),
        ];
        for (rotation, expected) in cases {
            let ray = screen_view_ray([160, 120], [160, 120], 160, rotation);
            assert_eq!(cube_face(ray), expected);
            assert_eq!(
                cube_atlas_uv(ray, expected),
                cube_atlas_uv(
                    match expected {
                        CubeFace::PositiveY => [0, 1, 0],
                        CubeFace::NegativeY => [0, -1, 0],
                        _ => unreachable!(),
                    },
                    expected,
                )
            );
        }
    }

    #[test]
    fn packet_uv_crosses_a_tile_seam_locally() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [0, 0],
        );
        assert_eq!(uv, [[61, 36], [67, 36], [61, 42], [67, 42]]);
        assert_eq!(uv[1][0] - uv[0][0], 6);
    }

    #[test]
    fn scrolling_cannot_reintroduce_a_packet_seam() {
        let uv = packet_quad_uv(
            [[-3, 4], [3, 4], [-3, 10], [3, 10]],
            [64, 32],
            [64, 64],
            [98, 151],
        );
        for axis in 0..2 {
            let minimum = uv.iter().map(|sample| sample[axis]).min().unwrap();
            let maximum = uv.iter().map(|sample| sample[axis]).max().unwrap();
            assert!(maximum - minimum <= 6);
        }
    }

    #[test]
    fn more_than_half_period_gradient_keeps_its_direction() {
        let uv = packet_quad_uv(
            [[372, 0], [366, -66], [376, 0], [371, -63]],
            [0, 0],
            [128, 128],
            [0, 0],
        );

        assert_eq!(uv, [[244, 128], [238, 62], [248, 128], [243, 65]]);
        assert_eq!(i16::from(uv[1][1]) - i16::from(uv[0][1]), -66);
    }

    #[test]
    fn view_ray_background_has_constant_tagged_packet_cost() {
        let mut packets = vec![0u32; VIEW_RAY_SKY_PACKET_WORDS];
        let submitted = unsafe {
            submit_view_ray_layered_sky(
                0x0105,
                0x1234,
                [0, 0],
                [128, 128],
                Mat3I16 {
                    m: [[0, 0, 0x1000], [0, -0x1000, 0], [0x1000, 0, 0]],
                },
                [320, 240],
                [160, 120],
                160,
                0,
                packets.as_mut_ptr(),
            )
        };
        assert_eq!(
            unsafe { submitted.next_packet.offset_from(packets.as_ptr()) as usize },
            VIEW_RAY_SKY_PACKET_WORDS
        );
        assert_eq!(submitted.packets, 243);
        assert_eq!(submitted.hardware_triangles, 480);
        assert_eq!(packets[0] >> 24, 1);
        assert_eq!(packets[0] & 0x00ff_ffff, 2047);
        assert_eq!(packets[2] >> 24, 9);
        assert_eq!(packets[2] & 0x00ff_ffff, 2047);
    }

    #[test]
    fn cube_background_stays_within_its_packet_envelope() {
        let mut packets = vec![0u32; VIEW_RAY_CUBE_SKY_PACKET_WORDS];
        let submitted = unsafe {
            submit_view_ray_cube_sky(
                0x0105,
                0x1234,
                Mat3I16 {
                    m: [[0, 0, 0x1000], [0, -0x1000, 0], [0x1000, 0, 0]],
                },
                [320, 240],
                [160, 120],
                160,
                packets.as_mut_ptr(),
            )
        };
        let used = unsafe { submitted.next_packet.offset_from(packets.as_ptr()) as usize };
        assert!(used <= VIEW_RAY_CUBE_SKY_PACKET_WORDS);
        assert!(submitted.packets > 1);
        assert!(submitted.hardware_triangles >= submitted.packets - 1);
        assert!(submitted.hardware_triangles <= (submitted.packets - 1) * 2);
        assert!(matches!(packets[0] >> 24, 7 | 9));
        assert_eq!(packets[0] & 0x00ff_ffff, 2047);
        assert_eq!(packets[used - super::SKY_WINDOW_PACKET_WORDS] >> 24, 1);
    }

    #[test]
    fn host_adapter_tags_every_cube_packet_for_its_deeper_sky_slot() {
        const HOST_SKY_SLOT: u16 = 4094;
        let mut packets = vec![0u32; VIEW_RAY_CUBE_SKY_PACKET_WORDS];
        let submitted = unsafe {
            submit_view_ray_cube_sky_to_slot(
                0x0105,
                0x1234,
                Mat3I16 {
                    m: [[0, 0, 0x1000], [0, -0x1000, 0], [0x1000, 0, 0]],
                },
                [320, 240],
                [160, 120],
                160,
                HOST_SKY_SLOT,
                packets.as_mut_ptr(),
            )
        };
        let used = unsafe { submitted.next_packet.offset_from(packets.as_ptr()) as usize };
        let mut offset = 0usize;
        while offset < used {
            let tag = packets[offset];
            assert_eq!(tag & 0x00ff_ffff, u32::from(HOST_SKY_SLOT));
            offset += 1 + (tag >> 24) as usize;
        }
        assert_eq!(offset, used);
    }

    #[test]
    fn cube_background_packet_envelope_covers_full_camera_rotation() {
        let mut packets = vec![0u32; VIEW_RAY_CUBE_SKY_PACKET_WORDS];
        let mut maximum_used = 0usize;
        for roll in (0..256).step_by(32) {
            for pitch in (0..256).step_by(16) {
                for yaw in (0..256).step_by(8) {
                    let submitted = unsafe {
                        submit_view_ray_cube_sky(
                            0x0105,
                            0x1234,
                            Mat3I16::rotate_xyz(pitch, yaw, roll),
                            [320, 240],
                            [160, 120],
                            160,
                            packets.as_mut_ptr(),
                        )
                    };
                    let used =
                        unsafe { submitted.next_packet.offset_from(packets.as_ptr()) as usize };
                    maximum_used = maximum_used.max(used);
                    assert!(used <= VIEW_RAY_CUBE_SKY_PACKET_WORDS);
                    assert!(submitted.packets > 1);
                    assert_eq!(packets[used - super::SKY_WINDOW_PACKET_WORDS] >> 24, 1);
                }
            }
        }
        assert!(maximum_used < super::CUBE_SKY_PACKET_BUDGET_WORDS);
    }
}
