//! Shared destructible health state for brush and typed world-object targets.

use psx_level::{destructible_affinity, destructible_flags, LevelDestructibleRecord};

/// Player/enemy attack channel used by a destructible affinity test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DamageChannel {
    /// Horizon lane attack.
    Horizon,
    /// Zenith lane attack.
    Zenith,
}

/// Result of applying one damage event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageOutcome {
    /// A live compatible state accepted damage.
    pub connected: bool,
    /// This event reduced the state to zero health.
    pub broke: bool,
}

/// Invalid cooked shared-destructible data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestructibleInitError {
    /// More records were cooked than this fixed-capacity owner can store.
    CapacityExceeded,
    /// A record has no health.
    ZeroHealth(usize),
    /// A record uses an unknown attack affinity code.
    UnknownAffinity(usize),
    /// A record contains runtime bits this build does not understand.
    UnknownFlags(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DestructibleState {
    max_health: u16,
    health: u16,
    affinity: u8,
    enabled: bool,
}

impl DestructibleState {
    const EMPTY: Self = Self {
        max_health: 1,
        health: 0,
        affinity: destructible_affinity::BOTH,
        enabled: false,
    };
}

/// Fixed-capacity runtime owner for the level's shared destructible states.
pub struct RuntimeDestructibles<const MAX: usize> {
    count: usize,
    states: [DestructibleState; MAX],
    /// One hit per state during a single authored swing.
    swing_hits: u32,
}

impl<const MAX: usize> RuntimeDestructibles<MAX> {
    /// Empty state suitable for static initialization.
    pub const EMPTY: Self = Self {
        count: 0,
        states: [DestructibleState::EMPTY; MAX],
        swing_hits: 0,
    };

    /// Rebuild runtime health from cooked configuration.
    pub fn init(
        &mut self,
        records: &[LevelDestructibleRecord],
    ) -> Result<(), DestructibleInitError> {
        if records.len() > MAX || records.len() > u32::BITS as usize {
            return Err(DestructibleInitError::CapacityExceeded);
        }
        *self = Self::EMPTY;
        for (index, record) in records.iter().enumerate() {
            if record.max_health == 0 {
                return Err(DestructibleInitError::ZeroHealth(index));
            }
            if record.damage_affinity > destructible_affinity::BOTH {
                return Err(DestructibleInitError::UnknownAffinity(index));
            }
            if record.flags & !destructible_flags::ENABLED != 0 {
                return Err(DestructibleInitError::UnknownFlags(index));
            }
            let enabled = record.flags & destructible_flags::ENABLED != 0;
            self.states[index] = DestructibleState {
                max_health: record.max_health,
                health: if enabled { record.max_health } else { 0 },
                affinity: record.damage_affinity,
                enabled,
            };
        }
        self.count = records.len();
        Ok(())
    }

    /// Begin a new authored swing, allowing each state to connect once.
    pub fn begin_swing(&mut self) {
        self.swing_hits = 0;
    }

    /// Restore every enabled state to full health.
    pub fn reset(&mut self) {
        self.swing_hits = 0;
        for state in &mut self.states[..self.count] {
            state.health = if state.enabled { state.max_health } else { 0 };
        }
    }

    /// Force one enabled state into its already-broken form when restoring a
    /// persistent world flag from a save block.
    pub fn restore_broken(&mut self, index: usize) -> bool {
        let Some(state) = self
            .states
            .get_mut(..self.count)
            .and_then(|states| states.get_mut(index))
        else {
            return false;
        };
        if !state.enabled {
            return false;
        }
        state.health = 0;
        true
    }

    /// Whether one state exists, is enabled, and still has health.
    pub fn alive(&self, index: usize) -> bool {
        self.states
            .get(..self.count)
            .and_then(|states| states.get(index))
            .is_some_and(|state| state.enabled && state.health != 0)
    }

    /// Whether this swing already connected with the state.
    pub fn hit_this_swing(&self, index: usize) -> bool {
        index < u32::BITS as usize && self.swing_hits & (1u32 << index) != 0
    }

    /// Apply damage once to a compatible live state.
    pub fn apply_damage(
        &mut self,
        index: usize,
        channel: DamageChannel,
        damage: u16,
    ) -> DamageOutcome {
        if damage == 0 || index >= self.count || self.hit_this_swing(index) {
            return DamageOutcome::default();
        }
        let state = &mut self.states[index];
        let accepts = match state.affinity {
            destructible_affinity::HORIZON => matches!(channel, DamageChannel::Horizon),
            destructible_affinity::ZENITH => matches!(channel, DamageChannel::Zenith),
            destructible_affinity::BOTH => true,
            _ => false,
        };
        if !state.enabled || state.health == 0 || !accepts {
            return DamageOutcome::default();
        }
        state.health = state.health.saturating_sub(damage);
        self.swing_hits |= 1u32 << index;
        DamageOutcome {
            connected: true,
            broke: state.health == 0,
        }
    }
}

impl<const MAX: usize> Default for RuntimeDestructibles<MAX> {
    fn default() -> Self {
        Self::EMPTY
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shared_state_filters_affinity_and_only_connects_once_per_swing() {
        let mut states = RuntimeDestructibles::<4>::EMPTY;
        states
            .init(&[LevelDestructibleRecord {
                max_health: 20,
                persistent_flag: 31,
                damage_affinity: destructible_affinity::HORIZON,
                flags: destructible_flags::ENABLED,
            }])
            .unwrap();
        assert!(!states.apply_damage(0, DamageChannel::Zenith, 20).connected);
        assert!(states.apply_damage(0, DamageChannel::Horizon, 10).connected);
        assert!(!states.apply_damage(0, DamageChannel::Horizon, 10).connected);
        states.begin_swing();
        assert!(states.apply_damage(0, DamageChannel::Horizon, 10).broke);
        assert!(!states.alive(0));
    }

    #[test]
    fn shared_state_rejects_unknown_runtime_flags() {
        let mut states = RuntimeDestructibles::<1>::EMPTY;
        assert_eq!(
            states.init(&[LevelDestructibleRecord {
                max_health: 1,
                persistent_flag: 7,
                damage_affinity: destructible_affinity::BOTH,
                flags: destructible_flags::ENABLED << 1,
            }]),
            Err(DestructibleInitError::UnknownFlags(0)),
        );
    }

    #[test]
    fn persistent_restore_removes_an_enabled_target_without_a_damage_event() {
        let mut states = RuntimeDestructibles::<1>::EMPTY;
        states
            .init(&[LevelDestructibleRecord {
                max_health: 20,
                persistent_flag: 9,
                damage_affinity: destructible_affinity::BOTH,
                flags: destructible_flags::ENABLED,
            }])
            .unwrap();
        assert!(states.alive(0));
        assert!(states.restore_broken(0));
        assert!(!states.alive(0));
    }
}
