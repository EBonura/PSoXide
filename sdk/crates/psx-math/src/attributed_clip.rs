// SPDX-License-Identifier: GPL-2.0-or-later
//! Allocation-free convex clipping shared by the PS1 renderers.
//!
//! This module owns Sutherland-Hodgman traversal and buffer order. Each
//! renderer supplies a monomorphized plane adapter for its distance domain,
//! fixed-point interpolation and attribute rounding. The adapters are an A/B
//! bridge: once baseline equivalence is proven, all three games can be moved
//! deliberately to one measured numeric policy.

use crate::int32::div_u64_by_u32;
use core::mem::MaybeUninit;

/// Cyclic traversal order for one clip plane.
///
/// The two variants retain the same convex region but choose a different
/// first output vertex. That affects triangle-fan diagonals and is therefore
/// preserved explicitly during equivalence work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClipTraversal {
    /// Visit previous-to-current, writing a crossing before the current point.
    PreviousToCurrent,
    /// Visit current-to-next, writing the current point before a crossing.
    CurrentToNext,
}

/// Q12 fraction for a crossing whose signed endpoint distances straddle zero.
///
/// Callers canonicalise the geometric endpoint order before calling this
/// helper. That makes two faces which visit a shared edge in opposite
/// directions produce the same fixed-point fraction and therefore the same
/// clipped vertex. The division deliberately truncates: it is the cheapest
/// R3000 policy and matches HL-PSX's crack-free reference implementation.
#[inline(always)]
pub fn crossing_fraction_q12_i32(first_distance: i32, second_distance: i32) -> i32 {
    ratio_q12_i32(first_distance, first_distance.wrapping_sub(second_distance))
}

/// Exact Q16 crossing fraction of an edge from its two signed plane distances.
///
/// `(first << 16) / (first - second)`, clamped to `0..=65536`, computed with
/// 32-bit operations only: the R3000A has no 64-bit divide, and the earlier
/// `i64` form of this function was the last 64-bit division helper linked
/// into the PXBSP world path. The magnitude runs through the hardware divide
/// when `first << 16` fits, and through [`div_u64_by_u32`] otherwise; both
/// are exact, so the result equals the wide form bit for bit.
#[inline(always)]
pub fn crossing_fraction_q16_i32(first_distance: i32, second_distance: i32) -> u32 {
    // Sign of the ratio: positive only when the numerator and the difference
    // agree, which is every straddling edge; equal-sign inputs clamp to an end.
    let difference_positive = first_distance > second_distance;
    if first_distance == second_distance {
        return 1 << 16;
    }
    if (first_distance >= 0) != difference_positive {
        return 0;
    }
    let numerator = first_distance.unsigned_abs();
    let denominator = first_distance.abs_diff(second_distance);
    let magnitude = if numerator < (1 << 16) {
        (numerator << 16) / denominator
    } else {
        // `numerator >> 16 < denominator` because `numerator <= denominator`
        // fails only when the ratio exceeds one, and `div_u64_by_u32` still
        // needs `hi < divisor`; guard that case by the clamp below.
        if (numerator >> 16) >= denominator {
            return 1 << 16;
        }
        div_u64_by_u32(numerator >> 16, numerator << 16, denominator)
    };
    magnitude.min(1 << 16)
}

/// Exact `first + (second - first) * fraction / 65536`, truncating toward
/// negative infinity like a shift, in 32-bit operations.
///
/// The product is split so nothing exceeds `i32` for `|second - first|` below
/// `2^23`: enough for `i16` positions and screen coordinates, byte attributes
/// and Q12 depths, which is everything a clipped world vertex carries.
#[inline(always)]
pub fn lerp_q16_i32_exact(first: i32, second: i32, fraction_q16: u32) -> i32 {
    let delta = second.wrapping_sub(first);
    debug_assert!(delta.unsigned_abs() < (1 << 23));
    let high = (fraction_q16 >> 8) as i32;
    let low = (fraction_q16 & 0xff) as i32;
    // delta * f / 2^16 == delta * (f >> 8) / 2^8 + delta * (f & 255) / 2^16,
    // and floor distributes here because the first term is exact after one
    // more shift: (delta * high + ((delta * low) >> 8)) >> 8.
    first.wrapping_add(
        (delta
            .wrapping_mul(high)
            .wrapping_add((delta.wrapping_mul(low)) >> 8))
            >> 8,
    )
}

/// Bounded Q12 interpolation with truncation toward the lower fixed point.
///
/// This is the cheap path for Quake/PXBSP `i16` positions and byte attributes:
/// their maximum delta times 4096 fits in `i32`. Renderers with wider source
/// coordinates must use [`lerp_q12_i32_wide`].
#[inline(always)]
pub fn lerp_q12_i32(first: i32, second: i32, fraction: i32) -> i32 {
    let delta = second.wrapping_sub(first);
    first.wrapping_add(delta.wrapping_mul(fraction) >> 12)
}

/// Bounded Q12 interpolation rounded to nearest, matching Quake-PSX's
/// cached-depth near clip.
#[inline(always)]
pub fn lerp_q12_i32_rounded(first: i32, second: i32, fraction: i32) -> i32 {
    first.wrapping_add(
        (second
            .wrapping_sub(first)
            .wrapping_mul(fraction)
            .wrapping_add(1 << 11))
            >> 12,
    )
}

/// Overflow-safe form of [`lerp_q12_i32`] for large screen-space coordinates.
#[inline(always)]
pub fn lerp_q12_i32_wide(first: i32, second: i32, fraction: i32) -> i32 {
    let delta = second.wrapping_sub(first);
    first
        .wrapping_add((delta / 4096).wrapping_mul(fraction))
        .wrapping_add(((delta % 4096).wrapping_mul(fraction)) >> 12)
}

/// Overflow-safe non-negative Q12 ratio, clamped to one.
///
/// HL-PSX also uses this for projection and guard-band interpolation outside
/// polygon traversal, preventing a second overflow fix under a game-specific
/// name.
#[inline(always)]
pub fn ratio_q12_i32(numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 || (numerator < 0) != (denominator < 0) {
        return 0;
    }
    let mut numerator = numerator.unsigned_abs();
    let mut denominator = denominator.unsigned_abs();
    while numerator > (i32::MAX as u32 >> 12) {
        numerator = (numerator + 1) >> 1;
        denominator = (denominator + 1) >> 1;
    }
    if denominator == 0 {
        return 0;
    }
    ((numerator << 12) / denominator).min(4096) as i32
}

/// Renderer-specific policy for one attributed clip plane.
///
/// Implement this on a small adapter type. Generic dispatch is resolved at
/// compile time; no trait object or indirect call enters the console hot path.
/// `source_index` allows callers to reuse cached transformed distances.
pub trait AttributedClipPlane<Vertex> {
    /// Signed-distance representation used by the renderer.
    type Distance: Copy;

    /// Signed distance of `vertex` from the retained half-space.
    fn distance(&self, source_index: usize, vertex: &Vertex) -> Self::Distance;

    /// Whether `distance` lies in the retained half-space.
    fn inside(&self, distance: Self::Distance) -> bool;

    /// Interpolate every authored attribute at an edge/plane crossing.
    fn intersection(
        &self,
        first_index: usize,
        first: &Vertex,
        first_distance: Self::Distance,
        second_index: usize,
        second: &Vertex,
        second_distance: Self::Distance,
    ) -> Vertex;
}

/// `destination` filled before the clip finished. The vertices written so far
/// are not a usable polygon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipOverflow;

impl core::fmt::Display for ClipOverflow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("clip output does not fit the destination")
    }
}

/// Clip one attributed polygon against one half-space into `destination`,
/// returning how many vertices it wrote.
///
/// Every write is bounds-checked. A convex `source` clipped by a plane whose
/// `inside` answer is the same each time a vertex is evaluated gains at most
/// one vertex, so `source.len() + 1` slots always suffice for it; any input
/// fits `2 * source.len()`, the most one vertex and one crossing per source
/// vertex can emit.
///
/// # Errors
///
/// [`ClipOverflow`] when `destination` is too small.
#[inline(always)]
pub fn clip_to_plane<Vertex: Copy, Plane: AttributedClipPlane<Vertex>>(
    source: &[Vertex],
    destination: &mut [Vertex],
    plane: &Plane,
    traversal: ClipTraversal,
) -> Result<usize, ClipOverflow> {
    // SAFETY: `destination` is a live exclusive slice, so its pointer is
    // valid for writes of `destination.len()` elements and cannot alias
    // `source`; with `CHECK_CAPACITY` the kernel bounds every write by that
    // length.
    unsafe {
        clip_convex_plane_raw::<_, _, true>(
            source,
            destination.as_mut_ptr(),
            destination.len(),
            plane,
            traversal,
        )
    }
    .map_err(|_| ClipOverflow)
}

/// [`clip_to_plane`] without the bounds check on each write, for fixed
/// scratch whose size is proven.
///
/// # Safety
///
/// `destination.len()` must be at least `2 * source.len()`; or at least
/// `source.len() + 1` when `source` is convex and `plane.inside` gives the
/// same answer every time a vertex's distance is evaluated (the
/// `CurrentToNext` traversal evaluates each distance twice).
#[inline(always)]
pub unsafe fn clip_to_plane_unchecked<Vertex: Copy, Plane: AttributedClipPlane<Vertex>>(
    source: &[Vertex],
    destination: &mut [Vertex],
    plane: &Plane,
    traversal: ClipTraversal,
) -> usize {
    // SAFETY: `destination` is a live exclusive slice, valid for writes of
    // its length and not aliasing `source`; this fn's `# Safety` bounds the
    // emitted count by that length. Without `CHECK_CAPACITY` the kernel never
    // returns `Err`.
    match unsafe {
        clip_convex_plane_raw::<_, _, false>(
            source,
            destination.as_mut_ptr(),
            destination.len(),
            plane,
            traversal,
        )
    } {
        Ok(written) | Err(written) => written,
    }
}

/// Clip one convex attributed polygon against one half-space.
///
/// With `CHECK_CAPACITY` true this is [`clip_to_plane`] returning the count
/// written before an overflow; with it false, [`clip_to_plane_unchecked`].
///
/// # Safety
///
/// When `CHECK_CAPACITY` is false, [`clip_to_plane_unchecked`]'s contract.
/// `source.len() + 1` slots are enough only for a convex source and a plane
/// whose `inside` answer is stable per vertex; `2 * source.len()` always is.
#[deprecated(
    note = "use the safe `clip_to_plane` (checked) or `clip_to_plane_unchecked` (proven capacity)"
)]
#[inline(always)]
pub unsafe fn clip_convex_plane<
    Vertex: Copy,
    Plane: AttributedClipPlane<Vertex>,
    const CHECK_CAPACITY: bool,
>(
    source: &[Vertex],
    destination: &mut [Vertex],
    plane: &Plane,
    traversal: ClipTraversal,
) -> usize {
    // SAFETY: `destination` is a live exclusive slice, so its pointer is
    // valid for writes of `destination.len()` elements and cannot alias
    // `source`. The capacity requirement when `CHECK_CAPACITY` is false is
    // forwarded unchanged from this fn's `# Safety` section.
    match unsafe {
        clip_convex_plane_raw::<_, _, CHECK_CAPACITY>(
            source,
            destination.as_mut_ptr(),
            destination.len(),
            plane,
            traversal,
        )
    } {
        Ok(written) | Err(written) => written,
    }
}

/// [`clip_to_plane`] into uninitialised scratch, so console renderers can
/// keep stack or scratchpad scratch as `MaybeUninit` and pay only for the
/// vertices clipping emits. On `Ok(n)` the first `n` entries are initialised.
///
/// # Errors
///
/// [`ClipOverflow`] when `destination` is too small.
#[inline(always)]
pub fn clip_to_plane_uninit<Vertex: Copy, Plane: AttributedClipPlane<Vertex>>(
    source: &[Vertex],
    destination: &mut [MaybeUninit<Vertex>],
    plane: &Plane,
    traversal: ClipTraversal,
) -> Result<usize, ClipOverflow> {
    // SAFETY: `MaybeUninit<Vertex>` has `Vertex`'s layout, so the cast
    // pointer is aligned and valid for writes of `destination.len()`
    // vertices; the kernel only writes destination slots, bounds-checked
    // with `CHECK_CAPACITY`, and the exclusive borrow rules out aliasing.
    unsafe {
        clip_convex_plane_raw::<_, _, true>(
            source,
            destination.as_mut_ptr().cast::<Vertex>(),
            destination.len(),
            plane,
            traversal,
        )
    }
    .map_err(|_| ClipOverflow)
}

/// [`clip_to_plane_uninit`] without the bounds check on each write. The
/// first returned-count entries of `destination` are initialised.
///
/// # Safety
///
/// [`clip_to_plane_unchecked`]'s capacity contract.
#[inline(always)]
pub unsafe fn clip_to_plane_uninit_unchecked<Vertex: Copy, Plane: AttributedClipPlane<Vertex>>(
    source: &[Vertex],
    destination: &mut [MaybeUninit<Vertex>],
    plane: &Plane,
    traversal: ClipTraversal,
) -> usize {
    // SAFETY: as in `clip_to_plane_uninit`, with the capacity bound from this
    // fn's `# Safety`. Without `CHECK_CAPACITY` the kernel never returns `Err`.
    match unsafe {
        clip_convex_plane_raw::<_, _, false>(
            source,
            destination.as_mut_ptr().cast::<Vertex>(),
            destination.len(),
            plane,
            traversal,
        )
    } {
        Ok(written) | Err(written) => written,
    }
}

/// [`clip_convex_plane`] into uninitialised scratch. The first
/// returned-count entries of `destination` are initialised on return.
///
/// # Safety
///
/// [`clip_convex_plane`]'s contract.
#[deprecated(
    note = "use the safe `clip_to_plane_uninit` (checked) or `clip_to_plane_uninit_unchecked` (proven capacity)"
)]
#[inline(always)]
pub unsafe fn clip_convex_plane_uninit<
    Vertex: Copy,
    Plane: AttributedClipPlane<Vertex>,
    const CHECK_CAPACITY: bool,
>(
    source: &[Vertex],
    destination: &mut [MaybeUninit<Vertex>],
    plane: &Plane,
    traversal: ClipTraversal,
) -> usize {
    // SAFETY: `MaybeUninit<Vertex>` has the same layout as `Vertex`, so the
    // cast pointer is aligned and valid for writes of `destination.len()`
    // vertices; the raw kernel only writes (never reads) destination slots.
    // The exclusive borrow rules out aliasing `source`, and the capacity
    // requirement is forwarded from this fn's `# Safety` section.
    match unsafe {
        clip_convex_plane_raw::<_, _, CHECK_CAPACITY>(
            source,
            destination.as_mut_ptr().cast::<Vertex>(),
            destination.len(),
            plane,
            traversal,
        )
    } {
        Ok(written) | Err(written) => written,
    }
}

/// The Sutherland-Hodgman kernel. `Err(written)` when `CHECK_CAPACITY` found
/// `destination` full; without it every write is the caller's to bound and
/// the result is always `Ok`.
#[inline(always)]
unsafe fn clip_convex_plane_raw<
    Vertex: Copy,
    Plane: AttributedClipPlane<Vertex>,
    const CHECK_CAPACITY: bool,
>(
    source: &[Vertex],
    destination: *mut Vertex,
    destination_len: usize,
    plane: &Plane,
    traversal: ClipTraversal,
) -> Result<usize, usize> {
    if source.is_empty() {
        return Ok(0);
    }
    if !CHECK_CAPACITY {
        debug_assert!(destination_len >= source.len().saturating_add(1));
    }

    #[inline(always)]
    unsafe fn emit<Vertex: Copy, const CHECK_CAPACITY: bool>(
        destination: *mut Vertex,
        destination_len: usize,
        written: &mut usize,
        vertex: Vertex,
    ) -> bool {
        if CHECK_CAPACITY && *written >= destination_len {
            return false;
        }
        debug_assert!(*written < destination_len);
        // SAFETY: `*written < destination_len`, checked above when
        // `CHECK_CAPACITY` is set and otherwise guaranteed by the caller of
        // the public entry points; `destination` is valid for writes of
        // `destination_len` elements, so slot `*written` is in bounds.
        unsafe { destination.add(*written).write(vertex) };
        *written += 1;
        true
    }

    let mut written = 0usize;
    match traversal {
        ClipTraversal::PreviousToCurrent => {
            let mut previous_index = source.len() - 1;
            // SAFETY: `source` is non-empty (early return above), so
            // `len - 1` is a valid index.
            let mut previous = unsafe { *source.get_unchecked(previous_index) };
            let mut previous_distance = plane.distance(previous_index, &previous);
            let mut current_index = 0usize;
            while current_index < source.len() {
                // SAFETY: the loop condition keeps `current_index < len`.
                let current = unsafe { *source.get_unchecked(current_index) };
                let current_distance = plane.distance(current_index, &current);
                let current_inside = plane.inside(current_distance);
                if current_inside != plane.inside(previous_distance) {
                    let crossing = plane.intersection(
                        previous_index,
                        &previous,
                        previous_distance,
                        current_index,
                        &current,
                        current_distance,
                    );
                    // SAFETY: `destination`/`destination_len` come straight from the
                    // caller, who guarantees room for every emitted vertex when
                    // `CHECK_CAPACITY` is false; with it set, `emit` bounds-checks.
                    if unsafe {
                        !emit::<Vertex, CHECK_CAPACITY>(
                            destination,
                            destination_len,
                            &mut written,
                            crossing,
                        )
                    } {
                        return Err(written);
                    }
                }
                if current_inside
                    // SAFETY: same capacity contract as the crossing emit above:
                    // the caller sized `destination`, or `emit` bounds-checks.
                    && unsafe {
                        !emit::<Vertex, CHECK_CAPACITY>(
                            destination,
                            destination_len,
                            &mut written,
                            current,
                        )
                    }
                {
                    return Err(written);
                }
                previous_index = current_index;
                previous = current;
                previous_distance = current_distance;
                current_index += 1;
            }
        }
        ClipTraversal::CurrentToNext => {
            let mut current_index = 0usize;
            while current_index < source.len() {
                let next_index = if current_index + 1 == source.len() {
                    0
                } else {
                    current_index + 1
                };
                // SAFETY: the loop condition keeps `current_index < len`.
                let current = unsafe { *source.get_unchecked(current_index) };
                // SAFETY: `next_index` is `current_index + 1` only when that is
                // still `< len`, otherwise 0, which is valid for a non-empty slice.
                let next = unsafe { *source.get_unchecked(next_index) };
                let current_distance = plane.distance(current_index, &current);
                let next_distance = plane.distance(next_index, &next);
                let current_inside = plane.inside(current_distance);
                if current_inside
                    // SAFETY: same capacity contract as the crossing emit above:
                    // the caller sized `destination`, or `emit` bounds-checks.
                    && unsafe {
                        !emit::<Vertex, CHECK_CAPACITY>(
                            destination,
                            destination_len,
                            &mut written,
                            current,
                        )
                    }
                {
                    return Err(written);
                }
                if current_inside != plane.inside(next_distance) {
                    let crossing = plane.intersection(
                        current_index,
                        &current,
                        current_distance,
                        next_index,
                        &next,
                        next_distance,
                    );
                    // SAFETY: `destination`/`destination_len` come straight from the
                    // caller, who guarantees room for every emitted vertex when
                    // `CHECK_CAPACITY` is false; with it set, `emit` bounds-checks.
                    if unsafe {
                        !emit::<Vertex, CHECK_CAPACITY>(
                            destination,
                            destination_len,
                            &mut written,
                            crossing,
                        )
                    } {
                        return Err(written);
                    }
                }
                current_index += 1;
            }
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
    struct Vertex(i32, i32);

    struct Plane<'a>(Option<&'a [i32]>);

    impl AttributedClipPlane<Vertex> for Plane<'_> {
        type Distance = i32;

        fn distance(&self, index: usize, vertex: &Vertex) -> i32 {
            self.0.map_or(vertex.0, |distances| distances[index])
        }

        fn inside(&self, distance: i32) -> bool {
            distance >= 0
        }

        fn intersection(
            &self,
            _: usize,
            first: &Vertex,
            first_distance: i32,
            _: usize,
            second: &Vertex,
            second_distance: i32,
        ) -> Vertex {
            Vertex(
                0,
                first.1
                    + (second.1 - first.1) * first_distance / (first_distance - second_distance),
            )
        }
    }

    fn clip<const CHECK: bool>(
        source: &[Vertex],
        traversal: ClipTraversal,
    ) -> ([Vertex; 8], usize) {
        let mut output = [Vertex::default(); 8];
        assert!(2 * source.len() <= output.len());
        let count = if CHECK {
            clip_to_plane(source, &mut output, &Plane(None), traversal).expect("fits")
        } else {
            // SAFETY: one plane emits at most two vertices per source vertex,
            // and the assert above leaves room for that.
            unsafe { clip_to_plane_unchecked(source, &mut output, &Plane(None), traversal) }
        };
        (output, count)
    }

    #[test]
    fn traversal_preserves_each_historical_fan_start() {
        let source = [Vertex(-2, 0), Vertex(2, 40), Vertex(2, 80)];
        let (previous, previous_count) = clip::<true>(&source, ClipTraversal::PreviousToCurrent);
        assert_eq!(previous_count, 4);
        assert_eq!(
            &previous[..previous_count],
            &[Vertex(0, 40), Vertex(0, 20), source[1], source[2]]
        );
        let (current, current_count) = clip::<false>(&source, ClipTraversal::CurrentToNext);
        assert_eq!(current_count, 4);
        assert_eq!(
            &current[..current_count],
            &[Vertex(0, 20), source[1], source[2], Vertex(0, 40)]
        );
    }

    #[test]
    fn cached_distance_domain_is_authoritative() {
        let source = [Vertex(100, 0), Vertex(100, 40), Vertex(100, 80)];
        let mut output = [Vertex::default(); 4];
        let count = clip_to_plane(
            &source,
            &mut output,
            &Plane(Some(&[-2, 2, 2])),
            ClipTraversal::CurrentToNext,
        );
        assert_eq!(count, Ok(4));
        assert_eq!(output, [Vertex(0, 20), source[1], source[2], Vertex(0, 40)]);
    }

    /// math-01: a non-convex source crossing the plane at every edge emits
    /// two vertices per source vertex, more than the `len + 1` the old docs
    /// called sufficient. The safe form reports it; `2 * len` always fits.
    #[test]
    fn non_convex_input_overflows_len_plus_one_but_fits_twice_len() {
        let zigzag = [Vertex(1, 0), Vertex(-1, 10), Vertex(1, 20), Vertex(-1, 30)];
        for traversal in [
            ClipTraversal::PreviousToCurrent,
            ClipTraversal::CurrentToNext,
        ] {
            let mut tight = [Vertex::default(); 5];
            assert_eq!(
                clip_to_plane(&zigzag, &mut tight, &Plane(None), traversal),
                Err(ClipOverflow)
            );
            let mut roomy = [Vertex::default(); 8];
            assert_eq!(
                clip_to_plane(&zigzag, &mut roomy, &Plane(None), traversal),
                Ok(6)
            );
            let mut uninit = [MaybeUninit::<Vertex>::uninit(); 5];
            assert_eq!(
                clip_to_plane_uninit(&zigzag, &mut uninit, &Plane(None), traversal),
                Err(ClipOverflow)
            );
        }
    }

    /// The deprecated forwarders keep their old results: the checked form
    /// returns the count written before the overflow.
    #[allow(deprecated)]
    #[test]
    fn deprecated_forwarders_keep_partial_counts() {
        let zigzag = [Vertex(1, 0), Vertex(-1, 10), Vertex(1, 20), Vertex(-1, 30)];
        let mut tight = [Vertex::default(); 5];
        // SAFETY: `CHECK_CAPACITY` is true, so every write is bounds-checked.
        let count = unsafe {
            clip_convex_plane::<_, _, true>(
                &zigzag,
                &mut tight,
                &Plane(None),
                ClipTraversal::PreviousToCurrent,
            )
        };
        assert_eq!(count, 5);
    }

    #[test]
    fn canonical_q12_crossing_is_direction_independent() {
        let interpolate =
            |mut first: i32, mut second: i32, mut first_distance: i32, mut second_distance: i32| {
                if second < first {
                    core::mem::swap(&mut first, &mut second);
                    core::mem::swap(&mut first_distance, &mut second_distance);
                }
                lerp_q12_i32(
                    first,
                    second,
                    crossing_fraction_q12_i32(first_distance, second_distance),
                )
            };
        assert_eq!(interpolate(10, 110, 7, -3), 79);
        assert_eq!(interpolate(110, 10, -3, 7), 79);
    }

    #[test]
    fn measured_numeric_policies_keep_their_rounding_contracts() {
        let q12 = crossing_fraction_q12_i32(7, -3);
        assert_eq!(lerp_q12_i32(10, 110, q12), 79);
        assert_eq!(lerp_q12_i32_rounded(10, 110, q12), 80);

        let q16 = crossing_fraction_q16_i32(7, -3);
        assert_eq!(lerp_q16_i32_exact(10, 110, q16), 79);
    }

    #[test]
    fn all_inside_and_all_outside_are_stable() {
        let inside = [Vertex(1, 1), Vertex(2, 2), Vertex(3, 3)];
        let outside = [Vertex(-1, 1), Vertex(-2, 2), Vertex(-3, 3)];
        let (inside_output, inside_count) = clip::<true>(&inside, ClipTraversal::PreviousToCurrent);
        assert_eq!(inside_count, inside.len());
        assert_eq!(&inside_output[..inside_count], &inside);
        let (_, outside_count) = clip::<true>(&outside, ClipTraversal::PreviousToCurrent);
        assert_eq!(outside_count, 0);
    }

    /// The `i64` forms these replaced, kept as the host oracles.
    fn crossing_fraction_q16_i64(first: i64, second: i64) -> i64 {
        ((first << 16) / first.wrapping_sub(second)).clamp(0, 1 << 16)
    }

    fn lerp_q16_i64(first: i32, second: i32, fraction: i64) -> i32 {
        (first as i64 + ((second.wrapping_sub(first) as i64).wrapping_mul(fraction) >> 16)) as i32
    }

    #[test]
    #[cfg_attr(
        miri,
        ignore = "exhaustive sweep, minutes under Miri; make test runs it natively"
    )]
    fn q16_crossing_and_lerp_match_the_wide_oracles() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for iteration in 0..100_000u32 {
            let span: i32 = if iteration % 3 == 0 {
                i32::MAX
            } else {
                1 << 24
            };
            let first = (next() as i32) % span;
            let second = (next() as i32) % span;
            if first == second {
                continue;
            }
            let expected = crossing_fraction_q16_i64(i64::from(first), i64::from(second));
            let actual = crossing_fraction_q16_i32(first, second);
            assert_eq!(i64::from(actual), expected, "first {first} second {second}");
            let a = (next() as i32) % (1 << 22);
            let b = (next() as i32) % (1 << 22);
            assert_eq!(
                lerp_q16_i32_exact(a, b, actual),
                lerp_q16_i64(a, b, expected),
                "lerp {a} {b} fraction {actual}"
            );
        }
    }
}
