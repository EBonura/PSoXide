//! Pure segment/hitbox intersection used by animated studio traces.

/// Segment-vs-AABB in Q0.12. Inputs are in one studio bone's local coordinate
/// system; the returned entry fraction orders overlapping actor boxes.
#[inline]
pub fn ray_aabb_fraction_q12(
    start: [i32; 3],
    end: [i32; 3],
    mins: [i32; 3],
    maxs: [i32; 3],
) -> Option<i32> {
    let mut enter = 0i32;
    let mut leave = 4096i32;
    for axis in 0..3 {
        let delta = end[axis] - start[axis];
        if delta == 0 {
            if start[axis] < mins[axis] || start[axis] > maxs[axis] {
                return None;
            }
            continue;
        }
        let mut a = ((mins[axis] - start[axis]) << 12) / delta;
        let mut b = ((maxs[axis] - start[axis]) << 12) / delta;
        if a > b {
            core::mem::swap(&mut a, &mut b);
        }
        enter = enter.max(a);
        leave = leave.min(b);
        if enter > leave {
            return None;
        }
    }
    (leave >= 0 && enter <= 4096).then_some(enter.clamp(0, 4096))
}

#[cfg(test)]
mod tests {
    use super::ray_aabb_fraction_q12;

    #[test]
    fn centre_and_face_edges_hit() {
        let bounds = ([-16, -36, -16], [16, 36, 16]);
        assert_eq!(
            ray_aabb_fraction_q12([0, 0, -100], [0, 0, 100], bounds.0, bounds.1),
            Some(1720)
        );
        assert!(ray_aabb_fraction_q12([16, 0, -100], [16, 0, 100], bounds.0, bounds.1).is_some());
    }

    #[test]
    fn just_outside_body_is_not_a_false_positive() {
        assert_eq!(
            ray_aabb_fraction_q12([17, 0, -100], [17, 0, 100], [-16, -36, -16], [16, 36, 16]),
            None
        );
    }

    #[test]
    fn segment_stopping_before_box_misses() {
        assert_eq!(
            ray_aabb_fraction_q12([0, 0, -100], [0, 0, -20], [-16, -16, -16], [16, 16, 16]),
            None
        );
    }

    #[test]
    fn reversed_direction_returns_nearest_entry() {
        assert_eq!(
            ray_aabb_fraction_q12([0, 0, 100], [0, 0, -100], [-16, -16, -16], [16, 16, 16]),
            Some(1720)
        );
    }
}
