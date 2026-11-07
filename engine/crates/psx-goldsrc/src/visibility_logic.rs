//! Allocation-free visibility rules shared by the runtime and host tests.

pub const BRUSH_RENDER_OPAQUE: u8 = 0;
pub const BRUSH_RENDER_AVERAGE: u8 = 1;
pub const BRUSH_RENDER_ADDITIVE: u8 = 2;
pub const BRUSH_RENDER_CUTOUT: u8 = 3;

/// Convert the cooked brush render class into the PS1 packet blend class.
///
/// GoldSrc's alpha-tested mode is still an ordinary opaque PS1 packet: index
/// zero in its 4-bit texture provides the cutout. Its distinct cooked value is
/// retained solely so visibility code does not treat the transparent texels as
/// a solid visual occluder.
#[inline]
pub const fn brush_packet_blend(render_class: u8) -> u8 {
    match render_class {
        BRUSH_RENDER_AVERAGE => BRUSH_RENDER_AVERAGE,
        BRUSH_RENDER_ADDITIVE => BRUSH_RENDER_ADDITIVE,
        BRUSH_RENDER_OPAQUE | BRUSH_RENDER_CUTOUT => BRUSH_RENDER_OPAQUE,
        _ => BRUSH_RENDER_OPAQUE,
    }
}

/// Whether a brush mover may block a studio model's render-only sight ray.
///
/// GoldSrc still draws entities behind translucent/additive brush entities;
/// those brushes blend over the already-rendered scene. Alpha-tested brushes
/// have visible holes and likewise cannot conservatively occlude the complete
/// actor. Rotating movers are also excluded because their cooked point BSP is
/// not transformed for this inexpensive LOS walk.
#[inline]
pub const fn brush_blocks_actor_visual_los(render_class: u8, visual_rotating: bool) -> bool {
    render_class == BRUSH_RENDER_OPAQUE && !visual_rotating
}

/// Shared conservative camera-frustum verdict in view space.
///
/// Every renderable kind reaches this predicate after its own bounds have been
/// transformed by the authoritative camera. Keeping the plane approximation
/// here prevents world faces, brush entities, and studio actors from drifting
/// into subtly different edge/near behavior.
#[inline]
pub const fn sphere_in_camera_frustum(center: [i32; 3], radius: i32, near: i32, far: i32) -> bool {
    if center[2] + radius < near || center[2] - radius > far {
        return false;
    }
    let z = if center[2] > near { center[2] } else { near };
    if center[0].abs() * 2 > z * 2 + radius * 3 {
        return false;
    }
    center[1].abs() * 4 <= z * 3 + radius * 5
}

/// Camera-leaf cached candidate rule. Live movers must remain candidates even
/// while currently hidden: their transform can enter this unchanged PVS later.
#[inline]
pub const fn pvs_candidate(uses_live_bounds: bool, cooked_visible: bool) -> bool {
    uses_live_bounds || cooked_visible
}

/// Final per-visual visibility rule after the current mover AABB was relinked.
#[inline]
pub const fn pvs_visible_now(
    uses_live_bounds: bool,
    cooked_visible: bool,
    live_visible: bool,
) -> bool {
    if uses_live_bounds {
        live_visible
    } else {
        cooked_visible
    }
}

/// Translate authored bounds and apply GoldSrc's one-unit linking epsilon.
#[inline]
pub const fn translated_padded_bounds(
    mins: [i32; 3],
    maxs: [i32; 3],
    delta: [i32; 3],
) -> ([i32; 3], [i32; 3]) {
    let mut out_min = [0; 3];
    let mut out_max = [0; 3];
    let mut axis = 0usize;
    while axis < 3 {
        out_min[axis] = mins[axis].saturating_add(delta[axis]).saturating_sub(1);
        out_max[axis] = maxs[axis].saturating_add(delta[axis]).saturating_add(1);
        axis += 1;
    }
    (out_min, out_max)
}

/// Convert the absolute draw transform used by an origin brush into the
/// displacement expected by bounds authored at that brush's spawn position.
/// World-authored dmodels have an authored transform of zero, so the same
/// operation also covers ordinary func_train brushes without a special case.
#[inline]
pub const fn mover_bounds_delta(current: [i32; 3], authored: [i32; 3]) -> [i32; 3] {
    [
        current[0] - authored[0],
        current[1] - authored[1],
        current[2] - authored[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        brush_blocks_actor_visual_los, brush_packet_blend, mover_bounds_delta, pvs_candidate,
        pvs_visible_now, sphere_in_camera_frustum, translated_padded_bounds, BRUSH_RENDER_ADDITIVE,
        BRUSH_RENDER_AVERAGE, BRUSH_RENDER_CUTOUT, BRUSH_RENDER_OPAQUE,
    };

    #[test]
    fn non_opaque_brushes_do_not_hide_studio_actors() {
        assert!(brush_blocks_actor_visual_los(BRUSH_RENDER_OPAQUE, false));
        assert!(!brush_blocks_actor_visual_los(BRUSH_RENDER_AVERAGE, false));
        assert!(!brush_blocks_actor_visual_los(BRUSH_RENDER_ADDITIVE, false));
        assert!(!brush_blocks_actor_visual_los(BRUSH_RENDER_CUTOUT, false));
        assert!(!brush_blocks_actor_visual_los(BRUSH_RENDER_OPAQUE, true));
    }

    #[test]
    fn cutout_brushes_keep_opaque_ps1_packets() {
        assert_eq!(brush_packet_blend(BRUSH_RENDER_CUTOUT), BRUSH_RENDER_OPAQUE);
        assert_eq!(
            brush_packet_blend(BRUSH_RENDER_AVERAGE),
            BRUSH_RENDER_AVERAGE
        );
        assert_eq!(
            brush_packet_blend(BRUSH_RENDER_ADDITIVE),
            BRUSH_RENDER_ADDITIVE
        );
    }

    #[test]
    fn every_renderable_uses_the_same_conservative_camera_planes() {
        assert!(sphere_in_camera_frustum([0, 0, 16], 0, 16, 1200));
        assert!(sphere_in_camera_frustum([1210, 0, 1200], 16, 16, 1200));
        assert!(!sphere_in_camera_frustum([1300, 0, 1200], 16, 16, 1200));
        assert!(sphere_in_camera_frustum([0, 910, 1200], 16, 16, 1200));
        assert!(!sphere_in_camera_frustum([0, 1000, 1200], 16, 16, 1200));
        assert!(!sphere_in_camera_frustum([0, 0, 1220], 16, 16, 1200));
    }

    #[test]
    fn fixed_camera_keeps_a_moving_brush_eligible_until_it_enters_the_pvs() {
        // The cook-time/spawn leaf is hidden. A static entity is discarded,
        // but a mover stays in the candidate list and follows its live verdict.
        assert!(!pvs_candidate(false, false));
        assert!(pvs_candidate(true, false));
        assert!(!pvs_visible_now(true, false, false));
        assert!(pvs_visible_now(true, false, true));
    }

    #[test]
    fn live_bounds_follow_the_render_translation_with_goldsrc_padding() {
        let (mins, maxs) = translated_padded_bounds([10, -5, 30], [20, 5, 40], [110, 7, -10]);
        assert_eq!(mins, [119, 1, 19]);
        assert_eq!(maxs, [131, 13, 31]);
    }

    #[test]
    fn origin_brush_bounds_use_motion_relative_to_the_authored_transform() {
        // Hazard Course target1: the BSP model is local to an origin brush.
        // Its draw offset is therefore an absolute transform, not a delta that
        // can be added directly to already-world-space logic bounds.
        assert_eq!(
            mover_bounds_delta([-2526, -184, 870], [-2526, -268, 870]),
            [0, 84, 0]
        );
        // World-authored train models retain the historical zero-origin path.
        assert_eq!(mover_bounds_delta([32, -16, 8], [0, 0, 0]), [32, -16, 8]);
    }
}
