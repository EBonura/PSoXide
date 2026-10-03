//! Pure packed state/math for `func_pushable` brush entities.
//!
//! Runtime position lives in the existing `ENT_CACHE.origin`; this word uses
//! the existing `ENT_PHASE` slot for signed planar velocity, supporting mover,
//! and a one-bit trigger/PVS refresh marker. No per-cart BSS is needed.

pub const SUPPORT_NONE: u16 = u16::MAX;
pub const SUPPORT_WORLD: u16 = u16::MAX - 1;
pub const CONTACT_NONE: u8 = 0;
pub const CONTACT_PUSH: u8 = 1;
pub const CONTACT_PULL: u8 = 2;

const COLLISION_META_VALID: u16 = 0x8000;
const COLLISION_HULL_SHIFT: u16 = 8;
const COLLISION_MIN_CORR_SHIFT: u16 = 10;

const SUPPORT_SHIFT: u32 = 16;
const SUPPORT_MASK: u32 = 0x1ff;
const DIRTY_BIT: u32 = 1 << 25;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct State {
    pub vx: i8,
    pub vz: i8,
    pub vy: i8,
    pub support: u16,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollisionProxy {
    pub center: [i32; 3],
    pub half: [i32; 3],
}

/// Cart speed limit in units per tick, from the cooked low byte. The cooker
/// derives it from the entity's friction (`400 - friction` units per second
/// at the game's tick rate); the byte above carries collision metadata.
#[inline(always)]
pub const fn max_speed(packed_speed_half_x: i32) -> i32 {
    packed_speed_half_x & 0xff
}

/// Half extents (X, Y up, Z) of the player-class hulls a cart can collide
/// as, by the two-bit hull code in its cooked metadata: crouching, standing
/// and large. Code zero names no hull.
const COLLISION_HULL_HALF: [[i32; 3]; 4] = [[0, 0, 0], [16, 18, 16], [16, 36, 16], [32, 32, 32]];

/// The box a cart collides with the world as. A cart without collision
/// metadata sweeps its visual box. Otherwise it uses the player-class hull
/// its size implies, anchored at the cart's minimum corner, which on every
/// axis the cooker flagged is padded out by one more unit. An unknown hull
/// code yields an empty box at the origin.
#[inline(always)]
pub const fn collision_proxy(
    local_center: [i32; 3],
    visual_half: [i32; 3],
    packed_speed_half_x: i32,
) -> CollisionProxy {
    let meta = packed_speed_half_x as u16;
    if meta & COLLISION_META_VALID == 0 {
        return CollisionProxy {
            center: local_center,
            half: visual_half,
        };
    }
    let code = ((meta >> COLLISION_HULL_SHIFT) & 3) as usize;
    if code == 0 {
        return CollisionProxy {
            center: [0; 3],
            half: [0; 3],
        };
    }
    let half = COLLISION_HULL_HALF[code];
    let corrected = (meta >> COLLISION_MIN_CORR_SHIFT) & 7;
    let mut center = local_center;
    let mut axis = 0;
    while axis < 3 {
        let pad = if corrected & (1 << axis) != 0 { 1 } else { 0 };
        center[axis] = local_center[axis] - visual_half[axis] - pad + half[axis];
        axis += 1;
    }
    CollisionProxy { center, half }
}

/// Classify one point trace used by a translated pushable face. Generic
/// gameplay rays intentionally ignore `startsolid`, but a box sweep may only
/// ignore it when the endpoint is clear (the sample is escaping rounding or
/// existing penetration). Remaining solid at the endpoint is a blocked move.
#[inline(always)]
pub const fn blocking_sweep_fraction(startsolid: bool, endsolid: bool, frac: i32) -> Option<i32> {
    if startsolid {
        if endsolid {
            Some(0)
        } else {
            None
        }
    } else if frac < 4096 {
        Some(frac)
    } else {
        None
    }
}

#[inline(always)]
pub const fn pack(vx: i8, vz: i8, vy: i8, support: u16, dirty: bool) -> i32 {
    let support_code = if support == SUPPORT_NONE {
        0
    } else if support == SUPPORT_WORLD {
        1
    } else if support < 510 {
        support + 2
    } else {
        0
    };
    let vy = if vy < -32 {
        -32
    } else if vy > 31 {
        31
    } else {
        vy
    };
    ((vx as u8 as u32)
        | ((vz as u8 as u32) << 8)
        | ((support_code as u32) << SUPPORT_SHIFT)
        | if dirty { DIRTY_BIT } else { 0 }
        | (((vy as i32 & 0x3f) as u32) << 26)) as i32
}

#[inline(always)]
pub const fn unpack(word: i32) -> State {
    let bits = word as u32;
    let support_code = ((bits >> SUPPORT_SHIFT) & SUPPORT_MASK) as u16;
    let vy6 = ((bits >> 26) & 0x3f) as i8;
    State {
        vx: bits as u8 as i8,
        vz: (bits >> 8) as u8 as i8,
        vy: if vy6 & 0x20 != 0 { vy6 - 64 } else { vy6 },
        support: if support_code == 0 {
            SUPPORT_NONE
        } else if support_code == 1 {
            SUPPORT_WORLD
        } else {
            support_code - 2
        },
        dirty: bits & DIRTY_BIT != 0,
    }
}

#[inline]
fn isqrt(n: i32) -> i32 {
    psx_math::int32::isqrt_i32(n)
}

/// Limit a planar velocity to `max_speed`. Inside the limit it is returned
/// as is; outside, both components are scaled by `max_speed / length`
/// with the length rounded up and the products rounded toward zero, so the
/// result never exceeds the limit after rounding.
#[inline]
pub fn clamp_velocity(vx: i32, vz: i32, max_speed: i32) -> (i8, i8) {
    // A packed velocity component is an i8, so no limit exceeds 127.
    let max_speed = max_speed.clamp(0, i8::MAX as i32);
    let length_squared = vx * vx + vz * vz;
    if length_squared <= max_speed * max_speed {
        return (vx as i8, vz as i8);
    }
    let mut length = isqrt(length_squared);
    if length * length < length_squared {
        length += 1;
    }
    (
        (vx * max_speed / length) as i8,
        (vz * max_speed / length) as i8,
    )
}

/// Add the player's push velocity and clamp it to the entity's max speed.
#[inline]
pub fn accelerate(state: State, wish_x: i32, wish_z: i32, max_speed: i32) -> State {
    let (vx, vz) = clamp_velocity(
        state.vx as i32 + wish_x,
        state.vz as i32 + wish_z,
        max_speed,
    );
    State {
        vx,
        vz,
        vy: state.vy,
        dirty: state.dirty,
        support: state.support,
    }
}

/// One-unit ground drag once the player is no longer touching the cart.
#[inline]
pub const fn decay(state: State) -> State {
    State {
        vx: if state.vx > 0 {
            state.vx - 1
        } else if state.vx < 0 {
            state.vx + 1
        } else {
            0
        },
        vz: if state.vz > 0 {
            state.vz - 1
        } else if state.vz < 0 {
            state.vz + 1
        } else {
            0
        },
        vy: state.vy,
        dirty: state.dirty,
        support: state.support,
    }
}

/// Apply one tick of vertical gravity only while the cart has no support.
#[inline]
pub const fn fall_step(state: State, gravity: i8) -> State {
    State {
        vx: state.vx,
        vz: state.vz,
        vy: if state.support == SUPPORT_NONE {
            let next = state.vy as i16 - gravity as i16;
            if next < -32 {
                -32
            } else if next > 31 {
                31
            } else {
                next as i8
            }
        } else {
            0
        },
        support: state.support,
        dirty: state.dirty,
    }
}

/// How the player's movement acts on a cart this tick.
///
/// Nothing moves a cart unless the player is on the ground, not standing on
/// that cart, and giving movement input. Holding use turns pushing off: only
/// the cart the player is aiming at responds, and it is pulled. Otherwise a
/// cart the player touches is pushed while the input heads toward it
/// (`toward > 0`).
#[inline(always)]
pub const fn contact_mode(
    grounded: bool,
    standing_on_cart: bool,
    touching: bool,
    use_held: bool,
    use_target: bool,
    has_wish: bool,
    toward: i32,
) -> u8 {
    if !grounded || standing_on_cart || !has_wish {
        return CONTACT_NONE;
    }
    if use_held {
        return if use_target {
            CONTACT_PULL
        } else {
            CONTACT_NONE
        };
    }
    if touching && toward > 0 {
        CONTACT_PUSH
    } else {
        CONTACT_NONE
    }
}

/// A pulled cart follows a quarter of the player's input, rounded toward
/// zero but never to zero: any input moves it at least one unit.
#[inline(always)]
pub const fn pull_component(wish: i32) -> i32 {
    let quarter = wish / 4;
    if quarter == 0 {
        wish.signum()
    } else {
        quarter
    }
}

/// Leave a small Q12 guard before a traced wall plane. This converts a point
/// sweep hit into a conservative cart-face clamp and prevents rounding from
/// taking the full step through thin geometry.
#[inline]
pub const fn safe_hit_fraction(hit_fraction: i32) -> i32 {
    if hit_fraction <= 32 {
        0
    } else if hit_fraction >= 4096 {
        4064
    } else {
        hit_fraction - 32
    }
}

/// Apply a Q12 sweep fraction; a blocked trace can never take the full delta.
#[inline(always)]
pub const fn swept_component(delta: i32, fraction: i32) -> i32 {
    let fraction = if fraction < 0 {
        0
    } else if fraction > 4096 {
        4096
    } else {
        fraction
    };
    (delta * fraction) >> 12
}

/// Convert a downward support probe hit to cart-origin settlement. The probe
/// begins two units above the AABB bottom, so its raw hit delta would sink the
/// cart two units too far into the support plane.
#[inline]
pub const fn support_settle(probe_y: i32, hit_y: i32) -> i32 {
    let dy = hit_y - probe_y + 2;
    if dy < 0 {
        dy
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_state_round_trips_signed_velocity_support_and_dirty() {
        let state = unpack(pack(-9, 7, -12, 148, true));
        assert_eq!(state.vx, -9);
        assert_eq!(state.vz, 7);
        assert_eq!(state.vy, -12);
        assert_eq!(state.support, 148);
        assert!(state.dirty);
        assert_eq!(
            unpack(pack(0, 0, 0, SUPPORT_NONE, false)).support,
            SUPPORT_NONE
        );
        assert_eq!(
            unpack(pack(0, 0, 0, SUPPORT_WORLD, false)).support,
            SUPPORT_WORLD
        );
    }

    #[test]
    fn friction_220_speed_clamps_to_nine_units_per_tick() {
        let state = State {
            vx: 0,
            vz: 0,
            vy: 0,
            support: SUPPORT_NONE,
            dirty: false,
        };
        let pushed = accelerate(state, 30, 0, 9);
        assert_eq!((pushed.vx, pushed.vz), (9, 0));
        let diagonal = accelerate(state, 9, 9, 9);
        assert!(
            diagonal.vx as i32 * diagonal.vx as i32 + diagonal.vz as i32 * diagonal.vz as i32 <= 81
        );
        let oblique = clamp_velocity(10, 3, 9);
        assert_eq!(oblique, (8, 2));
        assert!(oblique.0 as i32 * oblique.0 as i32 + oblique.1 as i32 * oblique.1 as i32 <= 81);
    }

    #[test]
    fn collision_metadata_reconstructs_goldsrc_large_hull_at_padded_mins() {
        // c1a0e sample_cart2: speed 9, hull 2 (encoded 3), corrections on
        // world X/Z, local visual centre -54/-4/0 and half 66/32/32.
        let packed = (66i32 << 16) | 0x9709;
        assert_eq!(max_speed(packed), 9);
        assert_eq!(
            collision_proxy([-54, -4, 0], [66, 32, 32], packed),
            CollisionProxy {
                center: [-89, -4, -1],
                half: [32, 32, 32],
            }
        );
    }

    #[test]
    fn legacy_room_keeps_its_visual_sweep_box() {
        let packed = (66i32 << 16) | 9;
        assert_eq!(max_speed(packed), 9);
        assert_eq!(
            collision_proxy([-54, -4, 0], [66, 32, 32], packed),
            CollisionProxy {
                center: [-54, -4, 0],
                half: [66, 32, 32],
            }
        );
    }

    #[test]
    fn drag_converges_and_sweep_never_tunnels_to_full_delta() {
        let state = State {
            vx: -2,
            vz: 1,
            vy: 0,
            support: 3,
            dirty: true,
        };
        assert_eq!((decay(state).vx, decay(state).vz), (-1, 0));
        assert_eq!(swept_component(9, 2048), 4);
        assert_eq!(swept_component(-9, 2048), -5);
        assert_eq!(swept_component(9, 0), 0);
        assert_eq!(safe_hit_fraction(16), 0);
        assert_eq!(safe_hit_fraction(2048), 2016);
        assert!(swept_component(9, safe_hit_fraction(2048)) < 9);
    }

    #[test]
    fn startsolid_pushable_sample_only_escapes_toward_clear_space() {
        assert_eq!(blocking_sweep_fraction(true, true, 4096), Some(0));
        assert_eq!(blocking_sweep_fraction(true, false, 4096), None);
        assert_eq!(blocking_sweep_fraction(false, false, 2048), Some(2048));
        assert_eq!(blocking_sweep_fraction(false, false, 4096), None);
    }

    #[test]
    fn only_grounded_side_contact_pushes_and_use_enables_pull() {
        assert_eq!(
            contact_mode(true, false, true, false, false, true, 10),
            CONTACT_PUSH
        );
        assert_eq!(
            contact_mode(true, false, true, false, false, true, -10),
            CONTACT_NONE
        );
        assert_eq!(
            contact_mode(true, false, false, true, true, true, -10),
            CONTACT_PULL
        );
        assert_eq!(
            contact_mode(true, false, true, true, true, true, 10),
            CONTACT_PULL
        );
        assert_eq!(
            contact_mode(true, false, true, true, false, true, 10),
            CONTACT_NONE
        );
        assert_eq!(
            contact_mode(false, false, true, true, true, true, -10),
            CONTACT_NONE
        );
        assert_eq!(
            contact_mode(true, true, true, true, true, true, -10),
            CONTACT_NONE
        );
        assert_eq!(
            contact_mode(true, false, false, true, true, false, -10),
            CONTACT_NONE
        );
        assert_eq!(pull_component(9), 2);
        assert_eq!(pull_component(-9), -2);
        assert_eq!(pull_component(1), 1);
    }

    #[test]
    fn unsupported_cart_falls_but_world_or_mover_support_cancels_fall() {
        let unsupported = State {
            vx: 0,
            vz: 0,
            vy: 0,
            support: SUPPORT_NONE,
            dirty: false,
        };
        assert_eq!(fall_step(unsupported, 2).vy, -2);
        assert_eq!(fall_step(fall_step(unsupported, 2), 2).vy, -4);

        let world = State {
            vy: -20,
            support: SUPPORT_WORLD,
            ..unsupported
        };
        assert_eq!(fall_step(world, 2).vy, 0);
        let lift = State {
            vy: -20,
            support: 148,
            ..unsupported
        };
        assert_eq!(fall_step(lift, 2).vy, 0);
    }

    #[test]
    fn support_probe_settles_from_the_cart_bottom_not_probe_origin() {
        assert_eq!(support_settle(-537, -544), -5);
        assert_eq!(support_settle(-542, -543), 0);
        assert_eq!(support_settle(-543, -543), 0);
    }
}
