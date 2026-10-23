//! The coarse band: rooms between the full-detail radius and the far cards
//! drawn as a handful of Gouraud triangles each.
//!
//! The cook merges each room's quads by vertex clustering
//! ([`CoarseVertex`], `LEVEL_HAS_COARSE`): a room becomes a few dozen
//! triangles with a colour per vertex (the material's mean colour times the
//! baked light), standing at the room's real place. This draw projects the
//! vertices of every such room whose centre is within the band, in view and
//! not drawn whole, and writes one untextured triangle packet per triangle,
//! keyed into the ordering table by the same depth law as the room quads.
//! The triangles are drawn both sides: sorting by depth keeps the near side
//! of a hill in front of its far side.

use alloc::vec::Vec;

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use super::{GridFrame, GridScreen, NEAR, OFF_ANY, OFF_BOTTOM, OFF_LEFT, OFF_RIGHT, OFF_TOP};
use crate::format::COARSE_MAX_VERTICES;
use crate::view::LevelView;

/// Words of one coarse packet: the tag and a Gouraud triangle.
pub const COARSE_PACKET_WORDS: usize = 7;
/// GP0 command of an untextured Gouraud triangle.
const GOURAUD_TRIANGLE_COMMAND: u32 = 0x3000_0000;

/// What a coarse draw needs besides the level.
pub struct CoarseCamera<'a> {
    /// The view's right and forward axes in plan ([`super::plan_axes`]).
    pub right: [i32; 2],
    pub forward: [i32; 2],
    /// Half the horizontal field of view as `tan` times 4096, with margin.
    pub half_tan_q12: i32,
    /// The eye, world runtime units.
    pub eye: [i32; 3],
    /// Screen size in pixels.
    pub screen: [i32; 2],
    /// Smallest depth, in GTE `SZ` units, a corner may have.
    pub near_sz: u16,
    /// A room whose centre is farther than this (plan, runtime units) is not
    /// drawn.
    pub far: i32,
    /// The rooms the draw looks at, ascending ([`rooms_within`]): a room not
    /// listed is not drawn. The list is made once and reused while the eye
    /// stays within its slack, so a frame does not walk every room of the
    /// level to find the few in the band.
    pub rooms: &'a [u16],
}

/// Fill `out` with the rooms whose centre is within `radius` units of `eye`
/// on both plan axes: every room a coarse draw with `far <= radius` can
/// draw, for an eye that has not moved more than `radius - far` since.
pub fn rooms_within(level: &LevelView<'_>, eye: [i32; 3], radius: i32, out: &mut Vec<u16>) {
    out.clear();
    for room in 0..level.rooms.len().min(usize::from(u16::MAX)) {
        let (lo, hi) = level.room_bounds(room);
        let (dx, dz) = ((lo[0] + hi[0]) / 2 - eye[0], (lo[2] + hi[2]) / 2 - eye[2]);
        if dx.abs() <= radius && dz.abs() <= radius {
            out.push(room as u16);
        }
    }
}

/// Scratch of the coarse draw, kept between frames.
pub struct CoarseRenderer {
    screen: Vec<GridScreen>,
    colour: Vec<u32>,
}

/// Zeroed corner, for filling the scratch once.
const BLANK: GridScreen = GridScreen {
    sxy: 0,
    sz: 0,
    code: 0,
};

impl Default for CoarseRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CoarseRenderer {
    /// Scratch for [`Self::draw`]; it takes its memory at the first draw.
    pub const fn new() -> Self {
        Self {
            screen: Vec::new(),
            colour: Vec::new(),
        }
    }

    /// Draw the coarse mesh of every room in the band that `skip` does not
    /// name. The GTE must hold the world view (rotation and the translation
    /// of the world origin).
    ///
    /// Packets go forward from `out[0]`, or backward from the end of `out`
    /// when `reverse`. The frame's `cells_visible` counts the rooms drawn,
    /// `quads_visible` the triangles looked at and `packets` the ones written.
    pub fn draw(
        &mut self,
        level: &LevelView<'_>,
        skip: &[bool],
        camera: &CoarseCamera<'_>,
        out: &mut [u32],
        reverse: bool,
    ) -> GridFrame {
        let mut frame = GridFrame::default();
        if level.coarse.is_none() {
            return frame;
        }
        if self.screen.is_empty() {
            self.screen.resize(COARSE_MAX_VERTICES, BLANK);
            self.colour.resize(COARSE_MAX_VERTICES, 0);
        }
        let start = out.as_mut_ptr();
        // SAFETY: one past the end of the slice.
        let end = unsafe { start.add(out.len()) };
        let mut next = if reverse { end } else { start };
        let mut packets = 0u32;
        let mut tested = 0u16;
        let eye = camera.eye;
        'rooms: for &room in camera.rooms {
            let room = usize::from(room);
            if skip.get(room).copied().unwrap_or(false) {
                continue;
            }
            let (Some(&v0), Some(&v1), Some(&t0), Some(&t1)) = (
                level.coarse_vertex_first.get(room),
                level.coarse_vertex_first.get(room + 1),
                level.coarse_triangle_first.get(room),
                level.coarse_triangle_first.get(room + 1),
            ) else {
                continue;
            };
            if v0 == v1 || t0 == t1 {
                continue;
            }
            let (lo, hi) = level.room_bounds(room);
            let (dx, dz) = ((lo[0] + hi[0]) / 2 - eye[0], (lo[2] + hi[2]) / 2 - eye[2]);
            if dx.abs() > camera.far || dz.abs() > camera.far {
                continue;
            }
            // Plan view cone on the room's centre, its half extent as slack:
            // behind the eye or outside the field of view is skipped before
            // anything is projected.
            let slack = ((hi[0] - lo[0]).max(hi[2] - lo[2]) >> 1) + 16;
            let ahead = (camera.forward[0] * dx + camera.forward[1] * dz) >> 12;
            let across = (camera.right[0] * dx + camera.right[1] * dz) >> 12;
            if ahead < -slack || across.abs() > ((ahead.max(0) * camera.half_tan_q12) >> 12) + slack
            {
                continue;
            }
            let vertices = level
                .coarse_vertices
                .get(usize::from(v0)..usize::from(v1))
                .unwrap_or(&[]);
            let triangles = level
                .coarse_triangles
                .get(usize::from(t0)..usize::from(t1))
                .unwrap_or(&[]);
            frame.cells_visible += 1;
            let count = vertices.len().min(COARSE_MAX_VERTICES);
            for (k, vertex) in vertices.iter().take(count).enumerate() {
                let p =
                    scene::project_vertex_scheduled(GteVec3I16::new(vertex.x, vertex.y, vertex.z));
                self.screen[k] = screen_of(p, camera);
                self.colour[k] = colour_word(vertex.rgb);
            }
            for triangle in triangles {
                tested = tested.wrapping_add(1);
                let (a, b, c) = (
                    usize::from(triangle[0]),
                    usize::from(triangle[1]),
                    usize::from(triangle[2]),
                );
                // Corners index the room's own vertices: `validate_full`
                // proved it at load; the scratch holds 255 and `count` is
                // clamped to that, so a bad corner only reads a stale slot.
                let (sa, sb, sc) = (
                    &self.screen[a.min(COARSE_MAX_VERTICES - 1)],
                    &self.screen[b.min(COARSE_MAX_VERTICES - 1)],
                    &self.screen[c.min(COARSE_MAX_VERTICES - 1)],
                );
                let any = sa.code | sb.code | sc.code;
                if any & NEAR != 0 || sa.code & sb.code & sc.code & OFF_ANY != 0 {
                    continue;
                }
                // A triangle wholly on screen is safe for the GPU; the others
                // are checked against its size limits.
                if any != 0 && !gpu_safe(sa, sb, sc) {
                    continue;
                }
                let sum = u32::from(sa.sz) + u32::from(sb.sz) + u32::from(sc.sz);
                let otz = (sum * 0x155) >> 12;
                if otz == 0 || otz >= 2048 {
                    frame.depth_skipped += 1;
                    continue;
                }
                // SAFETY: both pointers lie in the packet storage.
                let left = unsafe {
                    if reverse {
                        next.offset_from(start)
                    } else {
                        end.offset_from(next)
                    }
                };
                if left < COARSE_PACKET_WORDS as isize {
                    frame.overflowed = true;
                    break 'rooms;
                }
                let (ca, cb, cc) = (
                    self.colour[a.min(COARSE_MAX_VERTICES - 1)],
                    self.colour[b.min(COARSE_MAX_VERTICES - 1)],
                    self.colour[c.min(COARSE_MAX_VERTICES - 1)],
                );
                // SAFETY: room for one packet was just checked.
                unsafe {
                    let at = if reverse {
                        next.sub(COARSE_PACKET_WORDS)
                    } else {
                        next
                    };
                    at.write(((COARSE_PACKET_WORDS as u32 - 1) << 24) | otz);
                    at.add(1).write(GOURAUD_TRIANGLE_COMMAND | ca);
                    at.add(2).write(sa.sxy);
                    at.add(3).write(cb);
                    at.add(4).write(sb.sxy);
                    at.add(5).write(cc);
                    at.add(6).write(sc.sxy);
                    next = if reverse {
                        at
                    } else {
                        at.add(COARSE_PACKET_WORDS)
                    };
                }
                packets += 1;
            }
        }
        frame.packets = packets;
        frame.quads_visible = tested;
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
}

/// Expand RGB555 to a GP0 colour word's low 24 bits.
#[inline(always)]
fn colour_word(rgb555: u16) -> u32 {
    let c = u32::from(rgb555);
    (c & 31) << 3 | ((c >> 5) & 31) << 11 | ((c >> 10) & 31) << 19
}

/// Outcode of a projected vertex.
#[inline(always)]
fn screen_of(p: scene::Projected, camera: &CoarseCamera<'_>) -> GridScreen {
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

/// Whether the GPU draws a triangle with these corners as given: every
/// corner inside 11 bits and the span under the 1024 by 512 limit.
#[inline(always)]
fn gpu_safe(a: &GridScreen, b: &GridScreen, c: &GridScreen) -> bool {
    let (mut left, mut right, mut top, mut bottom) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for corner in [a, b, c] {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_word_puts_red_low_and_blue_high() {
        assert_eq!(colour_word(0x7fff), 0x00f8_f8f8);
        assert_eq!(colour_word(0x001f), 0x0000_00f8);
        assert_eq!(colour_word(0x7c00), 0x00f8_0000);
        assert_eq!(colour_word(0x03e0), 0x0000_f800);
    }
}
