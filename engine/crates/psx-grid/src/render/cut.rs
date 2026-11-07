//! Cutting a quad where its texture would bend.
//!
//! The GPU interpolates texture coordinates linearly in screen space, so a
//! quad whose edges run through a large range of depth shows a picture that
//! bends along its diagonal. `psx_engine::tess` predicts the displacement of
//! an edge from its projected end points (see [`edge_exceeds`]); a quad with
//! an edge over the budget is cut here.
//!
//! The decision belongs to the edge, not to the quad: an edge is cut if and
//! only if its own two projected end points say so, and its midpoint is the
//! projection of the average of its two room positions. Two quads that share
//! an edge (they share its two cached vertices) therefore cut it at the same
//! point, and no vertex ever sits off the straight edge a neighbour draws.
//! That leaves no cracks, and it is why a quad is never cut across just
//! because the opposite edge of a pair asks for it.
//!
//! Which edges of a quad are cut gives its shape:
//! - none: the quad as it is;
//! - the two opposite edges of a pair: two quads;
//! - all four: four quads around a centre vertex;
//! - anything else: a fan around a centre vertex.
//!
//! The cut is one level deep: halving an edge divides its error by about
//! four, and a second level would make two quads that share an edge depend on
//! how each reached it.

use psx_gte::math::Vec3I16 as GteVec3I16;
use psx_gte::scene;

use super::emit::write_quad;
use super::*;

/// Slots of a cut's vertices: the four corners, the midpoints of edges 0 to
/// 3, then the centre.
const MID: usize = 4;
const CENTRE: usize = 8;

/// Which edges are cut, numbered like the corners: edge `k` joins corner `k`
/// to corner `k + 1`.
const PAIR_ACROSS: u8 = 0b0101;
const PAIR_ALONG: u8 = 0b1010;
const ALL_EDGES: u8 = 0b1111;

/// The vertex slots of the pieces of each shape, in ring order.
const ACROSS: [[usize; 4]; 2] = [[0, MID, MID + 2, 3], [MID, 1, 2, MID + 2]];
const ALONG: [[usize; 4]; 2] = [[0, 1, MID + 1, MID + 3], [MID + 3, MID + 1, 2, 3]];
const QUARTERS: [[usize; 4]; 4] = [
    [0, MID, CENTRE, MID + 3],
    [MID, 1, MID + 1, CENTRE],
    [CENTRE, MID + 1, 2, MID + 2],
    [MID + 3, CENTRE, MID + 2, 3],
];

/// Per byte floor average of two packed values; `mask` is the packed `0xfe`
/// of each byte, so a carry never crosses a byte.
#[inline(always)]
fn average(a: u32, b: u32, mask: u32) -> u32 {
    (a & b) + (((a ^ b) & mask) >> 1)
}

/// The bits of the edges of the quad of `screen` whose texture would bend: an
/// edge bends when its affine error, `L |zb - za| / (2 (za + zb))` pixels for
/// a screen length `L` (see `psx_engine::tess`), passes the budget. Written
/// out edge by edge on the corners' packed words so it stays in registers.
#[inline(always)]
pub(super) fn bend_edges(screen: &[&GridScreen; 4]) -> u8 {
    let (a, b, c, d) = (screen[0], screen[1], screen[2], screen[3]);
    let (pa, pb, pc, pd) = (a.sxy, b.sxy, c.sxy, d.sxy);
    let (za, zb, zc, zd) = (
        u32::from(a.sz),
        u32::from(b.sz),
        u32::from(c.sz),
        u32::from(d.sz),
    );
    // Most quads cannot bend: no edge is longer than the box of the corners
    // nor steps deeper than the corners' depth range, and the budget scales
    // with the nearer depth. One test of the box rules them out before any
    // edge is measured.
    let x = |p: u32| p as u16 as i16;
    let y = |p: u32| (p >> 16) as u16 as i16;
    let (left, right) = (
        x(pa).min(x(pb)).min(x(pc)).min(x(pd)),
        x(pa).max(x(pb)).max(x(pc)).max(x(pd)),
    );
    let (top, bottom) = (
        y(pa).min(y(pb)).min(y(pc)).min(y(pd)),
        y(pa).max(y(pb)).max(y(pc)).max(y(pd)),
    );
    let (width, height) = (
        right.abs_diff(left).min(4095),
        bottom.abs_diff(top).min(4095),
    );
    let (long, short) = if width > height {
        (width, height)
    } else {
        (height, width)
    };
    let reach = u32::from(long) + ((u32::from(short) * 3) >> 3);
    let (near, far) = (za.min(zb).min(zc).min(zd), za.max(zb).max(zc).max(zd));
    if reach * (far - near) * 2 <= near * u32::from(WARP_BUDGET_Q3) {
        return 0;
    }
    let bends = |p: u32, z: u32, q: u32, w: u32| -> bool {
        let dx = (p as u16 as i16).abs_diff(q as u16 as i16).min(4095);
        let dy = ((p >> 16) as u16 as i16)
            .abs_diff((q >> 16) as u16 as i16)
            .min(4095);
        let (hi, lo) = if dx > dy { (dx, dy) } else { (dy, dx) };
        // length < 2^13, depth step < 2^16, times four: under 2^31.
        let length = u32::from(hi) + ((u32::from(lo) * 3) >> 3);
        z != 0 && w != 0 && length * z.abs_diff(w) * 4 > (z + w) * u32::from(WARP_BUDGET_Q3)
    };
    u8::from(bends(pa, za, pb, zb))
        | u8::from(bends(pb, zb, pc, zc)) << 1
        | u8::from(bends(pc, zc, pd, zd)) << 2
        | u8::from(bends(pd, zd, pa, za)) << 3
}

/// The vertices of one cut: the corners of the quad, the midpoints of its cut
/// edges and its centre, as parallel arrays so nothing is copied about.
struct Cut {
    screen: [GridScreen; 9],
    position: [[i16; 3]; 9],
    uv: [u16; 9],
    color: [u32; 9],
}

impl Cut {
    /// The vertex at `slot`: the projection of the room position `position`
    /// with the attributes `uv` and `color`.
    #[inline(always)]
    fn place(&mut self, slot: usize, position: [i16; 3], uv: u32, color: u32) {
        let p = scene::project_vertex(GteVec3I16::new(position[0], position[1], position[2]));
        self.screen[slot] = GridScreen {
            sxy: u32::from(p.sx as u16) | (u32::from(p.sy as u16) << 16),
            sz: p.sz,
            code: 0,
        };
        self.position[slot] = position;
        self.uv[slot] = uv as u16;
        self.color[slot] = color;
    }

    /// The vertex halfway along the edge from corner `i` to corner `j`: the
    /// projection of the average of the positions, the average of the
    /// attributes. Symmetric in `i` and `j`, so neighbours that share an
    /// edge agree on it.
    #[inline(never)]
    fn halfway(&mut self, slot: usize, i: usize, j: usize) {
        let (a, b) = (self.position[i], self.position[j]);
        let position = [0, 1, 2].map(|k| ((i32::from(a[k]) + i32::from(b[k])) >> 1) as i16);
        let uv = average(self.uv[i].into(), self.uv[j].into(), 0xfefe);
        let color = average(self.color[i], self.color[j], 0x00fe_fefe);
        self.place(slot, position, uv, color);
    }

    /// The centre: the projection of the average of the four positions, the
    /// mean of the two diagonals' attributes.
    #[inline(never)]
    fn middle(&mut self) {
        let p = &self.position;
        let position = [0, 1, 2].map(|k| {
            ((i32::from(p[0][k]) + i32::from(p[1][k]) + i32::from(p[2][k]) + i32::from(p[3][k]))
                >> 2) as i16
        });
        let uv = average(
            average(self.uv[0].into(), self.uv[2].into(), 0xfefe),
            average(self.uv[1].into(), self.uv[3].into(), 0xfefe),
            0xfefe,
        );
        let color = average(
            average(self.color[0], self.color[2], 0x00fe_fefe),
            average(self.color[1], self.color[3], 0x00fe_fefe),
            0x00fe_fefe,
        );
        self.place(CENTRE, position, uv, color);
    }
}

/// Where and how the pieces are written.
pub(super) struct Writer<'a> {
    pub material: &'a GridMaterial,
    pub origin: u32,
    pub window: u32,
    pub packets: u32,
}

/// Write a quad cut at its bending `edges`.
///
/// # Safety
/// `next` must have room for the worst case, [`ROBUST_WORST_WORDS`] words.
#[inline(never)]
pub(super) unsafe fn cut_quad(
    mut next: *mut u32,
    room: &RoomView<'_>,
    quad: &Quad,
    screen: [&GridScreen; 4],
    edges: u8,
    write: &mut Writer<'_>,
) -> *mut u32 {
    let mut cut = Cut {
        screen: [GridScreen::default(); 9],
        position: [[0; 3]; 9],
        uv: [0; 9],
        color: [0; 9],
    };
    for (k, corner) in screen.iter().enumerate() {
        // SAFETY: validation proved the index.
        let v = unsafe { room.render.vertices.get_unchecked(usize::from(quad.v[k])) };
        cut.screen[k] = **corner;
        cut.position[k] = [v.x, v.y, v.z];
        cut.uv[k] = quad.uv[k];
        cut.color[k] = quad.color[k];
    }
    for k in 0..4 {
        if edges & (1 << k) != 0 {
            cut.halfway(MID + k, k, (k + 1) & 3);
        }
    }
    if edges != PAIR_ACROSS && edges != PAIR_ALONG {
        cut.middle();
    }
    let shapes: &[[usize; 4]] = match edges {
        PAIR_ACROSS => &ACROSS,
        PAIR_ALONG => &ALONG,
        ALL_EDGES => &QUARTERS,
        _ => &[],
    };
    for shape in shapes {
        // SAFETY: room per the contract.
        next = unsafe { put_quad(next, &cut, *shape, write) };
    }
    if shapes.is_empty() {
        // A fan around the centre over the ring with the cut midpoints; its
        // triangles go out as quads that repeat their last corner.
        let mut ring = [0usize; 8];
        let mut count = 0;
        for k in 0..4 {
            ring[count] = k;
            count += 1;
            if edges & (1 << k) != 0 {
                ring[count] = MID + k;
                count += 1;
            }
        }
        for k in 0..count {
            let last = ring[(k + 1) % count];
            // SAFETY: room per the contract.
            next = unsafe { put_quad(next, &cut, [CENTRE, ring[k], last, last], write) };
        }
    }
    next
}

/// Whether every corner lies beyond the same side of the screen.
#[inline(always)]
fn off_screen(corners: &[&GridScreen; 4]) -> bool {
    let (mut left, mut right, mut above, mut below) = (true, true, true, true);
    for c in corners {
        let (x, y) = (c.sxy as u16 as i16, (c.sxy >> 16) as u16 as i16);
        left &= x < 0;
        right &= x > 319;
        above &= y < 0;
        below &= y > 239;
    }
    left | right | above | below
}

/// Write the quad of the vertices at `slots` unless it is off screen or out
/// of the ordering table.
///
/// # Safety
/// `next` must have room for [`QUAD_PACKET_WORDS`].
#[inline(never)]
unsafe fn put_quad(
    next: *mut u32,
    cut: &Cut,
    slots: [usize; 4],
    write: &mut Writer<'_>,
) -> *mut u32 {
    let s = slots.map(|slot| &cut.screen[slot]);
    if off_screen(&s) {
        return next;
    }
    let otz =
        (u32::from(s[0].sz) + u32::from(s[1].sz) + u32::from(s[2].sz) + u32::from(s[3].sz)) >> 4;
    if otz == 0 || otz >= 2048 {
        return next;
    }
    let quad = Quad {
        color: slots.map(|slot| cut.color[slot]),
        uv: slots.map(|slot| cut.uv[slot]),
        ..Quad::default()
    };
    write.packets += 1;
    // SAFETY: room per the contract.
    unsafe {
        write_quad(
            next,
            &quad,
            write.material,
            s,
            otz,
            write.origin,
            write.window,
        )
    }
}

#[cfg(test)]
mod tests {
    use psx_engine::tess::{edge_exceeds, ScreenDepth};

    use super::*;

    fn screen_depth(corner: &GridScreen) -> ScreenDepth {
        ScreenDepth {
            x: i32::from(corner.sxy as u16 as i16),
            y: i32::from((corner.sxy >> 16) as u16 as i16),
            z: i32::from(corner.sz),
        }
    }

    fn corner(x: i16, y: i16, z: u16) -> GridScreen {
        GridScreen {
            sxy: u32::from(x as u16) | (u32::from(y as u16) << 16),
            sz: z,
            code: 0,
        }
    }

    fn cut_of(position: [[i16; 3]; 2], uv: [u16; 2], color: [u32; 2]) -> Cut {
        let mut cut = Cut {
            screen: [GridScreen::default(); 9],
            position: [[0; 3]; 9],
            uv: [0; 9],
            color: [0; 9],
        };
        cut.position[0] = position[0];
        cut.position[1] = position[1];
        cut.uv[..2].copy_from_slice(&uv);
        cut.color[..2].copy_from_slice(&color);
        cut
    }

    #[test]
    fn averages_never_carry_between_bytes() {
        assert_eq!(average(0x00ff, 0x01ff, 0xfefe), 0x00ff);
        assert_eq!(average(0x0100, 0x0001, 0xfefe), 0x0000);
        assert_eq!(average(0xff_ff_ff, 0x00_00_00, 0x00fe_fefe), 0x7f_7f_7f);
    }

    #[test]
    fn a_midpoint_does_not_depend_on_the_direction_of_the_edge() {
        let mut forward = cut_of(
            [[10, 0, 100], [80, 0, 201]],
            [0x0001, 0x1011],
            [0x00_20_40_60, 0x00_23_43_63],
        );
        let mut backward = cut_of(
            [[80, 0, 201], [10, 0, 100]],
            [0x1011, 0x0001],
            [0x00_23_43_63, 0x00_20_40_60],
        );
        forward.halfway(MID, 0, 1);
        backward.halfway(MID, 0, 1);
        assert_eq!(
            (forward.screen[MID].sxy, forward.screen[MID].sz),
            (backward.screen[MID].sxy, backward.screen[MID].sz)
        );
        assert_eq!(forward.position[MID], backward.position[MID]);
        assert_eq!(forward.uv[MID], backward.uv[MID]);
        assert_eq!(forward.color[MID], backward.color[MID]);
    }

    #[test]
    fn an_edge_is_judged_by_its_end_points_alone() {
        // Two quads that share the edge from (300, 20) at depth 400 to
        // (300, 200) at depth 2400 say the same of it, whatever else they
        // hold.
        let (near, far) = (corner(300, 20, 400), corner(300, 200, 2400));
        let a = [corner(10, 10, 400), near, far, corner(10, 230, 2400)];
        let b = [near, corner(310, 15, 400), corner(310, 205, 2400), far];
        let (ea, eb) = (
            bend_edges(&[&a[0], &a[1], &a[2], &a[3]]),
            bend_edges(&[&b[0], &b[1], &b[2], &b[3]]),
        );
        // Edge 1 of `a` runs near -> far; edge 3 of `b` runs far -> near.
        assert_eq!((ea >> 1) & 1, (eb >> 3) & 1);
        assert_eq!((ea >> 1) & 1, 1, "the edge runs through a deep range");
    }

    #[test]
    fn a_flat_quad_is_left_alone() {
        let s = [
            corner(100, 100, 600),
            corner(200, 100, 600),
            corner(200, 200, 600),
            corner(100, 200, 600),
        ];
        assert_eq!(bend_edges(&[&s[0], &s[1], &s[2], &s[3]]), 0);
    }

    #[test]
    fn the_edge_test_matches_the_engines_predicate() {
        let mut seed = 0x1234_5678u32;
        let mut next = |range: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) % range
        };
        let mut bent = 0;
        for _ in 0..20_000 {
            let s = [(); 4].map(|_| {
                corner(
                    next(900) as i16 - 300,
                    next(700) as i16 - 200,
                    60 + next(3000) as u16,
                )
            });
            let budget = u32::from(WARP_BUDGET_Q3);
            let direct = (0..4).any(|k| {
                edge_exceeds(
                    screen_depth(&s[k]),
                    screen_depth(&s[(k + 1) & 3]),
                    budget,
                    0,
                )
            });
            let found = bend_edges(&[&s[0], &s[1], &s[2], &s[3]]) != 0;
            assert_eq!(direct, found, "{:?}", s.map(|c| (c.sxy, c.sz)));
            bent += usize::from(found);
        }
        assert!(bent > 1000, "the sample must exercise the bound");
    }

    #[test]
    fn every_shape_names_slots_that_exist() {
        for shape in ACROSS.iter().chain(&ALONG).chain(&QUARTERS) {
            assert!(shape.iter().all(|&slot| slot <= CENTRE));
        }
    }
}
