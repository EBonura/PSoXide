//! The far skin: the whole level as flat coloured top surfaces and the
//! skirts that hang under them, drawn for the rooms that are not drawn
//! whole.
//!
//! Each [`SkinLeaf`](crate::SkinLeaf) is a square of sectors with one top
//! height and one colour. A leaf draws its top (when the eye is above it) and
//! a skirt on each edge that faces the eye and drops to a lower neighbour.
//! Every polygon is an untextured Gouraud quad keyed into the ordering table
//! by the same depth law as the room quads, so the skin sorts among them.
//! There is no near-plane clip: a leaf with a corner at or behind the near
//! plane is not drawn (a leaf is only drawn for rooms beyond the drawn
//! radius, far from the eye).
//!
//! The draw runs in three small loops over a job list (select, project, emit)
//! instead of one loop doing everything per leaf: each loop body fits the
//! instruction cache, and one fat loop does not.

use alloc::vec::Vec;

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;
use psx_math::int32::isqrt_i32;

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
/// Most leaves one frame draws from.
const JOB_CAP: usize = 320;

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

/// One leaf the frame draws from.
#[derive(Copy, Clone, Default)]
struct Job {
    x0: i16,
    z0: i16,
    /// Top height, runtime units.
    y: i16,
    /// Edge, runtime units.
    size: i16,
    /// The leaf's `look` word: colour and skirt nibbles.
    look: u32,
    /// Bit 0: draw the top. Bits 1 to 4: draw the skirt of side 0 to 3.
    vis: u8,
}

/// Scratch of the skin draw, kept between frames.
pub struct SkinRenderer {
    jobs: Vec<Job>,
    /// Per job, its four top corners.
    tops: Vec<[GridScreen; 4]>,
    /// Two bottom corners per skirt drawn, in job and side order.
    lows: Vec<GridScreen>,
}

impl Default for SkinRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl SkinRenderer {
    /// Scratch for [`Self::draw`]; it takes its memory at the first draw of a
    /// level that has a skin, so a level without one pays nothing.
    pub fn new() -> Self {
        Self {
            jobs: Vec::new(),
            tops: Vec::new(),
            lows: Vec::new(),
        }
    }

    /// Draw the skin of every room `skip` does not name. The GTE must hold
    /// the world view (rotation, and the translation of the world origin).
    ///
    /// Packets go forward from `out[0]`, or backward from the end of `out`
    /// when `reverse`; the returned frame says how many words and packets.
    /// A leaf that does not fit sets `overflowed` and ends the draw. The
    /// frame's `cells_visible` counts the rooms that passed the frustum,
    /// `screen_culled` the leaves looked at and `quads_visible` the leaves
    /// drawn from.
    pub fn draw(
        &mut self,
        level: &LevelView<'_>,
        skip: &[bool],
        camera: &SkinCamera<'_>,
        out: &mut [u32],
        reverse: bool,
    ) -> GridFrame {
        self.draw_with_marks(level, skip, camera, out, reverse, &mut |_, _| {})
    }

    /// [`Self::draw`] calling `mark(phase, begin)` around each of its three
    /// phases (0 select, 1 project, 2 emit), for timing.
    pub fn draw_with_marks(
        &mut self,
        level: &LevelView<'_>,
        skip: &[bool],
        camera: &SkinCamera<'_>,
        out: &mut [u32],
        reverse: bool,
        mark: &mut dyn FnMut(u8, bool),
    ) -> GridFrame {
        let mut frame = GridFrame::default();
        let Some(header) = level.skin else {
            return frame;
        };
        mark(0, true);
        self.select(level, skip, camera, &mut frame);
        mark(0, false);
        frame.quads_visible = self.jobs.len() as u16;
        mark(1, true);
        self.project(i32::from(header.height_step), camera);
        mark(1, false);
        mark(2, true);
        if reverse {
            self.emit::<true>(out, &mut frame);
        } else {
            self.emit::<false>(out, &mut frame);
        }
        mark(2, false);
        frame
    }

    /// Choose the leaves to draw from: the rooms in reach and in view, at the
    /// level of detail their distance can afford, and in them the leaves in
    /// the view cone that show a top or a skirt.
    #[inline(never)]
    fn select(
        &mut self,
        level: &LevelView<'_>,
        skip: &[bool],
        camera: &SkinCamera<'_>,
        frame: &mut GridFrame,
    ) {
        self.jobs.clear();
        let Some(header) = level.skin else {
            return;
        };
        if self.jobs.capacity() == 0 {
            self.jobs.reserve_exact(JOB_CAP);
            self.tops.reserve_exact(JOB_CAP);
            self.lows.reserve_exact(JOB_CAP * 2);
        }
        let sector = level.sector_units() as i32;
        let origin = [
            i32::from(header.origin[0]) * sector,
            i32::from(header.origin[1]) * sector,
        ];
        let base_y = i32::from(header.base_y);
        let step = i32::from(header.height_step);
        let eye = camera.eye;
        let lods = usize::from(header.lod_count).max(1);
        let mut tested = 0u16;
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
            let lod = usize::from(plan_gap >= camera.lod_gap[0])
                + usize::from(plan_gap >= camera.lod_gap[1]);
            let at = room * lods + lod.min(lods - 1);
            let (Some(&first), Some(&last)) =
                (level.skin_cells.get(at), level.skin_cells.get(at + 1))
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
                tested = tested.wrapping_add(1);
                let n = sector << leaf.size_log2();
                let x0 = origin[0] + i32::from(leaf.x()) * sector;
                let z0 = origin[1] + i32::from(leaf.z()) * sector;
                // Plan view cone on the leaf's centre, with its size as
                // slack: behind the eye or outside the field of view is
                // skipped before anything is projected.
                let (dx, dz) = (x0 + n / 2 - eye[0], z0 + n / 2 - eye[2]);
                let ahead = (camera.forward[0] * dx + camera.forward[1] * dz) >> 12;
                let across = (camera.right[0] * dx + camera.right[1] * dz) >> 12;
                if ahead < -n
                    || ahead > camera.reach + n
                    || across.abs() > ((ahead.max(0) * camera.half_tan_q12) >> 12) + n
                {
                    continue;
                }
                let y = base_y + i32::from(leaf.height()) * step;
                // The top shows from above; an edge's skirt shows from its
                // outer side and only where there is somewhere to drop to.
                let mut vis = u8::from(eye[1] > y);
                let (x1, z1) = (x0 + n, z0 + n);
                let faces = [eye[2] < z0, eye[0] > x1, eye[2] > z1, eye[0] < x0];
                for side in 0..4u8 {
                    if faces[usize::from(side)] && leaf.skirt(side) != 0 {
                        vis |= 2 << side;
                    }
                }
                if vis == 0 {
                    continue;
                }
                if self.jobs.len() == JOB_CAP {
                    break 'rooms;
                }
                self.jobs.push(Job {
                    x0: x0 as i16,
                    z0: z0 as i16,
                    y: y as i16,
                    size: n as i16,
                    look: leaf.look,
                    vis,
                });
            }
        }
        frame.screen_culled = tested;
    }

    /// Project the corners of every job: its four top corners, and the two
    /// bottom corners of each skirt that can show.
    #[inline(never)]
    fn project(&mut self, step: i32, camera: &SkinCamera<'_>) {
        self.tops.clear();
        self.lows.clear();
        for job in &mut self.jobs {
            let (x0, z0, y) = (i32::from(job.x0), i32::from(job.z0), i32::from(job.y));
            let (x1, z1) = (x0 + i32::from(job.size), z0 + i32::from(job.size));
            let tops = project4([(x0, y, z0), (x1, y, z0), (x1, y, z1), (x0, y, z1)], camera);
            for side in 0..4usize {
                let bit = 2u8 << side;
                if job.vis & bit == 0 {
                    continue;
                }
                let (a, b) = (side, (side + 1) & 3);
                // A skirt hangs below its top edge: wholly past a side or the
                // bottom of the screen when the edge is, and not drawn at all
                // at the near plane.
                if tops[a].code & tops[b].code & (OFF_LEFT | OFF_RIGHT | OFF_BOTTOM) != 0
                    || (tops[a].code | tops[b].code) & NEAR != 0
                {
                    job.vis &= !bit;
                    continue;
                }
                let nibble = (job.look >> (15 + 4 * side as u32)) & 15;
                let depth = i32::from(SKIRT_STEPS[nibble as usize]) * step;
                let corner = |i: usize| match i {
                    0 => (x0, z0),
                    1 => (x1, z0),
                    2 => (x1, z1),
                    _ => (x0, z1),
                };
                let (ax, az) = corner(a);
                let (bx, bz) = corner(b);
                self.lows.push(screen_of(
                    scene::project_vertex_scheduled(vertex(bx, y - depth, bz)),
                    camera,
                ));
                self.lows.push(screen_of(
                    scene::project_vertex_scheduled(vertex(ax, y - depth, az)),
                    camera,
                ));
            }
            self.tops.push(tops);
        }
    }

    /// Write the packets of every job's top and skirts.
    #[inline(never)]
    fn emit<const REVERSE: bool>(&self, out: &mut [u32], frame: &mut GridFrame) {
        let start = out.as_mut_ptr();
        // SAFETY: one past the end of the slice.
        let end = unsafe { start.add(out.len()) };
        // Forward: the next word to write. Reverse: the first word written.
        let mut next = if REVERSE { end } else { start };
        let mut packets = 0u32;
        let mut low_at = 0usize;
        'jobs: for (job, tops) in self.jobs.iter().zip(&self.tops) {
            let rgb = (job.look & 0x7fff) as u16;
            if job.vis & 1 != 0 && drawable(tops) {
                if !write_quad::<REVERSE>(&mut next, start, end, tops, colour_word(rgb, 4)) {
                    frame.overflowed = true;
                    break 'jobs;
                }
                packets += 1;
            }
            for side in 0..4usize {
                if job.vis & (2 << side) == 0 {
                    continue;
                }
                let (a, b) = (side, (side + 1) & 3);
                let quad = [tops[a], tops[b], self.lows[low_at], self.lows[low_at + 1]];
                low_at += 2;
                if drawable(&quad) {
                    if !write_quad::<REVERSE>(
                        &mut next,
                        start,
                        end,
                        &quad,
                        colour_word(rgb, SKIRT_SHADE_QUARTERS),
                    ) {
                        frame.overflowed = true;
                        break 'jobs;
                    }
                    packets += 1;
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

/// A GTE vertex from world coordinates the cook proved fit 16 bits.
#[inline(always)]
fn vertex(x: i32, y: i32, z: i32) -> GteVec3I16 {
    GteVec3I16::new(x as i16, y as i16, z as i16)
}

/// Project four corners: a triangle of the GTE and one more.
#[inline(always)]
fn project4(corners: [(i32, i32, i32); 4], camera: &SkinCamera<'_>) -> [GridScreen; 4] {
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
    left > -1000
        && right < 1000
        && top > -1000
        && bottom < 1000
        && right - left <= 1023
        && bottom - top <= 511
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

/// Write one packet at `next` (forward from `start`, or backward from
/// `end`), keyed by the corners' depth. Returns false when the storage is
/// full. A quad outside the ordering table writes nothing and still returns
/// true.
#[inline(always)]
fn write_quad<const REVERSE: bool>(
    next: &mut *mut u32,
    start: *mut u32,
    end: *mut u32,
    s: &[GridScreen; 4],
    colour: u32,
) -> bool {
    let sum = u32::from(s[0].sz) + u32::from(s[1].sz) + u32::from(s[2].sz) + u32::from(s[3].sz);
    let otz = sum >> 4;
    if otz == 0 || otz >= 2048 {
        return true;
    }
    let cur = *next;
    // SAFETY: both pointers lie in the packet storage.
    let left = unsafe {
        if REVERSE {
            cur.offset_from(start)
        } else {
            end.offset_from(cur)
        }
    };
    if left < SKIN_PACKET_WORDS as isize {
        return false;
    }
    // SAFETY: room for one packet was just checked.
    unsafe {
        let at = if REVERSE {
            cur.sub(SKIN_PACKET_WORDS)
        } else {
            cur
        };
        // The GPU splits along the second and third corners, so the corners
        // go out as (1, 0, 2, 3), as the room quads do.
        at.write(((SKIN_PACKET_WORDS as u32 - 1) << 24) | otz);
        at.add(1).write(GOURAUD_QUAD_COMMAND | colour);
        at.add(2).write(s[1].sxy);
        at.add(3).write(colour);
        at.add(4).write(s[0].sxy);
        at.add(5).write(colour);
        at.add(6).write(s[2].sxy);
        at.add(7).write(colour);
        at.add(8).write(s[3].sxy);
        *next = if REVERSE {
            at
        } else {
            at.add(SKIN_PACKET_WORDS)
        };
    }
    true
}
