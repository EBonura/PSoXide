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

/// Built-in boost protocol. Zero is intentionally `None`: the editor-playtest
/// scene lives in link-time-zero storage and this keeps its loadout valid before
/// gameplay initialization stamps the authored defaults.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum BoostProtocol {
    /// No protocol assigned.
    #[default]
    None = 0,
    /// Outgoing attack damage.
    Rupture = 1,
    /// Incoming damage reduction.
    Shell = 2,
    /// Walk/run movement speed.
    Surge = 3,
}

impl BoostProtocol {
    /// Cycle order used by the inventory's compact assignment controls.
    pub const fn next(self) -> Self {
        match self {
            Self::None => Self::Rupture,
            Self::Rupture => Self::Shell,
            Self::Shell => Self::Surge,
            Self::Surge => Self::None,
        }
    }

    /// Compact protocol label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "NONE",
            Self::Rupture => "RUPTURE",
            Self::Shell => "SHELL",
            Self::Surge => "SURGE",
        }
    }

    /// Dense stockpile index for collectible protocols. `None` is a socket
    /// state rather than a collectible and therefore has no inventory slot.
    pub const fn inventory_index(self) -> Option<usize> {
        match self {
            Self::None => None,
            Self::Rupture => Some(0),
            Self::Shell => Some(1),
            Self::Surge => Some(2),
        }
    }

    /// Stat family affected by this protocol.
    pub const fn stat_label(self) -> &'static str {
        match self {
            Self::None => "NO STAT EFFECT",
            Self::Rupture => "ATTACK OUTPUT",
            Self::Shell => "DAMAGE GUARD",
            Self::Surge => "MOVE SPEED",
        }
    }

    /// Compact inventory-row copy with a bounded two-character stack count.
    pub const fn inventory_label(self, count: u8) -> &'static str {
        match (self, count) {
            (Self::Rupture, 0) => "RUPTURE // 00",
            (Self::Rupture, 1) => "RUPTURE // 01",
            (Self::Rupture, 2) => "RUPTURE // 02",
            (Self::Rupture, 3) => "RUPTURE // 03",
            (Self::Rupture, 4) => "RUPTURE // 04",
            (Self::Rupture, 5) => "RUPTURE // 05",
            (Self::Rupture, 6) => "RUPTURE // 06",
            (Self::Rupture, 7) => "RUPTURE // 07",
            (Self::Rupture, 8) => "RUPTURE // 08",
            (Self::Rupture, 9) => "RUPTURE // 09",
            (Self::Rupture, _) => "RUPTURE // 9+",
            (Self::Shell, 0) => "SHELL   // 00",
            (Self::Shell, 1) => "SHELL   // 01",
            (Self::Shell, 2) => "SHELL   // 02",
            (Self::Shell, 3) => "SHELL   // 03",
            (Self::Shell, 4) => "SHELL   // 04",
            (Self::Shell, 5) => "SHELL   // 05",
            (Self::Shell, 6) => "SHELL   // 06",
            (Self::Shell, 7) => "SHELL   // 07",
            (Self::Shell, 8) => "SHELL   // 08",
            (Self::Shell, 9) => "SHELL   // 09",
            (Self::Shell, _) => "SHELL   // 9+",
            (Self::Surge, 0) => "SURGE   // 00",
            (Self::Surge, 1) => "SURGE   // 01",
            (Self::Surge, 2) => "SURGE   // 02",
            (Self::Surge, 3) => "SURGE   // 03",
            (Self::Surge, 4) => "SURGE   // 04",
            (Self::Surge, 5) => "SURGE   // 05",
            (Self::Surge, 6) => "SURGE   // 06",
            (Self::Surge, 7) => "SURGE   // 07",
            (Self::Surge, 8) => "SURGE   // 08",
            (Self::Surge, 9) => "SURGE   // 09",
            (Self::Surge, _) => "SURGE   // 9+",
            (Self::None, _) => "NONE    // --",
        }
    }

    /// Menu button copy for one endpoint.
    pub const fn slot_label(self, pole: VitalityPole) -> &'static str {
        match (pole, self) {
            (VitalityPole::Empty, Self::None) => "E // NONE",
            (VitalityPole::Empty, Self::Rupture) => "E // RUPTURE",
            (VitalityPole::Empty, Self::Shell) => "E // SHELL",
            (VitalityPole::Empty, Self::Surge) => "E // SURGE",
            (VitalityPole::Full, Self::None) => "F // NONE",
            (VitalityPole::Full, Self::Rupture) => "F // RUPTURE",
            (VitalityPole::Full, Self::Shell) => "F // SHELL",
            (VitalityPole::Full, Self::Surge) => "F // SURGE",
        }
    }

    /// Maximum effect copy shown for a selected socket. Actual effect is this
    /// maximum multiplied by the socket's live polarity influence.
    pub const fn effect_label(self, pole: VitalityPole) -> &'static str {
        match (pole, self) {
            (_, Self::None) => "NO PROTOCOL ASSIGNED",
            (VitalityPole::Empty, Self::Rupture) => "ATK MAX +30%",
            (VitalityPole::Full, Self::Rupture) => "ATK MAX +10%",
            (VitalityPole::Empty, Self::Shell) => "DEF MAX +20%",
            (VitalityPole::Full, Self::Shell) => "DEF MAX +8%",
            (VitalityPole::Empty, Self::Surge) => "SPD MAX +15%",
            (VitalityPole::Full, Self::Surge) => "SPD MAX +6%",
        }
    }

    const fn maximum_bonus_q12(self, pole: VitalityPole) -> u16 {
        match (pole, self) {
            (_, Self::None) => 0,
            (VitalityPole::Empty, Self::Rupture) => 1229,
            (VitalityPole::Full, Self::Rupture) => 410,
            (VitalityPole::Empty, Self::Shell) => 819,
            (VitalityPole::Full, Self::Shell) => 328,
            (VitalityPole::Empty, Self::Surge) => 614,
            (VitalityPole::Full, Self::Surge) => 246,
        }
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
    slots: [BoostProtocol; 4],
}

impl PowerUpLoadout {
    /// Empty loadout; also the all-zero boot representation.
    pub const EMPTY: Self = Self {
        slots: [BoostProtocol::None; 4],
    };

    /// Default player loadout. Collected protocols begin in the inventory and
    /// must be deliberately assigned to one of these four empty sockets.
    pub const DEFAULT: Self = Self::EMPTY;

    /// Protocol assigned to one socket.
    pub const fn protocol(self, slot: BoostSlotId) -> BoostProtocol {
        self.slots[slot.index()]
    }

    /// Replace one socket and return the protocol that was equipped there.
    pub fn set(&mut self, slot: BoostSlotId, protocol: BoostProtocol) -> BoostProtocol {
        let previous = self.slots[slot.index()];
        self.slots[slot.index()] = protocol;
        previous
    }

    /// Cycle one socket through the built-in protocol inventory.
    pub fn cycle(&mut self, slot: BoostSlotId) -> BoostProtocol {
        let next = self.slots[slot.index()].next();
        self.slots[slot.index()] = next;
        next
    }

    /// Resolve the live fixed-point modifiers contributed by all four sockets.
    pub fn modifiers(self, vitality: &DualVitality) -> VitalityModifiers {
        let mut outgoing_bonus = 0u32;
        let mut incoming_reduction = 0u32;
        let mut movement_bonus = 0u32;
        for slot in BoostSlotId::ALL {
            let protocol = self.protocol(slot);
            let influence = vitality.pool(slot.channel()).polarity().at(slot.pole());
            let contribution = (u32::from(protocol.maximum_bonus_q12(slot.pole()))
                * u32::from(influence))
                / u32::from(VITALITY_Q12_ONE);
            match protocol {
                BoostProtocol::None => {}
                BoostProtocol::Rupture => {
                    outgoing_bonus = outgoing_bonus.saturating_add(contribution)
                }
                BoostProtocol::Shell => {
                    incoming_reduction = incoming_reduction.saturating_add(contribution)
                }
                BoostProtocol::Surge => {
                    movement_bonus = movement_bonus.saturating_add(contribution)
                }
            }
        }
        // Multiple Shell sockets add, but incoming damage can never be reduced
        // by more than 35%; this prevents an endpoint loadout becoming immunity.
        let incoming_reduction = incoming_reduction.min(1434);
        VitalityModifiers {
            outgoing_damage_q12: u16::try_from(
                u32::from(VITALITY_Q12_ONE).saturating_add(outgoing_bonus),
            )
            .unwrap_or(u16::MAX),
            incoming_damage_q12: VITALITY_Q12_ONE.saturating_sub(incoming_reduction as u16),
            movement_speed_q12: u16::try_from(
                u32::from(VITALITY_Q12_ONE).saturating_add(movement_bonus),
            )
            .unwrap_or(u16::MAX),
        }
    }
}

impl Default for PowerUpLoadout {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Allocation-free stockpile of collected, currently unequipped protocols.
///
/// Equipped copies live in [`PowerUpLoadout`]. Assignment transfers exactly
/// one copy between the stockpile and a socket, so repeated menu operations
/// cannot duplicate or silently discard an item.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoostInventory {
    counts: [u8; 3],
}

impl BoostInventory {
    /// Maximum copies retained for one protocol type.
    pub const MAX_STACK: u8 = 99;

    /// Empty stockpile; also the all-zero boot representation.
    pub const EMPTY: Self = Self { counts: [0; 3] };

    /// Minimal stock supplied by the default project until authored world
    /// pickups seed the inventory: one unassigned Rupture protocol.
    pub const STARTER: Self = Self { counts: [1, 0, 0] };

    /// Unequipped copies of one collectible protocol.
    pub const fn count(self, protocol: BoostProtocol) -> u8 {
        match protocol.inventory_index() {
            Some(index) => self.counts[index],
            None => 0,
        }
    }

    /// Add collected copies, returning the number accepted before the stack
    /// cap. `None` and a zero amount are ignored.
    pub fn add(&mut self, protocol: BoostProtocol, amount: u8) -> u8 {
        let Some(index) = protocol.inventory_index() else {
            return 0;
        };
        let room = Self::MAX_STACK.saturating_sub(self.counts[index]);
        let accepted = amount.min(room);
        self.counts[index] = self.counts[index].saturating_add(accepted);
        accepted
    }

    /// Consume one unequipped copy when available.
    pub fn take(&mut self, protocol: BoostProtocol) -> bool {
        let Some(index) = protocol.inventory_index() else {
            return false;
        };
        if self.counts[index] == 0 {
            return false;
        }
        self.counts[index] -= 1;
        true
    }

    /// Assign one collected copy to a socket and return its previous protocol
    /// to the stockpile. Selecting the already-equipped protocol is a no-op.
    /// Passing `None` unequips without requiring an inventory item.
    pub fn assign(
        &mut self,
        loadout: &mut PowerUpLoadout,
        slot: BoostSlotId,
        protocol: BoostProtocol,
    ) -> bool {
        let previous = loadout.protocol(slot);
        if previous == protocol {
            return true;
        }
        if previous != BoostProtocol::None && self.count(previous) >= Self::MAX_STACK {
            return false;
        }
        if protocol != BoostProtocol::None && !self.take(protocol) {
            return false;
        }
        let previous = loadout.set(slot, protocol);
        if previous != BoostProtocol::None {
            let returned = self.add(previous, 1);
            debug_assert_eq!(returned, 1);
        }
        true
    }

    /// Copy used by the compact selected-item detail readout.
    pub const fn owned_label(self, protocol: BoostProtocol) -> &'static str {
        match self.count(protocol) {
            0 => "OWNED // 00",
            1 => "OWNED // 01",
            2 => "OWNED // 02",
            3 => "OWNED // 03",
            4 => "OWNED // 04",
            5 => "OWNED // 05",
            6 => "OWNED // 06",
            7 => "OWNED // 07",
            8 => "OWNED // 08",
            9 => "OWNED // 09",
            _ => "OWNED // 9+",
        }
    }
}

impl Default for BoostInventory {
    fn default() -> Self {
        Self::STARTER
    }
}

/// Live player-stat multipliers derived from vitality and assigned protocols.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct VitalityModifiers {
    /// Outgoing health damage multiplier (`4096 = 1.0x`).
    pub outgoing_damage_q12: u16,
    /// Incoming health damage multiplier after defense (`4096 = 1.0x`).
    pub incoming_damage_q12: u16,
    /// Walk/run speed multiplier (`4096 = 1.0x`).
    pub movement_speed_q12: u16,
}

impl VitalityModifiers {
    /// No-op modifiers at the neutral midpoint or with no assigned protocols.
    pub const IDENTITY: Self = Self {
        outgoing_damage_q12: VITALITY_Q12_ONE,
        incoming_damage_q12: VITALITY_Q12_ONE,
        movement_speed_q12: VITALITY_Q12_ONE,
    };

    /// Scale outgoing damage, rounding to the nearest integer.
    pub fn outgoing_damage(self, damage: u16) -> u16 {
        scale_u16_q12(damage, self.outgoing_damage_q12)
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
                BoostProtocol::Rupture,
                BoostProtocol::Shell,
                BoostProtocol::Surge,
                BoostProtocol::Rupture,
            ],
        };
        let mut vitality = DualVitality::equal(100);
        let full = loadout.modifiers(&vitality);
        assert!(full.outgoing_damage_q12 > VITALITY_Q12_ONE);
        assert!(full.incoming_damage_q12 < VITALITY_Q12_ONE);

        let _ = vitality.apply_damage(VitalityChannelId::One, 50);
        let _ = vitality.apply_damage(VitalityChannelId::Two, 50);
        assert_eq!(loadout.modifiers(&vitality), VitalityModifiers::IDENTITY);
    }

    #[test]
    fn the_same_protocol_is_stronger_in_an_empty_socket() {
        let mut vitality = DualVitality::equal(100);
        let empty_rupture = PowerUpLoadout {
            slots: [
                BoostProtocol::Rupture,
                BoostProtocol::None,
                BoostProtocol::None,
                BoostProtocol::None,
            ],
        };
        let full_rupture = PowerUpLoadout {
            slots: [
                BoostProtocol::None,
                BoostProtocol::Rupture,
                BoostProtocol::None,
                BoostProtocol::None,
            ],
        };
        let full_bonus = full_rupture.modifiers(&vitality).outgoing_damage_q12;
        let _ = vitality.apply_damage(VitalityChannelId::One, 100);
        let empty_bonus = empty_rupture.modifiers(&vitality).outgoing_damage_q12;
        assert_eq!(full_bonus, 4506);
        assert_eq!(empty_bonus, 5325);
    }

    #[test]
    fn starter_inventory_assigns_its_only_copy_to_one_empty_socket() {
        let mut inventory = BoostInventory::STARTER;
        let mut loadout = PowerUpLoadout::DEFAULT;

        assert_eq!(inventory.count(BoostProtocol::Rupture), 1);
        assert_eq!(inventory.count(BoostProtocol::Shell), 0);
        assert_eq!(inventory.count(BoostProtocol::Surge), 0);
        for slot in BoostSlotId::ALL {
            assert_eq!(loadout.protocol(slot), BoostProtocol::None);
        }
        assert!(inventory.assign(
            &mut loadout,
            BoostSlotId::ZenithFull,
            BoostProtocol::Rupture,
        ));

        assert_eq!(
            loadout.protocol(BoostSlotId::ZenithFull),
            BoostProtocol::Rupture
        );
        assert_eq!(inventory.count(BoostProtocol::Rupture), 0);
        for slot in [
            BoostSlotId::HorizonEmpty,
            BoostSlotId::HorizonFull,
            BoostSlotId::ZenithEmpty,
        ] {
            assert_eq!(loadout.protocol(slot), BoostProtocol::None);
        }
    }

    #[test]
    fn inventory_assignment_is_atomic_when_stock_is_empty() {
        let mut inventory = BoostInventory::EMPTY;
        let mut loadout = PowerUpLoadout::DEFAULT;
        let before = loadout;

        assert!(!inventory.assign(
            &mut loadout,
            BoostSlotId::HorizonEmpty,
            BoostProtocol::Surge,
        ));
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
