//! Far panels: the level beyond the drawn rooms as depth-meshed pictures baked at the
//! lattice's eyes.
//!
//! A *panel* is the level seen from a bake eye `B` along one of [`HEADINGS`] headings, as a
//! pinhole picture whose focal length is the game's own, so one texel is about one screen
//! pixel wherever the content stands. It is cut into [`COLS`] by [`ROWS`] tiles of
//! [`TILE_TEXELS`] by [`PICTURE_ROWS`] 4-bit texels: exactly one atlas slot of the far cards,
//! one disc sector, palette in the row under the picture. A tile with nothing in it is not
//! stored.
//!
//! Beside the picture the bake keeps a *depth mesh*: the distance along the heading at the
//! corners of a grid of cells, [`CELLS_X`] by [`CELLS_Y`] to a tile ([`MESH_W`] by [`MESH_H`]
//! vertices, one byte each, see [`depth_units`]). The draw stands every vertex at
//! `B + depth * ray(i, j)` in the world and projects it from the real eye, one textured quad
//! to a cell. At `B` the quads reproduce the baked picture; away from it nearer content
//! moves more than farther content, as it does in the world, so the picture stays consistent
//! with the geometry as the eye walks. Which bake eye serves the eye, and which tiles are in
//! VRAM, is the stream's business (`psx_game_runtime::panel_stream`).
//!
//! # Cost
//!
//! A vertex is three multiplications and one projection, cached for the frame; a cell is one
//! 11-word packet. A view of the level holds a few dozen tiles.

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use super::cards::{
    page_word, slot_clut_word, write_card, Blend, ATLAS_COLUMNS, PICTURE_ROWS, SLOT_ROWS,
    VIEW_COS_Q12, VIEW_SIN_Q12,
};
use super::{GridFrame, GridScreen, NEAR, OFF_ANY, OFF_BOTTOM, OFF_LEFT, OFF_RIGHT, OFF_TOP};

/// Headings a lattice eye's panels are baked along, `45` degrees apart: panel `k` looks along
/// `(cos, sin)` of `k * 45` degrees in the world's `(x, z)` plane
/// ([`VIEW_COS_Q12`], [`VIEW_SIN_Q12`]).
pub const HEADINGS: usize = 8;
/// Tile columns across a panel.
pub const COLS: usize = 5;
/// Tile rows down a panel.
pub const ROWS: usize = 7;
/// Tiles of a panel.
pub const TILES: usize = COLS * ROWS;
/// Texels across a tile.
pub const TILE_TEXELS: i32 = 64;
/// Mesh cells across a tile.
pub const CELLS_X: usize = 2;
/// Mesh cells down a tile.
pub const CELLS_Y: usize = 3;
/// Texels across a cell.
pub const CELL_W: i32 = TILE_TEXELS / CELLS_X as i32;
/// Texel rows down a cell.
pub const CELL_H: i32 = PICTURE_ROWS / CELLS_Y as i32;
/// Mesh vertices across a panel.
pub const MESH_W: usize = COLS * CELLS_X + 1;
/// Mesh vertices down a panel.
pub const MESH_H: usize = ROWS * CELLS_Y + 1;
/// Mesh vertices of a panel.
pub const MESH: usize = MESH_W * MESH_H;
/// Texel row of the horizon (the principal point), counted from the top of the panel.
pub const HORIZON_ROW: i32 = 3 * PICTURE_ROWS;
/// Texel column of the principal point.
pub const CENTRE_COLUMN: i32 = COLS as i32 * TILE_TEXELS / 2;
/// Tile value of an atlas slot that holds nothing.
pub const NO_TILE_SLOT: u8 = 0xff;
/// Bytes of a panel's header sector that the depth mesh takes.
pub const HEADER_DEPTH_BYTES: usize = MESH;

/// Steps of the depth byte.
pub const DEPTH_STEPS: usize = 256;
/// Depth steps are counted in this fraction of a sector (`1 / 16`).
pub const DEPTH_SECTOR_SHIFT: u32 = 4;

const DEPTH_TABLE: [u16; DEPTH_STEPS] = build_depth_table();

/// From three sectors to two hundred in 256 steps of a little under two percent.
const fn build_depth_table() -> [u16; DEPTH_STEPS] {
    let mut table = [0u16; DEPTH_STEPS];
    let mut depth: u32 = 3 << DEPTH_SECTOR_SHIFT;
    let mut step = 0;
    while step < DEPTH_STEPS {
        table[step] = depth as u16;
        // 1.66 percent, at least one.
        let by = depth * 1180 >> 16;
        depth += if by == 0 { 1 } else { by };
        step += 1;
    }
    table
}

/// The distance a depth byte stands for, in sixteenths of a sector.
pub const fn depth_units(byte: u8) -> u32 {
    DEPTH_TABLE[byte as usize] as u32
}

/// The depth byte whose distance is nearest `sixteenths`.
pub fn depth_byte(sixteenths: u32) -> u8 {
    let mut best = 0usize;
    let mut distance = u32::MAX;
    for (i, &d) in DEPTH_TABLE.iter().enumerate() {
        let off = u32::from(d).abs_diff(sixteenths);
        if off < distance {
            best = i;
            distance = off;
        }
    }
    best as u8
}

/// The mesh's rays: per column and row, the offset of the ray from the heading, `Q12`
/// (`(texel - principal point) / focal`).
#[derive(Copy, Clone, Debug)]
pub struct PanelRays {
    /// Across, positive to the right of the heading.
    pub across: [i16; MESH_W],
    /// Down, positive below the horizon.
    pub down: [i16; MESH_H],
}

impl PanelRays {
    /// The rays of a panel baked for a view with projection plane `focal` (pixels).
    pub const fn new(focal: i32) -> Self {
        let focal = if focal < 1 { 1 } else { focal };
        let mut across = [0i16; MESH_W];
        let mut down = [0i16; MESH_H];
        let mut i = 0;
        while i < MESH_W {
            across[i] = clamp_ray(((i as i32 * CELL_W - CENTRE_COLUMN) << 12) / focal);
            i += 1;
        }
        let mut j = 0;
        while j < MESH_H {
            down[j] = clamp_ray(((j as i32 * CELL_H - HORIZON_ROW) << 12) / focal);
            j += 1;
        }
        Self { across, down }
    }
}

const fn clamp_ray(v: i32) -> i16 {
    if v < -8191 {
        -8191
    } else if v > 8191 {
        8191
    } else {
        v as i16
    }
}

/// What the draw needs of the camera.
#[derive(Copy, Clone, Debug)]
pub struct PanelCamera {
    /// Screen size in pixels.
    pub screen: [i32; 2],
    /// Smallest depth, in GTE `SZ` units, a corner may have.
    pub near_sz: u16,
    /// Texture-page word of the atlas's first page, and its top-left VRAM pixel.
    pub tpage: u16,
    pub atlas: (u16, u16),
    /// Runtime units in a sector, for the depth mesh.
    pub sector: i32,
    /// Ordering-table slot no cell goes in front of: the depth the real rooms end at, so a
    /// panel never covers geometry drawn whole.
    pub otz_floor: u32,
}

/// One panel to draw.
#[derive(Copy, Clone, Debug)]
pub struct PanelRef<'a> {
    /// Where it was baked from, world runtime units.
    pub eye: [i32; 3],
    /// Its heading, `0..HEADINGS`.
    pub heading: u8,
    /// The depth mesh, row-major.
    pub depth: &'a [u8; MESH],
    /// Per tile (column-major, `col * ROWS + row`), the atlas slot holding its picture, or
    /// [`NO_TILE_SLOT`] for a tile that is empty or not there yet.
    pub slots: &'a [u8; TILES],
}

/// A vertex projected for the frame, kept for the cells that share it.
#[derive(Copy, Clone)]
pub struct PanelScratch {
    stamp: u16,
    seen: [u16; MESH],
    screen: [GridScreen; MESH],
}

impl PanelScratch {
    /// An empty cache.
    pub const fn new() -> Self {
        Self {
            stamp: 0,
            seen: [0; MESH],
            screen: [GridScreen {
                sxy: 0,
                sz: 0,
                code: 0,
            }; MESH],
        }
    }

    /// Forget every projected vertex (call between two panels).
    fn clear(&mut self) {
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen = [0; MESH];
            self.stamp = 1;
        }
    }
}

impl Default for PanelScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcode of a projected vertex.
#[inline(always)]
fn screen_of(p: scene::Projected, camera: &PanelCamera) -> GridScreen {
    let (sx, sy) = (i32::from(p.sx), i32::from(p.sy));
    let mut code = 0u16;
    if p.sz < camera.near_sz {
        code = NEAR;
    } else {
        if sx < 0 {
            code |= OFF_LEFT;
        }
        if sx >= camera.screen[0] {
            code |= OFF_RIGHT;
        }
        if sy < 0 {
            code |= OFF_TOP;
        }
        if sy >= camera.screen[1] {
            code |= OFF_BOTTOM;
        }
    }
    GridScreen {
        sxy: (p.sx as u16 as u32) | ((p.sy as u16 as u32) << 16),
        sz: p.sz,
        code,
    }
}

/// The world position of vertex `(i, j)` of a panel baked at `eye` along `heading`.
#[inline(always)]
fn mesh_point(
    panel: &PanelRef<'_>,
    rays: &PanelRays,
    sector: i32,
    i: usize,
    j: usize,
) -> (i32, i32, i32) {
    let depth = ((depth_units(panel.depth[j * MESH_W + i]) as i32) * sector) >> DEPTH_SECTOR_SHIFT;
    let h = usize::from(panel.heading) % HEADINGS;
    let (c, s) = (i32::from(VIEW_COS_Q12[h]), i32::from(VIEW_SIN_Q12[h]));
    let a = i32::from(rays.across[i]);
    let b = i32::from(rays.down[j]);
    // The ray is forward + a * right + b * down, right = (-s, c), down = -y.
    let dx = c - ((a * s) >> 12);
    let dz = s + ((a * c) >> 12);
    (
        panel.eye[0] + ((depth * dx) >> 12),
        panel.eye[1] - ((depth * b) >> 12),
        panel.eye[2] + ((depth * dz) >> 12),
    )
}

#[inline(always)]
fn clamp_world(v: i32) -> i16 {
    v.clamp(-32000, 32000) as i16
}

/// Whether a quad with these corners is worth a packet: nothing at or behind the near plane,
/// not wholly off one edge, safe for the GPU.
#[inline(always)]
fn drawable(s: &[GridScreen; 4]) -> bool {
    let any = s[0].code | s[1].code | s[2].code | s[3].code;
    if any & NEAR != 0 {
        return false;
    }
    if s[0].code & s[1].code & s[2].code & s[3].code & OFF_ANY != 0 {
        return false;
    }
    let (mut left, mut right, mut top, mut bottom) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for corner in s {
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

/// Draw the tiles of `panel` that are in VRAM.
///
/// The GTE must hold the world view (rotation and the translation of the world origin).
/// Packets go forward from `out[0]`, or backward from the end of `out` when `reverse`;
/// `used` is how many words of `out` are taken already, from that end, and the frame's
/// `packets` and `packet_words` count what this call wrote.
pub fn draw_panel(
    panel: &PanelRef<'_>,
    rays: &PanelRays,
    camera: &PanelCamera,
    scratch: &mut PanelScratch,
    out: &mut [u32],
    reverse: bool,
) -> GridFrame {
    let mut frame = GridFrame::default();
    scratch.clear();
    let start = out.as_mut_ptr();
    // SAFETY: one past the end of the slice.
    let end = unsafe { start.add(out.len()) };
    let mut next = if reverse { end } else { start };
    let mut packets = 0u32;
    let stamp = scratch.stamp;
    let vertex = |scratch: &mut PanelScratch, i: usize, j: usize| -> GridScreen {
        let at = j * MESH_W + i;
        if scratch.seen[at] == stamp {
            return scratch.screen[at];
        }
        let (x, y, z) = mesh_point(panel, rays, camera.sector, i, j);
        let p = scene::project_vertex_scheduled(GteVec3I16::new(
            clamp_world(x),
            clamp_world(y),
            clamp_world(z),
        ));
        let s = screen_of(p, camera);
        scratch.seen[at] = stamp;
        scratch.screen[at] = s;
        s
    };
    for col in 0..COLS {
        for row in 0..ROWS {
            let tile = col * ROWS + row;
            let slot = panel.slots[tile];
            if slot == NO_TILE_SLOT {
                continue;
            }
            let (i0, j0) = (col * CELLS_X, row * CELLS_Y);
            // The tile's corners first: a tile wholly off screen or behind costs four vertices.
            let corners = [
                vertex(scratch, i0, j0),
                vertex(scratch, i0 + CELLS_X, j0),
                vertex(scratch, i0 + CELLS_X, j0 + CELLS_Y),
                vertex(scratch, i0, j0 + CELLS_Y),
            ];
            frame.cells_visible += 1;
            let any = corners[0].code | corners[1].code | corners[2].code | corners[3].code;
            if any & NEAR != 0 {
                frame.depth_skipped += 1;
                continue;
            }
            if corners[0].code & corners[1].code & corners[2].code & corners[3].code & OFF_ANY != 0
            {
                frame.screen_culled += 1;
                continue;
            }
            let slot = usize::from(slot);
            let (column, atlas_row) = (slot % ATLAS_COLUMNS, slot / ATLAS_COLUMNS);
            let u_tile = ((column % 4) as i32 * TILE_TEXELS) as u32;
            let v_tile = (atlas_row as i32 * SLOT_ROWS) as u32;
            let tpage = page_word(camera.tpage, column / 4);
            let clut = slot_clut_word(camera.atlas, slot);
            let mut drawn = false;
            for cy in 0..CELLS_Y {
                for cx in 0..CELLS_X {
                    let (i, j) = (i0 + cx, j0 + cy);
                    let s = [
                        vertex(scratch, i, j),
                        vertex(scratch, i + 1, j),
                        vertex(scratch, i + 1, j + 1),
                        vertex(scratch, i, j + 1),
                    ];
                    if !drawable(&s) {
                        continue;
                    }
                    let sum = u32::from(s[0].sz)
                        + u32::from(s[1].sz)
                        + u32::from(s[2].sz)
                        + u32::from(s[3].sz);
                    let otz = (sum >> 4).max(camera.otz_floor).clamp(1, 2047);
                    // SAFETY: both pointers lie in the packet storage.
                    let left = unsafe {
                        if reverse {
                            next.offset_from(start)
                        } else {
                            end.offset_from(next)
                        }
                    };
                    if left < super::cards::CARD_PACKET_WORDS as isize {
                        frame.overflowed = true;
                        break;
                    }
                    let u0 = (u_tile + cx as u32 * CELL_W as u32) as u8;
                    let v0 = (v_tile + cy as u32 * CELL_H as u32) as u8;
                    let u1 = u0 + (CELL_W - 1) as u8;
                    let v1 = v0 + (CELL_H - 1) as u8;
                    // SAFETY: room for one packet was just checked.
                    unsafe {
                        let at = if reverse {
                            next.sub(super::cards::CARD_PACKET_WORDS)
                        } else {
                            next
                        };
                        write_card(
                            at,
                            &s,
                            [(u0, v0), (u1, v0), (u1, v1), (u0, v1)],
                            clut,
                            tpage,
                            128,
                            Blend::Opaque,
                            otz,
                        );
                        next = if reverse {
                            at
                        } else {
                            at.add(super::cards::CARD_PACKET_WORDS)
                        };
                    }
                    packets += 1;
                    drawn = true;
                }
                if frame.overflowed {
                    break;
                }
            }
            frame.quads_visible += u16::from(drawn);
            if frame.overflowed {
                break;
            }
        }
        if frame.overflowed {
            break;
        }
    }
    frame.packets = packets;
    // SAFETY: both pointers lie in the packet storage.
    frame.packet_words = unsafe {
        if reverse {
            end.offset_from(next)
        } else {
            next.offset_from(start)
        }
    } as usize;
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn the_depth_table_climbs_from_three_sectors_to_about_two_hundred() {
        assert_eq!(depth_units(0), 48);
        for k in 1..DEPTH_STEPS {
            assert!(depth_units(k as u8) > depth_units(k as u8 - 1), "step {k}");
        }
        let last = depth_units(255);
        // 16 units a sector: 150 to 250 sectors.
        assert!((2400..4000).contains(&last), "{last}");
        // Every step is under two percent of the one before.
        for k in 1..DEPTH_STEPS {
            let (a, b) = (depth_units(k as u8 - 1), depth_units(k as u8));
            assert!((b - a) * 100 <= a * 2 + 100, "step {k}: {a} {b}");
        }
    }

    #[test]
    fn a_depth_byte_round_trips_to_its_nearest_distance() {
        for byte in [0u8, 1, 40, 128, 200, 255] {
            assert_eq!(depth_byte(depth_units(byte)), byte);
        }
        assert_eq!(depth_byte(0), 0);
        assert_eq!(depth_byte(100_000), 255);
    }

    #[test]
    fn the_rays_cross_the_principal_point_at_the_middle_column_and_horizon_row() {
        let rays = PanelRays::new(305);
        assert_eq!(rays.across[MESH_W / 2], 0);
        assert_eq!(rays.down[3 * CELLS_Y], 0);
        // 160 texels left of the middle at focal 305: tan of 27.7 degrees.
        assert!((i32::from(rays.across[0]) + (160 << 12) / 305).abs() <= 1);
        assert!(rays.across[0] < 0 && rays.across[MESH_W - 1] > 0);
        assert!(rays.down[0] < 0 && rays.down[MESH_H - 1] > 0);
    }

    fn setup(eye: [i32; 3], yaw: (i16, i16)) {
        use crate::render::{load_view_rotation, view_rotation};
        use psx_gte::math::Vec3I32;
        psx_gte::host::reset();
        scene::set_screen_offset(160 << 16, 120 << 16);
        scene::set_projection_plane(305);
        let rotation = view_rotation(0, 0x1000, yaw.0, yaw.1);
        let _ = load_view_rotation(
            Vec3I32::new(eye[0] << 12, eye[1] << 12, eye[2] << 12),
            rotation,
        );
    }

    fn camera() -> PanelCamera {
        PanelCamera {
            screen: [320, 240],
            near_sz: 154,
            tpage: 0x0105,
            atlas: (320, 0),
            sector: 80,
            otz_floor: 0,
        }
    }

    fn corner_xy(packet: &[u32]) -> [(i32, i32); 4] {
        let xy = |w: u32| {
            (
                i32::from(w as u16 as i16),
                i32::from((w >> 16) as u16 as i16),
            )
        };
        // top-left, top-right, bottom-left, bottom-right
        [xy(packet[5]), xy(packet[3]), xy(packet[9]), xy(packet[7])]
    }

    #[test]
    fn a_panel_seen_from_its_bake_eye_lands_where_it_was_baked() {
        // Eye at the bake eye looking along heading 0 (+x): every mesh vertex projects to the
        // texel it was baked at, whatever its depth.
        for depth_byte in [30u8, 120, 220] {
            let eye = [1000, 200, 1000];
            setup(eye, (0, 0x1000));
            let depth = [depth_byte; MESH];
            let mut slots = [NO_TILE_SLOT; TILES];
            let (col, row) = (2usize, 3usize);
            slots[col * ROWS + row] = 5;
            let panel = PanelRef {
                eye,
                heading: 0,
                depth: &depth,
                slots: &slots,
            };
            let rays = PanelRays::new(305);
            let mut scratch = PanelScratch::new();
            let mut out = vec![0u32; 11 * 8];
            let frame = draw_panel(&panel, &rays, &camera(), &mut scratch, &mut out, false);
            assert_eq!(frame.packets as usize, CELLS_X * CELLS_Y);
            // The first cell is the tile's top-left: its top-left corner is texel
            // (col * 64, row * 63) of the panel.
            let first = corner_xy(&out[..11]);
            let want_x = 160 + (col as i32 * TILE_TEXELS - CENTRE_COLUMN);
            let want_y = 120 + (row as i32 * PICTURE_ROWS - HORIZON_ROW);
            assert!(
                (first[0].0 - want_x).abs() <= 2 && (first[0].1 - want_y).abs() <= 2,
                "depth {depth_byte}: {:?} want ({want_x}, {want_y})",
                first[0]
            );
            // And its width is one cell wide.
            assert!((first[1].0 - first[0].0 - CELL_W).abs() <= 2, "{first:?}");
        }
    }

    #[test]
    fn a_nearer_mesh_moves_more_than_a_farther_one_when_the_eye_steps_aside() {
        let at = |depth_byte: u8, step: i32| -> i32 {
            let eye = [1000, 200, 1000 + step];
            setup(eye, (0, 0x1000));
            let depth = [depth_byte; MESH];
            let mut slots = [NO_TILE_SLOT; TILES];
            slots[2 * ROWS + 3] = 5;
            let panel = PanelRef {
                eye: [1000, 200, 1000],
                heading: 0,
                depth: &depth,
                slots: &slots,
            };
            let rays = PanelRays::new(305);
            let mut scratch = PanelScratch::new();
            let mut out = vec![0u32; 11 * 8];
            let frame = draw_panel(&panel, &rays, &camera(), &mut scratch, &mut out, false);
            assert!(frame.packets > 0);
            corner_xy(&out[..11])[1].0
        };
        // 200 units to the +z side (the heading's right): the picture slides left, the
        // nearer one by more.
        let (near, far) = (20u8, 140u8);
        let moved_near = at(near, 0) - at(near, 200);
        let moved_far = at(far, 0) - at(far, 200);
        assert!(moved_near > moved_far, "{moved_near} {moved_far}");
        assert!(moved_far > 0);
    }

    #[test]
    fn tiles_not_in_vram_are_not_drawn_and_packets_fit_the_storage() {
        let eye = [1000, 200, 1000];
        setup(eye, (0, 0x1000));
        let depth = [100u8; MESH];
        let slots = [NO_TILE_SLOT; TILES];
        let panel = PanelRef {
            eye,
            heading: 0,
            depth: &depth,
            slots: &slots,
        };
        let rays = PanelRays::new(305);
        let mut scratch = PanelScratch::new();
        let mut out: Vec<u32> = vec![0; 64];
        let frame = draw_panel(&panel, &rays, &camera(), &mut scratch, &mut out, false);
        assert_eq!(frame.packets, 0);
        // Too little storage: the draw says so and writes whole packets only.
        let mut slots = [NO_TILE_SLOT; TILES];
        slots[2 * ROWS + 3] = 1;
        let panel = PanelRef {
            slots: &slots,
            ..panel
        };
        let mut out = vec![0u32; 11 * 2 + 5];
        let frame = draw_panel(&panel, &rays, &camera(), &mut scratch, &mut out, false);
        assert!(frame.overflowed);
        assert_eq!(frame.packets, 2);
        assert_eq!(frame.packet_words, 22);
    }
}
