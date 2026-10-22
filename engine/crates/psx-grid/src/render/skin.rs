//! The far skin: the whole level as flat coloured top surfaces and the
//! skirts that hang under them, drawn for the rooms that are not drawn
//! whole.
//!
//! Each [`SkinLeaf`] is a square of sectors with one top height and one
//! colour. A leaf draws its top (when the eye is above it) and a skirt on
//! each edge that faces the eye and drops to a lower neighbour. Every
//! polygon is an untextured Gouraud quad keyed into the ordering table by
//! the same depth law as the room quads, so the skin sorts among them. There
//! is no near-plane clip: a leaf with a corner at or behind the near plane is
//! not drawn (a leaf is only drawn for rooms beyond the drawn radius, far
//! from the eye).

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_math::int32::isqrt_i32;
use psx_gte::scene;

use super::frustum::{FrustumPlanes, CLIP_ALL_PLANES};
use super::{GridFrame, GridScreen, NEAR, OFF_ANY, OFF_BOTTOM, OFF_LEFT, OFF_RIGHT, OFF_TOP};
use crate::format::SKIRT_STEPS;
use crate::view::LevelView;

/// Words of one skin packet: the tag and a Gouraud quad of four corners.
pub const SKIN_PACKET_WORDS: usize = 9;
/// GP0 command of an untextured Gouraud quad.
const GOURAUD_QUAD_COMMAND: u32 = 0x3800_0000;
/// Skirts are drawn this much darker than the top, out of 4 (3 is 75 percent).
const SKIRT_SHADE_QUARTERS: u32 = 3;

/// What a skin draw needs besides the level.
pub struct SkinCamera<'a> {
    /// World frustum planes for a camera at the origin
    /// ([`FrustumPlanes::at_eye`]), the planes already loaded.
    pub frustum: &'a FrustumPlanes,
    /// The eye, world runtime units.
    pub eye: [i32; 3],
    /// Screen size in pixels.
    pub screen: [i32; 2],
    /// Smallest depth, in GTE `SZ` units, a corner may have.
    pub near_sz: u16,
    /// Rooms and leaves farther than this many runtime units from the eye
    /// along both plan axes are not drawn (the ordering table's far end).
    pub reach: i32,
    /// The view's right and forward axes in plan, `[x, z]` each, as Q12 unit
    /// vectors ([`plan_axes`]).
    pub right: [i32; 2],
    pub forward: [i32; 2],
    /// Half the horizontal field of view as `tan` times 4096, with margin.
    pub half_tan_q12: i32,
    /// Plan gap from the eye (runtime units) from which a room draws its
    /// medium, then its coarse, level of detail.
    pub lod_gap: [i32; 2],
}

/// The view's right and forward axes in plan as Q12 unit vectors, from the
/// rows of the view rotation (`rotation[0]` is view right, `rotation[2]` view
/// depth, in world axes, whatever scale and pitch the matrix carries).
pub fn plan_axes(rotation: &[[i16; 3]; 3]) -> ([i32; 2], [i32; 2]) {
    let unit = |row: &[i16; 3]| {
        let (x, z) = (i32::from(row[0]), i32::from(row[2]));
        let length = isqrt_i32(x * x + z * z).max(1);
        [(x << 12) / length, (z << 12) / length]
    };
    (unit(&rotation[0]), unit(&rotation[2]))
}

/// Draw the skin of every room `skip` does not name. The frame's
/// `cells_visible` counts the rooms that passed the frustum, `screen_culled`
/// the leaves looked at and `quads_visible` the leaves projected.
///
/// The GTE must hold the
/// world view (rotation, and the translation of the world origin).
///
/// Packets go forward from `out[0]`, or backward from the end of `out` when
/// `reverse`; the returned frame says how many words and packets. A leaf
/// that does not fit sets `overflowed` and ends the draw.
pub fn draw_skin(
    level: &LevelView<'_>,
    skip: &[bool],
    camera: &SkinCamera<'_>,
    out: &mut [u32],
    reverse: bool,
) -> GridFrame {
    if reverse {
        draw_dir::<true>(level, skip, camera, out)
    } else {
        draw_dir::<false>(level, skip, camera, out)
    }
}

/// Expand an RGB555 colour to a GP0 colour word's low 24 bits, optionally
/// darkened by `quarters` out of 4.
#[inline(always)]
fn colour_word(rgb555: u16, quarters: u32) -> u32 {
    let c = u32::from(rgb555);
    let scale = |v: u32| ((v << 3) * quarters) >> 2;
    scale(c & 31) | scale((c >> 5) & 31) << 8 | scale((c >> 10) & 31) << 16
}

/// Outcode of a projected corner.
#[inline(always)]
fn screen_of(p: scene::Projected, camera: &SkinCamera<'_>) -> GridScreen {
    let sx = i32::from(p.sx);
    let sy = i32::from(p.sy);
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

/// Whether the GPU draws a quad with these corners as given (see the room
/// emitter's check): every corner inside 11 bits and the span under the
/// 1024 by 512 limit.
#[inline(always)]
fn gpu_safe(corners: &[GridScreen; 4]) -> bool {
    let (mut left, mut right, mut top, mut bottom) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for corner in corners {
        let x = i32::from(corner.sxy as u16 as i16);
        let y = i32::from((corner.sxy >> 16) as u16 as i16);
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    left > -1000 && right < 1000 && top > -1000 && bottom < 1000
        && right - left <= 1023
        && bottom - top <= 511
}

/// A GTE vertex from world coordinates the cook proved fit 16 bits.
#[inline(always)]
fn vertex(x: i32, y: i32, z: i32) -> GteVec3I16 {
    GteVec3I16::new(x as i16, y as i16, z as i16)
}

/// Project four corners: a triangle of the GTE and one more.
#[inline(always)]
fn project4(
    corners: [(i32, i32, i32); 4],
    camera: &SkinCamera<'_>,
) -> [GridScreen; 4] {
    let t = scene::project_triangle_scheduled(
        vertex(corners[0].0, corners[0].1, corners[0].2),
        vertex(corners[1].0, corners[1].1, corners[1].2),
        vertex(corners[2].0, corners[2].1, corners[2].2),
    );
    let d = scene::project_vertex_scheduled(vertex(corners[3].0, corners[3].1, corners[3].2));
    [
        screen_of(t[0], camera),
        screen_of(t[1], camera),
        screen_of(t[2], camera),
        screen_of(d, camera),
    ]
}

/// Whether a quad of these corners is worth a packet: nothing at or behind
/// the near plane, not wholly off one screen edge, safe for the GPU.
#[inline(always)]
fn drawable(s: &[GridScreen; 4]) -> bool {
    let any = s[0].code | s[1].code | s[2].code | s[3].code;
    if any & NEAR != 0 {
        return false;
    }
    if s[0].code & s[1].code & s[2].code & s[3].code & OFF_ANY != 0 {
        return false;
    }
    gpu_safe(s)
}

#[inline(never)]
fn draw_dir<const REVERSE: bool>(
    level: &LevelView<'_>,
    skip: &[bool],
    camera: &SkinCamera<'_>,
    out: &mut [u32],
) -> GridFrame {
    let mut frame = GridFrame::default();
    let (Some(header), true) = (level.skin, !level.skin_leaves.is_empty()) else {
        return frame;
    };
    let sector = level.sector_units() as i32;
    let step = i32::from(header.height_step);
    let origin = [
        i32::from(header.origin[0]) * sector,
        i32::from(header.origin[1]) * sector,
    ];
    let eye = camera.eye;
    let start = out.as_mut_ptr();
    // SAFETY: one past the end of the slice.
    let end = unsafe { start.add(out.len()) };
    // Forward: the next word to write. Reverse: the first word written.
    let mut next = if REVERSE { end } else { start };
    let mut packets = 0u32;
    // Emit one quad. Returns false when the storage is full.
    let mut emit = |s: [&GridScreen; 4], colour: u32| -> bool {
        let sum = u32::from(s[0].sz) + u32::from(s[1].sz) + u32::from(s[2].sz) + u32::from(s[3].sz);
        let otz = sum >> 4;
        if otz == 0 || otz >= 2048 {
            return true;
        }
        // SAFETY: both pointers lie in the packet storage.
        let left = unsafe {
            if REVERSE {
                next.offset_from(start)
            } else {
                end.offset_from(next)
            }
        };
        if left < SKIN_PACKET_WORDS as isize {
            return false;
        }
        // SAFETY: room for one packet was just checked.
        unsafe {
            let at = if REVERSE {
                next.sub(SKIN_PACKET_WORDS)
            } else {
                next
            };
            // The GPU splits along the second and third corners, so the
            // corners go out as (1, 0, 2, 3), as the room quads do.
            at.write(((SKIN_PACKET_WORDS as u32 - 1) << 24) | otz);
            at.add(1).write(GOURAUD_QUAD_COMMAND | colour);
            at.add(2).write(s[1].sxy);
            at.add(3).write(colour);
            at.add(4).write(s[0].sxy);
            at.add(5).write(colour);
            at.add(6).write(s[2].sxy);
            at.add(7).write(colour);
            at.add(8).write(s[3].sxy);
            next = if REVERSE { at } else { at.add(SKIN_PACKET_WORDS) };
        }
        packets += 1;
        true
    };
    'rooms: for room in 0..level.rooms.len() {
        if skip.get(room).copied().unwrap_or(false) {
            continue;
        }
        let (lo, hi) = level.room_bounds(room);
        let gap = |axis: usize| (lo[axis] - eye[axis]).max(eye[axis] - hi[axis]).max(0);
        let plan_gap = gap(0).max(gap(2));
        if plan_gap > camera.reach {
            continue;
        }
        // A room draws the level of detail its distance can afford.
        let lods = usize::from(header.lod_count).max(1);
        let lod = usize::from(plan_gap >= camera.lod_gap[0]) + usize::from(plan_gap >= camera.lod_gap[1]);
        let at = room * lods + lod.min(lods - 1);
        let (Some(&first), Some(&last)) = (level.skin_cells.get(at), level.skin_cells.get(at + 1))
        else {
            continue;
        };
        if first == last {
            continue;
        }
        if camera
            .frustum
            .cull_world_aabb(eye, lo, hi, CLIP_ALL_PLANES)
            .is_none()
        {
            continue;
        }
        frame.cells_visible += 1;
        for leaf in level
            .skin_leaves
            .get(usize::from(first)..usize::from(last))
            .unwrap_or(&[])
        {
            frame.screen_culled = frame.screen_culled.saturating_add(1);
            let n = sector << leaf.size_log2();
            let x0 = origin[0] + i32::from(leaf.x()) * sector;
            let z0 = origin[1] + i32::from(leaf.z()) * sector;
            let (x1, z1) = (x0 + n, z0 + n);
            // Plan view cone, on the leaf's centre with its half diagonal as
            // slack: behind the eye or outside the field of view is skipped
            // before anything is projected.
            let (dx, dz) = (x0 + n / 2 - eye[0], z0 + n / 2 - eye[2]);
            let ahead = (camera.forward[0] * dx + camera.forward[1] * dz) >> 12;
            let across = (camera.right[0] * dx + camera.right[1] * dz) >> 12;
            let slack = n;
            if ahead < -slack
                || across.abs() > ((ahead.max(0) * camera.half_tan_q12) >> 12) + slack
                || ahead > camera.reach + slack
            {
                continue;
            }
            let h = i32::from(header.base_y) + i32::from(leaf.height()) * step;
            let top = eye[1] > h;
            // Edges facing the eye with somewhere to drop to: -Z, +X, +Z, -X.
            let faces = [eye[2] < z0, eye[0] > x1, eye[2] > z1, eye[0] < x0];
            let mut skirts = [0i32; 4];
            let mut any = top;
            for side in 0..4u8 {
                let steps = i32::from(SKIRT_STEPS[usize::from(leaf.skirt(side))]);
                if faces[usize::from(side)] && steps > 0 {
                    skirts[usize::from(side)] = steps * step;
                    any = true;
                }
            }
            if !any {
                continue;
            }
            frame.quads_visible += 1;
            let tops = project4(
                [(x0, h, z0), (x1, h, z0), (x1, h, z1), (x0, h, z1)],
                camera,
            );
            if top
                && drawable(&tops)
                && !emit(
                    [&tops[0], &tops[1], &tops[2], &tops[3]],
                    colour_word(leaf.rgb555(), 4),
                )
            {
                frame.overflowed = true;
                break 'rooms;
            }
            // Each skirt hangs from a top edge (corner pair) down to `depth`.
            let edges = [(0usize, 1usize), (1, 2), (2, 3), (3, 0)];
            for (side, &(a, b)) in edges.iter().enumerate() {
                let depth = skirts[side];
                // A skirt hangs below its top edge: wholly past a side or the
                // bottom of the screen when the edge is.
                if depth == 0
                    || tops[a].code & tops[b].code & (OFF_LEFT | OFF_RIGHT | OFF_BOTTOM) != 0
                    || (tops[a].code | tops[b].code) & NEAR != 0
                {
                    continue;
                }
                let corner = |i: usize| match i {
                    0 => (x0, z0),
                    1 => (x1, z0),
                    2 => (x1, z1),
                    _ => (x0, z1),
                };
                let (ax, az) = corner(a);
                let (bx, bz) = corner(b);
                let low_a = scene::project_vertex_scheduled(vertex(ax, h - depth, az));
                let low_b = scene::project_vertex_scheduled(vertex(bx, h - depth, bz));
                let quad = [
                    tops[a],
                    tops[b],
                    screen_of(low_b, camera),
                    screen_of(low_a, camera),
                ];
                if drawable(&quad)
                    && !emit(
                        [&quad[0], &quad[1], &quad[2], &quad[3]],
                        colour_word(leaf.rgb555(), SKIRT_SHADE_QUARTERS),
                    )
                {
                    frame.overflowed = true;
                    break 'rooms;
                }
            }
        }
    }
    frame.packets = packets;
    // SAFETY: both pointers lie in the packet storage.
    frame.packet_words = unsafe {
        if REVERSE {
            end.offset_from(next)
        } else {
            next.offset_from(start)
        }
    } as usize;
    frame
}
