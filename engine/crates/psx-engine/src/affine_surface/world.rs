//! The world affine path: an error-bounded quad lattice with crack sealing.
//!
//! [`submit_world_batch`] projects a batch of compact (page-local) convex
//! fans and writes their textured Gouraud packets. Each face gets one split
//! level for all of it, from the affine error its extreme depths and screen
//! box can produce against a budget in eighths of a pixel
//! ([`crate::tess::error_level`]):
//!
//! - level zero: the face is drawn whole, a four-corner face as one GP0(3Ch)
//!   quad, a fan with neighbouring roots at one ordering key paired into
//!   quads;
//! - a four-corner face above zero: a lattice of GT4 cells, each axis split
//!   only as far as its own two edges need (a wall receding sideways takes a
//!   strip, not a grid);
//! - any other fan above zero: every root triangle split once (a quad and
//!   two triangles) or twice (four triangles and six quads).
//!
//! The face shares its level across its internal diagonals, so only its own
//! boundary edges can meet a neighbour split differently. With
//! [`WorldProfile::underdraw`] those edges are sealed: the slivers between
//! the unsplit chord and the split polyline are drawn behind the face's
//! pieces (keyed at its far vertex), so a neighbour that drew the edge as one
//! chord leaves no pinhole.
//!
//! The GPU drops a primitive more than 1023 pixels across or 511 down. A
//! face whose screen box is over that takes at least two levels, and its
//! packets are then checked: one still over the limit leaves the ordering
//! table and is replaced by smaller triangles cut at its edge midpoints
//! (which the GPU maps exactly as the whole), inside the room the caller
//! reserved for the face.
//!
//! Faces wholly off one screen edge are rejected before they are split;
//! individual pieces are not tested on the CPU, the GPU's draw area clips
//! them. Packets carry no texture-window state: the caller keeps the full
//! window in force for the pass (packets that set another window restore it
//! inside their own node). Tags carry the data-word count in bits 24..31 and
//! the ordering-table slot in bits 0..15, for the tagged-stream linker.

use core::mem::size_of;
use core::ptr;

use psx_gpu::prim::{ClassicQuadTexturedGouraud, ClassicTriTexturedGouraud};
use psx_gte::math::Vec3I16;
use psx_gte::scene::{self, project_triangle_scheduled, project_vertex_scheduled, Projected};
use psx_hw::gpu::packet;

use super::{AffineVertex, SurfaceSubmit};
use crate::tess;

type V = AffineVertex;

/// Extra vertex slots the caller reserves after a world batch for the
/// points a split face generates.
pub const WORLD_SCRATCH_VERTICES: usize = 12;

/// Words the caller reserves in the packet storage for each triangle of a
/// face before it submits the face (nineteen of the largest packets): the
/// most a face may write, and so the room the oversize repair works inside.
pub const WORLD_PACKET_WORDS_PER_TRIANGLE: usize = 19 * 13;

/// One convex fan of a world batch: its vertex range and its material.
///
/// The descriptor is read as two words (they live in RAM, where every load
/// stalls), so it is aligned to four bytes.
#[repr(C, align(4))]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorldSurface {
    /// First vertex in the batch vertex array.
    pub first_vertex: u16,
    /// Number of vertices in this convex fan (at least three).
    pub vertex_count: u16,
    /// Texture-page word.
    pub tpage: u16,
    /// CLUT word.
    pub clut: u16,
}

/// Viewport, ordering table and sealing policy of one world renderer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct WorldProfile {
    /// Viewport width in pixels.
    pub screen_width: i16,
    /// Viewport height in pixels.
    pub screen_height: i16,
    /// Number of ordering-table slots.
    pub ot_depth: u16,
    /// Faces whose nearest vertex is at least this deep (GTE SZ units) take
    /// level zero without the screen-extent pass. Sound when it is at least
    /// `F * sqrt(H / (2 * budget_px))` for the largest face extent `F`
    /// (camera-space units) the cooker emits and projection distance `H`:
    /// such a face's affine displacement cannot reach the budget. Zero tests
    /// every face.
    pub error_gate_depth: u16,
    /// Seal the split boundary edges of every split face (see the module
    /// documentation). Off, a split face next to an unsplit one can leave
    /// pinholes along their shared edge.
    pub underdraw: bool,
    /// Least ordering-table slots a seal keys behind its face's average.
    pub underdraw_slot_bias: u16,
}

impl WorldProfile {
    /// First-person brush world at 320x240 with 2,048 slots, every split edge
    /// sealed. The gate (930 SZ) is `F * sqrt(H / (2 * 16))` for 240-unit
    /// face cuts (`F` = 416, a 240-unit cube's diagonal) at `H` = 160 and a
    /// 16-pixel budget; zoomed views (larger `H`) can leave faces just past
    /// it a little under-split.
    pub const FIRST_PERSON: Self = Self {
        screen_width: 320,
        screen_height: 240,
        ot_depth: 2048,
        error_gate_depth: 930,
        underdraw: true,
        underdraw_slot_bias: 8,
    };
}

/// A world renderer's profile as a compile-time fact: the batch writer is
/// specialised to it, so its viewport, table size and policy cost no loads.
pub trait WorldLattice {
    /// The profile.
    const PROFILE: WorldProfile;
}

/// [`WorldProfile::FIRST_PERSON`], as a type parameter.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct FirstPerson;

impl WorldLattice for FirstPerson {
    const PROFILE: WorldProfile = WorldProfile::FIRST_PERSON;
}

/// Error budget, in eighths of a pixel, a caller can switch to when its
/// packet storage runs short: four times a 16-pixel budget. Faces still
/// split as far as the GPU's extent limit needs.
pub const WORLD_COARSE_ERROR_BUDGET_Q3: u8 = 255;

/// Packet cursor of one surface's material.
struct PacketWriter {
    next: *mut u32,
    packets: u32,
    clut_high_word: u32,
    tpage_high_word: u32,
    /// Error budget in eighths of a pixel, for the quad lattice's axes.
    budget_q3: u32,
}

impl PacketWriter {
    /// A surface's GP0 material as one word: tpage in the low half, CLUT in
    /// the high half, which is the second word of a [`WorldSurface`] as it
    /// lies in memory. One register instead of two keeps both out of the RAM
    /// stack in the fan loop.
    #[inline(always)]
    fn with_material(next: *mut u32, material: u32, budget_q3: u32) -> Self {
        Self {
            next,
            packets: 0,
            clut_high_word: material & 0xffff_0000,
            tpage_high_word: material << 16,
            budget_q3,
        }
    }

    #[inline(always)]
    unsafe fn emit_tri(&mut self, corners: [&V; 3], otz: u16) {
        // SAFETY: per the caller.
        unsafe { self.emit_tri_with(corners, corners, otz) };
    }

    #[inline(always)]
    unsafe fn emit_quad(&mut self, corners: [&V; 4], otz: u16) {
        let packet = ClassicQuadTexturedGouraud::with_staged_slot_prepacked_colors(
            [
                (corners[0].screen[0], corners[0].screen[1]),
                (corners[1].screen[0], corners[1].screen[1]),
                (corners[2].screen[0], corners[2].screen[1]),
                (corners[3].screen[0], corners[3].screen[1]),
            ],
            [
                uv_word(corners[0]),
                uv_word(corners[1]),
                uv_word(corners[2]),
                uv_word(corners[3]),
            ],
            [
                corners[0].color,
                corners[1].color,
                corners[2].color,
                corners[3].color,
            ],
            self.clut_high_word,
            self.tpage_high_word,
            otz,
        );
        // SAFETY: the caller reserved room for the face's packets.
        unsafe {
            ptr::write(self.next.cast::<ClassicQuadTexturedGouraud>(), packet);
            self.next = self
                .next
                .add(size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>());
        }
        self.packets = self.packets.wrapping_add(1);
    }

    /// The submission result: every packet is one triangle or one quad, so
    /// the quad count follows from the stream length and the hot path only
    /// counts packets.
    #[inline(always)]
    fn finish(self, output: *mut u32) -> SurfaceSubmit {
        // SAFETY: `next` advanced from `output` within one allocation.
        let words = unsafe { self.next.offset_from(output) as u32 };
        let tri_words = (size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>()) as u32;
        let quad_words = (size_of::<ClassicQuadTexturedGouraud>() / size_of::<u32>()) as u32;
        let quads =
            words.wrapping_sub(self.packets.wrapping_mul(tri_words)) / (quad_words - tri_words);
        SurfaceSubmit {
            next_packet: self.next,
            packets: self.packets,
            hardware_triangles: self.packets.wrapping_add(quads),
        }
    }
}

#[inline(always)]
const fn uv_word(vertex: &V) -> u16 {
    vertex.uv[0] as u16 | ((vertex.uv[1] as u16) << 8)
}

/// Midpoint of two vertices: positions, both UV bytes and each colour
/// channel averaged, rounding down (as [`super::midpoint`], in fewer
/// instructions).
#[inline(always)]
pub(super) fn midpoint(a: &V, b: &V) -> V {
    let color = ((a.color & 0x00ff_00ff) + (b.color & 0x00ff_00ff)) >> 1 & 0x00ff_00ff
        | ((a.color & 0x0000_ff00) + (b.color & 0x0000_ff00)) >> 1 & 0x0000_ff00;
    let a_uv = u16::from_le_bytes(a.uv);
    let b_uv = u16::from_le_bytes(b.uv);
    // Average both packed bytes independently. Clearing each byte's low bit
    // before the shift prevents a carry from U into V.
    let uv = (a_uv & b_uv).wrapping_add(((a_uv ^ b_uv) & 0xfefe) >> 1);
    V {
        position: [
            ((a.position[0] as i32 + b.position[0] as i32) >> 1) as i16,
            ((a.position[1] as i32 + b.position[1] as i32) >> 1) as i16,
            ((a.position[2] as i32 + b.position[2] as i32) >> 1) as i16,
        ],
        uv: uv.to_le_bytes(),
        color,
        screen: [0; 2],
        depth: 0,
    }
}

#[inline(always)]
unsafe fn position(vertex: *const V) -> Vec3I16 {
    // SAFETY: the caller hands an initialised record.
    let position = unsafe { (*vertex).position };
    Vec3I16::new(position[0], position[1], position[2])
}

#[inline(always)]
unsafe fn store_projection(vertex: *mut V, out: Projected) {
    // SAFETY: the caller hands a writable record.
    unsafe {
        ptr::write(ptr::addr_of_mut!((*vertex).screen), [out.sx, out.sy]);
        ptr::write(ptr::addr_of_mut!((*vertex).depth), u32::from(out.sz));
    }
}

#[inline(always)]
unsafe fn project_three_consecutive(vertices: *mut V) {
    // SAFETY: three records from `vertices` per the caller.
    unsafe {
        let out = project_triangle_scheduled(
            position(vertices),
            position(vertices.add(1)),
            position(vertices.add(2)),
        );
        store_projection(vertices, out[0]);
        store_projection(vertices.add(1), out[1]);
        store_projection(vertices.add(2), out[2]);
    }
}

#[inline(always)]
unsafe fn project_one(vertex: *mut V) {
    // SAFETY: one record per the caller.
    unsafe { store_projection(vertex, project_vertex_scheduled(position(vertex))) };
}

/// Ordering key of a seal drawn behind its own face's pieces: never nearer
/// than the face's average plus the profile's bias, and at the face's far
/// vertex when that is deeper, so no piece is covered.
#[inline(always)]
fn underlay_otz<L: WorldLattice>(average_otz: u16, far_depth: u16) -> u16 {
    average_otz
        .saturating_add(L::PROFILE.underdraw_slot_bias)
        .max(far_depth >> 2)
        .min(L::PROFILE.ot_depth - 1)
}

/// The ordering key of three cached GTE depths (AVSZ3 with ZSF3 = 0x155).
#[inline(always)]
fn average3_depths(a: u16, b: u16, c: u16) -> u16 {
    scene::classic_ordering_depth3_from_sum(u32::from(a) + u32::from(b) + u32::from(c))
}

#[inline(always)]
fn average3(vertices: [&V; 3]) -> u16 {
    average3_depths(
        vertices[0].depth as u16,
        vertices[1].depth as u16,
        vertices[2].depth as u16,
    )
}

/// Outcode of a screen point against the zero-origin viewport, with a fast
/// all-inside return.
#[inline(always)]
fn screen_outcode<L: WorldLattice>(position: [i16; 2]) -> u8 {
    let right = L::PROFILE.screen_width as i32 - 1;
    let bottom = L::PROFILE.screen_height as i32 - 1;
    let x = position[0] as i32;
    let y = position[1] as i32;
    if (x as u32) <= right as u32 && (y as u32) <= bottom as u32 {
        return 0;
    }
    ((x as u32 >> 31) as u8)
        | ((((right - x) as u32 >> 31) as u8) << 1)
        | (((y as u32 >> 31) as u8) << 2)
        | ((((bottom - y) as u32 >> 31) as u8) << 3)
}

#[inline(always)]
unsafe fn sorted_tri(writer: &mut PacketWriter, corners: [&V; 3]) {
    let otz = average3(corners);
    if otz > 0 {
        // SAFETY: room per the caller.
        unsafe { writer.emit_tri(corners, otz) };
    }
}

#[inline(always)]
unsafe fn sorted_quad(writer: &mut PacketWriter, corners: [&V; 4]) {
    let depth_sum = corners[0].depth as u16 as u32
        + corners[1].depth as u16 as u32
        + corners[2].depth as u16 as u32
        + corners[3].depth as u16 as u32;
    let otz = (depth_sum >> 4) as u16;
    if otz > 0 {
        // SAFETY: room per the caller.
        unsafe { writer.emit_quad(corners, otz) };
    }
}

/// The seal key of a root triangle: its average behind by the bias, or its
/// far vertex.
#[inline(always)]
fn root_underlay_otz<L: WorldLattice>(roots: [&V; 3], root_otz: u16) -> u16 {
    let far = (roots[0].depth as u16)
        .max(roots[1].depth as u16)
        .max(roots[2].depth as u16);
    underlay_otz::<L>(root_otz, far)
}

/// Split one root triangle once: a quad and two triangles, then the seals of
/// the boundary edges named by `underdraw_edges` (bit 0 the edge 0-1, bit 1
/// 1-2, bit 2 2-0).
///
/// # Safety
/// `scratch` must hold three writable records; room per the caller.
#[inline(always)]
unsafe fn subdivide_once<L: WorldLattice>(
    writer: &mut PacketWriter,
    root0: &V,
    root1: &V,
    root2: &V,
    scratch: *mut V,
    root_otz: u16,
    underdraw_edges: u8,
) {
    // SAFETY: three scratch records per the contract.
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root2, root0));
        project_three_consecutive(scratch);
    }
    // SAFETY: just written.
    let (h01, h12, h20) = unsafe { (&*scratch, &*scratch.add(1), &*scratch.add(2)) };
    // SAFETY: room per the contract.
    unsafe {
        sorted_quad(writer, [root0, h01, h20, h12]);
        sorted_tri(writer, [h01, root1, h12]);
        sorted_tri(writer, [h12, root2, h20]);
    }
    if L::PROFILE.underdraw {
        let underdraw = root_underlay_otz::<L>([root0, root1, root2], root_otz);
        // SAFETY: room per the contract.
        unsafe {
            if underdraw_edges & 1 != 0 {
                writer.emit_tri([root0, root1, h01], underdraw);
            }
            if underdraw_edges & 2 != 0 {
                // This seal takes root 0's and root 1's UVs and colours, not
                // root 1's and root 2's: the original kernel drew it so, and
                // keeping it keeps its picture (the sliver lies behind the
                // face's own pieces and shows only through a crack).
                writer.emit_tri_with([root1, root2, h12], [root0, root1, h12], underdraw);
            }
            if underdraw_edges & 4 != 0 {
                writer.emit_tri([root2, root0, h20], underdraw);
            }
        }
    }
}

impl PacketWriter {
    /// A triangle at `projected`'s screen positions with `attributes`' UVs
    /// and colours.
    #[inline(always)]
    unsafe fn emit_tri_with(&mut self, projected: [&V; 3], attributes: [&V; 3], otz: u16) {
        let packet = ClassicTriTexturedGouraud::with_staged_slot_prepacked_colors(
            [
                (projected[0].screen[0], projected[0].screen[1]),
                (projected[1].screen[0], projected[1].screen[1]),
                (projected[2].screen[0], projected[2].screen[1]),
            ],
            [
                uv_word(attributes[0]),
                uv_word(attributes[1]),
                uv_word(attributes[2]),
            ],
            [
                attributes[0].color,
                attributes[1].color,
                attributes[2].color,
            ],
            self.clut_high_word,
            self.tpage_high_word,
            otz,
        );
        // SAFETY: the caller reserved room for the face's packets.
        unsafe {
            ptr::write(self.next.cast::<ClassicTriTexturedGouraud>(), packet);
            self.next = self
                .next
                .add(size_of::<ClassicTriTexturedGouraud>() / size_of::<u32>());
        }
        self.packets = self.packets.wrapping_add(1);
    }
}

/// Split one root triangle twice: four corner triangles and six quads, then
/// the seals of the boundary edges named by `underdraw_edges`.
///
/// # Safety
/// `scratch` must hold [`WORLD_SCRATCH_VERTICES`] writable records; room per
/// the caller.
#[inline(always)]
unsafe fn subdivide_twice<L: WorldLattice>(
    writer: &mut PacketWriter,
    root0: &V,
    root1: &V,
    root2: &V,
    scratch: *mut V,
    root_otz: u16,
    underdraw_edges: u8,
) {
    // SAFETY: twelve scratch records per the contract; each midpoint reads
    // only records written before it.
    unsafe {
        ptr::write(scratch, midpoint(root0, root1));
        ptr::write(scratch.add(1), midpoint(root1, root2));
        ptr::write(scratch.add(2), midpoint(root0, root2));
        ptr::write(scratch.add(3), midpoint(root0, &*scratch));
        ptr::write(scratch.add(4), midpoint(root1, &*scratch));
        ptr::write(scratch.add(5), midpoint(root1, &*scratch.add(1)));
        ptr::write(scratch.add(6), midpoint(&*scratch.add(1), root2));
        ptr::write(scratch.add(7), midpoint(&*scratch.add(2), root2));
        ptr::write(scratch.add(8), midpoint(&*scratch.add(2), root0));
        ptr::write(scratch.add(9), midpoint(&*scratch.add(2), &*scratch));
        ptr::write(
            scratch.add(10),
            midpoint(&*scratch.add(9), &*scratch.add(5)),
        );
        ptr::write(
            scratch.add(11),
            midpoint(&*scratch.add(2), &*scratch.add(1)),
        );
        project_three_consecutive(scratch);
        project_three_consecutive(scratch.add(3));
        project_three_consecutive(scratch.add(6));
        project_three_consecutive(scratch.add(9));
    }
    // SAFETY: just written.
    let v = unsafe { core::slice::from_raw_parts(scratch, WORLD_SCRATCH_VERTICES) };
    // SAFETY: room per the contract.
    unsafe {
        sorted_tri(writer, [root0, &v[3], &v[8]]);
        sorted_tri(writer, [&v[8], &v[9], &v[2]]);
        sorted_tri(writer, [&v[2], &v[11], &v[7]]);
        sorted_tri(writer, [&v[7], &v[6], root2]);
        sorted_quad(writer, [&v[3], &v[0], &v[8], &v[9]]);
        sorted_quad(writer, [&v[0], &v[4], &v[9], &v[10]]);
        sorted_quad(writer, [&v[4], root1, &v[10], &v[5]]);
        sorted_quad(writer, [&v[9], &v[10], &v[2], &v[11]]);
        sorted_quad(writer, [&v[10], &v[5], &v[11], &v[1]]);
        sorted_quad(writer, [&v[11], &v[1], &v[7], &v[6]]);
    }
    if L::PROFILE.underdraw {
        let underdraw = root_underlay_otz::<L>([root0, root1, root2], root_otz);
        // GP0(3Ch) splits [a,b,c,d] into [b,d,c] then [a,b,c]: each edge's
        // two slivers in one quad and one triangle.
        // SAFETY: room per the contract.
        unsafe {
            if underdraw_edges & 1 != 0 {
                writer.emit_quad([root1, &v[0], root0, &v[3]], underdraw);
                writer.emit_tri([&v[0], root1, &v[4]], underdraw);
            }
            if underdraw_edges & 2 != 0 {
                writer.emit_quad([root2, &v[1], root1, &v[5]], underdraw);
                writer.emit_tri([&v[1], root2, &v[6]], underdraw);
            }
            if underdraw_edges & 4 != 0 {
                writer.emit_quad([root2, &v[2], root0, &v[8]], underdraw);
                writer.emit_tri([&v[2], root2, &v[7]], underdraw);
            }
        }
    }
}

/// Largest span, in pixels, of a primitive the GPU draws: it drops any
/// triangle two of whose vertices lie more than 1023 pixels apart across or
/// 511 down.
const GPU_MAX_SPAN_X: i32 = 1023;
const GPU_MAX_SPAN_Y: i32 = 511;

/// Level a face needs so that no primitive it emits breaks the GPU's extent
/// limit: 2 when its screen box is wider or taller than one primitive may
/// be, else 0. The warp bound alone leaves a face that faces the camera
/// unsplit however large it is on screen, and the GPU would drop it whole.
#[inline(always)]
fn gpu_extent_level(dx: i32, dy: i32) -> u8 {
    if dx > GPU_MAX_SPAN_X || dy > GPU_MAX_SPAN_Y {
        2
    } else {
        0
    }
}

/// Set in the level [`face_level_flagged`] returns when the face's screen box
/// is over the GPU's extent limit.
const EXTENT_BIG: u8 = 0x80;

#[inline(always)]
fn extent_flag(extent: u8) -> u8 {
    if extent != 0 {
        EXTENT_BIG
    } else {
        0
    }
}

/// Error-bounded split level shared by every root of a face.
///
/// An edge's affine displacement is `L |zb - za| / (2 (za + zb))`. Of all the
/// face's edges and diagonals, the pair of extreme depths has the largest
/// depth ratio, and the screen bounding box's diagonal is at least as long
/// as any of them, so one test bounds every root at once, and the whole face
/// sharing one level leaves no T-junction on its internal diagonals. Faces
/// beyond `gate_depth` skip the screen pass; in front of it the level is at
/// least [`gpu_extent_level`] of the screen box.
///
/// # Safety
/// `vertices` must hold `vertex_count` (at least one) projected records.
#[inline(always)]
unsafe fn face_level_flagged(
    vertices: *const V,
    vertex_count: usize,
    budget_q3: u32,
    gate_depth: u16,
) -> u8 {
    // SAFETY: `vertex_count` records per the contract.
    let end = unsafe { vertices.add(vertex_count) };
    // SAFETY: inside the range the caller hands over.
    let first = unsafe { &*vertices };
    let (mut z0, mut z1) = (first.depth, first.depth);
    // SAFETY: inside the range the caller hands over.
    let mut vertex = unsafe { vertices.add(1) };
    while vertex != end {
        // SAFETY: `vertex` is inside the range.
        let z = unsafe { (*vertex).depth };
        if z < z0 {
            z0 = z;
        } else if z > z1 {
            z1 = z;
        }
        // SAFETY: inside the range the caller hands over.
        vertex = unsafe { vertex.add(1) };
    }
    if z0 == 0 || (gate_depth != 0 && z0 >= u32::from(gate_depth)) {
        return 0;
    }
    // i32 accumulators: the loads sign-extend once and the compares need no
    // re-extension.
    let (mut x0, mut y0) = (i32::from(first.screen[0]), i32::from(first.screen[1]));
    let (mut x1, mut y1) = (x0, y0);
    // SAFETY: inside the range the caller hands over.
    let mut vertex = unsafe { vertices.add(1) };
    while vertex != end {
        // SAFETY: `vertex` is inside the range.
        let v = unsafe { &*vertex };
        let (x, y) = (i32::from(v.screen[0]), i32::from(v.screen[1]));
        if x < x0 {
            x0 = x;
        } else if x > x1 {
            x1 = x;
        }
        if y < y0 {
            y0 = y;
        } else if y > y1 {
            y1 = y;
        }
        // SAFETY: inside the range the caller hands over.
        vertex = unsafe { vertex.add(1) };
    }
    let extent = gpu_extent_level(x1 - x0, y1 - y0);
    // GTE depths are 16-bit, so they fit `i32`.
    let (z0, z1) = (z0 as i32, z1 as i32);
    // No edge inside the guard band is longer than 2813 px.
    if tess::error_level(2813, z0, z1, budget_q3) == 0 {
        return extent | extent_flag(extent);
    }
    let (dx, dy) = ((x1 - x0).min(4095) as u32, (y1 - y0).min(4095) as u32);
    let span = if dx > dy {
        dx + ((dy * 3) >> 3)
    } else {
        dy + ((dx * 3) >> 3)
    };
    tess::error_level(span, z0, z1, budget_q3).max(extent) | extent_flag(extent)
}

#[inline(always)]
fn edge_error_level(a: &V, b: &V, budget_q3: u32) -> u8 {
    tess::error_level(
        tess::screen_span(a.screen, b.screen),
        a.depth as i32,
        b.depth as i32,
        budget_q3,
    )
}

/// Point `j / n` of the way from `a` to `b` (`n` is 2 or 4, `0 < j < n`) by
/// the same recursive midpoints the triangle lattice takes, so both faces
/// sharing an edge that is split as often generate identical vertices.
#[inline(always)]
fn lattice_edge_point(a: &V, b: &V, j: usize, n: usize) -> V {
    let half = midpoint(a, b);
    if n == 2 || j == 2 {
        half
    } else if j == 1 {
        midpoint(a, &half)
    } else {
        midpoint(&half, b)
    }
}

/// Fill `row[0..=n]` with the points from `a` to `b` and project the ones
/// that are new (`ends_projected`: `a` and `b` already carry projections).
///
/// # Safety
/// `row` must hold `n + 1` writable records.
#[inline(always)]
unsafe fn fill_lattice_row(row: *mut V, a: V, b: V, n: usize, ends_projected: bool) {
    // SAFETY: `n + 1` records per the contract.
    unsafe {
        ptr::write(row, a);
        ptr::write(row.add(n), b);
        if n == 2 {
            ptr::write(row.add(1), midpoint(&a, &b));
        } else if n == 4 {
            let half = midpoint(&a, &b);
            ptr::write(row.add(1), midpoint(&a, &half));
            ptr::write(row.add(2), half);
            ptr::write(row.add(3), midpoint(&half, &b));
        }
        if ends_projected {
            if n == 2 {
                project_one(row.add(1));
            } else if n == 4 {
                project_three_consecutive(row.add(1));
            }
        } else if n == 1 {
            project_one(row);
            project_one(row.add(1));
        } else if n == 2 {
            project_three_consecutive(row);
        } else {
            project_three_consecutive(row);
            project_three_consecutive(row.add(2));
        }
    }
}

/// Seal the crack between a split boundary edge `p[0..=n]` and a neighbour
/// that drew it as one chord: the slivers between the chord and the split
/// polyline, the same shapes the triangle lattice's seals take.
///
/// # Safety
/// Room per the caller.
#[inline(always)]
unsafe fn seal_split_edge(writer: &mut PacketWriter, p: [&V; 5], n: usize, otz: u16) {
    // SAFETY: room per the contract.
    unsafe {
        if n == 2 {
            writer.emit_tri([p[0], p[2], p[1]], otz);
        } else if n == 4 {
            writer.emit_quad([p[4], p[2], p[0], p[1]], otz);
            writer.emit_tri([p[2], p[4], p[3]], otz);
        }
    }
}

/// A four-corner face (fan order `v0 v1 v2 v3`) as a quad lattice: `2^la`
/// cells along `v0 -> v1` and `2^lb` along `v0 -> v3`, each axis split only
/// as far as its own two edges need, the `v0 v2` diagonal counted on the
/// axis with the larger depth change, and both at least as far as the GPU's
/// extent limit needs. Cells are GT4 in the GPU's Z order
/// (`[p(i+1,j), p(i+1,j+1), p(i,j), p(i,j+1)]`), which puts the GPU's split
/// on the `v0 v2` diagonal the fan pairing uses. Rows are generated two at a
/// time in `scratch` (at most ten records).
///
/// # Safety
/// `vertices` must hold four projected records and `scratch` ten writable
/// ones; room per the caller.
#[inline(always)]
unsafe fn submit_quad_lattice<L: WorldLattice>(
    vertices: *mut V,
    scratch: *mut V,
    writer: &mut PacketWriter,
    face_level: u8,
) {
    // SAFETY: four records per the contract.
    let c = unsafe {
        [
            &*vertices,
            &*vertices.add(1),
            &*vertices.add(2),
            &*vertices.add(3),
        ]
    };
    let depth_sum = c.iter().map(|v| v.depth as u16 as u32).sum::<u32>();
    let face_otz = (depth_sum >> 4) as u16;
    if face_otz == 0 || face_otz >= L::PROFILE.ot_depth {
        return;
    }
    let dz = |a: &V, b: &V| a.depth.abs_diff(b.depth);
    let dz_a = dz(c[0], c[1]) + dz(c[3], c[2]);
    let dz_b = dz(c[1], c[2]) + dz(c[0], c[3]);
    let (mut la, mut lb) = (0u8, 0u8);
    if face_level != 0 {
        let budget = writer.budget_q3;
        la = edge_error_level(c[0], c[1], budget).max(edge_error_level(c[3], c[2], budget));
        lb = edge_error_level(c[1], c[2], budget).max(edge_error_level(c[0], c[3], budget));
        let diagonal = edge_error_level(c[0], c[2], budget);
        if diagonal > la.max(lb) {
            if dz_a >= dz_b {
                la = diagonal;
            } else {
                lb = diagonal;
            }
        }
        // The edges' warp can be zero on a face the GPU would still drop for
        // its size: split both axes as far as the face needs.
        let span = |axis: usize| {
            let values = c.map(|v| i32::from(v.screen[axis]));
            values.iter().max().unwrap_or(&0) - values.iter().min().unwrap_or(&0)
        };
        let extent = gpu_extent_level(span(0), span(1));
        la = la.max(extent);
        lb = lb.max(extent);
    }
    if la == 0 && lb == 0 {
        // SAFETY: room per the contract.
        unsafe { writer.emit_quad([c[1], c[2], c[0], c[3]], face_otz) };
        return;
    }

    let na = 1usize << la;
    let nb = 1usize << lb;
    let seal = L::PROFILE.underdraw;
    let far = c.iter().map(|v| v.depth as u16).max().unwrap_or(0);
    let seal_otz = underlay_otz::<L>(face_otz, far);
    let ends = |r: &[V]| -> [*const V; 5] {
        let at = |k: usize| &r[k.min(na)] as *const V;
        [at(0), at(1), at(2), at(3), at(na)]
    };

    // SAFETY: rows of five in the ten scratch records.
    let rows = [scratch, unsafe { scratch.add(5) }];
    // SAFETY: the first row lies in the scratch records.
    unsafe { fill_lattice_row(rows[0], *c[0], *c[1], na, true) };
    if seal && na > 1 {
        // SAFETY: the row was just filled.
        let r = unsafe { core::slice::from_raw_parts(rows[0], na + 1) };
        let e = ends(r);
        // SAFETY: the row was just filled; room per the contract.
        unsafe { seal_split_edge(writer, e.map(|v| &*v), na, seal_otz) };
    }
    let mut j = 1usize;
    while j <= nb {
        let prev = rows[(j - 1) & 1];
        let cur = rows[j & 1];
        // SAFETY: the rows lie in the scratch records.
        unsafe {
            if j == nb {
                fill_lattice_row(cur, *c[3], *c[2], na, true);
            } else {
                let left = lattice_edge_point(c[0], c[3], j, nb);
                let right = lattice_edge_point(c[1], c[2], j, nb);
                fill_lattice_row(cur, left, right, na, false);
            }
        }
        // SAFETY: both rows are filled.
        let p = unsafe { core::slice::from_raw_parts(prev, na + 1) };
        // SAFETY: both rows are filled.
        let q = unsafe { core::slice::from_raw_parts(cur, na + 1) };
        let mut i = 0usize;
        while i < na {
            // SAFETY: room per the contract.
            unsafe { sorted_quad(writer, [&p[i + 1], &q[i + 1], &p[i], &q[i]]) };
            i += 1;
        }
        if seal && nb > 1 {
            // Column edges v0-v3 (row starts) and v1-v2 (row ends): their
            // slivers need rows 1 and 2 (the quad) or rows 2 and 3 (the
            // triangle) of a four-row lattice, or row 1 of a two-row one.
            // SAFETY: room per the contract.
            unsafe {
                if nb == 2 && j == 1 {
                    writer.emit_tri([c[0], c[3], &q[0]], seal_otz);
                    writer.emit_tri([c[1], c[2], &q[na]], seal_otz);
                } else if nb == 4 && j == 2 {
                    writer.emit_quad([c[3], &q[0], c[0], &p[0]], seal_otz);
                    writer.emit_quad([c[2], &q[na], c[1], &p[na]], seal_otz);
                } else if nb == 4 && j == 3 {
                    writer.emit_tri([&p[0], c[3], &q[0]], seal_otz);
                    writer.emit_tri([&p[na], c[2], &q[na]], seal_otz);
                }
            }
        }
        if j == nb && seal && na > 1 {
            let e = ends(q);
            // SAFETY: room per the contract.
            unsafe { seal_split_edge(writer, e.map(|v| &*v), na, seal_otz) };
        }
        j += 1;
    }
}

/// One vertex of a packet being cut: screen position, packet UV and RGB.
#[derive(Copy, Clone)]
struct CutVertex {
    xy: [i32; 2],
    uv: [i32; 2],
    rgb: [i32; 3],
}

impl CutVertex {
    /// Read vertex `index` of the packet at `at` (words `[rgb, xy, uv]` from
    /// word 1, three words each).
    ///
    /// # Safety
    /// `at` must be a written textured Gouraud packet with that vertex.
    #[inline(always)]
    unsafe fn read(at: *const u32, index: usize) -> Self {
        // SAFETY: per the contract.
        let [color, xy, uv] = unsafe {
            [
                ptr::read(at.add(1 + 3 * index)),
                ptr::read(at.add(2 + 3 * index)),
                ptr::read(at.add(3 + 3 * index)),
            ]
        };
        Self {
            xy: [i32::from(xy as i16), i32::from((xy >> 16) as i16)],
            uv: [(uv & 0xff) as i32, ((uv >> 8) & 0xff) as i32],
            rgb: [
                (color & 0xff) as i32,
                ((color >> 8) & 0xff) as i32,
                ((color >> 16) & 0xff) as i32,
            ],
        }
    }

    fn middle(a: &Self, b: &Self) -> Self {
        Self {
            xy: [(a.xy[0] + b.xy[0]) >> 1, (a.xy[1] + b.xy[1]) >> 1],
            uv: [(a.uv[0] + b.uv[0]) >> 1, (a.uv[1] + b.uv[1]) >> 1],
            rgb: [
                (a.rgb[0] + b.rgb[0]) >> 1,
                (a.rgb[1] + b.rgb[1]) >> 1,
                (a.rgb[2] + b.rgb[2]) >> 1,
            ],
        }
    }
}

/// Whether the GPU would drop a triangle for its size: two vertices more
/// than 1023 pixels apart across or 511 down.
fn cut_exceeds_gpu_extent(t: &[CutVertex; 3]) -> bool {
    let span = |axis: usize| {
        let v = [t[0].xy[axis], t[1].xy[axis], t[2].xy[axis]];
        v[0].max(v[1]).max(v[2]) - v[0].min(v[1]).min(v[2])
    };
    span(0) > GPU_MAX_SPAN_X || span(1) > GPU_MAX_SPAN_Y
}

/// Where cut triangles are appended: the writer's cursor and the end of the
/// room the face may use.
struct CutOutput {
    next: *mut u32,
    limit: *mut u32,
    packets: u32,
    otz: u32,
    clut_word: u32,
    tpage_word: u32,
}

/// Write `t` as GT3 packets that fit the GPU's extent: whole when it does,
/// else its four midpoint triangles, `levels` deep at most. A piece that
/// does not fit the room left is dropped, as the whole was.
///
/// # Safety
/// `out.next..out.limit` must be writable.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn cut_triangle(out: &mut CutOutput, t: [CutVertex; 3], levels: u8) {
    if levels != 0 && cut_exceeds_gpu_extent(&t) {
        let m = [
            CutVertex::middle(&t[0], &t[1]),
            CutVertex::middle(&t[1], &t[2]),
            CutVertex::middle(&t[2], &t[0]),
        ];
        let next = levels - 1;
        for piece in [
            [t[0], m[0], m[2]],
            [m[0], t[1], m[1]],
            [m[2], m[1], t[2]],
            [m[0], m[1], m[2]],
        ] {
            // SAFETY: per the contract.
            unsafe { cut_triangle(out, piece, next) };
        }
        return;
    }
    let words = 1 + ClassicTriTexturedGouraud::WORDS as usize;
    // SAFETY: both pointers lie in the reserved room.
    if unsafe { out.limit.offset_from(out.next) } < words as isize {
        return;
    }
    let pack = |v: &CutVertex| (v.xy[0] as u16 as u32) | ((v.xy[1] as u16 as u32) << 16);
    let rgb =
        |v: &CutVertex| (v.rgb[0] as u32) | ((v.rgb[1] as u32) << 8) | ((v.rgb[2] as u32) << 16);
    let uv = |v: &CutVertex| (v.uv[0] as u32) | ((v.uv[1] as u32) << 8);
    let packet = [
        ((ClassicTriTexturedGouraud::WORDS as u32) << 24) | out.otz,
        packet::SHADED_TEXTURED_TRIANGLE | rgb(&t[0]),
        pack(&t[0]),
        uv(&t[0]) | out.clut_word,
        rgb(&t[1]),
        pack(&t[1]),
        uv(&t[1]) | out.tpage_word,
        rgb(&t[2]),
        pack(&t[2]),
        uv(&t[2]),
    ];
    for (index, word) in packet.into_iter().enumerate() {
        // SAFETY: room checked above.
        unsafe { ptr::write(out.next.add(index), word) };
    }
    // SAFETY: as above.
    out.next = unsafe { out.next.add(words) };
    out.packets += 1;
}

/// Repair the packets a face whose screen box is over the GPU's extent limit
/// wrote from `start`: a packet over the limit is taken out of the ordering
/// table (its slot becomes the skip value) and replaced by smaller triangles
/// cut at the edge midpoints of its screen vertices, which the GPU's affine
/// mapping draws as it would have drawn the whole, at the packet's own slot.
/// The pieces are appended inside the room the caller reserved for the face,
/// as many as fit. Rare, small and out of line.
///
/// # Safety
/// `start..writer.next` must be the face's packets and
/// `start..start + budget_words` writable.
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn split_oversize_packets(writer: &mut PacketWriter, start: *mut u32, budget_words: usize) {
    let end = writer.next;
    let mut out = CutOutput {
        next: end,
        // SAFETY: per the contract.
        limit: unsafe { start.add(budget_words) },
        packets: 0,
        otz: 0,
        clut_word: 0,
        tpage_word: 0,
    };
    let mut at = start;
    while at < end {
        // SAFETY: `at` is a packet tag inside the face's packets.
        let tag = unsafe { ptr::read(at) };
        let body = (tag >> 24) as usize;
        // The box of the packet's vertices: if it fits, every triangle in it
        // does, so only a packet over the limit goes to `cut_packet`.
        let corners = if body == ClassicQuadTexturedGouraud::WORDS as usize {
            4
        } else {
            3
        };
        let (mut x0, mut x1, mut y0, mut y1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        let mut corner = 0;
        while corner < corners {
            // SAFETY: inside the packet.
            let xy = unsafe { ptr::read(at.add(2 + 3 * corner)) };
            let (x, y) = (i32::from(xy as i16), i32::from((xy >> 16) as i16));
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
            corner += 1;
        }
        if (x1 - x0 > GPU_MAX_SPAN_X || y1 - y0 > GPU_MAX_SPAN_Y) && tag & 0xffff != 0xffff {
            // SAFETY: per the contract.
            unsafe { cut_packet(&mut out, at, tag, corners == 4) };
        }
        // SAFETY: the next packet, or `end`.
        at = unsafe { at.add(1 + body) };
    }
    writer.next = out.next;
    writer.packets += out.packets;
}

/// [`split_oversize_packets`] for one packet whose box is over the limit: if
/// one of the triangles the GPU draws from it is over the limit it leaves the
/// ordering table and its triangles are cut instead.
///
/// # Safety
/// As [`split_oversize_packets`], for the packet at `at`.
#[cold]
#[inline(never)]
#[cfg_attr(target_arch = "mips", optimize(size))]
unsafe fn cut_packet(out: &mut CutOutput, at: *mut u32, tag: u32, quad: bool) {
    // SAFETY: per the contract.
    let v = unsafe {
        [
            CutVertex::read(at, 0),
            CutVertex::read(at, 1),
            CutVertex::read(at, 2),
            CutVertex::read(at, if quad { 3 } else { 2 }),
        ]
    };
    let halves = [[v[0], v[1], v[2]], [v[1], v[2], v[3]]];
    if cut_exceeds_gpu_extent(&halves[0]) || (quad && cut_exceeds_gpu_extent(&halves[1])) {
        out.otz = tag & 0xffff;
        // SAFETY: per the contract.
        unsafe {
            out.clut_word = ptr::read(at.add(3)) & 0xffff_0000;
            out.tpage_word = ptr::read(at.add(6)) & 0xffff_0000;
            ptr::write(at, (tag & 0xffff_0000) | 0xffff);
            cut_triangle(out, halves[0], 3);
            if quad {
                cut_triangle(out, halves[1], 3);
            }
        }
    }
}

/// Project, split and write the packets of a batch of compact world faces:
/// the world affine path (see the module documentation).
///
/// `budget_q3` is the affine error allowed, in eighths of a pixel (128 is 16
/// pixels); a caller near the end of its packet storage can pass
/// [`WORLD_COARSE_ERROR_BUDGET_Q3`] to trade warp for packets instead of
/// dropping faces. Zero leaves only faces with no depth change unsplit.
///
/// The GTE view (rotation, translation, projection and ZSF3) must be loaded.
/// Vertices are projected in place.
///
/// # Safety
/// - `vertices` must hold `vertex_count` initialised records followed by
///   [`WORLD_SCRATCH_VERTICES`] writable records.
/// - Every surface's range must lie within `vertex_count`, with at least
///   three vertices.
/// - `output` must have room for [`WORLD_PACKET_WORDS_PER_TRIANGLE`] words
///   per fan triangle of the batch.
#[inline(never)]
pub unsafe fn submit_world_batch<L: WorldLattice>(
    vertices: *mut AffineVertex,
    vertex_count: usize,
    surfaces: *const WorldSurface,
    surface_count: usize,
    output: *mut u32,
    budget_q3: u8,
) -> SurfaceSubmit {
    if vertices.is_null()
        || surfaces.is_null()
        || output.is_null()
        || vertex_count == 0
        || surface_count == 0
    {
        return SurfaceSubmit {
            next_packet: output,
            packets: 0,
            hardware_triangles: 0,
        };
    }
    // The descriptor is read as two words, low halfword first.
    const _: () = assert!(cfg!(target_endian = "little"));

    let mut vertex = 0usize;
    // SAFETY: `vertex_count` records per the contract.
    unsafe {
        while vertex + 2 < vertex_count {
            project_three_consecutive(vertices.add(vertex));
            vertex += 3;
        }
        while vertex < vertex_count {
            project_one(vertices.add(vertex));
            vertex += 1;
        }
    }

    let budget = u32::from(budget_q3);
    let ot_depth = L::PROFILE.ot_depth;
    // SAFETY: the scratch records follow the batch.
    let generated = unsafe { vertices.add(vertex_count) };
    let mut next_packet = output;
    let mut packets = 0u32;
    // SAFETY: `surface_count` descriptors per the contract.
    let surface_end = unsafe { surfaces.add(surface_count) };
    let mut surface_ptr = surfaces;
    while surface_ptr != surface_end {
        // Two word loads instead of four halfword ones: the descriptors sit
        // in RAM, where each load stalls.
        // SAFETY: inside the descriptors; aligned to four.
        let [range, material] = unsafe { ptr::read(surface_ptr.cast::<[u32; 2]>()) };
        // SAFETY: inside the range the caller hands over.
        surface_ptr = unsafe { surface_ptr.add(1) };
        // Opaque per surface, so the loops below address the vertices from it
        // instead of from hoisted copies parked on the RAM stack.
        // SAFETY: the range lies in the batch per the contract.
        let first = unsafe { opaque(vertices).add((range & 0xffff) as usize) };
        let count = (range >> 16) as usize;
        debug_assert!(count >= 3);
        debug_assert!((range & 0xffff) as usize + count <= vertex_count);

        // Whole-surface screen rejection.
        let mut surface_clip = 0x0fu8;
        let mut clip_index = 0usize;
        while clip_index < count && surface_clip != 0 {
            // SAFETY: inside the surface.
            surface_clip &= screen_outcode::<L>(unsafe { (*first.add(clip_index)).screen });
            clip_index += 1;
        }
        if surface_clip != 0 {
            continue;
        }

        // SAFETY: the surface's projected records.
        let flagged =
            unsafe { face_level_flagged(first, count, budget, L::PROFILE.error_gate_depth) };
        let level = flagged & !EXTENT_BIG;
        if level != 0 {
            let mut lent = PacketWriter::with_material(next_packet, material, budget);
            // SAFETY: scratch and room per the contract.
            unsafe {
                if count == 4 {
                    split_quad::<L>(first, generated, &mut lent, level);
                } else {
                    split_fan::<L>(first, count, generated, &mut lent, level);
                }
                if flagged & EXTENT_BIG != 0 {
                    split_oversize_packets(
                        &mut lent,
                        next_packet,
                        (count - 2) * WORLD_PACKET_WORDS_PER_TRIANGLE,
                    );
                }
            }
            next_packet = lent.next;
            packets += lent.packets;
            continue;
        }
        if count == 4 {
            // The quad lattice at level zero: one GT4 in the GPU's Z order
            // when the face is in the table.
            // SAFETY: four records.
            let c = unsafe { [&*first, &*first.add(1), &*first.add(2), &*first.add(3)] };
            let depth_sum = c[0].depth as u16 as u32
                + c[1].depth as u16 as u32
                + c[2].depth as u16 as u32
                + c[3].depth as u16 as u32;
            let face_otz = (depth_sum >> 4) as u16;
            if face_otz != 0 && face_otz < ot_depth {
                // SAFETY: room per the contract.
                let next = unsafe {
                    leaf_quad_words(next_packet, c[1], c[2], c[0], c[3], face_otz, material)
                };
                packets += u32::from(next != next_packet);
                next_packet = next;
            }
            continue;
        }

        // Unsplit fan: adjacent roots at one ordering key pair into a GT4.
        // GP0 quads split on q1-q2; the order [previous, current, root, next]
        // puts that edge on the fan's shared 0-2 diagonal with the two
        // triangles in the staged stream's reverse link order, so the affine
        // anchors at the seam are exact. `previous` is always the vertex
        // before `current` (a pair steps both by two), and the root's depth is
        // a one-cycle scratchpad load: neither needs a register across the
        // leaf calls.
        // SAFETY: inside the surface.
        let root = unsafe { &*first };
        // SAFETY: inside the range the caller hands over.
        let end = unsafe { first.add(count) };
        // SAFETY: inside the range the caller hands over.
        let mut current = unsafe { first.add(2) };
        while current != end {
            // SAFETY: `current` is inside the surface, past its second vertex.
            let previous_ref = unsafe { &*current.sub(1) };
            // SAFETY: inside the range the caller hands over.
            let current_ref = unsafe { &*current };
            // SAFETY: inside the range the caller hands over.
            let root_depth = unsafe { ptr::read_volatile(ptr::addr_of!(root.depth)) } as u16;
            let otz = average3_depths(
                root_depth,
                previous_ref.depth as u16,
                current_ref.depth as u16,
            );
            if otz > 0 && otz < ot_depth {
                // SAFETY: inside the range the caller hands over.
                let next = unsafe { current.add(1) };
                if next != end {
                    // SAFETY: inside the surface.
                    let next_ref = unsafe { &*next };
                    let next_otz = average3_depths(
                        root_depth,
                        current_ref.depth as u16,
                        next_ref.depth as u16,
                    );
                    if next_otz > 0 && next_otz < ot_depth && next_otz == otz {
                        // SAFETY: room per the contract.
                        let emitted = unsafe {
                            leaf_quad_words(
                                next_packet,
                                previous_ref,
                                current_ref,
                                root,
                                next_ref,
                                otz,
                                material,
                            )
                        };
                        packets += u32::from(emitted != next_packet);
                        next_packet = emitted;
                        // SAFETY: inside the range the caller hands over.
                        current = unsafe { next.add(1) };
                        continue;
                    }
                }
                // SAFETY: room per the contract.
                let emitted = unsafe {
                    leaf_tri_words(next_packet, root, previous_ref, current_ref, otz, material)
                };
                packets += u32::from(emitted != next_packet);
                next_packet = emitted;
            }
            // SAFETY: inside the range the caller hands over.
            current = unsafe { current.add(1) };
        }
    }

    PacketWriter {
        packets,
        ..PacketWriter::with_material(next_packet, 0, 0)
    }
    .finish(output)
}

/// The hot loop's packet emitters as frameless leaf calls. Inlined, LLVM
/// merged the GT3 and GT4 store tails into one sequence addressed through a
/// register per packet word, and the registers that took pushed the material
/// words onto the RAM stack, reloaded for every packet. Each returns the
/// cursor after its packet.
///
/// # Safety
/// Room for the packet at `next`; the vertices are projected records.
#[inline(never)]
unsafe fn leaf_tri_words(
    next: *mut u32,
    a: *const V,
    b: *const V,
    c: *const V,
    otz: u16,
    material: u32,
) -> *mut u32 {
    let mut writer = PacketWriter::with_material(next, material, 0);
    // SAFETY: per the contract.
    unsafe { writer.emit_tri([&*a, &*b, &*c], otz) };
    writer.next
}

/// See [`leaf_tri_words`].
///
/// # Safety
/// As [`leaf_tri_words`].
#[inline(never)]
unsafe fn leaf_quad_words(
    next: *mut u32,
    a: *const V,
    b: *const V,
    c: *const V,
    d: *const V,
    otz: u16,
    material: u32,
) -> *mut u32 {
    let mut writer = PacketWriter::with_material(next, material, 0);
    // SAFETY: per the contract.
    unsafe { writer.emit_quad([&*a, &*b, &*c, &*d], otz) };
    writer.next
}

/// A four-corner face whose level is not zero: the quad lattice.
///
/// # Safety
/// As [`submit_quad_lattice`].
#[cold]
#[inline(never)]
unsafe fn split_quad<L: WorldLattice>(
    first: *mut V,
    generated: *mut V,
    writer: &mut PacketWriter,
    level: u8,
) {
    // SAFETY: per the contract.
    unsafe { submit_quad_lattice::<L>(first, generated, writer, level) };
}

/// The same pointer, opaque to the optimiser, so addresses derived from it
/// are recomputed where they are used instead of hoisted out of a loop and
/// spilled to the RAM stack.
#[inline(always)]
fn opaque(pointer: *mut V) -> *mut V {
    #[cfg(target_arch = "mips")]
    {
        let mut pointer = pointer;
        // SAFETY: an empty asm block that only names the register.
        unsafe {
            core::arch::asm!(
                "# {0}",
                inout(reg) pointer,
                options(nomem, nostack, preserves_flags)
            )
        };
        pointer
    }
    #[cfg(not(target_arch = "mips"))]
    pointer
}

/// A fan whose level is not zero: every root splits the same way (a
/// face-wide level never pairs roots). A face-wide level splits every
/// internal diagonal the same way on both sides, so only the face's own
/// edges are sealed. The split bodies inline here, so a face pays one call
/// and one register save, not one per root.
///
/// # Safety
/// `first` must hold `count` projected records and `generated`
/// [`WORLD_SCRATCH_VERTICES`] writable ones; room per the caller.
#[inline(never)]
unsafe fn split_fan<L: WorldLattice>(
    first: *mut V,
    count: usize,
    generated: *mut V,
    lent: &mut PacketWriter,
    level: u8,
) {
    // A local copy, so the cursor stays in registers across the roots.
    let mut writer = PacketWriter { ..*lent };
    let writer = &mut writer;
    // SAFETY: per the contract.
    let root = unsafe { &*first };
    let ot_depth = L::PROFILE.ot_depth;
    // SAFETY: inside the range the caller hands over.
    let end = unsafe { first.add(count) };
    // SAFETY: inside the range the caller hands over.
    let first_previous = unsafe { first.add(1) };
    let mut previous = first_previous;
    // SAFETY: inside the range the caller hands over.
    let mut current = unsafe { first.add(2) };
    while current != end {
        let scratch = opaque(generated);
        // SAFETY: inside the surface.
        let previous_ref = unsafe { &*previous };
        // SAFETY: inside the range the caller hands over.
        let current_ref = unsafe { &*current };
        let otz = average3_depths(
            root.depth as u16,
            previous_ref.depth as u16,
            current_ref.depth as u16,
        );
        if otz > 0 && otz < ot_depth {
            // SAFETY: inside the range the caller hands over.
            let next = unsafe { current.add(1) };
            let underdraw_edges =
                2 | u8::from(previous == first_previous) | (u8::from(next == end) << 2);
            // SAFETY: scratch and room per the contract.
            unsafe {
                if level == 2 {
                    subdivide_twice::<L>(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        scratch,
                        otz,
                        underdraw_edges,
                    );
                } else if level == 1 {
                    subdivide_once::<L>(
                        writer,
                        root,
                        previous_ref,
                        current_ref,
                        scratch,
                        otz,
                        underdraw_edges,
                    );
                } else {
                    writer.emit_tri([root, previous_ref, current_ref], otz);
                }
            }
        }
        previous = current;
        // SAFETY: inside the range the caller hands over.
        current = unsafe { current.add(1) };
    }
    lent.next = writer.next;
    lent.packets = writer.packets;
}
