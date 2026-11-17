//! Fixed-capacity vitality field state: the checkpoints that restore health
//! and energy.
//!
//! Authored records stay in the generated manifest; the mutable console state
//! is one claim bit per field plus a single fractional-rate accumulator for
//! the field currently containing the player.

use psx_level::{LevelVitalityCircleRecord, RoomIndex, MAX_VITALITY_CIRCLES};

use crate::poi::xz_within_radius;
use psx_level::CombatStats;

use crate::combatant::Combatant;
use crate::vitality::VitalityChannelId;

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
            if !xz_within_radius([player_x, player_z], [circle.x, circle.z], radius) {
                continue;
            }
            let bit = 1u32 << index;
            self.claimed |= bit;
            newly_claimed |= bit;
        }
        newly_claimed
    }

    /// Apply the containing claimed field once for one 60 Hz simulation tick.
    /// A field matching `fighter`'s stance restores health and energy at its
    /// refill rate; a mismatched one drains health at its drain rate, but a
    /// field never kills. Returns whether voluntary stance swapping must be
    /// locked this tick.
    pub fn tick(
        &mut self,
        circles: &[LevelVitalityCircleRecord],
        room: RoomIndex,
        player_x: i32,
        player_z: i32,
        fighter: &mut Combatant,
        stats: &CombatStats,
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
        let aligned = circle_axis(circle) == fighter.stance.active();
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
                fighter.restore(amount, amount, stats);
            } else {
                // Environmental field drain is never a death source; combat
                // can still finish the fighter.
                let current = fighter.health.current();
                fighter
                    .health
                    .apply_damage(amount.min(current.saturating_sub(1)));
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
    xz_within_radius([x, z], [circle.x, circle.z], u32::from(circle.radius))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    const STATS: CombatStats = CombatStats::DEFAULT;

    /// A fighter on `side` with `health` and `energy` left.
    fn fighter(side: VitalityChannelId, health: u16, energy: u16) -> Combatant {
        let mut c = Combatant::EMPTY;
        c.reset(
            &STATS,
            psx_level::combat_verbs::CombatKits::from_bits(
                psx_level::combat_verbs::CombatKits::MELEE
                    | psx_level::combat_verbs::CombatKits::RANGED,
            ),
        );
        if side != c.stance.active() {
            assert!(c.request_swap(0));
        }
        c.health.apply_damage(STATS.max_health - health);
        c.flow.energy = energy;
        c
    }

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
        let mut c = fighter(VitalityChannelId::One, 200, 100);
        assert!(!state.tick(&[CIRCLE], RoomIndex(2), 1000, 200, &mut c, &STATS));
        assert!(state.is_claimed(0));
    }

    #[test]
    fn field_restores_health_and_energy_on_match_and_drains_on_mismatch() {
        let mut state = VitalityCircleState::EMPTY;
        state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::One);
        let mut c = fighter(VitalityChannelId::One, 50, 30);
        assert!(state.tick(&[CIRCLE], RoomIndex(2), 100, 200, &mut c, &STATS));
        assert_eq!((c.health(), c.flow.energy), (51, 31));
        let mut c = fighter(VitalityChannelId::Two, 100, 30);
        assert!(state.tick(&[CIRCLE], RoomIndex(2), 100, 200, &mut c, &STATS));
        assert_eq!((c.health(), c.flow.energy), (98, 30));
    }

    #[test]
    fn drain_never_kills() {
        let mut state = VitalityCircleState::EMPTY;
        state.claim_struck(&[CIRCLE], RoomIndex(2), 100, 200, 0, VitalityChannelId::One);
        let mut c = fighter(VitalityChannelId::Two, 2, 100);
        for _ in 0..60 {
            state.tick(&[CIRCLE], RoomIndex(2), 100, 200, &mut c, &STATS);
        }
        assert_eq!(c.health(), 1);
        assert!(!c.defeated());
    }
}
