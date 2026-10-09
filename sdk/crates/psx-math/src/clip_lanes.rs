// SPDX-License-Identifier: GPL-2.0-or-later
//! One-plane convex clip of eight-lane `i32` vertices.
//!
//! GoldSrc's renderers (Half-Life, Counter-Strike) clip textured polygons
//! against the view frustum in view space, one plane at a time, carrying
//! position, colour and UV as eight `i32` lanes: `[x, y, z, r, g, b, u, v]`.
//! Every plane they use is linear in two of those lanes, and every crossing is
//! interpolated the same way, so the whole pass is one function of the plane's
//! four numbers. [`clip_lanes8_plane_reference`] states it with the generic
//! [`attributed_clip`](crate::attributed_clip) kernel; with the `asm-kernels`
//! feature on the console, [`clip_lanes8_plane`] runs a hand-scheduled kernel
//! that produces the same bytes.
//!
//! # Definition
//!
//! A vertex `v` is inside when
//! `v[primary_offset / 4] * primary + v[2] * depth + bias >= 0` (wrapping
//! `i32` arithmetic). The polygon is traversed previous-to-current; each edge
//! whose endpoints disagree emits a crossing before the current vertex, and an
//! inside vertex is copied. A crossing orders its endpoints by `(x, y, z)`
//! (signed, lexicographic) so a shared edge interpolates identically from both
//! sides, takes `t = ratio_q12_i32(d_a, d_a - d_b)` with the distances in that
//! order, and sets every lane to [`lerp_q12_i32_wide`] of the two endpoints;
//! lane 2 is then replaced by `forced_depth` when `force_depth` is non-zero (a
//! near plane lands exactly on its distance). When `count` vertices would
//! overflow `capacity` the pass stops and returns `capacity`.

use crate::attributed_clip::{
    clip_to_plane, lerp_q12_i32_wide, ratio_q12_i32, AttributedClipPlane, ClipTraversal,
};

/// One vertex: `[x, y, z, r, g, b, u, v]`.
pub type Lanes8 = [i32; 8];

/// A clip plane over [`Lanes8`] vertices; see the [module](self) definition.
///
/// `repr(C)` and read by the assembly kernel field by field.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LanePlane {
    /// Byte offset of the lane multiplied by `primary`: 0 for x, 4 for y.
    pub primary_offset: u32,
    /// Multiplier of the lane at `primary_offset`.
    pub primary: i32,
    /// Multiplier of lane 2 (z).
    pub depth: i32,
    /// Constant added to the distance.
    pub bias: i32,
    /// Non-zero to overwrite lane 2 of every crossing with `forced_depth`.
    pub force_depth: u32,
    /// The value for lane 2 of a crossing when `force_depth` is set.
    pub forced_depth: i32,
    /// Vertices the destination can hold.
    pub capacity: u32,
}

impl LanePlane {
    /// Signed distance of `vertex` from the plane.
    #[inline(always)]
    pub fn distance(&self, vertex: &Lanes8) -> i32 {
        vertex[(self.primary_offset / 4) as usize]
            .wrapping_mul(self.primary)
            .wrapping_add(vertex[2].wrapping_mul(self.depth))
            .wrapping_add(self.bias)
    }
}

struct Adapter<'a>(&'a LanePlane);

impl AttributedClipPlane<Lanes8> for Adapter<'_> {
    type Distance = i32;

    #[inline(always)]
    fn distance(&self, _: usize, vertex: &Lanes8) -> i32 {
        self.0.distance(vertex)
    }

    #[inline(always)]
    fn inside(&self, distance: i32) -> bool {
        distance >= 0
    }

    #[inline(always)]
    fn intersection(
        &self,
        _: usize,
        first: &Lanes8,
        first_distance: i32,
        _: usize,
        second: &Lanes8,
        second_distance: i32,
    ) -> Lanes8 {
        let (a, b, da, db) = if (second[0], second[1], second[2]) < (first[0], first[1], first[2]) {
            (second, first, second_distance, first_distance)
        } else {
            (first, second, first_distance, second_distance)
        };
        let t = ratio_q12_i32(da, da.wrapping_sub(db));
        let mut out = [0i32; 8];
        for lane in 0..8 {
            out[lane] = lerp_q12_i32_wide(a[lane], b[lane], t);
        }
        if self.0.force_depth != 0 {
            out[2] = self.0.forced_depth;
        }
        out
    }
}

/// Clip `count` vertices at `src` against `plane` into `dst`, returning how many
/// were written. The portable definition.
///
/// # Safety
///
/// `src` must be valid for reads of `count` vertices, `dst` for writes of
/// `plane.capacity` vertices, and the two ranges must not overlap.
pub unsafe fn clip_lanes8_plane_reference(
    src: *const Lanes8,
    count: usize,
    dst: *mut Lanes8,
    plane: &LanePlane,
) -> usize {
    let capacity = plane.capacity as usize;
    // SAFETY: the caller's ranges; `dst` is only written through the slice.
    let (source, destination) = unsafe {
        (
            core::slice::from_raw_parts(src, count),
            core::slice::from_raw_parts_mut(dst, capacity),
        )
    };
    clip_to_plane(
        source,
        destination,
        &Adapter(plane),
        ClipTraversal::PreviousToCurrent,
    )
    .unwrap_or(capacity)
}

/// [`clip_lanes8_plane_reference`], or the hand-scheduled kernel on the
/// console when the `asm-kernels` feature is on. Same safety contract; the
/// two produce identical bytes (`hello-asmprobe` checks it).
///
/// # Safety
///
/// As [`clip_lanes8_plane_reference`].
#[inline(always)]
pub unsafe fn clip_lanes8_plane(
    src: *const Lanes8,
    count: usize,
    dst: *mut Lanes8,
    plane: &LanePlane,
) -> usize {
    #[cfg(all(target_arch = "mips", feature = "asm-kernels"))]
    {
        // SAFETY: forwarded contract.
        unsafe { crate::asm_kernels::psx_math_clip_lanes8_plane(src, count, dst, plane) }
    }
    #[cfg(not(all(target_arch = "mips", feature = "asm-kernels")))]
    {
        // SAFETY: forwarded contract.
        unsafe { clip_lanes8_plane_reference(src, count, dst, plane) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(primary_offset: u32, primary: i32, depth: i32, bias: i32) -> LanePlane {
        LanePlane {
            primary_offset,
            primary,
            depth,
            bias,
            force_depth: 0,
            forced_depth: 0,
            capacity: 8,
        }
    }

    #[test]
    fn an_all_inside_polygon_is_copied() {
        let near = LanePlane {
            force_depth: 1,
            forced_depth: 8,
            ..plane(0, 0, 1, -8)
        };
        let src = [
            [0, 0, 100, 1, 2, 3, 4, 5],
            [10, 0, 120, 6, 7, 8, 9, 10],
            [0, 10, 90, 0, 0, 0, 0, 0],
        ];
        let mut dst = [[0i32; 8]; 8];
        // SAFETY: arrays of the stated sizes, disjoint.
        let n = unsafe { clip_lanes8_plane_reference(src.as_ptr(), 3, dst.as_mut_ptr(), &near) };
        assert_eq!(n, 3);
        assert_eq!(&dst[..3], &src[..]);
    }

    #[test]
    fn a_crossing_lands_on_the_plane_and_interpolates_every_lane() {
        let near = LanePlane {
            force_depth: 1,
            forced_depth: 8,
            ..plane(0, 0, 1, -8)
        };
        // One vertex at z = 4 (outside), two at z = 12 (inside): both crossings
        // sit at t = 0.5 in Q12, and the polygon gains a vertex.
        let a = [0, 0, 4, 0, 0, 0, 0, 0];
        let b = [16, 8, 12, 100, 200, 40, 10, 20];
        let c = [16, -8, 12, 60, 20, 20, 30, 40];
        let src = [a, b, c];
        let mut dst = [[0i32; 8]; 8];
        // SAFETY: as above.
        let n = unsafe { clip_lanes8_plane_reference(src.as_ptr(), 3, dst.as_mut_ptr(), &near) };
        assert_eq!(n, 4);
        assert_eq!(dst[0], [8, -4, 8, 30, 10, 10, 15, 20]);
        assert_eq!(dst[1], [8, 4, 8, 50, 100, 20, 5, 10]);
        assert_eq!(dst[2], b);
        assert_eq!(dst[3], c);
    }

    #[test]
    fn a_full_destination_reports_its_capacity() {
        let always = plane(0, 0, 0, 1);
        let src = [[1; 8]; 3];
        let mut dst = [[0i32; 8]; 8];
        let mut full = always;
        full.capacity = 2;
        // SAFETY: as above.
        let n = unsafe { clip_lanes8_plane_reference(src.as_ptr(), 3, dst.as_mut_ptr(), &full) };
        assert_eq!(n, 2);
    }
}
