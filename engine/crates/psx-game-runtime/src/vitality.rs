//! Dual-pool vitality and continuous endpoint-power policy.
//!
//! Every vitality pool has two boost sockets: one at empty and one at full.
//! Their influence fades linearly toward the pool midpoint, where both are
//! dormant. Damage routing remains game-owned: typed attacks may select one
//! channel, while legacy untyped attacks can use [`DualVitality::apply_spill`].

/// Fixed-point unity used by vitality influence and player modifiers.
pub const VITALITY_Q12_ONE: u16 = 4096;

/// One of the actor's two equal-status vitality channels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VitalityChannelId {
    /// First vitality channel (Horizon in Cortex Ignition).
    One = 0,
    /// Second vitality channel (Zenith in Cortex Ignition).
    Two = 1,
}

impl VitalityChannelId {
    /// Dense channel index for fixed-size runtime tables.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The other vitality channel.
    pub const fn other(self) -> Self {
        match self {
            Self::One => Self::Two,
            Self::Two => Self::One,
        }
    }
}

/// One endpoint of a vitality channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VitalityPole {
    /// Zero-health, high-risk endpoint.
    Empty = 0,
    /// Full-health, stable endpoint.
    Full = 1,
}

/// Whether a vitality channel is exactly at an endpoint or between them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VitalityEdgeState {
    /// No vitality remains.
    Empty,
    /// The channel is neither empty nor full.
    Between,
    /// Vitality is at its maximum.
    Full,
}

/// Opposing empty/full influence weights for one vitality channel.
///
/// Both values are Q12. At zero health `empty_q12` is 4096; at half health
/// both are zero; at full health `full_q12` is 4096. The two sides never
/// overlap, keeping the midpoint a genuine no-upgrade neutral state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct VitalityPolarity {
    /// Empty-end influence, `0..=4096`.
    pub empty_q12: u16,
    /// Full-end influence, `0..=4096`.
    pub full_q12: u16,
}

impl VitalityPolarity {
    /// Influence at one endpoint.
    pub const fn at(self, pole: VitalityPole) -> u16 {
        match pole {
            VitalityPole::Empty => self.empty_q12,
            VitalityPole::Full => self.full_q12,
        }
    }
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

    /// Create a pool at an explicit current value.
    ///
    /// Runtimes that keep `current` in a dense state table and `maximum` in
    /// the cooked record (the entity SoA does exactly that) rehydrate a pool
    /// through here rather than duplicating the maximum per actor. A zero
    /// authored maximum normalizes exactly as [`Self::full`] does, and a
    /// current past the maximum clamps.
    pub const fn at(current: u16, maximum: u16) -> Self {
        let maximum = if maximum == 0 { 1 } else { maximum };
        let current = if current > maximum { maximum } else { current };
        Self { current, maximum }
    }

    /// Current vitality.
    pub const fn current(self) -> u16 {
        self.current
    }

    /// Maximum vitality.
    pub const fn maximum(self) -> u16 {
        self.maximum
    }

    /// Exact endpoint state, retained for endpoint-specific presentation.
    pub const fn edge_state(self) -> VitalityEdgeState {
        if self.current == 0 {
            VitalityEdgeState::Empty
        } else if self.current >= self.maximum {
            VitalityEdgeState::Full
        } else {
            VitalityEdgeState::Between
        }
    }

    /// Continuous empty/full influence around the neutral midpoint.
    pub fn polarity(self) -> VitalityPolarity {
        let maximum = u32::from(self.maximum.max(1));
        let twice_current = u32::from(self.current.min(self.maximum)).saturating_mul(2);
        if twice_current < maximum {
            VitalityPolarity {
                empty_q12: (((maximum - twice_current) * u32::from(VITALITY_Q12_ONE)) / maximum)
                    as u16,
                full_q12: 0,
            }
        } else if twice_current > maximum {
            VitalityPolarity {
                empty_q12: 0,
                full_q12: (((twice_current - maximum) * u32::from(VITALITY_Q12_ONE)) / maximum)
                    as u16,
            }
        } else {
            VitalityPolarity::default()
        }
    }

    fn apply_damage(&mut self, damage: u16) -> u16 {
        let before = self.current;
        self.current = self.current.saturating_sub(damage);
        before - self.current
    }

    fn refill(&mut self) {
        self.current = self.maximum;
    }

    fn heal(&mut self, amount: u16) -> u16 {
        let before = self.current;
        self.current = self.current.saturating_add(amount).min(self.maximum);
        self.current - before
    }

    fn empty(&mut self) {
        self.current = 0;
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

    /// Rehydrate both channels from separately stored pools, so a runtime
    /// holding its two currents in parallel arrays can borrow this type's
    /// damage/spill/defeat rules verbatim instead of restating them.
    pub const fn from_pools(first: VitalityPool, second: VitalityPool) -> Self {
        Self {
            pools: [first, second],
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
    /// Typed attacks use this path: excess damage does not cross into the
    /// other colour/channel.
    pub fn apply_damage(
        &mut self,
        channel: VitalityChannelId,
        damage: u16,
    ) -> DualVitalityDamageOutcome {
        let was_defeated = self.is_defeated();
        let pool = &mut self.pools[channel.index()];
        let was_alive = pool.current > 0;
        let damage_applied = pool.apply_damage(damage);
        let channel_depleted = damage_applied > 0 && was_alive && pool.current == 0;
        let current = pool.current;
        DualVitalityDamageOutcome {
            channel,
            current,
            damage_applied,
            channel_depleted,
            actor_defeated: !was_defeated && channel_depleted && self.is_defeated(),
        }
    }

    /// Apply legacy/untyped damage to `first`, spilling only the excess into
    /// the other pool. This gives old attacks a deterministic migration path
    /// while preserving typed attacks' channel isolation.
    pub fn apply_spill(
        &mut self,
        first: VitalityChannelId,
        damage: u16,
    ) -> DualVitalitySpillOutcome {
        let was_defeated = self.is_defeated();
        let first_before = self.pool(first).current();
        let first_outcome = self.apply_damage(first, damage);
        let remainder = damage.saturating_sub(first_before);
        let second = first.other();
        let second_outcome = self.apply_damage(second, remainder);
        DualVitalitySpillOutcome {
            first,
            first_current: first_outcome.current,
            second_current: second_outcome.current,
            damage_applied: first_outcome
                .damage_applied
                .saturating_add(second_outcome.damage_applied),
            actor_defeated: !was_defeated && self.is_defeated(),
        }
    }

    /// Refill both channels for a spawn or checkpoint respawn.
    pub fn refill(&mut self) {
        self.pools[0].refill();
        self.pools[1].refill();
    }

    /// Restore up to `amount` to one channel, returning what was actually
    /// restored. Regeneration needs a partial top-up; `refill` is all or
    /// nothing.
    pub fn heal(&mut self, channel: VitalityChannelId, amount: u16) -> u16 {
        self.pools[channel.index()].heal(amount)
    }

    /// Empty both channels for an unconditional environmental death.
    pub fn empty_all(&mut self) {
        self.pools[0].empty();
        self.pools[1].empty();
    }
}

/// Result of one channel-targeted damage application.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DualVitalityDamageOutcome {
    /// Channel that received the damage.
    pub channel: VitalityChannelId,
    /// Remaining vitality in that channel.
    pub current: u16,
    /// Damage consumed by this channel after saturation.
    pub damage_applied: u16,
    /// Whether this application newly emptied the selected channel.
    pub channel_depleted: bool,
    /// Whether this application newly emptied the actor's second remaining
    /// channel and should therefore arm one death/game-over sequence.
    pub actor_defeated: bool,
}

/// Result of one legacy damage application that may cross both channels.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct DualVitalitySpillOutcome {
    /// Channel consumed first.
    pub first: VitalityChannelId,
    /// Remaining vitality in the first channel.
    pub first_current: u16,
    /// Remaining vitality in the other channel.
    pub second_current: u16,
    /// Total damage consumed across both channels.
    pub damage_applied: u16,
    /// Whether both channels became empty in this application.
    pub actor_defeated: bool,
}

/// Dense id into the cooked unique module table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoostModuleId(pub u16);

impl BoostModuleId {
    /// No module assigned or selected.
    pub const NONE: Self = Self(u16::MAX);

    /// Construct a cooked module id when it fits the fixed runtime inventory.
    pub const fn from_index(index: usize) -> Self {
        if index < psx_level::MAX_BOOST_MODULES {
            Self(index as u16)
        } else {
            Self::NONE
        }
    }

    /// Dense table index, absent for [`Self::NONE`].
    pub const fn index(self) -> Option<usize> {
        if self.0 == u16::MAX {
            None
        } else {
            Some(self.0 as usize)
        }
    }

    /// Whether no item is represented.
    pub const fn is_none(self) -> bool {
        self.0 == u16::MAX
    }
}

impl Default for BoostModuleId {
    fn default() -> Self {
        Self::NONE
    }
}

/// One of the four assignment sockets around the mirrored dual-health module.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BoostSlotId {
    /// Horizon empty endpoint.
    HorizonEmpty = 0,
    /// Horizon full endpoint.
    HorizonFull = 1,
    /// Zenith empty endpoint.
    ZenithEmpty = 2,
    /// Zenith full endpoint.
    ZenithFull = 3,
}

impl BoostSlotId {
    /// All slots in runtime/menu order.
    pub const ALL: [Self; 4] = [
        Self::HorizonEmpty,
        Self::HorizonFull,
        Self::ZenithEmpty,
        Self::ZenithFull,
    ];

    /// Dense index into [`PowerUpLoadout`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Owning vitality channel.
    pub const fn channel(self) -> VitalityChannelId {
        match self {
            Self::HorizonEmpty | Self::HorizonFull => VitalityChannelId::One,
            Self::ZenithEmpty | Self::ZenithFull => VitalityChannelId::Two,
        }
    }

    /// Endpoint this socket radiates from.
    pub const fn pole(self) -> VitalityPole {
        match self {
            Self::HorizonEmpty | Self::ZenithEmpty => VitalityPole::Empty,
            Self::HorizonFull | Self::ZenithFull => VitalityPole::Full,
        }
    }

    /// Decode a dense menu selection index.
    pub const fn from_index(index: u8) -> Self {
        match index {
            1 => Self::HorizonFull,
            2 => Self::ZenithEmpty,
            3 => Self::ZenithFull,
            _ => Self::HorizonEmpty,
        }
    }
}

/// Four assignable power-up sockets, two around each vitality channel.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PowerUpLoadout {
    slots: [BoostModuleId; 4],
}

impl PowerUpLoadout {
    /// Empty loadout; also the all-zero boot representation.
    pub const EMPTY: Self = Self {
        slots: [BoostModuleId::NONE; 4],
    };

    /// Default player loadout. Collected protocols begin in the inventory and
    /// must be deliberately assigned to one of these four empty sockets.
    pub const DEFAULT: Self = Self::EMPTY;

    /// Unique module assigned to one socket.
    pub const fn module(self, slot: BoostSlotId) -> BoostModuleId {
        self.slots[slot.index()]
    }

    /// Replace one socket and return the module that was equipped there.
    pub fn set(&mut self, slot: BoostSlotId, module: BoostModuleId) -> BoostModuleId {
        let previous = self.slots[slot.index()];
        self.slots[slot.index()] = module;
        previous
    }

    /// Resolve the live fixed-point modifiers contributed by all four sockets.
    pub fn modifiers(
        self,
        vitality: &DualVitality,
        modules: &[psx_level::BoostModuleRecord],
    ) -> VitalityModifiers {
        self.modifiers_for(vitality, modules, None)
    }

    /// The same sum restricted to one channel's two sockets.
    ///
    /// Under the active-stance rules only the active state's boons act, so the
    /// inactive state's sockets are inert until it is swapped to. Passing
    /// `None` keeps the historical behaviour of summing all four.
    pub fn modifiers_for(
        self,
        vitality: &DualVitality,
        modules: &[psx_level::BoostModuleRecord],
        active: Option<VitalityChannelId>,
    ) -> VitalityModifiers {
        let mut bonuses_q12 = [0i32; psx_level::boost_stat::COUNT];
        for slot in BoostSlotId::ALL {
            if active.is_some_and(|channel| slot.channel() != channel) {
                continue;
            }
            let module_id = self.module(slot);
            let Some(module) = module_id.index().and_then(|index| modules.get(index)) else {
                continue;
            };
            for index in 0..psx_level::boost_stat::COUNT {
                bonuses_q12[index] = bonuses_q12[index]
                    .saturating_add(module_stat_bonus_q12(vitality, slot, module, index));
            }
        }

        let multiplier = |bonus: i32| {
            i32::from(VITALITY_Q12_ONE)
                .saturating_add(bonus)
                .clamp(i32::from(VITALITY_Q12_ONE / 10), i32::from(u16::MAX)) as u16
        };
        // Defence is direct percentage damage reduction. Positive stacking is
        // capped at 80%; negative trade-offs remain meaningful and increase
        // incoming damage.
        let defence = bonuses_q12[psx_level::boost_stat::DEFENCE]
            .clamp(-i32::from(VITALITY_Q12_ONE) * 3, 3277);
        VitalityModifiers {
            horizon_damage_q12: multiplier(bonuses_q12[psx_level::boost_stat::HORIZON_ATTACK]),
            zenith_damage_q12: multiplier(bonuses_q12[psx_level::boost_stat::ZENITH_ATTACK]),
            incoming_damage_q12: (i32::from(VITALITY_Q12_ONE) - defence)
                .clamp(i32::from(VITALITY_Q12_ONE / 5), i32::from(u16::MAX))
                as u16,
            movement_speed_q12: multiplier(bonuses_q12[psx_level::boost_stat::MOVEMENT_SPEED]),
            attack_speed_q12: multiplier(bonuses_q12[psx_level::boost_stat::ATTACK_SPEED]),
            // Regeneration is an additive rate, not a multiplier: a module
            // reading "+10" adds that much recovery rather than scaling a base
            // the player cannot see. Negative stacking floors at zero so a
            // trade-off can cancel recovery but never drain the pool.
            regeneration_q12: bonuses_q12[psx_level::boost_stat::REGENERATION]
                .clamp(0, i32::from(u16::MAX)) as u16,
        }
    }
}

/// One module's live signed percentage-point contribution in Q12 for a
/// particular vitality socket and stat lane. The inventory uses the same
/// calculation as gameplay so its `MODULE` column cannot disagree with the
/// damage, defence, movement, or timeline multipliers actually applied.
pub fn module_stat_bonus_q12(
    vitality: &DualVitality,
    slot: BoostSlotId,
    module: &psx_level::BoostModuleRecord,
    stat: usize,
) -> i32 {
    let Some(percent) = module.percentages.get(stat) else {
        return 0;
    };
    let influence = vitality.pool(slot.channel()).polarity().at(slot.pole());
    // Empty-end sockets are the risk/reward side: 200% at zero health,
    // fading linearly to zero at the midpoint. Full-end sockets reach 100%
    // only at full health and also fade to zero at the midpoint.
    let potency_q12 = match slot.pole() {
        VitalityPole::Empty => i32::from(influence).saturating_mul(2),
        VitalityPole::Full => i32::from(influence),
    };
    let percent_q12 = i32::from(*percent).saturating_mul(i32::from(VITALITY_Q12_ONE)) / 100;
    percent_q12.saturating_mul(potency_q12) / i32::from(VITALITY_Q12_ONE)
}

impl Default for PowerUpLoadout {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Allocation-free set of collected, currently unequipped unique modules.
///
/// Equipped copies live in [`PowerUpLoadout`]. Assignment transfers exactly
/// one copy between the stockpile and a socket, so repeated menu operations
/// cannot duplicate or silently discard an item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoostInventory {
    owned: u32,
}

impl BoostInventory {
    /// Empty stockpile; also the all-zero boot representation.
    pub const EMPTY: Self = Self { owned: 0 };

    /// New games begin without implicit items. Authored pickups are the only
    /// source of modules, so the inventory list reflects the actual world
    /// state instead of presenting protocol categories as starter boons.
    pub const STARTER: Self = Self::EMPTY;

    /// Whether the stockpile contains no unequipped module.
    pub const fn is_empty(self) -> bool {
        self.owned == 0
    }

    /// The `index`th collected item, compacting away unowned module ids.
    pub const fn item_at(self, index: u8) -> BoostModuleId {
        let mut found = 0u8;
        let mut module_index = 0usize;
        while module_index < psx_level::MAX_BOOST_MODULES {
            if self.owned & (1u32 << module_index) != 0 {
                if found == index {
                    return BoostModuleId::from_index(module_index);
                }
                found = found.saturating_add(1);
            }
            module_index += 1;
        }
        BoostModuleId::NONE
    }

    /// Whether one unique module is currently unequipped and available.
    pub const fn contains(self, module: BoostModuleId) -> bool {
        let Some(index) = module.index() else {
            return false;
        };
        self.owned & (1u32 << index) != 0
    }

    /// Add a unique collected module. Duplicate grants are rejected.
    pub fn add(&mut self, module: BoostModuleId) -> bool {
        let Some(index) = module.index() else {
            return false;
        };
        let bit = 1u32 << index;
        if self.owned & bit != 0 {
            return false;
        }
        self.owned |= bit;
        true
    }

    /// Move one unequipped unique module out of the inventory.
    pub fn take(&mut self, module: BoostModuleId) -> bool {
        let Some(index) = module.index() else {
            return false;
        };
        let bit = 1u32 << index;
        if self.owned & bit == 0 {
            return false;
        }
        self.owned &= !bit;
        true
    }

    /// Assign one collected copy to a socket and return its previous protocol
    /// to the stockpile. Selecting the already-equipped protocol is a no-op.
    /// Passing `None` unequips without requiring an inventory item.
    pub fn assign(
        &mut self,
        loadout: &mut PowerUpLoadout,
        slot: BoostSlotId,
        module: BoostModuleId,
    ) -> bool {
        let previous = loadout.module(slot);
        if previous == module {
            return true;
        }
        if !module.is_none() && !self.take(module) {
            return false;
        }
        let previous = loadout.set(slot, module);
        if !previous.is_none() {
            let returned = self.add(previous);
            debug_assert!(returned);
        }
        true
    }
}

impl Default for BoostInventory {
    fn default() -> Self {
        Self::EMPTY
    }
}

/// Live player-stat multipliers derived from vitality and assigned modules.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VitalityModifiers {
    /// Horizon attack damage multiplier (`4096 = 1.0x`).
    pub horizon_damage_q12: u16,
    /// Zenith attack damage multiplier (`4096 = 1.0x`).
    pub zenith_damage_q12: u16,
    /// Incoming health damage multiplier after defense (`4096 = 1.0x`).
    pub incoming_damage_q12: u16,
    /// Walk/run speed multiplier (`4096 = 1.0x`).
    pub movement_speed_q12: u16,
    /// Whole attack-timeline speed multiplier (`4096 = 1.0x`).
    pub attack_speed_q12: u16,
    /// Extra recovery for the inactive pool, Q12 per tick, from the active
    /// state's Regeneration modules. Added to the authored base rate.
    pub regeneration_q12: u16,
}

impl VitalityModifiers {
    /// No-op modifiers at the neutral midpoint or with no assigned modules.
    pub const IDENTITY: Self = Self {
        horizon_damage_q12: VITALITY_Q12_ONE,
        zenith_damage_q12: VITALITY_Q12_ONE,
        incoming_damage_q12: VITALITY_Q12_ONE,
        movement_speed_q12: VITALITY_Q12_ONE,
        attack_speed_q12: VITALITY_Q12_ONE,
        // Additive, so identity is no extra recovery rather than unity.
        regeneration_q12: 0,
    };

    /// Scale outgoing damage for the authored attack axis.
    pub fn outgoing_damage(self, channel: VitalityChannelId, damage: u16) -> u16 {
        let multiplier = match channel {
            VitalityChannelId::One => self.horizon_damage_q12,
            VitalityChannelId::Two => self.zenith_damage_q12,
        };
        scale_u16_q12(damage, multiplier)
    }

    /// Scale incoming damage. A non-zero connecting hit always deals at least
    /// one point, even after fixed-point rounding.
    pub fn incoming_damage(self, damage: u16) -> u16 {
        if damage == 0 {
            0
        } else {
            scale_u16_q12(damage, self.incoming_damage_q12).max(1)
        }
    }

    /// Scale a signed movement speed, rounding to the nearest integer.
    pub fn movement_speed(self, speed: i32) -> i32 {
        scale_i32_q12(speed, self.movement_speed_q12)
    }

    /// Apply the live attack-speed multiplier to an authored Q8 playback rate.
    pub fn attack_speed_q8(self, speed_q8: u16) -> u16 {
        scale_u16_q12(speed_q8, self.attack_speed_q12).max(1)
    }
}

fn scale_u16_q12(value: u16, multiplier_q12: u16) -> u16 {
    let scaled = (u32::from(value) * u32::from(multiplier_q12) + u32::from(VITALITY_Q12_ONE / 2))
        / u32::from(VITALITY_Q12_ONE);
    scaled.min(u32::from(u16::MAX)) as u16
}

fn scale_i32_q12(value: i32, multiplier_q12: u16) -> i32 {
    let scaled = i64::from(value) * i64::from(multiplier_q12);
    let rounding = i64::from(VITALITY_Q12_ONE / 2) * i64::from(value.signum());
    ((scaled + rounding) / i64::from(VITALITY_Q12_ONE))
        .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODULES: [psx_level::BoostModuleRecord; 3] = [
        psx_level::BoostModuleRecord {
            name: "Kinetic Relay",
            description: "Horizon output.",
            effect_summary: "HRZ ATK +15%",
            assignment_label: "ASSIGN KINETIC RELAY: CHOOSE SLOT",
            remove_label: "REMOVE KINETIC RELAY",
            percentages: [15, 0, 0, 0, 0, 0],
        },
        psx_level::BoostModuleRecord {
            name: "Guard Matrix",
            description: "Damage guard.",
            effect_summary: "DEF +20%",
            assignment_label: "ASSIGN GUARD MATRIX: CHOOSE SLOT",
            remove_label: "REMOVE GUARD MATRIX",
            percentages: [0, 0, 20, 0, 0, 0],
        },
        psx_level::BoostModuleRecord {
            name: "Overdrive Coil",
            description: "Faster movement and attacks.",
            effect_summary: "MOVE +10% / ATK SPD +5%",
            assignment_label: "ASSIGN OVERDRIVE COIL: CHOOSE SLOT",
            remove_label: "REMOVE OVERDRIVE COIL",
            percentages: [0, 0, 0, 10, 5, 0],
        },
    ];

    /// A single hit larger than BOTH pools must defeat the actor, and the
    /// spill remainder must be computed in a width that can hold it. This is
    /// the case a narrowing subtraction silently gets wrong: `damage` is u16
    /// and so is the pool, so `damage - first_pool` looks safe right up until
    /// someone changes a type.
    #[test]
    fn one_hit_larger_than_both_pools_defeats_the_actor() {
        for damage in [u16::MAX, u16::MAX - 1, 60_000, 201, 200] {
            let mut vitality = DualVitality::equal(100);
            let outcome = vitality.apply_spill(VitalityChannelId::One, damage);
            assert!(outcome.actor_defeated, "damage {damage} must defeat");
            assert!(vitality.is_defeated(), "damage {damage}");
            assert_eq!(outcome.first_current, 0);
            assert_eq!(outcome.second_current, 0);
            assert_eq!(outcome.damage_applied, 200, "damage {damage}");
        }
    }

    /// Exactly one short leaves one point in the SECOND pool, alive.
    #[test]
    fn one_short_of_both_pools_leaves_the_actor_alive() {
        let mut vitality = DualVitality::equal(100);
        let outcome = vitality.apply_spill(VitalityChannelId::One, 199);
        assert!(!outcome.actor_defeated);
        assert_eq!((outcome.first_current, outcome.second_current), (0, 1));
        assert_eq!(outcome.damage_applied, 199);
        assert!(
            vitality
                .apply_spill(VitalityChannelId::Two, 1)
                .actor_defeated
        );
    }

    #[test]
    fn polarity_is_full_at_endpoints_and_zero_at_midpoint() {
        let mut vitality = DualVitality::equal(100);
        assert_eq!(
            vitality.pool(VitalityChannelId::One).polarity(),
            VitalityPolarity {
                empty_q12: 0,
                full_q12: 4096,
            }
        );

        let _ = vitality.apply_damage(VitalityChannelId::One, 25);
        assert_eq!(
            vitality.pool(VitalityChannelId::One).polarity(),
            VitalityPolarity {
                empty_q12: 0,
                full_q12: 2048,
            }
        );
        let _ = vitality.apply_damage(VitalityChannelId::One, 25);
        assert_eq!(
            vitality.pool(VitalityChannelId::One).polarity(),
            VitalityPolarity::default()
        );
        let _ = vitality.apply_damage(VitalityChannelId::One, 25);
        assert_eq!(
            vitality.pool(VitalityChannelId::One).polarity(),
            VitalityPolarity {
                empty_q12: 2048,
                full_q12: 0,
            }
        );
        let _ = vitality.apply_damage(VitalityChannelId::One, 25);
        assert_eq!(
            vitality.pool(VitalityChannelId::One).polarity(),
            VitalityPolarity {
                empty_q12: 4096,
                full_q12: 0,
            }
        );
    }

    #[test]
    fn defeat_requires_both_channels_to_be_empty() {
        let mut vitality = DualVitality::equal(100);
        let first = vitality.apply_damage(VitalityChannelId::One, 100);
        assert!(first.channel_depleted);
        assert!(!first.actor_defeated);
        assert!(!vitality.is_defeated());

        let second = vitality.apply_damage(VitalityChannelId::Two, 100);
        assert!(second.channel_depleted);
        assert!(second.actor_defeated);
        assert!(vitality.is_defeated());
    }

    #[test]
    fn legacy_damage_spills_in_order_and_only_defeats_after_both() {
        let mut vitality = DualVitality::equal(100);
        let first = vitality.apply_spill(VitalityChannelId::One, 130);
        assert_eq!(first.first_current, 0);
        assert_eq!(first.second_current, 70);
        assert!(!first.actor_defeated);

        let lethal = vitality.apply_spill(VitalityChannelId::One, 80);
        assert_eq!(lethal.second_current, 0);
        assert!(lethal.actor_defeated);
    }

    #[test]
    fn power_ups_fade_to_identity_at_the_midpoint() {
        let loadout = PowerUpLoadout {
            slots: [
                BoostModuleId(0),
                BoostModuleId(1),
                BoostModuleId(2),
                BoostModuleId::NONE,
            ],
        };
        let mut vitality = DualVitality::equal(100);
        let full = loadout.modifiers(&vitality, &MODULES);
        assert!(full.incoming_damage_q12 < VITALITY_Q12_ONE);

        let _ = vitality.apply_damage(VitalityChannelId::One, 50);
        let _ = vitality.apply_damage(VitalityChannelId::Two, 50);
        assert_eq!(
            loadout.modifiers(&vitality, &MODULES),
            VitalityModifiers::IDENTITY
        );
    }

    #[test]
    fn empty_socket_reaches_twice_the_full_socket_percentage() {
        let mut vitality = DualVitality::equal(100);
        let empty_kinetic = PowerUpLoadout {
            slots: [
                BoostModuleId(0),
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId::NONE,
            ],
        };
        let full_kinetic = PowerUpLoadout {
            slots: [
                BoostModuleId::NONE,
                BoostModuleId(0),
                BoostModuleId::NONE,
                BoostModuleId::NONE,
            ],
        };
        let full_bonus = full_kinetic
            .modifiers(&vitality, &MODULES)
            .horizon_damage_q12;
        let _ = vitality.apply_damage(VitalityChannelId::One, 100);
        let empty_bonus = empty_kinetic
            .modifiers(&vitality, &MODULES)
            .horizon_damage_q12;
        assert_eq!(
            empty_bonus - VITALITY_Q12_ONE,
            (full_bonus - VITALITY_Q12_ONE) * 2
        );
    }

    #[test]
    fn module_effect_is_global_while_owning_bar_drives_its_strength() {
        let loadout = PowerUpLoadout {
            slots: [
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId(0),
            ],
        };
        let vitality = DualVitality::equal(100);
        let modifiers = loadout.modifiers(&vitality, &MODULES);
        assert!(modifiers.horizon_damage_q12 > VITALITY_Q12_ONE);
        assert_eq!(modifiers.zenith_damage_q12, VITALITY_Q12_ONE);
    }

    #[test]
    fn defence_reduction_caps_at_eighty_percent() {
        const ARMOUR: [psx_level::BoostModuleRecord; 1] = [psx_level::BoostModuleRecord {
            name: "Armour",
            description: "",
            effect_summary: "DEF +100%",
            assignment_label: "",
            remove_label: "",
            percentages: [0, 0, 100, 0, 0, 0],
        }];
        let loadout = PowerUpLoadout {
            slots: [
                BoostModuleId(0),
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId::NONE,
            ],
        };
        let mut vitality = DualVitality::equal(100);
        let _ = vitality.apply_damage(VitalityChannelId::One, 100);
        let modifiers = loadout.modifiers(&vitality, &ARMOUR);
        assert_eq!(modifiers.incoming_damage_q12, VITALITY_Q12_ONE / 5);
    }

    #[test]
    fn collected_inventory_item_assigns_its_only_copy_to_one_empty_socket() {
        let mut inventory = BoostInventory::EMPTY;
        let mut loadout = PowerUpLoadout::DEFAULT;
        let kinetic = BoostModuleId(0);

        assert!(inventory.is_empty());
        assert_eq!(inventory.item_at(0), BoostModuleId::NONE);
        assert!(inventory.add(kinetic));

        assert!(inventory.contains(kinetic));
        assert!(!inventory.add(kinetic));
        for slot in BoostSlotId::ALL {
            assert_eq!(loadout.module(slot), BoostModuleId::NONE);
        }
        assert!(inventory.assign(&mut loadout, BoostSlotId::ZenithFull, kinetic,));

        assert_eq!(loadout.module(BoostSlotId::ZenithFull), kinetic);
        assert!(!inventory.contains(kinetic));
        assert!(inventory.is_empty());
        for slot in [
            BoostSlotId::HorizonEmpty,
            BoostSlotId::HorizonFull,
            BoostSlotId::ZenithEmpty,
        ] {
            assert_eq!(loadout.module(slot), BoostModuleId::NONE);
        }
    }

    #[test]
    fn item_list_compacts_away_unowned_module_ids() {
        let mut inventory = BoostInventory::EMPTY;
        assert_eq!(inventory.item_at(0), BoostModuleId::NONE);

        assert!(inventory.add(BoostModuleId(1)));
        assert!(inventory.add(BoostModuleId(2)));
        assert_eq!(inventory.item_at(0), BoostModuleId(1));
        assert_eq!(inventory.item_at(1), BoostModuleId(2));
        assert_eq!(inventory.item_at(2), BoostModuleId::NONE);
    }

    #[test]
    fn assigning_none_returns_the_socketed_item_to_inventory() {
        let mut inventory = BoostInventory::EMPTY;
        let mut loadout = PowerUpLoadout::DEFAULT;
        let guard = BoostModuleId(1);
        assert!(inventory.add(guard));
        assert!(inventory.assign(&mut loadout, BoostSlotId::HorizonFull, guard,));
        assert!(!inventory.contains(guard));

        assert!(inventory.assign(&mut loadout, BoostSlotId::HorizonFull, BoostModuleId::NONE,));
        assert_eq!(
            loadout.module(BoostSlotId::HorizonFull),
            BoostModuleId::NONE
        );
        assert!(inventory.contains(guard));
    }

    #[test]
    fn inventory_assignment_is_atomic_when_stock_is_empty() {
        let mut inventory = BoostInventory::EMPTY;
        let mut loadout = PowerUpLoadout::DEFAULT;
        let before = loadout;

        assert!(!inventory.assign(&mut loadout, BoostSlotId::HorizonEmpty, BoostModuleId(2),));
        assert_eq!(loadout, before);
        assert_eq!(inventory, BoostInventory::EMPTY);
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

/// Authored numbers governing the active/inactive vitality stance.
///
/// Every value is exposed rather than baked so the feel can be tuned without a
/// rebuild. Q12 fields are multipliers where 4096 is unity.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CombatStanceConfig {
    /// Damage taken when the incoming attack matches the active channel.
    pub aligned_damage_q12: u16,
    /// Damage taken when it does not.
    pub opposed_damage_q12: u16,
    /// Ticks after taking damage before the inactive pool starts recovering.
    pub regen_delay_ticks: u16,
    /// Ticks before a pool that reached zero starts recovering. Longer than
    /// [`Self::regen_delay_ticks`]: breaking a state is meant to cost.
    pub broken_regen_delay_ticks: u16,
    /// Health restored per tick, Q12, so a rate below one per tick is
    /// expressible without a separate accumulator in the caller.
    pub regen_per_tick_q12: u16,
    /// Fraction of maximum a broken pool must reach to be selectable again.
    pub break_threshold_q12: u16,
    /// Ticks before another voluntary swap is allowed.
    pub swap_cooldown_ticks: u16,
    /// Ticks the HUD spends animating a swap. Presentation only.
    pub swap_duration_ticks: u16,
}

impl CombatStanceConfig {
    /// Starting point for authoring: 50% aligned, 150% opposed.
    pub const DEFAULT: Self = Self {
        aligned_damage_q12: VITALITY_Q12_ONE / 2,
        opposed_damage_q12: VITALITY_Q12_ONE + VITALITY_Q12_ONE / 2,
        regen_delay_ticks: 90,
        broken_regen_delay_ticks: 300,
        regen_per_tick_q12: VITALITY_Q12_ONE / 4,
        break_threshold_q12: VITALITY_Q12_ONE / 4,
        swap_cooldown_ticks: 30,
        swap_duration_ticks: 12,
    };
}

impl Default for CombatStanceConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Why a swap happened, so presentation and cooldown can differ.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StanceSwap {
    /// The player pressed swap and it was allowed.
    Voluntary,
    /// The active pool broke, so the stance had nowhere to stay.
    Forced,
}

/// Result of one damage event routed through the stance.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct StanceDamageOutcome {
    /// Damage actually removed, after alignment scaling.
    pub damage_applied: u16,
    /// The active pool reached zero on this event.
    pub broke: bool,
    /// The break forced a swap to the other channel.
    pub forced_swap: bool,
    /// Both pools are empty.
    pub defeated: bool,
}

/// Which vitality channel is active, and the timers around swapping.
///
/// Only the active pool takes damage; only the inactive pool recovers. Swapping
/// is therefore the healing mechanic, and the cooldown is what stops it being
/// free.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CombatStance {
    active: VitalityChannelId,
    swap_cooldown: u16,
    swap_elapsed: u16,
    regen_delay: [u16; 2],
    broken: [bool; 2],
    regen_fraction_q12: [u16; 2],
}

impl CombatStance {
    /// Start in `active` with both pools whole and no timers running.
    pub const fn new(active: VitalityChannelId) -> Self {
        Self {
            active,
            swap_cooldown: 0,
            swap_elapsed: 0,
            regen_delay: [0; 2],
            broken: [false; 2],
            regen_fraction_q12: [0; 2],
        }
    }

    /// The channel taking damage and using its boons.
    pub const fn active(self) -> VitalityChannelId {
        self.active
    }

    /// The channel recovering.
    pub const fn inactive(self) -> VitalityChannelId {
        self.active.other()
    }

    /// Whether a channel is broken and unselectable.
    pub const fn is_broken(self, channel: VitalityChannelId) -> bool {
        self.broken[channel.index()]
    }

    /// Ticks left before another voluntary swap.
    pub const fn swap_cooldown(self) -> u16 {
        self.swap_cooldown
    }

    /// How far through the swap animation the HUD is, Q12.
    pub fn swap_progress_q12(self, config: &CombatStanceConfig) -> u16 {
        if config.swap_duration_ticks == 0 || self.swap_elapsed >= config.swap_duration_ticks {
            return VITALITY_Q12_ONE;
        }
        ((u32::from(self.swap_elapsed) * u32::from(VITALITY_Q12_ONE))
            / u32::from(config.swap_duration_ticks)) as u16
    }

    /// Whether the stance-change presentation window is still active.
    ///
    /// [`Self::swap_progress_q12`] intentionally saturates at one so UI
    /// bindings can consume it directly; render effects need this separate
    /// predicate to avoid tinting the actor forever after the first swap.
    pub const fn swap_in_progress(self, config: &CombatStanceConfig) -> bool {
        config.swap_duration_ticks > 0 && self.swap_elapsed < config.swap_duration_ticks
    }

    /// Whether the player may swap right now.
    pub const fn can_swap(self) -> bool {
        self.swap_cooldown == 0 && !self.broken[self.inactive().index()]
    }

    /// Attempt a voluntary swap.
    pub fn request_swap(&mut self, config: &CombatStanceConfig) -> Option<StanceSwap> {
        if !self.can_swap() {
            return None;
        }
        self.swap_to(self.inactive(), config);
        Some(StanceSwap::Voluntary)
    }

    fn swap_to(&mut self, channel: VitalityChannelId, config: &CombatStanceConfig) {
        self.active = channel;
        self.swap_cooldown = config.swap_cooldown_ticks;
        self.swap_elapsed = 0;
    }

    /// Route one incoming attack. Only the active pool is damaged; the
    /// multiplier is chosen by whether the attack's channel matches it.
    pub fn apply_damage(
        &mut self,
        vitality: &mut DualVitality,
        attack: VitalityChannelId,
        damage: u16,
        config: &CombatStanceConfig,
    ) -> StanceDamageOutcome {
        let scale = if attack == self.active {
            config.aligned_damage_q12
        } else {
            config.opposed_damage_q12
        };
        let scaled = ((u32::from(damage) * u32::from(scale)) / u32::from(VITALITY_Q12_ONE))
            .min(u32::from(u16::MAX)) as u16;
        let outcome = vitality.apply_damage(self.active, scaled);

        let active = self.active.index();
        if outcome.damage_applied > 0 {
            self.regen_delay[active] = config.regen_delay_ticks;
            self.regen_fraction_q12[active] = 0;
        }

        let mut result = StanceDamageOutcome {
            damage_applied: outcome.damage_applied,
            broke: outcome.channel_depleted,
            forced_swap: false,
            defeated: vitality.is_defeated(),
        };
        if outcome.channel_depleted {
            self.broken[active] = true;
            // A broken pool cannot stay active, so the swap ignores the
            // cooldown: refusing it would leave the player with no selectable
            // state at all. It still costs the longer recovery delay.
            self.regen_delay[active] = config.broken_regen_delay_ticks;
            if !result.defeated {
                self.swap_to(self.inactive(), config);
                result.forced_swap = true;
            }
        }
        result
    }

    /// Advance one fixed tick: cooldowns, then recovery on the inactive pool.
    ///
    /// `regen_bonus_q12` is the Regeneration boon's contribution, which comes
    /// from the *active* state's boons and heals the inactive pool.
    pub fn tick(
        &mut self,
        vitality: &mut DualVitality,
        config: &CombatStanceConfig,
        regen_bonus_q12: u16,
    ) {
        self.swap_cooldown = self.swap_cooldown.saturating_sub(1);
        self.swap_elapsed = self.swap_elapsed.saturating_add(1);

        let inactive = self.inactive();
        let index = inactive.index();
        if self.regen_delay[index] > 0 {
            self.regen_delay[index] -= 1;
            return;
        }

        let pool = vitality.pool(inactive);
        if pool.current() >= pool.maximum() {
            return;
        }
        let rate = config.regen_per_tick_q12.saturating_add(regen_bonus_q12);
        let carried = self.regen_fraction_q12[index].saturating_add(rate);
        let whole = carried / VITALITY_Q12_ONE;
        self.regen_fraction_q12[index] = carried % VITALITY_Q12_ONE;
        if whole > 0 {
            vitality.heal(inactive, whole);
        }

        // Selectable again once recovery passes the authored threshold.
        if self.broken[index] {
            let pool = vitality.pool(inactive);
            let threshold = ((u32::from(pool.maximum()) * u32::from(config.break_threshold_q12))
                / u32::from(VITALITY_Q12_ONE)) as u16;
            if pool.current() >= threshold.max(1) {
                self.broken[index] = false;
            }
        }
    }
}

#[cfg(test)]
mod stance_tests {
    use super::*;

    fn config() -> CombatStanceConfig {
        CombatStanceConfig {
            regen_delay_ticks: 2,
            broken_regen_delay_ticks: 5,
            regen_per_tick_q12: VITALITY_Q12_ONE,
            break_threshold_q12: VITALITY_Q12_ONE / 2,
            swap_cooldown_ticks: 3,
            swap_duration_ticks: 4,
            ..CombatStanceConfig::DEFAULT
        }
    }

    #[test]
    fn only_the_active_pool_takes_damage() {
        let mut vitality = DualVitality::equal(100);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        stance.apply_damage(&mut vitality, VitalityChannelId::One, 20, &config());
        assert_eq!(
            vitality.pool(VitalityChannelId::Two).current(),
            100,
            "the inactive pool is never touched by an attack"
        );
    }

    #[test]
    fn alignment_halves_and_mismatch_multiplies() {
        let config = config();
        let mut aligned = DualVitality::equal(200);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        // Attack on the active channel: 50%.
        let hit = stance.apply_damage(&mut aligned, VitalityChannelId::One, 40, &config);
        assert_eq!(hit.damage_applied, 20);

        let mut opposed = DualVitality::equal(200);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        // Attack on the other channel while One is active: 150%.
        let hit = stance.apply_damage(&mut opposed, VitalityChannelId::Two, 40, &config);
        assert_eq!(hit.damage_applied, 60);
        assert_eq!(
            opposed.pool(VitalityChannelId::One).current(),
            140,
            "the opposed hit still lands on the active pool, only harder"
        );
    }

    #[test]
    fn breaking_the_active_pool_forces_a_swap_through_the_cooldown() {
        let config = config();
        let mut vitality = DualVitality::equal(30);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        // Put a cooldown on the clock so the forced swap has one to ignore.
        stance.request_swap(&config);
        stance.request_swap(&config);
        assert!(!stance.can_swap(), "a voluntary swap is on cooldown");

        let active = stance.active();
        let hit = stance.apply_damage(&mut vitality, active, 1000, &config);
        assert!(hit.broke);
        assert!(hit.forced_swap, "a broken pool cannot stay active");
        assert!(!hit.defeated, "the other pool is still whole");
        assert_eq!(stance.active(), active.other());
        assert!(stance.is_broken(active));
        assert!(
            !stance.can_swap(),
            "swapping back into a broken pool is refused"
        );
    }

    #[test]
    fn a_broken_pool_waits_longer_then_becomes_selectable_at_the_threshold() {
        let config = config();
        let mut vitality = DualVitality::equal(10);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        stance.apply_damage(&mut vitality, VitalityChannelId::One, 1000, &config);
        assert!(stance.is_broken(VitalityChannelId::One));

        // The longer delay runs before a single point comes back.
        for _ in 0..config.broken_regen_delay_ticks {
            stance.tick(&mut vitality, &config, 0);
            assert_eq!(vitality.pool(VitalityChannelId::One).current(), 0);
        }
        // Then one point per tick. The threshold is half of ten.
        for _ in 0..4 {
            stance.tick(&mut vitality, &config, 0);
        }
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 4);
        assert!(stance.is_broken(VitalityChannelId::One), "still under half");
        stance.tick(&mut vitality, &config, 0);
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 5);
        assert!(
            !stance.is_broken(VitalityChannelId::One),
            "reaching the threshold makes it selectable again"
        );
    }

    #[test]
    fn only_the_inactive_pool_regenerates_and_only_after_the_delay() {
        let config = config();
        let mut vitality = DualVitality::equal(100);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        // Damage the inactive pool by swapping, taking a hit, swapping back.
        stance.apply_damage(&mut vitality, VitalityChannelId::One, 40, &config);
        let hurt = vitality.pool(VitalityChannelId::One).current();
        stance.request_swap(&config);

        // The delay set by that damage still has to run out.
        for _ in 0..config.regen_delay_ticks {
            stance.tick(&mut vitality, &config, 0);
            assert_eq!(vitality.pool(VitalityChannelId::One).current(), hurt);
        }
        stance.tick(&mut vitality, &config, 0);
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), hurt + 1);
        assert_eq!(
            vitality.pool(VitalityChannelId::Two).current(),
            100,
            "the active pool never regenerates"
        );
    }

    #[test]
    fn the_regeneration_boon_speeds_the_inactive_pool() {
        let config = config();
        let mut plain =
            DualVitality::from_pools(VitalityPool::at(50, 100), VitalityPool::full(100));
        let mut boosted =
            DualVitality::from_pools(VitalityPool::at(50, 100), VitalityPool::full(100));
        let mut a = CombatStance::new(VitalityChannelId::Two);
        let mut b = CombatStance::new(VitalityChannelId::Two);
        for _ in 0..4 {
            a.tick(&mut plain, &config, 0);
            b.tick(&mut boosted, &config, VITALITY_Q12_ONE);
        }
        assert_eq!(plain.pool(VitalityChannelId::One).current(), 54);
        assert_eq!(
            boosted.pool(VitalityChannelId::One).current(),
            58,
            "the boon doubles the authored rate here"
        );
    }

    #[test]
    fn a_fractional_rate_accumulates_instead_of_rounding_to_nothing() {
        let mut config = config();
        // A quarter point per tick must still heal, four ticks at a time.
        config.regen_per_tick_q12 = VITALITY_Q12_ONE / 4;
        config.regen_delay_ticks = 0;
        let mut vitality =
            DualVitality::from_pools(VitalityPool::at(50, 100), VitalityPool::full(100));
        let mut stance = CombatStance::new(VitalityChannelId::Two);
        for _ in 0..3 {
            stance.tick(&mut vitality, &config, 0);
        }
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 50);
        stance.tick(&mut vitality, &config, 0);
        assert_eq!(vitality.pool(VitalityChannelId::One).current(), 51);
    }

    #[test]
    fn death_needs_both_pools_empty() {
        let config = config();
        let mut vitality = DualVitality::equal(10);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        let first = stance.apply_damage(&mut vitality, VitalityChannelId::One, 1000, &config);
        assert!(!first.defeated, "one broken pool is not death");
        let second = stance.apply_damage(&mut vitality, stance.active(), 1000, &config);
        assert!(second.defeated);
        assert!(
            !second.forced_swap,
            "there is nowhere to swap to once both are gone"
        );
    }

    #[test]
    fn only_the_active_states_sockets_contribute() {
        // Two sockets sit on each channel. Under the stance rules the inactive
        // state's boons are inert, so a per-channel sum must not reach across.
        for channel in [VitalityChannelId::One, VitalityChannelId::Two] {
            let mine = BoostSlotId::ALL
                .into_iter()
                .filter(|slot| slot.channel() == channel)
                .count();
            assert_eq!(mine, 2, "each state owns exactly two boon slots");
        }
        for slot in BoostSlotId::ALL {
            assert_ne!(
                slot.channel(),
                slot.channel().other(),
                "a socket belongs to one state only"
            );
        }
    }

    #[test]
    fn a_regeneration_module_on_the_active_state_speeds_the_resting_pool() {
        // The lane is additive, not a multiplier: "+10" adds recovery rather
        // than scaling a base the player never sees.
        let modules = [psx_level::BoostModuleRecord {
            name: "Mending Coil",
            description: "Rest recovery.",
            effect_summary: "REGEN +10",
            assignment_label: "ASSIGN MENDING COIL: CHOOSE SLOT",
            remove_label: "REMOVE MENDING COIL",
            percentages: [0, 0, 0, 0, 0, 10],
        }];
        let vitality = DualVitality::equal(100);

        let mut loadout = PowerUpLoadout::EMPTY;
        // Socket it on channel One's full-end pole: an empty-end socket is
        // weighted by how hurt the pool is and contributes nothing at full
        // health, which would prove nothing here.
        let slot = BoostSlotId::ALL
            .into_iter()
            .find(|slot| {
                slot.channel() == VitalityChannelId::One && slot.pole() == VitalityPole::Full
            })
            .expect("channel one owns a full-end socket");
        loadout.set(slot, BoostModuleId::from_index(0));

        let active_one = loadout.modifiers_for(&vitality, &modules, Some(VitalityChannelId::One));
        let active_two = loadout.modifiers_for(&vitality, &modules, Some(VitalityChannelId::Two));
        assert!(
            active_one.regeneration_q12 > 0,
            "the state holding the module contributes while active"
        );
        assert_eq!(
            active_two.regeneration_q12, 0,
            "the other state's sockets are dormant"
        );
    }

    #[test]
    fn the_swap_animation_reports_progress_and_completes() {
        let config = config();
        let mut vitality = DualVitality::equal(100);
        let mut stance = CombatStance::new(VitalityChannelId::One);
        stance.request_swap(&config);
        assert!(stance.swap_in_progress(&config));
        assert_eq!(stance.swap_progress_q12(&config), 0);
        stance.tick(&mut vitality, &config, 0);
        stance.tick(&mut vitality, &config, 0);
        assert_eq!(stance.swap_progress_q12(&config), VITALITY_Q12_ONE / 2);
        for _ in 0..4 {
            stance.tick(&mut vitality, &config, 0);
        }
        assert_eq!(stance.swap_progress_q12(&config), VITALITY_Q12_ONE);
        assert!(!stance.swap_in_progress(&config));
    }
}
