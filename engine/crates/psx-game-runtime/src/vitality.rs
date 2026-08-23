//! Dual-pool vitality policy shared by players and enemies.
//!
//! Damage routing remains game-owned: callers choose which channel receives a
//! hit. This module owns the invariant that an actor is defeated only when both
//! equal-status channels are empty, plus the empty/full boost-slot vocabulary
//! used by the HUD and inventory UI.

/// One of the actor's two equal-status vitality channels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VitalityChannelId {
    /// First vitality channel.
    One = 0,
    /// Second vitality channel.
    Two = 1,
}

impl VitalityChannelId {
    const fn index(self) -> usize {
        self as usize
    }
}

/// Whether a vitality channel is exactly at an endpoint or between them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VitalityEdgeState {
    /// No vitality remains; the high-risk empty boost socket is active.
    Empty,
    /// The channel is neither empty nor full; neither endpoint socket is active.
    Between,
    /// Vitality is at its maximum; the stable full boost socket is active.
    Full,
}

/// One bounded vitality pool.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VitalityPool {
    current: u16,
    maximum: u16,
}

impl VitalityPool {
    /// Create a full pool. A zero authored maximum is normalized to one so all
    /// ratio consumers keep a valid denominator.
    pub const fn full(maximum: u16) -> Self {
        let maximum = if maximum == 0 { 1 } else { maximum };
        Self {
            current: maximum,
            maximum,
        }
    }

    /// Current vitality.
    pub const fn current(self) -> u16 {
        self.current
    }

    /// Maximum vitality.
    pub const fn maximum(self) -> u16 {
        self.maximum
    }

    /// Exact endpoint state used to select an empty/full boost socket.
    pub const fn edge_state(self) -> VitalityEdgeState {
        if self.current == 0 {
            VitalityEdgeState::Empty
        } else if self.current >= self.maximum {
            VitalityEdgeState::Full
        } else {
            VitalityEdgeState::Between
        }
    }

    fn apply_damage(&mut self, damage: u16) -> bool {
        let was_alive = self.current > 0;
        self.current = self.current.saturating_sub(damage);
        was_alive && self.current == 0
    }

    fn refill(&mut self) {
        self.current = self.maximum;
    }
}

/// A player's or enemy's two vitality pools.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DualVitality {
    pools: [VitalityPool; 2],
}

impl DualVitality {
    /// Create two full, equal-capacity vitality pools.
    pub const fn equal(maximum_each: u16) -> Self {
        let pool = VitalityPool::full(maximum_each);
        Self {
            pools: [pool, pool],
        }
    }

    /// Read one channel.
    pub const fn pool(&self, channel: VitalityChannelId) -> VitalityPool {
        self.pools[channel.index()]
    }

    /// Whether both vitality channels are empty.
    pub const fn is_defeated(&self) -> bool {
        self.pools[0].current == 0 && self.pools[1].current == 0
    }

    /// Apply damage to an explicitly selected channel.
    ///
    /// The caller deliberately owns channel selection. This keeps sequential,
    /// player-selectable, attack-typed, or alternating routing policies out of
    /// the shared health primitive.
    pub fn apply_damage(
        &mut self,
        channel: VitalityChannelId,
        damage: u16,
    ) -> DualVitalityDamageOutcome {
        let was_defeated = self.is_defeated();
        let pool = &mut self.pools[channel.index()];
        let channel_depleted = damage > 0 && pool.apply_damage(damage);
        let current = pool.current;
        DualVitalityDamageOutcome {
            channel,
            current,
            channel_depleted,
            actor_defeated: !was_defeated && channel_depleted && self.is_defeated(),
        }
    }

    /// Refill both channels for a spawn or checkpoint respawn.
    pub fn refill(&mut self) {
        self.pools[0].refill();
        self.pools[1].refill();
    }
}

/// Result of one channel-targeted damage application.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DualVitalityDamageOutcome {
    /// Channel that received the damage.
    pub channel: VitalityChannelId,
    /// Remaining vitality in that channel.
    pub current: u16,
    /// Whether this application newly emptied the selected channel.
    pub channel_depleted: bool,
    /// Whether this application newly emptied the actor's second remaining
    /// channel and should therefore arm one death/game-over sequence.
    pub actor_defeated: bool,
}

/// Boost assignments at the two endpoint states of one vitality channel.
///
/// The boost identifier remains generic until the project chooses its item
/// database representation. Effect strength belongs to that boost data, where
/// empty-state records can intentionally be tuned above full-state records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VitalityBoostSlots<T> {
    /// High-risk boost active while the channel is exactly empty.
    pub empty: Option<T>,
    /// Stable boost active while the channel is exactly full.
    pub full: Option<T>,
}

impl<T> VitalityBoostSlots<T> {
    /// Resolve the boost active for the channel's current endpoint state.
    pub const fn active(&self, state: VitalityEdgeState) -> Option<&T> {
        match state {
            VitalityEdgeState::Empty => self.empty.as_ref(),
            VitalityEdgeState::Full => self.full.as_ref(),
            VitalityEdgeState::Between => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defeat_requires_both_channels_to_be_empty() {
        let mut vitality = DualVitality::equal(100);
        let first = vitality.apply_damage(VitalityChannelId::One, 100);
        assert!(first.channel_depleted);
        assert!(!first.actor_defeated);
        assert!(!vitality.is_defeated());
        assert_eq!(
            vitality.pool(VitalityChannelId::One).edge_state(),
            VitalityEdgeState::Empty
        );
        assert_eq!(
            vitality.pool(VitalityChannelId::Two).edge_state(),
            VitalityEdgeState::Full
        );

        let second = vitality.apply_damage(VitalityChannelId::Two, 100);
        assert!(second.channel_depleted);
        assert!(second.actor_defeated);
        assert!(vitality.is_defeated());
    }

    #[test]
    fn defeated_actor_never_rearms_from_repeated_damage() {
        let mut vitality = DualVitality::equal(1);
        let _ = vitality.apply_damage(VitalityChannelId::One, 1);
        let lethal = vitality.apply_damage(VitalityChannelId::Two, 1);
        assert!(lethal.actor_defeated);

        let repeated = vitality.apply_damage(VitalityChannelId::Two, 20);
        assert!(!repeated.channel_depleted);
        assert!(!repeated.actor_defeated);
    }

    #[test]
    fn boost_slots_only_activate_at_exact_endpoints() {
        let slots = VitalityBoostSlots {
            empty: Some(10u16),
            full: Some(20u16),
        };
        assert_eq!(slots.active(VitalityEdgeState::Empty), Some(&10));
        assert_eq!(slots.active(VitalityEdgeState::Between), None);
        assert_eq!(slots.active(VitalityEdgeState::Full), Some(&20));
    }

    #[test]
    fn refill_restores_both_equal_pools() {
        let mut vitality = DualVitality::equal(75);
        let _ = vitality.apply_damage(VitalityChannelId::One, 50);
        let _ = vitality.apply_damage(VitalityChannelId::Two, 75);
        vitality.refill();
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 75);
        assert_eq!(vitality.pool(VitalityChannelId::Two).current(), 75);
    }
}
