//! Cell cull, facing test and corner projection of one room.

/// Height of the lid slab the open columns are bounded by, runtime units.
const SKY_LID: i32 = 9;
/// Runtime units a cell's box is grown by on every side before the vertical
/// frustum test, so a vertex the cook snapped just past the cell edge or its
/// recorded height range still counts as inside.
const CELL_BOX_SLACK: i32 = 2;
use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use super::*;

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

impl GridRenderer {
    /// Frustum cull by row spans. For each grid row, the near, left and right
    /// planes bound an interval of x; the columns inside it are queued. The
    /// vertical planes are not tested here: a column is floor to ceiling, so
    /// few are ever wholly above or below the view, and the per-corner
    /// outcodes reject what is off screen. Also notes whether any queued cell
    /// is open to the sky.
    #[inline(never)]
    pub(super) fn select_cells(
        &mut self,
        room: &RoomView<'_>,
        frustum: &FrustumPlanes,
        size: i32,
        camera: [i32; 3],
        frame: &mut GridFrame,
    ) {
        let head = room.col.header;
        let (grid_w, grid_d) = (usize::from(head.width), usize::from(head.depth));
        let out = self.cells.as_mut_ptr();
        let far_out = self.far.as_mut_ptr();
        let mut count = 0usize;
        let mut sky = 0u8;
        // Distance LOD: blocks whose centre is further than `far_cells`
        // sectors (Chebyshev) from the camera draw merged quads.
        self.lod_queue.clear();
        let shift = u32::from(head.lod_shift);
        let far_cells = i32::from(head.lod_far_cells);
        let lod = far_cells != 0 && !room.render.lod_blocks.is_empty();
        let blocks_w = (grid_w + (1 << shift) - 1) >> shift;
        let (camera_cx, camera_cz) = (
            div_floor_i32(camera[0], size),
            div_floor_i32(camera[2], size),
        );
        let frame_stamp = self.frame;
        // Sky: the first queued open cell whose block column (from the block's
        // lowest geometry up to the room's top plus a lid slab) is inside the
        // whole frustum shows the sky. Each block is tested once.
        let have_blocks = !room.render.lod_blocks.is_empty();
        let mut sky_seen = false;
        let planes = &frustum.planes;
        let (top, bottom) = (planes[3].0, planes[4].0);
        let (y_lo, y_hi) = (i32::from(head.min_y), i32::from(head.max_y));
        let width = grid_w as i32;
        for cz in 0..grid_d {
            let z0 = (cz as i32) * size;
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
            let first = div_floor_i32(lo, size).max(0);
            let last = div_floor_i32(hi, size).min(width - 1);
            // What a cell box adds to the top and bottom planes' distance is
            // its x and y terms; the row fixes the rest, with the rounding
            // band folded in so "outside" is a negative sum.
            let row_rest = |normal: &[i32; 3], constant: i32| {
                let z = if normal[2] < 0 {
                    z0 - CELL_BOX_SLACK
                } else {
                    z1 + CELL_BOX_SLACK
                };
                normal[2]
                    .wrapping_mul(z)
                    .wrapping_add(constant)
                    .wrapping_add(frustum.side_error)
            };
            let top_rest = row_rest(&top, planes[3].1);
            let bottom_rest = row_rest(&bottom, planes[4].1);
            let row = cz * grid_w;
            let mut cx = first;
            while cx <= last {
                let index = row + cx as usize;
                // SAFETY: 0 <= cx < width and cz < depth, and the view proved
                // `cells.len() == width * depth`.
                let cell = unsafe { room.render.cells.get_unchecked(index) };
                if cell.count != 0 {
                    let mut far = 0u8;
                    if lod && cell.flags & CELL_HAS_LOD != 0 {
                        let (bx, bz) = (cx >> shift, (cz as i32) >> shift);
                        let half = (1i32 << shift) >> 1;
                        let dx = ((bx << shift) + half - camera_cx).abs();
                        let dz = ((bz << shift) + half - camera_cz).abs();
                        if dx.max(dz) > far_cells {
                            far = 1;
                            let block = bz as usize * blocks_w + bx as usize;
                            // SAFETY: the cook writes one block per
                            // (bw * bd) and the renderer was sized for it.
                            let stamp = unsafe { self.block_stamp.get_unchecked_mut(block) };
                            if *stamp != frame_stamp {
                                *stamp = frame_stamp;
                                // SAFETY: as above.
                                let block = unsafe { room.render.lod_blocks.get_unchecked(block) };
                                let first = usize::from(block.first);
                                for q in first..first + usize::from(block.count) {
                                    self.lod_queue.push(q as u16);
                                }
                            }
                        }
                    }
                    // A cell wholly above or below the view draws nothing, and
                    // the row spans never test those planes. A far cell stays:
                    // its block's merged quads stand in for it. The sky test
                    // below still sees every cell, as before.
                    let above_or_below = far == 0 && {
                        // The box corner farthest along each plane's normal,
                        // against the distance the row's terms already hold.
                        let x0 = cx * size;
                        let (y_min, y_max) = (
                            i32::from(cell.min_y) - CELL_BOX_SLACK,
                            i32::from(cell.max_y) + CELL_BOX_SLACK,
                        );
                        let x = |normal: &[i32; 3]| {
                            if normal[0] < 0 {
                                x0 - CELL_BOX_SLACK
                            } else {
                                x0 + size + CELL_BOX_SLACK
                            }
                        };
                        let y = |normal: &[i32; 3]| if normal[1] < 0 { y_min } else { y_max };
                        top_rest
                            .wrapping_add(top[0].wrapping_mul(x(&top)))
                            .wrapping_add(top[1].wrapping_mul(y(&top)))
                            < 0
                            || bottom_rest
                                .wrapping_add(bottom[0].wrapping_mul(x(&bottom)))
                                .wrapping_add(bottom[1].wrapping_mul(y(&bottom)))
                                < 0
                    };
                    if !above_or_below {
                        // SAFETY: at most one entry per cell, and `cells`
                        // holds capacity for every cell of the largest room.
                        unsafe {
                            *out.add(count) = index as u16;
                            // Parallel to `cells`, same capacity.
                            *far_out.add(count) = far;
                        }
                        count += 1;
                    }
                    if cell.flags & CELL_OPEN_SKY != 0 && !sky_seen {
                        if have_blocks {
                            let (bx, bz) = ((cx >> shift) as usize, cz >> shift);
                            let block = bz * blocks_w + bx;
                            // SAFETY: the cook writes one block per (bw * bd).
                            let stamp = unsafe { self.sky_stamp.get_unchecked_mut(block) };
                            if *stamp != frame_stamp {
                                *stamp = frame_stamp;
                                // SAFETY: as above.
                                let b = unsafe { room.render.lod_blocks.get_unchecked(block) };
                                if b.flags & BLOCK_OPEN_SKY != 0 {
                                    let edge = 1i32 << shift;
                                    let x0 = (bx as i32) * edge * size;
                                    let z0 = (bz as i32) * edge * size;
                                    let x1 = (x0 + edge * size).min(width * size);
                                    let z1 = (z0 + edge * size).min((grid_d as i32) * size);
                                    let c = |v: i32| {
                                        v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
                                    };
                                    let lo = GteVec3I16 {
                                        x: c(x0),
                                        y: b.min_y,
                                        z: c(z0),
                                    };
                                    let hi = GteVec3I16 {
                                        x: c(x1),
                                        y: c(y_hi + SKY_LID),
                                        z: c(z1),
                                    };
                                    sky_seen = frustum.cull_aabb(lo, hi, CLIP_ALL_PLANES).is_some();
                                }
                            }
                        } else {
                            sky_seen = true;
                        }
                    }
                    sky |= cell.flags;
                }
                cx += 1;
            }
        }
        // SAFETY: `count` never exceeds the capacity.
        unsafe {
            self.cells.set_len(count);
            self.far.set_len(count);
        }
        frame.lod_quads = self.lod_queue.len() as u16;
        frame.cells_visible = count as u16;
        let _ = sky;
        frame.sky_visible = sky_seen;
    }

    /// Drop quads facing away from the camera and queue the corners of the
    /// rest. Cells in a far block skip the quads a merged quad replaces; the
    /// block's merged quads are tested and queued after the cells, as indices
    /// past the cell quads.
    #[inline(never)]
    pub(super) fn select_quads(
        &mut self,
        room: &RoomView<'_>,
        frustum: &FrustumPlanes,
        size: i32,
        camera: [i32; 3],
        frame: &mut GridFrame,
    ) {
        let [cx, cy, cz] = camera;
        let frame_stamp = self.frame;
        let stamp = self.stamp.as_mut_ptr();
        let quads_out = self.quads.as_mut_ptr();
        let work_out = self.work.as_mut_ptr();
        let mut quad_count = 0usize;
        let mut tested = 0usize;
        let mut replaced = 0usize;
        let mut work_count = 0usize;
        let quad_total = room.render.quads.len();
        macro_rules! queue {
            ($quad:expr, $slot:expr, $skip:expr) => {{
                let quad: &Quad = $quad;
                let flags = quad.flags;
                if flags & $skip != 0 {
                    replaced += 1;
                } else {
                    tested += 1;
                    let front = if flags & QUAD_DOUBLE != 0 {
                        true
                    } else if flags & QUAD_AXIS != 0 {
                        // Axis-aligned plane: the camera coordinate against
                        // the plane coordinate, no multiplies.
                        let axis = usize::from((flags >> QUAD_AXIS_SHIFT) & 3);
                        // SAFETY: the cook emits axis 0..=2 only (validation checks it).
                        let c = unsafe { *camera.get_unchecked(axis) };
                        (if flags & QUAD_AXIS_NEG != 0 { -c } else { c }) > i32::from(quad.d)
                    } else {
                        (i32::from(quad.n[0]) * cx
                            + i32::from(quad.n[1]) * cy
                            + i32::from(quad.n[2]) * cz)
                            >> 7
                            > i32::from(quad.d)
                    };
                    if front {
                        // SAFETY: at most one entry per quad, and `quads`
                        // holds capacity for every quad of the largest room.
                        unsafe {
                            *quads_out.add(quad_count) = $slot as u16;
                        }
                        quad_count += 1;
                        // SAFETY: validation proved every index is below the
                        // vertex count, which sized the stamp table and the
                        // work queue (each vertex is queued at most once).
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
            }};
        }
        for (i, &cell_index) in self.cells.iter().enumerate() {
            // SAFETY: cell indices were produced from valid grid coordinates.
            let cell = unsafe { room.render.cells.get_unchecked(usize::from(cell_index)) };
            // SAFETY: parallel to `cells`.
            let skip = if unsafe { *self.far.get_unchecked(i) } != 0 {
                QUAD_LOD_COVERED
            } else {
                0
            };
            let first = usize::from(cell.first);
            for q in first..first + usize::from(cell.count) {
                // SAFETY: validation proved first + count <= quads.len().
                let quad = unsafe { room.render.quads.get_unchecked(q) };
                queue!(quad, q, skip);
            }
        }
        let no_skip = core::hint::black_box(0u8);
        for &l in &self.lod_queue {
            // SAFETY: queued from valid LOD block ranges.
            let quad = unsafe { room.render.lod_quads.get_unchecked(usize::from(l)) };
            queue!(quad, quad_total + usize::from(l), no_skip);
        }
        // Free planes are not in any cell: each is frustum-tested as a box
        // and its run of quads goes through the same facing test.
        let clamp16 = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        for plane in room.render.plane_refs {
            let lo = GteVec3I16 {
                x: clamp16(i32::from(plane.x0) * size),
                y: plane.min_y,
                z: clamp16(i32::from(plane.z0) * size),
            };
            let hi = GteVec3I16 {
                x: clamp16((i32::from(plane.x1) + 1) * size),
                y: plane.max_y,
                z: clamp16((i32::from(plane.z1) + 1) * size),
            };
            if frustum.cull_aabb(lo, hi, CLIP_ALL_PLANES).is_none() {
                continue;
            }
            let first = usize::from(plane.quad);
            for q in first..first + usize::from(plane.count) {
                // SAFETY: validation proved quad + count <= quads.len().
                let quad = unsafe { room.render.quads.get_unchecked(q) };
                queue!(quad, q, no_skip);
            }
        }
        // SAFETY: the counts above never exceed the capacities.
        unsafe {
            self.quads.set_len(quad_count);
            self.work.set_len(work_count);
        }
        frame.quads_visible = quad_count as u16;
        frame.backface_culled = (tested - quad_count) as u16;
        frame.lod_replaced = replaced as u16;
    }

    /// Project every queued corner, three per GTE command.
    #[inline(never)]
    pub(super) fn project_corners(
        &mut self,
        room: &RoomView<'_>,
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
            let v = unsafe { room.render.vertices.get_unchecked(usize::from(index)) };
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
}
