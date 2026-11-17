//! The health bar, the boost sockets on the health and energy bars, and the
//! stance.
//!
//! Every fighter has one health bar ([`VitalityPool`]). Damage empties it and
//! nothing refills it during play: only a respawn, an arch or a checkpoint
//! restores it. The stance ([`CombatStance`]) is which of the two sides,
//! blade (Horizon) or Zenith, a fighter fights from; it no longer owns any
//! health.
//!
//! Every bar has two boost sockets, one at empty and one at full. Their
//! influence fades linearly toward the bar's midpoint, where both are
//! dormant. Two sockets sit on the health bar and two on the energy bar.

/// Fixed-point unity used by vitality influence and player modifiers.
pub const VITALITY_Q12_ONE: u16 = 4096;

/// One of the two stances, and the channel an attack comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VitalityChannelId {
    /// The blade stance (Horizon in Cortex Ignition).
    One = 0,
    /// The Zenith stance.
    Two = 1,
}

impl VitalityChannelId {
    /// Dense channel index for fixed-size runtime tables.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The other channel.
    pub const fn other(self) -> Self {
        match self {
            Self::One => Self::Two,
            Self::Two => Self::One,
        }
    }
}

/// One endpoint of a bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VitalityPole {
    /// Empty, high-risk endpoint.
    Empty = 0,
    /// Full, stable endpoint.
    Full = 1,
}

/// Whether a bar is exactly at an endpoint or between them.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VitalityEdgeState {
    /// Nothing remains.
    Empty,
    /// The bar is neither empty nor full.
    Between,
    /// The bar is at its maximum.
    Full,
}

/// One bounded bar: a fighter's health, or a view of its energy for the
/// boost sockets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VitalityPool {
    current: u16,
    maximum: u16,
}

impl VitalityPool {
    /// Create a full bar. A zero authored maximum is normalized to one so all
    /// ratio consumers keep a valid denominator.
    pub const fn full(maximum: u16) -> Self {
        let maximum = if maximum == 0 { 1 } else { maximum };
        Self {
            current: maximum,
            maximum,
        }
    }

    /// Create a bar at an explicit current value. A zero maximum normalizes
    /// exactly as [`Self::full`] does, and a current past the maximum clamps.
    pub const fn at(current: u16, maximum: u16) -> Self {
        let maximum = if maximum == 0 { 1 } else { maximum };
        let current = if current > maximum { maximum } else { current };
        Self { current, maximum }
    }

    /// Current value.
    pub const fn current(self) -> u16 {
        self.current
    }

    /// Maximum value.
    pub const fn maximum(self) -> u16 {
        self.maximum
    }

    /// Whether nothing remains: for health, the fighter is defeated.
    pub const fn is_empty(self) -> bool {
        self.current == 0
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

    /// Influence of one endpoint's socket, Q12: 4096 exactly at that
    /// endpoint, fading linearly to zero at the midpoint and staying zero
    /// past it. The two endpoints never overlap, which keeps the midpoint a
    /// genuine no-upgrade neutral state.
    pub fn influence_q12(self, pole: VitalityPole) -> u16 {
        let maximum = u32::from(self.maximum.max(1));
        let twice_current = u32::from(self.current.min(self.maximum)).saturating_mul(2);
        let distance = match pole {
            VitalityPole::Empty => maximum.saturating_sub(twice_current),
            VitalityPole::Full => twice_current.saturating_sub(maximum),
        };
        ((distance * u32::from(VITALITY_Q12_ONE)) / maximum) as u16
    }

    /// Remove up to `damage`, returning what was actually removed.
    pub fn apply_damage(&mut self, damage: u16) -> u16 {
        let before = self.current;
        self.current = self.current.saturating_sub(damage);
        before - self.current
    }

    /// Fill to the maximum.
    pub fn refill(&mut self) {
        self.current = self.maximum;
    }

    /// Restore up to `amount`, returning what was actually restored.
    pub fn heal(&mut self, amount: u16) -> u16 {
        let before = self.current;
        self.current = self.current.saturating_add(amount).min(self.maximum);
        self.current - before
    }

    /// Empty the bar, for an unconditional environmental death.
    pub fn empty(&mut self) {
        self.current = 0;
    }
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

/// The two bars a fighter's boost sockets sit on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BoostBar {
    /// The health bar.
    Health,
    /// The energy bar.
    Energy,
}

/// One of the four assignment sockets, two on each bar's endpoints.
///
/// The health sockets are the ones that sat on the Horizon pool and the
/// energy sockets the ones that sat on the Zenith pool, so a loadout keeps
/// its meaning across the change to one health bar. All four act in both
/// stances.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BoostSlotId {
    /// Health bar, empty endpoint.
    HealthEmpty = 0,
    /// Health bar, full endpoint.
    HealthFull = 1,
    /// Energy bar, empty endpoint.
    EnergyEmpty = 2,
    /// Energy bar, full endpoint.
    EnergyFull = 3,
}

impl BoostSlotId {
    /// All slots in runtime/menu order.
    pub const ALL: [Self; 4] = [
        Self::HealthEmpty,
        Self::HealthFull,
        Self::EnergyEmpty,
        Self::EnergyFull,
    ];

    /// Dense index into [`PowerUpLoadout`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The bar whose level drives this socket.
    pub const fn bar(self) -> BoostBar {
        match self {
            Self::HealthEmpty | Self::HealthFull => BoostBar::Health,
            Self::EnergyEmpty | Self::EnergyFull => BoostBar::Energy,
        }
    }

    /// Endpoint this socket radiates from.
    pub const fn pole(self) -> VitalityPole {
        match self {
            Self::HealthEmpty | Self::EnergyEmpty => VitalityPole::Empty,
            Self::HealthFull | Self::EnergyFull => VitalityPole::Full,
        }
    }

    /// Decode a dense menu selection index.
    pub const fn from_index(index: u8) -> Self {
        match index {
            1 => Self::HealthFull,
            2 => Self::EnergyEmpty,
            3 => Self::EnergyFull,
            _ => Self::HealthEmpty,
        }
    }
}

/// The levels the boost sockets read: the health bar and the energy bar.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoostBars {
    /// The fighter's health.
    pub health: VitalityPool,
    /// The fighter's energy, as current over its ceiling.
    pub energy: VitalityPool,
}

impl BoostBars {
    /// The bar one socket reads.
    pub const fn of(self, slot: BoostSlotId) -> VitalityPool {
        match slot.bar() {
            BoostBar::Health => self.health,
            BoostBar::Energy => self.energy,
        }
    }
}

/// Four assignable power-up sockets, two on each bar.
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

    /// Resolve the live fixed-point modifiers contributed by all four
    /// sockets. Every socket acts whichever stance is active.
    pub fn modifiers(
        self,
        bars: BoostBars,
        modules: &[psx_level::BoostModuleRecord],
    ) -> VitalityModifiers {
        let mut bonuses_q12 = [0i32; psx_level::boost_stat::COUNT];
        for slot in BoostSlotId::ALL {
            let module_id = self.module(slot);
            let Some(module) = module_id.index().and_then(|index| modules.get(index)) else {
                continue;
            };
            for (index, bonus) in bonuses_q12.iter_mut().enumerate() {
                *bonus = bonus.saturating_add(module_stat_bonus_q12(bars, slot, module, index));
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
        }
    }
}

/// One module's live signed percentage-point contribution in Q12 for a
/// particular socket and stat lane. The inventory uses the same calculation
/// as gameplay so its `MODULE` column cannot disagree with the damage,
/// defence, movement, or timeline multipliers actually applied.
pub fn module_stat_bonus_q12(
    bars: BoostBars,
    slot: BoostSlotId,
    module: &psx_level::BoostModuleRecord,
    stat: usize,
) -> i32 {
    let Some(percent) = module.percentages.get(stat) else {
        return 0;
    };
    let influence = bars.of(slot).influence_q12(slot.pole());
    // Empty-end sockets are the risk/reward side: 200% at an empty bar,
    // fading linearly to zero at the midpoint. Full-end sockets reach 100%
    // only at a full bar and also fade to zero at the midpoint.
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

/// Live player-stat multipliers derived from the bars and assigned modules.
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
}

impl VitalityModifiers {
    /// No-op modifiers at the neutral midpoint or with no assigned modules.
    pub const IDENTITY: Self = Self {
        horizon_damage_q12: VITALITY_Q12_ONE,
        zenith_damage_q12: VITALITY_Q12_ONE,
        incoming_damage_q12: VITALITY_Q12_ONE,
        movement_speed_q12: VITALITY_Q12_ONE,
        attack_speed_q12: VITALITY_Q12_ONE,
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

/// A Q12 ratio as a whole percentage, rounded half away from zero.
///
/// For HUD text. Split at the Q12 point like [`scale_i32_q12`], so nothing
/// leaves 32 bits.
pub const fn q12_to_percent(value_q12: i32) -> i32 {
    let magnitude = value_q12.unsigned_abs();
    let percent = (magnitude >> 12) * 100 + (((magnitude & 0xFFF) * 100 + 2048) >> 12);
    if value_q12 < 0 {
        -(percent as i32)
    } else {
        percent as i32
    }
}

/// `value * multiplier / 4096`, rounded half away from zero and saturated.
///
/// Splitting the magnitude at the Q12 point keeps the product in 32 bits:
/// `a * m = (a >> 12) * m * 4096 + (a & 4095) * m`, so the quotient is the
/// first product plus the rounded remainder term, with no 64-bit divide.
fn scale_i32_q12(value: i32, multiplier_q12: u16) -> i32 {
    let magnitude = value.unsigned_abs();
    let multiplier = u32::from(multiplier_q12);
    let half = u32::from(VITALITY_Q12_ONE / 2);
    let fraction = ((magnitude & 0xFFF) * multiplier + half) >> 12;
    let scaled = (magnitude >> 12)
        .checked_mul(multiplier)
        .and_then(|whole| whole.checked_add(fraction));
    match (scaled, value < 0) {
        (Some(scaled), false) => scaled.min(i32::MAX as u32) as i32,
        (Some(scaled), true) if scaled <= i32::MAX as u32 => -(scaled as i32),
        (_, false) => i32::MAX,
        (_, true) => i32::MIN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q12_percent_matches_64_bit_arithmetic() {
        for value in [
            i32::MIN,
            -4096,
            -2069,
            -21,
            -20,
            -1,
            0,
            1,
            20,
            21,
            2048,
            4096,
            6144,
            i32::MAX,
        ] {
            let scaled = i64::from(value) * 100;
            let expected = if scaled < 0 {
                (scaled - 2048) / 4096
            } else {
                (scaled + 2048) / 4096
            };
            assert_eq!(i64::from(q12_to_percent(value)), expected, "{value}");
        }
    }

    #[test]
    fn q12_scale_matches_64_bit_arithmetic() {
        let values = [
            i32::MIN,
            i32::MIN + 1,
            -2_147_000_000,
            -1_048_577,
            -4097,
            -4096,
            -2049,
            -2048,
            -2047,
            -1,
            0,
            1,
            2047,
            2048,
            2049,
            4095,
            4096,
            4097,
            123_456_789,
            i32::MAX - 1,
            i32::MAX,
        ];
        let multipliers = [
            0u16,
            1,
            2047,
            2048,
            4095,
            4096,
            4097,
            6144,
            32_768,
            u16::MAX,
        ];
        for value in values {
            for multiplier in multipliers {
                let scaled = i64::from(value) * i64::from(multiplier);
                let rounding = 2048 * i64::from(value.signum());
                let expected = ((scaled + rounding) / 4096)
                    .clamp(i64::from(i32::MIN), i64::from(i32::MAX))
                    as i32;
                assert_eq!(
                    scale_i32_q12(value, multiplier),
                    expected,
                    "{value} * {multiplier}"
                );
            }
        }
    }

    const MODULES: [psx_level::BoostModuleRecord; 3] = [
        psx_level::BoostModuleRecord {
            name: "Kinetic Relay",
            description: "Horizon output.",
            effect_summary: "HRZ ATK +15%",
            assignment_label: "ASSIGN KINETIC RELAY: CHOOSE SLOT",
            remove_label: "REMOVE KINETIC RELAY",
            percentages: [15, 0, 0, 0, 0],
        },
        psx_level::BoostModuleRecord {
            name: "Guard Matrix",
            description: "Damage guard.",
            effect_summary: "DEF +20%",
            assignment_label: "ASSIGN GUARD MATRIX: CHOOSE SLOT",
            remove_label: "REMOVE GUARD MATRIX",
            percentages: [0, 0, 20, 0, 0],
        },
        psx_level::BoostModuleRecord {
            name: "Overdrive Coil",
            description: "Faster movement and attacks.",
            effect_summary: "MOVE +10% / ATK SPD +5%",
            assignment_label: "ASSIGN OVERDRIVE COIL: CHOOSE SLOT",
            remove_label: "REMOVE OVERDRIVE COIL",
            percentages: [0, 0, 0, 10, 5],
        },
    ];

    /// Both bars full: health 200, energy 100.
    fn full_bars() -> BoostBars {
        BoostBars {
            health: VitalityPool::full(200),
            energy: VitalityPool::full(100),
        }
    }

    /// A single hit larger than the bar must empty it, whatever its size,
    /// and report only what the bar held.
    #[test]
    fn one_hit_larger_than_the_bar_empties_it() {
        for damage in [u16::MAX, u16::MAX - 1, 60_000, 201, 200] {
            let mut health = VitalityPool::full(200);
            assert_eq!(health.apply_damage(damage), 200, "damage {damage}");
            assert!(health.is_empty(), "damage {damage}");
        }
    }

    /// Exactly one short leaves one point, alive.
    #[test]
    fn one_short_of_the_bar_leaves_the_fighter_alive() {
        let mut health = VitalityPool::full(200);
        assert_eq!(health.apply_damage(199), 199);
        assert_eq!(health.current(), 1);
        assert!(!health.is_empty());
        assert_eq!(health.apply_damage(1), 1);
        assert!(health.is_empty());
    }

    #[test]
    fn influence_is_full_at_endpoints_and_zero_at_midpoint() {
        let influence = |current| {
            let bar = VitalityPool::at(current, 100);
            (
                bar.influence_q12(VitalityPole::Empty),
                bar.influence_q12(VitalityPole::Full),
            )
        };
        assert_eq!(influence(100), (0, 4096));
        assert_eq!(influence(75), (0, 2048));
        assert_eq!(influence(50), (0, 0));
        assert_eq!(influence(25), (2048, 0));
        assert_eq!(influence(0), (4096, 0));
    }

    #[test]
    fn defeat_is_the_one_bar_empty() {
        let mut health = VitalityPool::full(200);
        assert_eq!(health.apply_damage(100), 100);
        assert!(!health.is_empty(), "half the bar is not defeat");
        assert_eq!(health.apply_damage(100), 100);
        assert!(health.is_empty());
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
        let full = loadout.modifiers(full_bars(), &MODULES);
        assert!(full.incoming_damage_q12 < VITALITY_Q12_ONE);

        let midpoint = BoostBars {
            health: VitalityPool::at(100, 200),
            energy: VitalityPool::at(50, 100),
        };
        assert_eq!(
            loadout.modifiers(midpoint, &MODULES),
            VitalityModifiers::IDENTITY
        );
    }

    #[test]
    fn empty_socket_reaches_twice_the_full_socket_percentage() {
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
            .modifiers(full_bars(), &MODULES)
            .horizon_damage_q12;
        let empty_health = BoostBars {
            health: VitalityPool::at(0, 200),
            ..full_bars()
        };
        let empty_bonus = empty_kinetic
            .modifiers(empty_health, &MODULES)
            .horizon_damage_q12;
        assert_eq!(
            empty_bonus - VITALITY_Q12_ONE,
            (full_bonus - VITALITY_Q12_ONE) * 2
        );
    }

    #[test]
    fn module_effect_is_global_while_owning_bar_drives_its_strength() {
        // A Horizon-attack module on the energy bar's full socket: the
        // effect is the module's, the strength is the energy bar's.
        let loadout = PowerUpLoadout {
            slots: [
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId(0),
            ],
        };
        let modifiers = loadout.modifiers(full_bars(), &MODULES);
        assert!(modifiers.horizon_damage_q12 > VITALITY_Q12_ONE);
        assert_eq!(modifiers.zenith_damage_q12, VITALITY_Q12_ONE);
        let spent = BoostBars {
            energy: VitalityPool::at(50, 100),
            ..full_bars()
        };
        assert_eq!(
            loadout.modifiers(spent, &MODULES).horizon_damage_q12,
            VITALITY_Q12_ONE,
            "the energy bar at its midpoint silences it"
        );
    }

    #[test]
    fn defence_reduction_caps_at_eighty_percent() {
        const ARMOUR: [psx_level::BoostModuleRecord; 1] = [psx_level::BoostModuleRecord {
            name: "Armour",
            description: "",
            effect_summary: "DEF +100%",
            assignment_label: "",
            remove_label: "",
            percentages: [0, 0, 100, 0, 0],
        }];
        let loadout = PowerUpLoadout {
            slots: [
                BoostModuleId(0),
                BoostModuleId::NONE,
                BoostModuleId::NONE,
                BoostModuleId::NONE,
            ],
        };
        let empty_health = BoostBars {
            health: VitalityPool::at(0, 200),
            ..full_bars()
        };
        let modifiers = loadout.modifiers(empty_health, &ARMOUR);
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
        assert!(inventory.assign(&mut loadout, BoostSlotId::EnergyFull, kinetic,));

        assert_eq!(loadout.module(BoostSlotId::EnergyFull), kinetic);
        assert!(!inventory.contains(kinetic));
        assert!(inventory.is_empty());
        for slot in [
            BoostSlotId::HealthEmpty,
            BoostSlotId::HealthFull,
            BoostSlotId::EnergyEmpty,
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
        assert!(inventory.assign(&mut loadout, BoostSlotId::HealthFull, guard,));
        assert!(!inventory.contains(guard));

        assert!(inventory.assign(&mut loadout, BoostSlotId::HealthFull, BoostModuleId::NONE,));
        assert_eq!(loadout.module(BoostSlotId::HealthFull), BoostModuleId::NONE);
        assert!(inventory.contains(guard));
    }

    #[test]
    fn inventory_assignment_is_atomic_when_stock_is_empty() {
        let mut inventory = BoostInventory::EMPTY;
        let mut loadout = PowerUpLoadout::DEFAULT;
        let before = loadout;

        assert!(!inventory.assign(&mut loadout, BoostSlotId::HealthEmpty, BoostModuleId(2),));
        assert_eq!(loadout, before);
        assert_eq!(inventory, BoostInventory::EMPTY);
    }

    #[test]
    fn refill_restores_the_bar() {
        let mut health = VitalityPool::full(150);
        health.apply_damage(120);
        assert_eq!(health.heal(20), 20);
        assert_eq!(health.current(), 50);
        health.refill();
        assert_eq!(health.current(), 150);
        assert_eq!(health.heal(5), 0, "a full bar takes nothing");
    }
}

/// Authored numbers governing the stance swap.
///
/// How much a hit hurts is not here: that is the one stance rule over the
/// defender's `psx_level::CombatStats`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CombatStanceConfig {
    /// Ticks before another voluntary swap is allowed.
    pub swap_cooldown_ticks: u16,
    /// Ticks a swap takes: its presentation, and for a tactical controller
    /// how long the swap blocks its next stance decision.
    pub swap_duration_ticks: u16,
}

impl CombatStanceConfig {
    /// Starting point for authoring (a body without a Character's numbers).
    pub const DEFAULT: Self = Self::of(&psx_level::LevelCharacterRecord::DEFAULT_BODY);

    /// `character`'s authored swap: the same numbers for the player and
    /// every entity whose body it is.
    pub const fn of(character: &psx_level::LevelCharacterRecord) -> Self {
        Self {
            swap_cooldown_ticks: character.stance_swap_cooldown_ticks,
            swap_duration_ticks: character.stance_swap_duration_ticks,
        }
    }
}

impl Default for CombatStanceConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Which stance is active, and the timers around swapping.
///
/// The stance owns no health: swapping changes the weapon and which boost
/// sockets act, and only the cooldown limits it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CombatStance {
    active: VitalityChannelId,
    swap_cooldown: u16,
    swap_elapsed: u16,
}

impl CombatStance {
    /// Start in `active` with no timers running.
    pub const fn new(active: VitalityChannelId) -> Self {
        Self {
            active,
            swap_cooldown: 0,
            // A fresh stance is stable, not twelve ticks into a mutation.
            // Saturation keeps the no-swap state valid for any authored
            // duration without another flag.
            swap_elapsed: u16::MAX,
        }
    }

    /// The active stance.
    pub const fn active(self) -> VitalityChannelId {
        self.active
    }

    /// The stance a swap would go to.
    pub const fn inactive(self) -> VitalityChannelId {
        self.active.other()
    }

    /// Ticks left before another voluntary swap.
    pub const fn swap_cooldown(self) -> u16 {
        self.swap_cooldown
    }

    /// Ticks elapsed since the latest stance mutation began.
    ///
    /// A fresh stance returns `u16::MAX`, which lets presentation code avoid
    /// mistaking startup for a just-completed mutation. After a real swap the
    /// value advances through the mutation and its short completion echo.
    pub const fn swap_elapsed_ticks(self) -> u16 {
        self.swap_elapsed
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

    /// Whether a swap is allowed right now: only the cooldown refuses one.
    pub const fn can_swap(self) -> bool {
        self.swap_cooldown == 0
    }

    /// Attempt a voluntary swap under `config`'s cooldown.
    pub fn request_swap(&mut self, config: &CombatStanceConfig) -> bool {
        self.try_swap(config.swap_cooldown_ticks)
    }

    /// Attempt a voluntary swap that arms `cooldown_ticks` of cooldown.
    ///
    /// The same rule as [`Self::request_swap`] for a caller that holds the
    /// cooldown rather than a whole config (the enemies share the player's
    /// cooked cooldown this way).
    pub fn try_swap(&mut self, cooldown_ticks: u16) -> bool {
        if !self.can_swap() {
            return false;
        }
        self.active = self.inactive();
        self.swap_cooldown = cooldown_ticks;
        self.swap_elapsed = 0;
        true
    }

    /// Count `delta` ticks off the swap cooldown.
    pub fn cool_down(&mut self, delta: u16) {
        self.swap_cooldown = self.swap_cooldown.saturating_sub(delta);
    }

    /// Advance the swap presentation clock by `delta` ticks.
    pub fn advance_swap(&mut self, delta: u16) {
        self.swap_elapsed = self.swap_elapsed.saturating_add(delta);
    }

    /// One fixed tick: the cooldown and the swap clock.
    pub fn tick(&mut self) {
        self.cool_down(1);
        self.advance_swap(1);
    }

    /// Clear the swap cooldown: a parry earns the next swap at once.
    pub fn clear_cooldown(&mut self) {
        self.swap_cooldown = 0;
    }

    /// Start on `active` with no swap showing, keeping the cooldown: what a
    /// fighter that cannot swap is given at spawn.
    pub fn hold(&mut self, active: VitalityChannelId) {
        self.active = active;
        self.swap_elapsed = u16::MAX;
    }

    /// End any swap presentation, keeping the side and the cooldown.
    pub fn settle(&mut self) {
        self.swap_elapsed = u16::MAX;
    }

    /// Test hook: force the active side without a swap.
    #[cfg(test)]
    pub(crate) fn set_active(&mut self, channel: VitalityChannelId) {
        self.active = channel;
    }

    /// Test hook: force the remaining swap cooldown.
    #[cfg(test)]
    pub(crate) fn set_swap_cooldown(&mut self, ticks: u16) {
        self.swap_cooldown = ticks;
    }
}

#[cfg(test)]
mod stance_tests {
    use super::*;

    fn config() -> CombatStanceConfig {
        CombatStanceConfig {
            swap_cooldown_ticks: 3,
            swap_duration_ticks: 4,
        }
    }

    #[test]
    fn only_the_cooldown_refuses_a_swap() {
        let config = config();
        let mut stance = CombatStance::new(VitalityChannelId::One);
        assert!(stance.request_swap(&config));
        assert_eq!(stance.active(), VitalityChannelId::Two);
        assert!(!stance.request_swap(&config), "cooldown");
        for _ in 0..config.swap_cooldown_ticks {
            stance.tick();
        }
        assert!(stance.can_swap());
        assert!(stance.request_swap(&config));
        assert_eq!(stance.active(), VitalityChannelId::One);
    }

    #[test]
    fn every_socket_acts_in_both_stances() {
        // Health and energy sockets alike: the loadout sums all four, and
        // nothing in it depends on the active stance.
        let modules = [psx_level::BoostModuleRecord {
            name: "Edge",
            description: "Blade output.",
            effect_summary: "HRZ ATK +10",
            assignment_label: "ASSIGN EDGE: CHOOSE SLOT",
            remove_label: "REMOVE EDGE",
            percentages: [10, 0, 0, 0, 0],
        }];
        let bars = BoostBars {
            health: VitalityPool::full(200),
            energy: VitalityPool::full(100),
        };
        for slot in [BoostSlotId::HealthFull, BoostSlotId::EnergyFull] {
            let mut loadout = PowerUpLoadout::EMPTY;
            loadout.set(slot, BoostModuleId::from_index(0));
            assert!(
                loadout.modifiers(bars, &modules).horizon_damage_q12 > VITALITY_Q12_ONE,
                "{slot:?} acts"
            );
        }
    }

    #[test]
    fn the_swap_animation_reports_progress_and_completes() {
        let config = config();
        let mut stance = CombatStance::new(VitalityChannelId::One);
        assert!(stance.request_swap(&config));
        assert!(stance.swap_in_progress(&config));
        assert_eq!(stance.swap_progress_q12(&config), 0);
        stance.tick();
        stance.tick();
        assert_eq!(stance.swap_progress_q12(&config), VITALITY_Q12_ONE / 2);
        for _ in 0..4 {
            stance.tick();
        }
        assert_eq!(stance.swap_progress_q12(&config), VITALITY_Q12_ONE);
        assert!(!stance.swap_in_progress(&config));
    }
}
