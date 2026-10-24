//! The grid world renderer.
//!
//! A frame draws each visible room in turn:
//!
//! 1. [`GridRenderer::draw_room`] culls the room's cells by row spans against
//!    the near, left and right planes (the vertical planes are left to the
//!    per-corner outcodes: a column is floor to ceiling).
//! 2. Quads of the surviving cells are tested for facing before any vertex is
//!    projected; survivors stamp their corners in a per-room table.
//! 3. The stamped corners are projected on the GTE, three at a time, into a
//!    screen and depth cache.
//! 4. One textured Gouraud GP0 packet per surviving quad is written from the
//!    cache. A quad with a corner at or behind the near plane, or one the GPU
//!    would drop for its size, takes the shared near-plane path (world-space
//!    clip, exact reprojection, window clip and depth-band subdivision).
//!
//! Vertices are room-local `i16`. The caller sets the GTE translation to the
//! room's (rotated, camera-relative) origin before the call and passes the
//! frustum planes and camera in the same room-local space.

use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    submit_surface_batch_near, AffineSurface, AffineVertex, NearView, SurfaceProfile,
    AFFINE_PACKETS_PER_TRIANGLE,
};
use psx_gpu::material::TextureWindow;
use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use crate::format::*;
use crate::view::RoomView;

mod cards;
mod clip;
mod emit;
mod frustum;
mod panels;
mod projection;
mod select;
mod skin;

pub use cards::{
    draw_cards, draw_cards_marked, horizon_y, outer_bands, slot_clut_word, slot_origin, view_keep,
    view_toward, write_backdrop, Backdrop, Card, CardBand, CardCamera, CardTable, LiveSlot,
    ATLAS_COLUMNS, ATLAS_ROWS, BACKDROP_PACKET_WORDS, CARD_PACKET_WORDS, CARD_UNLIMITED, NO_SLOT,
    NO_VIEW, OUTER_LAYERS, PICTURE_ROWS, PITCH_TAN_Q12, SLOTS, SLOT_BYTES, SLOT_HALFWORDS,
    SLOT_ROWS, TEXELS, VIEWS, VIEW_COS_Q12, VIEW_SIN_Q12,
};
pub use clip::clip_polygon_plane;
#[cfg(target_arch = "mips")]
pub use frustum::load_gte_clip_planes;
pub use frustum::{FrustumPlanes, CLIP_ALL_PLANES};
pub use panels::{
    depth_byte, depth_units, draw_panel, PanelCamera, PanelRays, PanelRef, PanelScratch, CELLS,
    CELLS_X, CELLS_Y, CELL_EMPTY, CELL_H, CELL_MIXED, CELL_W, CENTRE_COLUMN, COLS,
    DEPTH_SECTOR_SHIFT, DEPTH_STEPS, HEADER_DEPTH_BYTES, HEADER_ROOMS_AT, HEADINGS, HORIZON_ROW,
    MESH, MESH_H, MESH_W, NO_TILE_SLOT, ROWS, TILES, TILE_TEXELS,
};
pub use projection::{
    classic_far_depth, load_view_rotation, view_rotation, Camera, ViewProjection, ViewTransform,
    VIEW_SCALE_Q12,
};
pub use skin::{plan_axes, SkinCamera, SkinRenderer, SKIN_PACKET_WORDS};

/// GP0 command byte of a textured Gouraud polygon, before the size and blend
/// bits.
pub const TEXTURED_GOURAUD_COMMAND: u32 = 0x3400_0000;

/// Depth-band profile of the near-plane path. A tile is small, so the warp a
/// near tile shows is bounded by its own size: one split inside 170 depth
/// units and none beyond, against the third-person profile's 340 and 170
/// for big patches.
pub(crate) const GRID_RENDER_PROFILE: SurfaceProfile = SurfaceProfile {
    split_once_below: 170,
    split_twice_below: 0,
    ..SurfaceProfile::THIRD_PERSON
};

/// A material slot as the runtime resolved it: page words and the texture's
/// page-local origin, or not resident while its texture is not uploaded.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridMaterial {
    /// Texture-page word including the blend bits.
    pub tpage: u16,
    pub clut: u16,
    /// Page-local origin of the texture, `u | v << 8`.
    pub uv_origin: u16,
    /// Extra GP0 command bits (the semi-transparency bit for blended
    /// materials), zero for opaque.
    pub command_bits: u32,
    pub resident: bool,
    /// The encoded GP0(E2) texture-window command that repeats this
    /// material's texture, or 0 when the texture has no window placement.
    /// Quads flagged [`QUAD2_REPEAT`] draw through it with raw UVs.
    pub window_word: u32,
}

impl GridMaterial {
    /// The UV origin to add and the GP0(E2) word a packet of `quad` carries.
    ///
    /// An ordinary quad adds the texture's page-local origin and resets the
    /// window. A `REPEAT` quad keeps its UVs raw and selects the material's
    /// window, whose offset puts them back on the texture; `None` when the
    /// material has no window to repeat through.
    #[inline(always)]
    pub(crate) fn addressing(&self, quad: &Quad, window_reset: u32) -> Option<(u32, u32)> {
        if quad.flags2 & QUAD2_REPEAT == 0 {
            Some((u32::from(self.uv_origin), window_reset))
        } else if self.window_word != 0 {
            Some((0, self.window_word))
        } else {
            None
        }
    }
}

/// What one frame produced. Counters add up across rooms.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridFrame {
    pub packet_words: usize,
    pub packets: u32,
    /// An open sector is inside the frustum.
    pub sky_visible: bool,
    /// The packet storage ran out; the caller may fence and draw again.
    pub overflowed: bool,
    pub cells_visible: u16,
    pub quads_visible: u16,
    pub vertices_projected: u16,
    pub robust_quads: u16,
    /// Packets the near-plane path produced.
    pub robust_packets: u16,
    pub backface_culled: u16,
    pub screen_culled: u16,
    pub material_missing: u16,
    pub depth_skipped: u16,
    /// Merged far quads queued, and cell quads they replaced.
    pub lod_quads: u16,
    pub lod_replaced: u16,
    /// Rooms not drawn because their tables outgrow the scratch (the core's
    /// maxima disagree with the chunk: a cook or version skew).
    pub rooms_refused: u16,
}

impl GridFrame {
    /// Fold the counters of a later room into this frame.
    pub fn absorb(&mut self, room: &GridFrame) {
        self.packet_words += room.packet_words;
        self.packets += room.packets;
        self.sky_visible |= room.sky_visible;
        self.overflowed |= room.overflowed;
        self.cells_visible = self.cells_visible.saturating_add(room.cells_visible);
        self.quads_visible = self.quads_visible.saturating_add(room.quads_visible);
        self.vertices_projected = self
            .vertices_projected
            .saturating_add(room.vertices_projected);
        self.robust_quads = self.robust_quads.saturating_add(room.robust_quads);
        self.robust_packets = self.robust_packets.saturating_add(room.robust_packets);
        self.backface_culled = self.backface_culled.saturating_add(room.backface_culled);
        self.screen_culled = self.screen_culled.saturating_add(room.screen_culled);
        self.material_missing = self.material_missing.saturating_add(room.material_missing);
        self.depth_skipped = self.depth_skipped.saturating_add(room.depth_skipped);
        self.lod_quads = self.lod_quads.saturating_add(room.lod_quads);
        self.lod_replaced = self.lod_replaced.saturating_add(room.lod_replaced);
        self.rooms_refused = self.rooms_refused.saturating_add(room.rooms_refused);
    }
}

/// The phases of one room draw, for timing marks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GridPhase {
    Cells,
    Quads,
    Project,
    Emit,
}

/// Projected corner: packed screen word, GTE depth and an outcode.
#[repr(C)]
#[derive(Copy, Clone, Default)]
pub(crate) struct GridScreen {
    pub sxy: u32,
    pub sz: u16,
    pub code: u16,
}

pub(crate) const OFF_LEFT: u16 = 1;
pub(crate) const OFF_RIGHT: u16 = 2;
pub(crate) const OFF_TOP: u16 = 4;
pub(crate) const OFF_BOTTOM: u16 = 8;
pub(crate) const NEAR: u16 = 16;
pub(crate) const OFF_ANY: u16 = OFF_LEFT | OFF_RIGHT | OFF_TOP | OFF_BOTTOM;

/// Words of the largest fast-path packet: tag, window reset, four corners.
pub(crate) const QUAD_PACKET_WORDS: usize = 14;
/// Worst case for one quad through the shared path (two fan triangles).
pub(crate) const ROBUST_WORST_WORDS: usize = AFFINE_PACKETS_PER_TRIANGLE * 15 * 3;

/// Everything a room draw needs besides the room's data.
pub struct RoomCamera<'a> {
    /// Frustum planes in room-local space.
    pub frustum: &'a FrustumPlanes,
    /// View rotation and the room-local GTE translation.
    pub view: &'a ViewTransform,
    /// The eye in room-local runtime units.
    pub camera: [i32; 3],
    /// Screen size in pixels.
    pub screen: [i32; 2],
    pub focal_length: i32,
    /// Runtime units per sector edge.
    pub sector: i32,
}

/// Per-frame working storage, sized for the largest room and reused.
pub struct GridRenderer {
    pub(crate) frame: u16,
    pub(crate) stamp: Vec<u16>,
    pub(crate) screen: Vec<GridScreen>,
    pub(crate) work: Vec<u16>,
    pub(crate) cells: Vec<u16>,
    /// Parallel to `cells`: 1 when the cell's block draws merged quads.
    pub(crate) far: Vec<u8>,
    pub(crate) quads: Vec<u16>,
    /// Frame stamp per LOD block, so each block's merged quads queue once.
    pub(crate) block_stamp: Vec<u16>,
    pub(crate) sky_stamp: Vec<u16>,
    pub(crate) lod_queue: Vec<u16>,
}

impl GridRenderer {
    /// Storage for rooms of up to `max_vertices` vertices, `max_cells` cells
    /// and `max_quads` quads.
    pub fn new(
        max_vertices: usize,
        max_cells: usize,
        max_quads: usize,
        max_blocks: usize,
        max_lod_quads: usize,
    ) -> Self {
        Self {
            frame: 0,
            stamp: vec![0; max_vertices],
            screen: vec![GridScreen::default(); max_vertices],
            work: Vec::with_capacity(max_vertices),
            cells: Vec::with_capacity(max_cells),
            far: Vec::with_capacity(max_cells),
            quads: Vec::with_capacity(max_quads + max_lod_quads),
            block_stamp: vec![0; max_blocks],
            sky_stamp: vec![0; max_blocks],
            lod_queue: Vec::with_capacity(max_lod_quads),
        }
    }

    /// Storage sized from the scratch maxima the level core carries, so it
    /// is right before any room chunk is resident and for any room that
    /// arrives later.
    pub fn with_maxima(maxima: &RenderMaxima) -> Self {
        Self::new(
            usize::from(maxima.vertices),
            usize::from(maxima.cells),
            usize::from(maxima.quads),
            usize::from(maxima.lod_blocks),
            usize::from(maxima.lod_quads),
        )
    }

    /// Storage sized for the largest room among `rooms`.
    pub fn for_rooms<'a>(rooms: impl Iterator<Item = &'a RoomView<'a>>) -> Self {
        let (mut v, mut c, mut q, mut b, mut l) = (0, 0, 0, 0, 0);
        for room in rooms {
            v = v.max(room.render.vertices.len());
            c = c.max(room.render.cells.len());
            q = q.max(room.render.quads.len());
            b = b.max(room.render.lod_blocks.len());
            l = l.max(room.render.lod_quads.len());
        }
        Self::new(v, c, q, b, l)
    }

    /// Whether the scratch is big enough for `room`. It always is when the
    /// scratch came from the core's maxima and the core matches its chunks.
    #[inline]
    fn fits(&self, room: &RoomView<'_>) -> bool {
        let r = &room.render;
        r.vertices.len() <= self.stamp.len()
            && r.lod_blocks.len() <= self.block_stamp.len()
            && r.cells.len() <= self.cells.capacity()
            && r.quads.len() + r.lod_quads.len() <= self.quads.capacity()
            && r.lod_quads.len() <= self.lod_queue.capacity()
    }

    /// Bytes of per-frame working storage this renderer holds.
    pub fn working_bytes(&self) -> usize {
        self.stamp.capacity() * 2
            + self.screen.capacity() * core::mem::size_of::<GridScreen>()
            + self.work.capacity() * 2
            + self.cells.capacity() * 3
            + self.quads.capacity() * 2
            + self.block_stamp.capacity() * 4
            + self.lod_queue.capacity() * 2
    }

    /// Draw one room into `out`. The GTE must already hold the view rotation,
    /// the room's translation, screen offset, projection plane and Z weights.
    /// With `reverse` the stream is written against the end of `out`
    /// instead of from its start (see `GridFrame::packet_words`).
    #[allow(clippy::too_many_arguments)]
    pub fn draw_room(
        &mut self,
        room: &RoomView<'_>,
        eye: &RoomCamera<'_>,
        materials: &[GridMaterial],
        out: &mut [u32],
        reverse: bool,
    ) -> GridFrame {
        self.draw_room_with_marks(room, eye, materials, out, reverse, &mut |_, _| {})
    }

    /// Like [`Self::draw_room`], calling `mark(phase, true)` before and
    /// `mark(phase, false)` after each phase so a caller can time them.
    pub fn draw_room_with_marks(
        &mut self,
        room: &RoomView<'_>,
        eye: &RoomCamera<'_>,
        materials: &[GridMaterial],
        out: &mut [u32],
        reverse: bool,
        mark: &mut dyn FnMut(GridPhase, bool),
    ) -> GridFrame {
        let mut frame = GridFrame::default();
        if !self.fits(room) {
            frame.rooms_refused = 1;
            return frame;
        }
        self.frame = self.frame.wrapping_add(1);
        if self.frame == 0 {
            self.stamp.fill(0);
            self.block_stamp.fill(0);
            self.sky_stamp.fill(0);
            self.frame = 1;
        }
        #[cfg(target_arch = "mips")]
        load_gte_clip_planes(&eye.frustum.planes);

        mark(GridPhase::Cells, true);
        self.select_cells(room, eye.frustum, eye.sector, eye.camera, &mut frame);
        mark(GridPhase::Cells, false);
        mark(GridPhase::Quads, true);
        self.select_quads(room, eye.frustum, eye.sector, eye.camera, &mut frame);
        mark(GridPhase::Quads, false);
        let near_sz = (eye.focal_length / 2 + 2).clamp(1, i32::from(u16::MAX)) as u16;
        mark(GridPhase::Project, true);
        self.project_corners(room, eye.screen, near_sz, &mut frame);
        mark(GridPhase::Project, false);
        mark(GridPhase::Emit, true);
        self.emit(room, eye, materials, out, &mut frame, reverse);
        mark(GridPhase::Emit, false);
        frame
    }
}
