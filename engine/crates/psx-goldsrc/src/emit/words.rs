//! Pure helpers both GoldSrc ports' emitters use: packed GPU words, screen-space tests and the
//! small fixed-point transforms of brush entities. Nothing here reads a port's state.

use psx_gte::math::Mat3I16;
use psx_gte::scene::Projected;
use psx_math::Wide;

/// `(r, g, b)` as a GPU colour word (`0x00bbggrr`).
#[inline(always)]
#[must_use]
pub const fn pack_rgb_word(rgb: (u8, u8, u8)) -> u32 {
    rgb.0 as u32 | ((rgb.1 as u32) << 8) | ((rgb.2 as u32) << 16)
}

/// The `(r, g, b)` of a GPU colour word.
#[inline(always)]
#[must_use]
pub const fn unpack_rgb_word(rgb: u32) -> (u8, u8, u8) {
    (rgb as u8, (rgb >> 8) as u8, (rgb >> 16) as u8)
}

/// Screen `(x, y)` as a GPU vertex word.
#[inline(always)]
#[must_use]
pub const fn pack_vertex_word(x: i16, y: i16) -> u32 {
    x as u16 as u32 | ((y as u16 as u32) << 16)
}

/// Texture `(u, v)` as a 16-bit UV word.
#[inline]
#[must_use]
pub const fn uv_word(uv: (u8, u8)) -> u16 {
    (uv.0 as u16) | ((uv.1 as u16) << 8)
}

/// The `(u, v)` of a 16-bit UV word, widened.
#[inline]
#[must_use]
pub const fn uv_word_pair_i32(word: u16) -> (i32, i32) {
    ((word & 0xff) as i32, ((word >> 8) & 0xff) as i32)
}

/// Per-lane average of two packed words without carries between lanes; `lane_mask` clears each
/// lane's top bit after the shift (`0x7f7f_7f7f` for bytes).
#[inline(always)]
#[must_use]
pub const fn average_packed_bytes(a: u32, b: u32, lane_mask: u32) -> u32 {
    (a & b) + (((a ^ b) & lane_mask) >> 1)
}

/// A Q12 matrix row times a vector, in the vector's units.
#[inline(always)]
#[must_use]
pub fn dot12(row: [i16; 3], e: [i32; 3]) -> i32 {
    ((row[0] as i32 * e[0]) + (row[1] as i32 * e[1]) + (row[2] as i32 * e[2])) >> 12
}

/// Cull when the screen triangle is not front-facing. The view rotation has determinant -1, so a
/// front face has a cross product below zero. The 32-bit cross is exact only for GTE-range
/// coordinates (|x|, |y| <= 1023); soft-projected coordinates use [`culled_soft`].
#[inline]
#[must_use]
pub fn culled(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> bool {
    // >= 0 (was <= 0): the det -1 camera (view_rotation) flips the projected
    // winding, so a front face now has cross >= 0.
    (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1) >= 0
}

/// [`culled`] for soft-projected coordinates, which reach tens of thousands of pixels near the
/// plane: a 64-bit cross keeps the sign exact at any magnitude (one MIPS `mult`). Neither an i32
/// cross (overflow) nor pre-scaled inputs (thin slivers collapse to a zero cross) are safe here.
#[inline]
#[must_use]
pub fn culled_soft(a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> bool {
    let cr = Wide::mul(b.0 - a.0, c.1 - a.1).mul_sub(c.0 - a.0, b.1 - a.1);
    !cr.is_negative() // >= 0 (was <= 0): det -1 camera flips the projected winding
}

/// Whether screen point `p` lies inside triangle `(a, b, c)`, either winding.
#[must_use]
pub fn point_in_tri(p: (i32, i32), a: (i32, i32), b: (i32, i32), c: (i32, i32)) -> bool {
    let s = |u: (i32, i32), v: (i32, i32)| (v.0 - u.0) * (p.1 - u.1) - (v.1 - u.1) * (p.0 - u.0);
    let d1 = s(a, b);
    let d2 = s(b, c);
    let d3 = s(c, a);
    let neg = d1 < 0 || d2 < 0 || d3 < 0;
    let pos = d1 > 0 || d2 > 0 || d3 > 0;
    !(neg && pos)
}

/// Whether the GPU draws the triangle: it rejects primitives wider than 1023 or taller than
/// 511 pixels outright, on silicon and in PSoXide.
#[inline]
#[must_use]
pub fn tri_fits_gpu(pa: &Projected, pb: &Projected, pc: &Projected) -> bool {
    let (min_x, max_x) = (
        (pa.sx).min(pb.sx).min(pc.sx) as i32,
        (pa.sx).max(pb.sx).max(pc.sx) as i32,
    );
    let (min_y, max_y) = (
        (pa.sy).min(pb.sy).min(pc.sy) as i32,
        (pa.sy).max(pb.sy).max(pc.sy) as i32,
    );
    max_x - min_x <= 1023 && max_y - min_y <= 511
}

/// Projected vertex `i` of a structure-of-arrays cache: packed screen XY words and depths.
///
/// # Safety
/// `xy` and `z` must be valid for reads at index `i`.
// The body keeps the ports' line layout: their sample profiles address inlined code by line
// offset, and unsafe blocks would move it. Every operation is one the caller vouches for.
#[allow(unsafe_op_in_unsafe_fn)]
#[inline(always)]
#[must_use]
pub unsafe fn projected_soa_get(xy: *const u32, z: *const u16, i: usize) -> Projected {
    let packed = xy.add(i).read();
    Projected {
        sx: packed as u16 as i16,
        sy: (packed >> 16) as u16 as i16,
        sz: z.add(i).read(),
    }
}

/// Store `p` as vertex `i` of a structure-of-arrays cache.
///
/// # Safety
/// `xy` and `z` must be valid for writes at index `i`.
// The body keeps the ports' line layout: their sample profiles address inlined code by line
// offset, and unsafe blocks would move it. Every operation is one the caller vouches for.
#[allow(unsafe_op_in_unsafe_fn)]
#[inline(always)]
pub unsafe fn projected_soa_set(xy: *mut u32, z: *mut u16, i: usize, p: Projected) {
    xy.add(i)
        .write((p.sx as u16 as u32) | ((p.sy as u16 as u32) << 16));
    z.add(i).write(p.sz);
}

/// Colour of the affine-error heatmap for an error in Q8 pixels: blue and green below the leave
/// threshold, yellow towards the split threshold, red to magenta beyond it.
#[inline(always)]
#[must_use]
pub fn affine_heatmap_color(error_q8: u32) -> (u8, u8, u8) {
    // Blue/green is below the leave threshold, yellow approaches the split
    // threshold, and red/magenta exposes severe residual error.
    if error_q8 <= 2 * 256 {
        (24, 96u8.saturating_add((error_q8 / 8) as u8), 255)
    } else if error_q8 <= 4 * 256 {
        let t = ((error_q8 - 2 * 256) >> 2).min(127) as u8;
        (t.saturating_mul(2), 255, 32)
    } else if error_q8 <= 8 * 256 {
        let t = ((error_q8 - 4 * 256) >> 4).min(255) as u8;
        (255, 255u8.saturating_sub(t), 16)
    } else {
        (255, 0, 160)
    }
}

/// Packed Q8 `pitch | yaw << 8 | roll << 16` angles of a rotating brush, with `angle_q12` added
/// on its physical axis (1 = roll, 2 = pitch, otherwise yaw). The cooker has already carried the
/// axis through the Z-up to Y-up reflection.
#[inline(never)]
#[must_use]
pub fn brush_angles_q8(packed: u32, axis: i32, angle_q12: u16) -> u32 {
    let mut pitch = (packed & 0xff) as u16;
    let mut yaw = ((packed >> 8) & 0xff) as u16;
    let mut roll = ((packed >> 16) & 0xff) as u16;
    let movement = angle_q12 >> 4;
    match axis {
        1 => roll = roll.wrapping_add(movement),
        2 => pitch = pitch.wrapping_add(movement),
        _ => yaw = yaw.wrapping_add(movement),
    }
    pitch as u32 | ((yaw as u32) << 8) | ((roll as u32) << 16)
}

/// World point `p` in the frame of a rotating platform at `off` with Q12 rotation `r` (the
/// transpose applied to `p - off`).
#[inline(never)]
#[must_use]
pub fn platrot_world_to_local(p: [i32; 3], off: [i32; 3], r: &Mat3I16) -> [i32; 3] {
    let d = [p[0] - off[0], p[1] - off[1], p[2] - off[2]];
    [
        ((r.m[0][0] as i32 * d[0]) + (r.m[1][0] as i32 * d[1]) + (r.m[2][0] as i32 * d[2])) >> 12,
        ((r.m[0][1] as i32 * d[0]) + (r.m[1][1] as i32 * d[1]) + (r.m[2][1] as i32 * d[2])) >> 12,
        ((r.m[0][2] as i32 * d[0]) + (r.m[1][2] as i32 * d[1]) + (r.m[2][2] as i32 * d[2])) >> 12,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_round_trip() {
        assert_eq!(pack_rgb_word((1, 2, 3)), 0x0003_0201);
        assert_eq!(unpack_rgb_word(0x0003_0201), (1, 2, 3));
        assert_eq!(pack_vertex_word(-1, 2), 0x0002_ffff);
        assert_eq!(uv_word_pair_i32(uv_word((7, 250))), (7, 250));
        assert_eq!(
            average_packed_bytes(0x0204_0608, 0x0406_080a, 0x7f7f_7f7f),
            0x0305_0709
        );
    }

    #[test]
    fn culling_and_containment() {
        // The view rotation's determinant is -1: a non-negative cross is a back face.
        assert!(culled((0, 0), (10, 0), (0, 10)));
        assert!(!culled((0, 0), (0, 10), (10, 0)));
        assert!(culled_soft((0, 0), (40_000, 0), (0, 40_000)));
        assert!(!culled_soft((0, 0), (0, 40_000), (40_000, 0)));
        assert!(point_in_tri((2, 2), (0, 0), (10, 0), (0, 10)));
        assert!(point_in_tri((2, 2), (0, 0), (0, 10), (10, 0)));
        assert!(!point_in_tri((20, 20), (0, 0), (10, 0), (0, 10)));
    }

    #[test]
    fn gpu_size_limits() {
        let p = |sx, sy| Projected { sx, sy, sz: 0 };
        assert!(tri_fits_gpu(&p(0, 0), &p(1023, 0), &p(0, 511)));
        assert!(!tri_fits_gpu(&p(0, 0), &p(1024, 0), &p(0, 10)));
        assert!(!tri_fits_gpu(&p(0, 0), &p(10, 0), &p(0, 512)));
    }

    #[test]
    fn brush_angles_add_on_their_axis() {
        assert_eq!(brush_angles_q8(0x0003_0201, 1, 16), 0x0004_0201);
        assert_eq!(brush_angles_q8(0x0003_0201, 2, 16), 0x0003_0202);
        assert_eq!(brush_angles_q8(0x0003_0201, 0, 16), 0x0003_0301);
    }
}
