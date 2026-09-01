//! Fixed-capacity dual-vitality field state.
//!
//! Authored records stay in the generated manifest; the mutable console state
//! is one claim bit per field plus a single fractional-rate accumulator for
//! the field currently containing the player.

use psx_level::{LevelVitalityCircleRecord, RoomIndex, MAX_VITALITY_CIRCLES};

use crate::vitality::{DualVitality, VitalityChannelId};

const TICKS_PER_SECOND: u16 = 60;
const NO_ACTIVE_CIRCLE: u8 = u8::MAX;

/// Mutable state for all vitality circles in one level (eight bytes on PSX).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VitalityCircleState {
    claimed: u32,
    active_circle: u8,
    rate_fraction: u16,
}

impl VitalityCircleState {
    /// No fields claimed and no player overlap.
    pub const EMPTY: Self = Self {
        claimed: 0,
        active_circle: NO_ACTIVE_CIRCLE,
        rate_fraction: 0,
    };

    /// True when the authored field was claimed by a matching-axis strike.
    pub const fn is_claimed(self, index: usize) -> bool {
        index < MAX_VITALITY_CIRCLES && self.claimed & (1u32 << index) != 0
    }

    /// Claim every matching field touched by a player strike.
    ///
    /// The attack reach expands the authored disc, making this compatible
    /// with both legacy arc attacks and authored weapon capsules without a
    /// second collision representation.
    pub fn claim_struck(
        &mut self,
        circles: &[LevelVitalityCircleRecord],
        room: RoomIndex,
        player_x: i32,
        player_z: i32,
        reach: u16,
        attack: VitalityChannelId,
    ) -> u32 {
        let mut newly_claimed = 0u32;
        for (index, circle) in circles.iter().take(MAX_VITALITY_CIRCLES).enumerate() {
            if circle.room != room || circle_axis(circle) != attack || self.is_claimed(index) {
                continue;
            }
            let radius = u32::from(circle.radius).saturating_add(u32::from(reach));
            if xz_distance_squared(player_x, player_z, circle.x, circle.z)
                > u64::from(radius) * u64::from(radius)
            {
                continue;
            }
            let bit = 1u32 << index;
            self.claimed |= bit;
            newly_claimed |= bit;
        }
        newly_claimed
    }

    /// Apply the containing claimed field once for one 60 Hz simulation tick.
    /// Returns whether voluntary stance swapping must be locked this tick.
    pub fn tick(
        &mut self,
        circles: &[LevelVitalityCircleRecord],
        room: RoomIndex,
        player_x: i32,
        player_z: i32,
        active: VitalityChannelId,
        vitality: &mut DualVitality,
    ) -> bool {
        let containing =
            circles
                .iter()
                .take(MAX_VITALITY_CIRCLES)
                .enumerate()
                .find(|(index, circle)| {
                    self.is_claimed(*index)
                        && circle.room == room
                        && within_circle(player_x, player_z, circle)
                });
        let Some((index, circle)) = containing else {
            self.active_circle = NO_ACTIVE_CIRCLE;
            self.rate_fraction = 0;
            return false;
        };
        let compact_index = index as u8;
        if self.active_circle != compact_index {
            self.active_circle = compact_index;
            self.rate_fraction = 0;
        }
        let aligned = circle_axis(circle) == active;
        let per_second = if aligned {
            circle.refill_per_second
        } else {
            circle.drain_per_second
        };
        let accumulated = self.rate_fraction.saturating_add(per_second);
        let amount = accumulated / TICKS_PER_SECOND;
        self.rate_fraction = accumulated % TICKS_PER_SECOND;
        if amount != 0 {
            if aligned {
                vitality.heal(active, amount);
            } else {
                // A circle may empty the active pool, except when the other
                // pool is already empty: environmental field drain is never a
                // direct two-pool death source. Combat can still finish it.
                let current = vitality.pool(active).current();
                let safe_amount = if vitality.pool(active.other()).current() == 0 {
                    amount.min(current.saturating_sub(1))
                } else {
                    amount
                };
                vitality.apply_damage(active, safe_amount);
            }
        }
        true
    }
}

#[inline]
/// Decode the manifest's compact axis byte.
pub const fn circle_axis(circle: &LevelVitalityCircleRecord) -> VitalityChannelId {
    if circle.axis == 0 {
        VitalityChannelId::One
    } else {
        VitalityChannelId::Two
    }
}

fn within_circle(x: i32, z: i32, circle: &LevelVitalityCircleRecord) -> bool {
    let radius = u64::from(circle.radius);
    xz_distance_squared(x, z, circle.x, circle.z) <= radius * radius
}

fn xz_distance_squared(ax: i32, az: i32, bx: i32, bz: i32) -> u64 {
    let dx = i64::from(ax) - i64::from(bx);
    let dz = i64::from(az) - i64::from(bz);
    (dx * dx + dz * dz) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vitality::{VitalityChannelId, VitalityPool};

    const CIRCLE: LevelVitalityCircleRecord = LevelVitalityCircleRecord {
        room: RoomIndex(2),
        x: 100,
        y: 0,
        z: 200,
        radius: 64,
        axis: 0,
        refill_per_second: 60,
        drain_per_second: 120,
    };

    #[test]
    fn matching_hit_claims_and_claim_persists_after_leaving() {
        assert!(core::mem::size_of::<VitalityCircleState>() <= 8);
        let mut state = VitalityCircleState::EMPTY;
        assert_eq!(
            state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::Two),
            0
        );
        assert_eq!(
            state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::One),
            1
        );
        let mut vitality = DualVitality::equal(100);
        assert!(!state.tick(
            &[CIRCLE],
            RoomIndex(2),
            1000,
            200,
            VitalityChannelId::One,
            &mut vitality
        ));
        assert!(state.is_claimed(0));
    }

    #[test]
    fn field_heals_match_drains_mismatch_and_locks_swap() {
        let mut state = VitalityCircleState::EMPTY;
        state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::One);
        let mut vitality =
            DualVitality::from_pools(VitalityPool::at(50, 100), VitalityPool::full(100));
        assert!(state.tick(
            &[CIRCLE],
            RoomIndex(2),
            100,
            200,
            VitalityChannelId::One,
            &mut vitality
        ));
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 51);
        assert!(state.tick(
            &[CIRCLE],
            RoomIndex(2),
            100,
            200,
            VitalityChannelId::Two,
            &mut vitality
        ));
        assert_eq!(vitality.pool(VitalityChannelId::Two).current(), 98);
    }

    #[test]
    fn drain_can_empty_one_pool_but_cannot_finish_a_two_pool_death() {
        let mut state = VitalityCircleState::EMPTY;
        state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::One);
        let mut vitality =
            DualVitality::from_pools(VitalityPool::at(100, 100), VitalityPool::at(1, 100));
        state.tick(
            &[CIRCLE],
            RoomIndex(2),
            100,
            200,
            VitalityChannelId::Two,
            &mut vitality,
        );
        assert_eq!(vitality.pool(VitalityChannelId::Two).current(), 0);

        let mut vitality =
            DualVitality::from_pools(VitalityPool::at(0, 100), VitalityPool::at(1, 100));
        for _ in 0..60 {
            state.tick(
                &[CIRCLE],
                RoomIndex(2),
                100,
                200,
                VitalityChannelId::Two,
                &mut vitality,
            );
        }
        assert_eq!(vitality.pool(VitalityChannelId::Two).current(), 1);
    }
}
