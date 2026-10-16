//! Tomb Raider-style grid world renderer (open-hall experiment).
//!
//! The world is a lattice of sector quads. The cooker (`psxed-project`
//! `brush_grid`) snaps every corner to the PXBSP lattice and deduplicates
//! them, so a corner shared by neighbouring tiles is one vertex. A frame:
//!
//! 1. Cull macro cells (four by four sectors), then the sectors of the
//!    survivors, against the five frustum planes with the hierarchical box
//!    test the BSP renderer uses.
//! 2. Walk the quads of the visible sectors. A quad whose plane faces away
//!    from the camera is dropped here, before any vertex of it is projected.
//!    Surviving quads mark their corners in a per-frame stamp table and queue
//!    each corner once.
//! 3. Project the queued corners on the GTE, three at a time, into a screen
//!    and depth cache.
//! 4. Write one textured Gouraud GP0 quad per surviving quad from the cache.
//!    A quad with a corner at or behind the near plane, or one the GPU would
//!    drop for its size, takes the shared near-plane path (world-space clip,
//!    exact reprojection, window clip and depth-band subdivision).
//!
//! No runtime subdivision on the fast path: tiles are small.
//!
//! Pack layout (little endian, mirrored by `psxed-project::brush_grid`):
//! sixteen header words, then material asset ids (`u16`, padded to a word),
//! vertices (8 bytes), cells (8), macro cells (8), quads (44).

use alloc::vec;
use alloc::vec::Vec;

use psx_engine::{
    submit_surface_batch_near, AffineSurface, AffineVertex, NearView, AFFINE_PACKETS_PER_TRIANGLE,
};
use psx_gpu::material::TextureWindow;
use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use psx_engine::SurfaceProfile;

use super::{
    clip_polygon_plane, open_hall_bands, FrustumPlanes, ViewTransform, PXBSP_CLIP_ALL_PLANES,
    TEXTURED_GOURAUD_COMMAND,
};

/// Depth-band profile of the near-plane path. A tile is small, so the warp a
/// near tile shows is bounded by its own size: one split inside 170 depth
/// units and none beyond, against the BSP profile's 340 and 170 for big
/// patches. `OPEN_HALL_SPLIT_BANDS` still overrides it at build time.
const GRID_RENDER_PROFILE: SurfaceProfile = open_hall_bands(SurfaceProfile {
    split_once_below: 170,
    split_twice_below: 0,
    ..SurfaceProfile::PXBSP_THIRD_PERSON
});
use crate::Vec3I16;

const MAGIC: u32 = 0x3144_5247;
const VERSION: u32 = 1;
const HEADER_WORDS: usize = 16;

/// Quad flag: three corners, the fourth index repeats the third.
pub const GRID_QUAD_TRIANGLE: u8 = 1;
/// Quad flag: the plane is axis aligned. `GridQuad::d` is then the signed plane
/// coordinate, bits 2 and 3 of the flags name the axis and bit 4 marks a
/// negative-facing plane, so the side test needs no multiplies.
pub const GRID_QUAD_AXIS: u8 = 2;
pub const GRID_QUAD_AXIS_SHIFT: u32 = 2;
pub const GRID_QUAD_AXIS_NEGATIVE: u8 = 0x10;

const _: () = assert!(cfg!(target_endian = "little"));

/// One lattice corner, world units. The pad keeps the GTE's `VZ` word clean.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridVertex {
    pub x: i16,
    pub y: i16,
    pub z: i16,
    pad: i16,
}

/// One sector's slice of the quad table and its vertical extent.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridCell {
    pub first: u16,
    pub count: u16,
    pub min_y: i16,
    pub max_y: i16,
}

/// Vertical extent of a four by four block of sectors, and whether any
/// sector in it is open to the sky.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridMacro {
    pub min_y: i16,
    pub max_y: i16,
    pub flags: u16,
    pad: u16,
}

/// One tile: a quad (or triangle) of four lattice corners.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridQuad {
    /// Corner indices in ring order.
    pub v: [u16; 4],
    /// Baked corner colours, `r | g << 8 | b << 16`.
    pub color: [u32; 4],
    /// Material-relative texels, `u | v << 8`, per corner.
    pub uv: [u16; 4],
    /// Plane constant for the Q7 normal: front when `n . camera > d`.
    pub d: i32,
    pub normal: [i8; 3],
    pub material: u8,
    pub flags: u8,
    pad: [u8; 3],
}

const _: () = assert!(core::mem::size_of::<GridVertex>() == 8);
const _: () = assert!(core::mem::size_of::<GridCell>() == 8);
const _: () = assert!(core::mem::size_of::<GridMacro>() == 8);
const _: () = assert!(core::mem::size_of::<GridQuad>() == 44);

/// Why a pack was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GridPackError {
    Truncated,
    BadMagic,
    BadVersion,
    Misaligned,
    Inconsistent,
}

/// A parsed pack. Tables borrow the pack bytes.
#[derive(Copy, Clone)]
pub struct GridPack<'a> {
    pub materials: &'a [u16],
    pub vertices: &'a [GridVertex],
    pub cells: &'a [GridCell],
    pub macros: &'a [GridMacro],
    pub quads: &'a [GridQuad],
    pub grid_w: usize,
    pub grid_d: usize,
    pub origin: [i32; 2],
    pub cell_size: i32,
    pub lid_y: i32,
    pub macro_w: usize,
    pub macro_d: usize,
    pub macro_shift: u32,
    /// Lowest and highest corner height in the pack, for the row-span cull.
    pub min_y: i32,
    pub max_y: i32,
    /// Whether any macro block holds an open sector.
    pub has_open_blocks: bool,
}

/// Floor of `a / b` for a non-zero `b` of either sign.
fn div_floor_i32(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Ceiling of `a / b` for a non-zero `b` of either sign.
fn div_ceil_i32(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && ((a < 0) == (b < 0)) {
        q + 1
    } else {
        q
    }
}

fn word(bytes: &[u8], index: usize) -> u32 {
    let at = index * 4;
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn table<'a, T: Copy>(
    bytes: &'a [u8],
    offset: &mut usize,
    count: usize,
) -> Result<&'a [T], GridPackError> {
    let size = core::mem::size_of::<T>();
    let length = count.checked_mul(size).ok_or(GridPackError::Truncated)?;
    let end = offset.checked_add(length).ok_or(GridPackError::Truncated)?;
    if end > bytes.len() {
        return Err(GridPackError::Truncated);
    }
    let pointer = bytes[*offset..].as_ptr();
    if !(pointer as usize).is_multiple_of(core::mem::align_of::<T>()) {
        return Err(GridPackError::Misaligned);
    }
    *offset = end;
    // SAFETY: bounds and alignment were checked above, the records are plain
    // integers with no invalid bit patterns, and the target is little endian.
    Ok(unsafe { core::slice::from_raw_parts(pointer.cast::<T>(), count) })
}

impl<'a> GridPack<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, GridPackError> {
        if bytes.len() < HEADER_WORDS * 4 {
            return Err(GridPackError::Truncated);
        }
        if word(bytes, 0) != MAGIC {
            return Err(GridPackError::BadMagic);
        }
        if word(bytes, 1) != VERSION {
            return Err(GridPackError::BadVersion);
        }
        let grid_w = (word(bytes, 2) & 0xffff) as usize;
        let grid_d = (word(bytes, 2) >> 16) as usize;
        let origin = [word(bytes, 3) as i32, word(bytes, 4) as i32];
        let cell_size = i32::from(word(bytes, 5) as u16 as i16);
        let lid_y = i32::from((word(bytes, 5) >> 16) as u16 as i16);
        let vertex_count = word(bytes, 6) as usize;
        let quad_count = word(bytes, 7) as usize;
        let cell_count = word(bytes, 8) as usize;
        let macro_count = word(bytes, 9) as usize;
        let material_count = word(bytes, 10) as usize;
        let macro_w = (word(bytes, 11) & 0xffff) as usize;
        let macro_d = (word(bytes, 11) >> 16) as usize;
        let macro_shift = word(bytes, 12);
        if cell_count != grid_w * grid_d || macro_count != macro_w * macro_d || cell_size <= 0 {
            return Err(GridPackError::Inconsistent);
        }
        let mut offset = HEADER_WORDS * 4;
        let materials = table::<u16>(bytes, &mut offset, material_count)?;
        offset = (offset + 3) & !3;
        let vertices = table::<GridVertex>(bytes, &mut offset, vertex_count)?;
        let cells = table::<GridCell>(bytes, &mut offset, cell_count)?;
        let macros = table::<GridMacro>(bytes, &mut offset, macro_count)?;
        let quads = table::<GridQuad>(bytes, &mut offset, quad_count)?;
        // Every index the renderer follows without a bounds check.
        for cell in cells {
            if usize::from(cell.first) + usize::from(cell.count) > quad_count {
                return Err(GridPackError::Inconsistent);
            }
        }
        for quad in quads {
            if quad.v.iter().any(|&v| usize::from(v) >= vertex_count)
                || (quad.flags & GRID_QUAD_AXIS != 0
                    && (quad.flags >> GRID_QUAD_AXIS_SHIFT) & 3 > 2)
                || usize::from(quad.material) >= material_count
            {
                return Err(GridPackError::Inconsistent);
            }
        }
        let mut min_y = i32::MAX;
        let mut max_y = i32::MIN;
        let mut has_open_blocks = false;
        for block in macros {
            if block.min_y <= block.max_y {
                min_y = min_y.min(i32::from(block.min_y));
                max_y = max_y.max(i32::from(block.max_y));
            }
            has_open_blocks |= block.flags & 1 != 0;
        }
        if min_y > max_y {
            min_y = 0;
            max_y = 0;
        }
        Ok(Self {
            materials,
            vertices,
            cells,
            macros,
            quads,
            grid_w,
            grid_d,
            origin,
            cell_size,
            lid_y,
            macro_w,
            macro_d,
            macro_shift,
            min_y,
            max_y,
            has_open_blocks,
        })
    }
}

/// A material slot as the runtime resolved it: page words and the texture's
/// page-local origin, or `None` while its texture is not resident.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridMaterial {
    /// Texture-page word with the blend bits clear.
    pub tpage: u16,
    pub clut: u16,
    /// Page-local origin of the texture, `u | v << 8`.
    pub uv_origin: u16,
    pub resident: bool,
}

/// What one frame produced.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GridFrame {
    pub packet_words: usize,
    pub packets: u32,
    /// An open sector's sky lid is inside the frustum.
    pub sky_visible: bool,
    /// The packet storage ran out; the caller may fence and draw again.
    pub overflowed: bool,
    pub cells_visible: u16,
    pub quads_visible: u16,
    pub vertices_projected: u16,
    pub robust_quads: u16,
    /// Quads tested for facing, dropped as back-facing, rejected off screen,
    /// skipped for a missing material, skipped for an OT depth out of range.
    pub backface_culled: u16,
    pub screen_culled: u16,
    pub material_missing: u16,
    pub depth_skipped: u16,
}

/// The phases of one grid frame, for timing marks.
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
struct GridScreen {
    sxy: u32,
    sz: u16,
    code: u16,
}

/// Outcode bits.
const OFF_LEFT: u16 = 1;
const OFF_RIGHT: u16 = 2;
const OFF_TOP: u16 = 4;
const OFF_BOTTOM: u16 = 8;
const NEAR: u16 = 16;
const OFF_ANY: u16 = OFF_LEFT | OFF_RIGHT | OFF_TOP | OFF_BOTTOM;

/// Words of the largest fast-path packet: tag, window reset, four corners.
const QUAD_PACKET_WORDS: usize = 14;
/// Worst case for one quad through the shared path (two fan triangles).
const ROBUST_WORST_WORDS: usize = AFFINE_PACKETS_PER_TRIANGLE * 15 * 3;

pub struct GridRenderer {
    frame: u16,
    stamp: Vec<u16>,
    screen: Vec<GridScreen>,
    work: Vec<u16>,
    cells: Vec<u16>,
    quads: Vec<u16>,
}

impl GridRenderer {
    pub fn new(pack: &GridPack<'_>) -> Self {
        Self {
            frame: 0,
            stamp: vec![0; pack.vertices.len()],
            screen: vec![GridScreen::default(); pack.vertices.len()],
            work: Vec::with_capacity(pack.vertices.len()),
            cells: Vec::with_capacity(pack.cells.len()),
            quads: Vec::with_capacity(pack.quads.len()),
        }
    }

    /// Bytes of per-frame working storage this renderer holds.
    pub fn working_bytes(&self) -> usize {
        self.stamp.capacity() * 2
            + self.screen.capacity() * core::mem::size_of::<GridScreen>()
            + self.work.capacity() * 2
            + self.cells.capacity() * 2
            + self.quads.capacity() * 2
    }

    /// Draw the visible grid into `out`.
    ///
    /// The GTE must already hold the view (`view`), screen offset, projection
    /// plane and Z weights. `camera` is the eye in world units.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        pack: &GridPack<'_>,
        frustum: &FrustumPlanes,
        view: &ViewTransform,
        camera: [i32; 3],
        screen: [i32; 2],
        focal_length: i32,
        materials: &[GridMaterial],
        out: &mut [u32],
    ) -> GridFrame {
        self.draw_with_marks(
            pack,
            frustum,
            view,
            camera,
            screen,
            focal_length,
            materials,
            out,
            &mut |_, _| {},
        )
    }

    /// Like [`Self::draw`], calling `mark(phase, true)` before and
    /// `mark(phase, false)` after each phase so a caller can time them.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_marks(
        &mut self,
        pack: &GridPack<'_>,
        frustum: &FrustumPlanes,
        view: &ViewTransform,
        camera: [i32; 3],
        screen: [i32; 2],
        focal_length: i32,
        materials: &[GridMaterial],
        out: &mut [u32],
        mark: &mut dyn FnMut(GridPhase, bool),
    ) -> GridFrame {
        let mut frame = GridFrame::default();
        self.frame = self.frame.wrapping_add(1);
        if self.frame == 0 {
            self.stamp.fill(0);
            self.frame = 1;
        }
        #[cfg(target_arch = "mips")]
        super::load_gte_clip_planes(&frustum.planes);

        mark(GridPhase::Cells, true);
        self.select_cells(pack, frustum, &mut frame);
        mark(GridPhase::Cells, false);
        mark(GridPhase::Quads, true);
        self.select_quads(pack, camera, &mut frame);
        mark(GridPhase::Quads, false);
        let near_sz = (focal_length / 2 + 2).clamp(1, i32::from(u16::MAX)) as u16;
        mark(GridPhase::Project, true);
        self.project_corners(pack, screen, near_sz, &mut frame);
        mark(GridPhase::Project, false);
        mark(GridPhase::Emit, true);
        self.emit(
            pack,
            frustum,
            view,
            screen,
            focal_length,
            materials,
            out,
            &mut frame,
        );
        mark(GridPhase::Emit, false);
        frame
    }

    /// Frustum cull by row spans. For each grid row, the near, left and right
    /// planes bound an interval of x; the columns inside it are queued. The
    /// vertical planes are not tested here: a column is floor to ceiling, so
    /// few are ever wholly above or below the view, and the per-corner
    /// outcodes reject what is off screen. Also decides whether any open
    /// sector's lid shows.
    #[inline(never)]
    fn select_cells(
        &mut self,
        pack: &GridPack<'_>,
        frustum: &FrustumPlanes,
        frame: &mut GridFrame,
    ) {
        let size = pack.cell_size;
        let clamp = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        // Sky: only the macro blocks that hold an open sector need a test.
        if pack.has_open_blocks {
            let shift = pack.macro_shift;
            let span = (1i32 << shift) * size;
            'sky: for mz in 0..pack.macro_d {
                for mx in 0..pack.macro_w {
                    // SAFETY: parse proved macros.len() == macro_w * macro_d.
                    let block = unsafe { pack.macros.get_unchecked(mz * pack.macro_w + mx) };
                    if block.flags & 1 == 0 {
                        continue;
                    }
                    let x0 = pack.origin[0] + ((mx << shift) as i32) * size;
                    let z0 = pack.origin[1] + ((mz << shift) as i32) * size;
                    // The open column, from the lowest geometry in the block up
                    // to the lid: the sky shows wherever such a column is in
                    // view, not only when the thin lid itself is.
                    let lid_min = Vec3I16 {
                        x: clamp(x0),
                        y: block.min_y,
                        z: clamp(z0),
                    };
                    let lid_max = Vec3I16 {
                        x: clamp(x0 + span),
                        y: clamp(pack.lid_y + 1),
                        z: clamp(z0 + span),
                    };
                    if frustum
                        .cull_aabb(lid_min, lid_max, PXBSP_CLIP_ALL_PLANES)
                        .is_some()
                    {
                        frame.sky_visible = true;
                        break 'sky;
                    }
                }
            }
        }
        let out = self.cells.as_mut_ptr();
        let mut count = 0usize;
        let planes = &frustum.planes;
        let (y_lo, y_hi) = (pack.min_y, pack.max_y);
        let grid_w = pack.grid_w as i32;
        for cz in 0..pack.grid_d {
            let z0 = pack.origin[1] + (cz as i32) * size;
            let z1 = z0 + size;
            let mut lo = -(1 << 20);
            let mut hi = 1 << 20;
            let mut empty = false;
            for (index, (normal, constant)) in planes.iter().take(3).enumerate() {
                let error = if index == 0 { 0 } else { frustum.side_error };
                // The point of the row slab that maximises the plane distance
                // apart from x.
                let y = if normal[1] > 0 { y_hi } else { y_lo };
                let z = if normal[2] > 0 { z1 } else { z0 };
                let rest = normal[1]
                    .wrapping_mul(y)
                    .wrapping_add(normal[2].wrapping_mul(z))
                    .wrapping_add(*constant)
                    .wrapping_add(error);
                // Visible needs `normal.x * x + rest >= 0`.
                let need = rest.wrapping_neg();
                let nx = normal[0];
                if nx > 0 {
                    lo = lo.max(div_ceil_i32(need, nx));
                } else if nx < 0 {
                    hi = hi.min(div_floor_i32(need, nx));
                } else if need > 0 {
                    empty = true;
                    break;
                }
            }
            if empty || lo > hi {
                continue;
            }
            let first = div_floor_i32(lo - pack.origin[0], size).max(0);
            let last = div_floor_i32(hi - pack.origin[0], size).min(grid_w - 1);
            let row = cz * pack.grid_w;
            let mut cx = first;
            while cx <= last {
                let index = row + cx as usize;
                // SAFETY: 0 <= cx < grid_w and cz < grid_d.
                if unsafe { pack.cells.get_unchecked(index) }.count != 0 {
                    // SAFETY: at most one entry per cell, and `cells` holds
                    // capacity for every cell in the pack.
                    unsafe { *out.add(count) = index as u16 };
                    count += 1;
                }
                cx += 1;
            }
        }
        // SAFETY: `count` never exceeds the capacity.
        unsafe { self.cells.set_len(count) };
        frame.cells_visible = count as u16;
    }

    /// Drop quads facing away from the camera and queue the corners of the
    /// rest.
    #[inline(never)]
    fn select_quads(&mut self, pack: &GridPack<'_>, camera: [i32; 3], frame: &mut GridFrame) {
        let [cx, cy, cz] = camera;
        let cam = [cx, cy, cz];
        let frame_stamp = self.frame;
        let stamp = self.stamp.as_mut_ptr();
        let quads_out = self.quads.as_mut_ptr();
        let work_out = self.work.as_mut_ptr();
        let mut quad_count = 0usize;
        let mut tested = 0usize;
        let mut work_count = 0usize;
        for &cell_index in &self.cells {
            // SAFETY: cell indices were produced from valid grid coordinates.
            let cell = unsafe { pack.cells.get_unchecked(usize::from(cell_index)) };
            let first = usize::from(cell.first);
            for q in first..first + usize::from(cell.count) {
                // SAFETY: parse proved first + count <= quads.len().
                let quad = unsafe { pack.quads.get_unchecked(q) };
                tested += 1;
                let flags = quad.flags;
                let front = if flags & GRID_QUAD_AXIS != 0 {
                    // Axis-aligned plane: the camera coordinate against the
                    // plane coordinate, no multiplies.
                    let axis = usize::from((flags >> GRID_QUAD_AXIS_SHIFT) & 3);
                    // SAFETY: the cooker emits axis 0..=2 only (parse checks it).
                    let c = unsafe { *cam.get_unchecked(axis) };
                    (if flags & GRID_QUAD_AXIS_NEGATIVE != 0 {
                        -c
                    } else {
                        c
                    }) > quad.d
                } else {
                    i32::from(quad.normal[0]) * cx
                        + i32::from(quad.normal[1]) * cy
                        + i32::from(quad.normal[2]) * cz
                        > quad.d
                };
                if !front {
                    continue;
                }
                // SAFETY: at most one entry per quad, and `quads` holds
                // capacity for every quad in the pack.
                unsafe {
                    *quads_out.add(quad_count) = q as u16;
                }
                quad_count += 1;
                // SAFETY: parse proved every index is below the vertex count,
                // which sized the stamp table and the work queue (each vertex
                // is queued at most once per frame).
                unsafe {
                    let mut corner = |v: u16| {
                        let slot = stamp.add(usize::from(v));
                        if *slot != frame_stamp {
                            *slot = frame_stamp;
                            *work_out.add(work_count) = v;
                            work_count += 1;
                        }
                    };
                    corner(quad.v[0]);
                    corner(quad.v[1]);
                    corner(quad.v[2]);
                    corner(quad.v[3]);
                }
            }
        }
        // SAFETY: the counts above never exceed the capacities.
        unsafe {
            self.quads.set_len(quad_count);
            self.work.set_len(work_count);
        }
        frame.quads_visible = quad_count as u16;
        frame.backface_culled = (tested - quad_count) as u16;
    }

    /// Project every queued corner, three per GTE command.
    #[inline(never)]
    fn project_corners(
        &mut self,
        pack: &GridPack<'_>,
        screen: [i32; 2],
        near_sz: u16,
        frame: &mut GridFrame,
    ) {
        let count = self.work.len();
        frame.vertices_projected = count as u16;
        let out = self.screen.as_mut_ptr();
        let work = self.work.as_ptr();
        let vertex = |index: u16| -> GteVec3I16 {
            // SAFETY: queued indices are below the vertex count.
            let v = unsafe { pack.vertices.get_unchecked(usize::from(index)) };
            GteVec3I16::new(v.x, v.y, v.z)
        };
        let store = |index: u16, p: scene::Projected| {
            let sx = i32::from(p.sx);
            let sy = i32::from(p.sy);
            // A corner at or behind the near plane has no meaningful screen
            // position (the GTE saturates the divide), so it carries only the
            // NEAR bit and the quad takes the clipping path instead of being
            // rejected on garbage coordinates.
            let mut code = 0u16;
            if p.sz < near_sz {
                code = NEAR;
            } else {
                if sx < 0 {
                    code |= OFF_LEFT;
                }
                if sx >= screen[0] {
                    code |= OFF_RIGHT;
                }
                if sy < 0 {
                    code |= OFF_TOP;
                }
                if sy >= screen[1] {
                    code |= OFF_BOTTOM;
                }
            }
            // SAFETY: as above.
            unsafe {
                *out.add(usize::from(index)) = GridScreen {
                    sxy: (p.sx as u16 as u32) | ((p.sy as u16 as u32) << 16),
                    sz: p.sz,
                    code,
                };
            }
        };
        let mut i = 0;
        while i + 3 <= count {
            // SAFETY: i + 2 < count.
            let (a, b, c) = unsafe { (*work.add(i), *work.add(i + 1), *work.add(i + 2)) };
            let p = scene::project_triangle_scheduled(vertex(a), vertex(b), vertex(c));
            store(a, p[0]);
            store(b, p[1]);
            store(c, p[2]);
            i += 3;
        }
        while i < count {
            // SAFETY: i < count.
            let a = unsafe { *work.add(i) };
            store(a, scene::project_vertex_scheduled(vertex(a)));
            i += 1;
        }
    }
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn emit(
        &mut self,
        pack: &GridPack<'_>,
        frustum: &FrustumPlanes,
        view: &ViewTransform,
        screen: [i32; 2],
        focal_length: i32,
        materials: &[GridMaterial],
        out: &mut [u32],
        frame: &mut GridFrame,
    ) {
        let start = out.as_mut_ptr();
        // SAFETY: one past the end of the slice.
        let end = unsafe { start.add(out.len()) };
        let mut next = start;
        let mut packets = 0u32;
        let cache = self.screen.as_ptr();
        let window_reset = TextureWindow::NONE.word();
        let mut robust_quads = 0u16;
        for &q in &self.quads {
            // SAFETY: queued from valid quad indices.
            let quad = unsafe { pack.quads.get_unchecked(usize::from(q)) };
            // SAFETY: parse proved every corner index is in range. A triangle
            // repeats its third corner, so the fourth lookup is harmless.
            let (s0, s1, s2, s3) = unsafe {
                (
                    &*cache.add(usize::from(quad.v[0])),
                    &*cache.add(usize::from(quad.v[1])),
                    &*cache.add(usize::from(quad.v[2])),
                    &*cache.add(usize::from(quad.v[3])),
                )
            };
            if s0.code & s1.code & s2.code & s3.code & OFF_ANY != 0 {
                frame.screen_culled += 1;
                continue;
            }
            let Some(material) = materials.get(usize::from(quad.material)) else {
                frame.material_missing += 1;
                continue;
            };
            if !material.resident {
                frame.material_missing += 1;
                continue;
            }
            let any = s0.code | s1.code | s2.code | s3.code;
            let triangle = quad.flags & GRID_QUAD_TRIANGLE != 0;
            // Every corner on screen and in front of the near plane: nothing
            // else to prove. Otherwise the corners must still fit the GPU.
            let fast = any == 0 || (any & NEAR == 0 && gpu_safe(&[s0, s1, s2, s3], triangle));
            if fast {
                let sum = u32::from(s0.sz) + u32::from(s1.sz) + u32::from(s2.sz);
                let otz = if triangle {
                    (sum * 0x155) >> 12
                } else {
                    (sum + u32::from(s3.sz)) >> 4
                };
                if otz == 0 || otz >= 2048 {
                    frame.depth_skipped += 1;
                    continue;
                }
                let words = if triangle {
                    QUAD_PACKET_WORDS - 3
                } else {
                    QUAD_PACKET_WORDS
                };
                // SAFETY: both pointers lie in the packet storage.
                if unsafe { end.offset_from(next) } < words as isize {
                    frame.overflowed = true;
                    break;
                }
                // SAFETY: room for `words` was just checked.
                unsafe {
                    next = if triangle {
                        write_triangle(next, quad, material, [s0, s1, s2], otz, window_reset)
                    } else {
                        write_quad(next, quad, material, [s0, s1, s2, s3], otz, window_reset)
                    };
                }
                packets += 1;
                continue;
            }
            // Near-plane, oversize and saturated quads share one path.
            // SAFETY: both pointers lie in the packet storage.
            if unsafe { end.offset_from(next) } < ROBUST_WORST_WORDS as isize {
                frame.overflowed = true;
                break;
            }
            robust_quads += 1;
            // SAFETY: room for the worst case was just checked.
            unsafe {
                next = robust_quad(
                    next,
                    end,
                    pack,
                    frustum,
                    view,
                    screen,
                    focal_length,
                    quad,
                    material,
                    &mut packets,
                );
            }
        }
        frame.robust_quads = robust_quads;
        frame.packets = packets;
        // SAFETY: both pointers lie in the packet storage.
        frame.packet_words = unsafe { next.offset_from(start) } as usize;
    }
}

/// Whether the GPU draws a quad with these screen corners as given: every
/// corner inside its 11-bit coordinates and the polygon under the 1024 by 512
/// span limit.
#[inline(always)]
fn gpu_safe(corners: &[&GridScreen; 4], triangle: bool) -> bool {
    let count = if triangle { 3 } else { 4 };
    let mut left = i32::MAX;
    let mut right = i32::MIN;
    let mut top = i32::MAX;
    let mut bottom = i32::MIN;
    for corner in corners.iter().take(count) {
        let x = i32::from(corner.sxy as u16 as i16);
        let y = i32::from((corner.sxy >> 16) as u16 as i16);
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    left > -1000
        && right < 1000
        && top > -1000
        && bottom < 1000
        && right - left <= 1023
        && bottom - top <= 511
}

/// Write one textured Gouraud quad packet; returns the next free word.
///
/// The GPU splits a quad `(a, b, c, d)` along `b-c`, so the corners go out as
/// `(1, 0, 2, 3)` to split along the 0-2 diagonal like the BSP writer does.
///
/// # Safety
/// `next` must have room for [`QUAD_PACKET_WORDS`].
#[inline(always)]
unsafe fn write_quad(
    next: *mut u32,
    quad: &GridQuad,
    material: &GridMaterial,
    s: [&GridScreen; 4],
    otz: u32,
    window_reset: u32,
) -> *mut u32 {
    let origin = u32::from(material.uv_origin);
    let clut = u32::from(material.clut) << 16;
    let tpage = u32::from(material.tpage) << 16;
    let command = TEXTURED_GOURAUD_COMMAND | 0x0800_0000;
    let uv = |k: usize| (u32::from(quad.uv[k]).wrapping_add(origin)) & 0xffff;
    // SAFETY: the caller reserved room for the whole packet.
    unsafe {
        next.write((13 << 24) | otz);
        next.add(1).write(window_reset);
        next.add(2).write(command | quad.color[1]);
        next.add(3).write(s[1].sxy);
        next.add(4).write(uv(1) | clut);
        next.add(5).write(quad.color[0]);
        next.add(6).write(s[0].sxy);
        next.add(7).write(uv(0) | tpage);
        next.add(8).write(quad.color[2]);
        next.add(9).write(s[2].sxy);
        next.add(10).write(uv(2));
        next.add(11).write(quad.color[3]);
        next.add(12).write(s[3].sxy);
        next.add(13).write(uv(3));
        next.add(14)
    }
}

/// Write one textured Gouraud triangle packet; returns the next free word.
///
/// # Safety
/// `next` must have room for `QUAD_PACKET_WORDS - 3`.
#[inline(always)]
unsafe fn write_triangle(
    next: *mut u32,
    quad: &GridQuad,
    material: &GridMaterial,
    s: [&GridScreen; 3],
    otz: u32,
    window_reset: u32,
) -> *mut u32 {
    let origin = u32::from(material.uv_origin);
    let clut = u32::from(material.clut) << 16;
    let tpage = u32::from(material.tpage) << 16;
    let uv = |k: usize| (u32::from(quad.uv[k]).wrapping_add(origin)) & 0xffff;
    // SAFETY: the caller reserved room for the whole packet.
    unsafe {
        next.write((10 << 24) | otz);
        next.add(1).write(window_reset);
        next.add(2).write(TEXTURED_GOURAUD_COMMAND | quad.color[0]);
        next.add(3).write(s[0].sxy);
        next.add(4).write(uv(0) | clut);
        next.add(5).write(quad.color[1]);
        next.add(6).write(s[1].sxy);
        next.add(7).write(uv(1) | tpage);
        next.add(8).write(quad.color[2]);
        next.add(9).write(s[2].sxy);
        next.add(10).write(uv(2));
        next.add(11)
    }
}

/// The shared path: clip against the near plane in world space, project the
/// result, and hand it to the depth-band splitter with the exact reprojection
/// and window clip the BSP uses.
///
/// # Safety
/// `next..end` must have room for [`ROBUST_WORST_WORDS`].
#[inline(never)]
#[allow(clippy::too_many_arguments)]
unsafe fn robust_quad(
    next: *mut u32,
    end: *mut u32,
    pack: &GridPack<'_>,
    frustum: &FrustumPlanes,
    view: &ViewTransform,
    screen: [i32; 2],
    focal_length: i32,
    quad: &GridQuad,
    material: &GridMaterial,
    packets: &mut u32,
) -> *mut u32 {
    let count = if quad.flags & GRID_QUAD_TRIANGLE != 0 {
        3
    } else {
        4
    };
    let mut source = [AffineVertex::default(); 4];
    // One index walks four parallel tables; the indexed form keeps the loop
    // a counted one on the guest.
    #[allow(clippy::needless_range_loop)]
    for k in 0..count {
        // SAFETY: parse proved the index.
        let v = unsafe { pack.vertices.get_unchecked(usize::from(quad.v[k])) };
        let uv = quad.uv[k].wrapping_add(material.uv_origin);
        source[k] = AffineVertex {
            position: [v.x, v.y, v.z],
            uv: [uv as u8, (uv >> 8) as u8],
            color: quad.color[k],
            screen: [0, 0],
            depth: 0,
        };
    }
    // Room for the clipped polygon plus the splitter's scratch vertices.
    let mut clipped = [AffineVertex::default(); 6 + psx_engine::AFFINE_SPLIT_SCRATCH_VERTICES];
    let near = &frustum.planes[0];
    let behind = source[..count]
        .iter()
        .any(|v| FrustumPlanes::distance(near, v.position) < 0);
    let polygon = if behind {
        let n = clip_polygon_plane(near, &source[..count], &mut clipped[..6]);
        if n < 3 {
            return next;
        }
        n
    } else {
        clipped[..count].copy_from_slice(&source[..count]);
        count
    };
    for vertex in clipped[..polygon].iter_mut() {
        let p = scene::project_vertex_scheduled(GteVec3I16::new(
            vertex.position[0],
            vertex.position[1],
            vertex.position[2],
        ));
        vertex.screen = [p.sx, p.sy];
        vertex.depth = u32::from(p.sz);
    }
    let surface = AffineSurface {
        first_vertex: 0,
        vertex_count: polygon as u16,
        tpage: material.tpage,
        clut: material.clut,
        uv_offset: [0; 2],
        compact: 1,
        texture_window_word: 0,
        color_command_word: TEXTURED_GOURAUD_COMMAND,
    };
    let near_view = NearView {
        rows: view.rotation.m,
        translation: [view.translation.x, view.translation.y, view.translation.z],
        focal_length,
        center: [screen[0] / 2, screen[1] / 2],
    };
    // SAFETY: `clipped` holds `polygon` projected vertices plus the splitter's
    // scratch slots; `next..end` has room per the caller's contract.
    let drawn = unsafe {
        submit_surface_batch_near(
            clipped.as_mut_ptr(),
            polygon,
            &surface,
            1,
            next,
            end,
            GRID_RENDER_PROFILE,
            &near_view,
        )
    };
    *packets += drawn.packets;
    drawn.next_packet
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn pack_bytes() -> Vec<u8> {
        let mut out = Vec::new();
        let header = [
            MAGIC,
            VERSION,
            1 | (1 << 16),
            0,
            0,
            36 | (100 << 16),
            4,
            1,
            1,
            1,
            1,
            1 | (1 << 16),
            2,
            0,
            0,
            0,
        ];
        for w in header {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&[7, 0, 0, 0]); // one material, padded
        for p in [[0i16, 0, 0], [36, 0, 0], [36, 0, 36], [0, 0, 36]] {
            for c in p {
                out.extend_from_slice(&c.to_le_bytes());
            }
            out.extend_from_slice(&0i16.to_le_bytes());
        }
        // cell
        for c in [0u16, 1] {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for c in [0i16, 0] {
            out.extend_from_slice(&c.to_le_bytes());
        }
        // macro
        for c in [0i16, 0, 0, 0] {
            out.extend_from_slice(&c.to_le_bytes());
        }
        // quad
        for v in [0u16, 1, 2, 3] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for _ in 0..4 {
            out.extend_from_slice(&0x0080_8080u32.to_le_bytes());
        }
        for _ in 0..4 {
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&[0, 127, 0, 0, 0, 0, 0, 0]);
        out
    }

    #[test]
    fn a_well_formed_pack_parses() {
        // Words are aligned because the Vec<u8> allocation is.
        let bytes = pack_bytes();
        let aligned: Vec<u32> = bytes
            .chunks(4)
            .map(|c| {
                u32::from_le_bytes([
                    c[0],
                    c.get(1).copied().unwrap_or(0),
                    c.get(2).copied().unwrap_or(0),
                    c.get(3).copied().unwrap_or(0),
                ])
            })
            .collect();
        let slice =
            unsafe { core::slice::from_raw_parts(aligned.as_ptr().cast::<u8>(), bytes.len()) };
        let pack = GridPack::parse(slice).expect("parses");
        assert_eq!(pack.vertices.len(), 4);
        assert_eq!(pack.quads.len(), 1);
        assert_eq!(pack.quads[0].normal, [0, 127, 0]);
        assert_eq!(pack.materials, &[7]);
    }
}
