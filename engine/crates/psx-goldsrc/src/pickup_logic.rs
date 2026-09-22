//! Pure pickup-contact rules shared by gameplay and host-side regression tests.

const ITEM_HALF_WIDTH: i32 = 16;
const WEAPON_HALF_WIDTH: i32 = 24;
const PLAYER_HALF_WIDTH: i32 = 16;
const PICKUP_HEIGHT: i32 = 16;

/// Overlap the linked GoldSrc boxes, with Y as this port's vertical axis.
/// SetObjectCollisionBox expands the player and ordinary items by one unit.
/// Weapons override it with an exact (-24,-24,0)..(24,24,16) box instead.
/// The caller supplies the live collision hull, including a blocked unduck.
#[inline]
fn touches_box(
    player_pos: [i32; 3],
    player_half_height: i32,
    item_pos: [i32; 3],
    pickup_half_width: i32,
    pickup_margin: i32,
    pickup_height: i32,
) -> bool {
    let margin = 1 + pickup_margin;
    let dy = item_pos[1] - player_pos[1];
    if dy < -player_half_height - pickup_height - margin || dy > player_half_height + margin {
        return false;
    }
    let dx = item_pos[0] - player_pos[0];
    let dz = item_pos[2] - player_pos[2];
    let reach = PLAYER_HALF_WIDTH + pickup_half_width + margin;
    dx.abs() <= reach && dz.abs() <= reach
}

#[inline]
pub fn touches_item(player_pos: [i32; 3], player_half_height: i32, item_pos: [i32; 3]) -> bool {
    touches_box(
        player_pos,
        player_half_height,
        item_pos,
        ITEM_HALF_WIDTH,
        1,
        PICKUP_HEIGHT,
    )
}

#[inline]
pub fn touches_weapon(player_pos: [i32; 3], player_half_height: i32, item_pos: [i32; 3]) -> bool {
    touches_box(
        player_pos,
        player_half_height,
        item_pos,
        WEAPON_HALF_WIDTH,
        0,
        PICKUP_HEIGHT,
    )
}

#[inline]
pub fn touches_sodacan(player_pos: [i32; 3], player_half_height: i32, can_pos: [i32; 3]) -> bool {
    touches_box(player_pos, player_half_height, can_pos, 8, 1, 8)
}

#[cfg(test)]
mod tests {
    use super::{touches_item, touches_weapon};

    #[test]
    fn soda_can_uses_its_eight_unit_trigger_with_live_player_hull() {
        for h in [18, 36] {
            for (point, expected) in [
                ([26, 0, 0], true),
                ([27, 0, 0], false),
                ([0, 0, -26], true),
                ([0, 0, -27], false),
                ([0, h + 10, 0], true),
                ([0, h + 11, 0], false),
                ([0, -h - 2, 0], true),
                ([0, -h - 3, 0], false),
            ] {
                assert_eq!(super::touches_sodacan(point, h, [0; 3]), expected);
            }
        }
    }

    #[test]
    fn crouching_keeps_floor_pickups_but_cannot_reach_a_standing_height_shelf() {
        for contact in [touches_item, touches_weapon] {
            assert!(contact([0, 36, 0], 36, [0, 0, 0]));
            assert!(contact([0, 18, 0], 18, [0, 0, 0]));
            assert!(contact([0, 36, 0], 36, [0, 50, 0]));
            assert!(!contact([0, 18, 0], 18, [0, 50, 0]));
        }
    }

    #[test]
    fn airborne_duck_changes_contact_without_shifting_origin() {
        for contact in [touches_item, touches_weapon] {
            assert!(contact([0, 60, 0], 36, [0, 10, 0]));
            assert!(!contact([0, 60, 0], 18, [0, 10, 0]));
        }
    }

    #[test]
    fn linked_box_margins_differ_for_items_and_weapons() {
        assert!(touches_item([0, 36, 0], 36, [34, -18, 34]));
        assert!(!touches_item([0, 36, 0], 36, [35, -18, 34]));
        assert!(!touches_item([0, 36, 0], 36, [34, -19, 34]));
        assert!(touches_weapon([0, 36, 0], 36, [41, -17, 41]));
        assert!(!touches_weapon([0, 36, 0], 36, [42, -17, 41]));
        assert!(!touches_weapon([0, 36, 0], 36, [41, -18, 41]));
    }

    /// Independently construct the original DLL's absolute boxes, then apply
    /// the engine's six BoundsIntersect comparisons. Include every boundary
    /// neighbourhood for both hulls and both pickup classes, translated away
    /// from the origin and covering diagonal corner contacts.
    #[test]
    fn matches_goldsrc_linked_aabb_oracle() {
        let player = [91, -57, 123];
        for half_height in [18, 36] {
            let pmin = [player[0] - 17, player[1] - half_height - 1, player[2] - 17];
            let pmax = [player[0] + 17, player[1] + half_height + 1, player[2] + 17];
            for weapon in [false, true] {
                let (mins, maxs) = if weapon {
                    ([-24, 0, -24], [24, 16, 24])
                } else {
                    ([-17, -1, -17], [17, 17, 17])
                };
                for x in -43..=43 {
                    for y in -56..=40 {
                        for z in [-43, -42, -41, -35, -34, 0, 34, 35, 41, 42, 43] {
                            let item = [player[0] + x, player[1] + y, player[2] + z];
                            let expected = (0..3).all(|axis| {
                                item[axis] + mins[axis] <= pmax[axis]
                                    && item[axis] + maxs[axis] >= pmin[axis]
                            });
                            let actual = if weapon {
                                touches_weapon(player, half_height, item)
                            } else {
                                touches_item(player, half_height, item)
                            };
                            assert_eq!(
                                actual, expected,
                                "h={half_height} weapon={weapon} offset={x},{y},{z}"
                            );
                        }
                    }
                }
            }
        }
    }
}
