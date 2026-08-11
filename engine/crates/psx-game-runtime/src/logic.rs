//! Logic-entity runtime (phase 3 of docs/game-runtime-plan.md): the
//! hl-psx entity-I/O core over cooked [`LevelLogicRecord`]s --
//! delay-queued events, depth-limited target fan-out, and
//! multisource/master AND-gating. Names were interned to u16 ids at
//! cook; the runtime only ever compares ids.
//!
//! Scope of this foundation slice: the event graph runs (queueing,
//! firing, re-arm/once semantics, master gates, player-touch trigger
//! volumes) and terminal effects surface as drainable fire marks --
//! the owning game maps them onto UI overlays / checkpoints / door
//! visuals in the next slice. With zero cooked records every entry
//! point returns immediately (the phase-3 budget's <1k idle rule).
//!
//! Visibility policy: the per-tick player-touch scan gates on the
//! portal-expanded active-room set; the delay queue and re-arm timers
//! process globally so a timed chain never freezes when the player
//! looks away (hl parity: PVS gates monster thinking, not logic).
//!
//! Crate rules hold: no statics, no unsafe, `const N` capacities,
//! `&'static` cooked records, all-zero [`LogicRuntime::EMPTY`].

use psx_level::{logic_flags, logic_kind, LevelLogicRecord, RoomIndex, LOGIC_NAME_NONE};

/// hl-parity use codes carried by queued events.
pub mod use_type {
    /// Force off/closed.
    pub const OFF: u8 = 0;
    /// Force on/open.
    pub const ON: u8 = 1;
    /// Toggle current state.
    pub const TOGGLE: u8 = 3;
}

/// Maximum same-tick fan-out recursion depth (hl-psx parity). Chains
/// deeper than this drop the remainder and count it in
/// [`LogicStats::depth_drops`].
pub const LOGIC_FIRE_DEPTH_MAX: u8 = 8;

/// Per-record runtime state.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogicState {
    /// Armed and reactive.
    Ready = 0,
    /// Fired; waiting `wait_ticks` to re-arm.
    Waiting = 1,
    /// Retired (fire-once records, killtargets, disabled records).
    Removed = 2,
}

impl LogicState {
    const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Ready,
            1 => Self::Waiting,
            _ => Self::Removed,
        }
    }
}

/// One delayed fire in flight. The all-zero pattern is inactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogicEvent {
    /// Absolute 60 Hz tick the event fires at.
    at: u32,
    /// Interned target to fire.
    target: u16,
    /// Interned target to remove.
    killtarget: u16,
    /// Bit 0 = active; bits 1..=2 = use code.
    meta: u16,
}

impl LogicEvent {
    const INACTIVE: Self = Self {
        at: 0,
        target: 0,
        killtarget: 0,
        meta: 0,
    };

    const fn active(self) -> bool {
        self.meta & 1 != 0
    }

    const fn use_code(self) -> u8 {
        ((self.meta >> 1) & 0x3) as u8
    }

    const fn meta_for(active: bool, use_code: u8) -> u16 {
        (active as u16) | (((use_code & 0x3) as u16) << 1)
    }
}

/// Counters for overlays / tests / the phase-3 budget line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogicStats {
    /// Events currently queued.
    pub queued: u16,
    /// Enqueue attempts dropped because the queue was full.
    pub queue_drops: u16,
    /// Fan-out chains truncated at [`LOGIC_FIRE_DEPTH_MAX`].
    pub depth_drops: u16,
    /// Total records fired since init.
    pub fired: u16,
}

/// Per-tick inputs: the player pose (trigger-volume touches are
/// player-only this slice) and the active-room gate set.
#[derive(Clone, Copy)]
pub struct LogicTickInput<'a> {
    /// Player position, room-local engine units.
    pub player: [i32; 3],
    /// Room containing the player.
    pub player_room: RoomIndex,
    /// Portal-expanded active-room set gating the touch scan.
    pub active_rooms: &'a [RoomIndex],
}

/// Owned logic runtime over the cooked record table. Record `i`
/// mirrors `records[i]`; records past `MAX_LOGIC` count as overflow.
/// `LOGIC_FIRED_WORDS` is the fired-bitset word count the game
/// derives as `(MAX_LOGIC + 31) / 32` (the `BoxProps` broken-words
/// pattern).
pub struct LogicRuntime<
    const MAX_LOGIC: usize,
    const LOGIC_FIRED_WORDS: usize,
    const MAX_EVENTS: usize,
> {
    /// Live record count = `min(records.len(), MAX_LOGIC)`.
    count: u16,
    /// Cooked records past `MAX_LOGIC`.
    overflow: u16,
    /// Per-record [`LogicState`] as raw u8.
    state: [u8; MAX_LOGIC],
    /// Re-arm deadline while [`LogicState::Waiting`].
    rearm_at: [u32; MAX_LOGIC],
    /// Kind-specific counter: multisource satisfied-input count,
    /// door open flag.
    counter: [i16; MAX_LOGIC],
    /// Fired marks the owning game drains for terminal effects (one
    /// bit per record).
    fired_marks: [u32; LOGIC_FIRED_WORDS],
    /// The delay queue.
    events: [LogicEvent; MAX_EVENTS],
    /// Rolling stats.
    queue_drops: u16,
    depth_drops: u16,
    fired: u16,
    /// Optional owner-supplied per-record activation for touch scans.
    spatial_activation_enabled: bool,
    // psx-numeric-allow-next-line: fixed 64-record activation mask; bit ops only, two-word on R3000
    spatial_active_mask: u64,
}

impl<const MAX_LOGIC: usize, const LOGIC_FIRED_WORDS: usize, const MAX_EVENTS: usize>
    LogicRuntime<MAX_LOGIC, LOGIC_FIRED_WORDS, MAX_EVENTS>
{
    /// All-zero state; `const` for link-time-zero scene storage. Not
    /// meaningful until [`Self::init_from_records`] runs.
    pub const EMPTY: Self = Self {
        count: 0,
        overflow: 0,
        state: [0; MAX_LOGIC],
        rearm_at: [0; MAX_LOGIC],
        counter: [0; MAX_LOGIC],
        fired_marks: [0; LOGIC_FIRED_WORDS],
        events: [LogicEvent::INACTIVE; MAX_EVENTS],
        queue_drops: 0,
        depth_drops: 0,
        fired: 0,
        spatial_activation_enabled: false,
        spatial_active_mask: 0,
    };

    /// Reset and arm runtime state 1:1 from the cooked records
    /// (checkpoint respawn calls this again).
    pub fn init_from_records(&mut self, records: &'static [LevelLogicRecord]) {
        *self = Self::EMPTY;
        let count = records.len().min(MAX_LOGIC);
        self.count = count as u16;
        self.overflow = (records.len() - count).min(u16::MAX as usize) as u16;
        for (index, record) in records.iter().enumerate().take(count) {
            let enabled = record.flags & logic_flags::ENABLED != 0;
            self.state[index] = if enabled {
                LogicState::Ready as u8
            } else {
                LogicState::Removed as u8
            };
            self.counter[index] = if record.flags & logic_flags::START_ON != 0 {
                1
            } else {
                0
            };
        }
    }

    /// Live record count.
    pub fn count(&self) -> usize {
        usize::from(self.count)
    }

    /// Cooked records that did not fit `MAX_LOGIC` at init.
    pub fn overflow_count(&self) -> u16 {
        self.overflow
    }

    /// Select an owner-defined per-record activation mask, or restore the
    /// legacy room-window gate with `None`.
    pub fn set_spatial_active_mask(&mut self, mask: Option<u64>) {
        self.spatial_activation_enabled = mask.is_some();
        self.spatial_active_mask = mask.unwrap_or(0);
    }

    /// Rolling counters.
    pub fn stats(&self) -> LogicStats {
        let mut queued = 0u16;
        let mut i = 0usize;
        while i < MAX_EVENTS {
            if self.events[i].active() {
                queued += 1;
            }
            i += 1;
        }
        LogicStats {
            queued,
            queue_drops: self.queue_drops,
            depth_drops: self.depth_drops,
            fired: self.fired,
        }
    }

    /// True when record `index` is retired.
    pub fn is_removed(&self, index: usize) -> bool {
        index >= self.count() || LogicState::from_raw(self.state[index]) == LogicState::Removed
    }

    /// Door-open flag for [`logic_kind::DOOR`] records (the owning
    /// game maps this onto its linked visual/collision entity).
    pub fn door_open(&self, index: usize) -> bool {
        index < self.count() && self.counter[index] != 0
    }

    /// Drain the fired mark for record `index`: true once per fire.
    /// The owning game polls the records it renders effects for
    /// (message overlays, checkpoint saves).
    pub fn take_fired(&mut self, index: usize) -> bool {
        if index >= self.count() || index >= MAX_LOGIC {
            return false;
        }
        let word = index / 32;
        let mask = 1u32 << (index % 32);
        if word >= self.fired_marks.len() {
            return false;
        }
        let hit = self.fired_marks[word] & mask != 0;
        self.fired_marks[word] &= !mask;
        hit
    }

    /// Fire the record named `target` as if a script had used it
    /// (the external entry point: interact prompts, enemy deaths).
    pub fn fire_by_name(
        &mut self,
        records: &'static [LevelLogicRecord],
        target: u16,
        code: u8,
        now: u32,
    ) {
        self.fire_targets(records, target, code, now, 0);
    }

    /// Fire exactly record `index` (the interact-prompt entry point:
    /// an interactable's paired record is addressed by index, so two
    /// nodes sharing a name never double-fire). Master gating and
    /// removed/waiting state apply as usual. Returns whether the
    /// record reacted.
    pub fn fire_index(
        &mut self,
        records: &'static [LevelLogicRecord],
        index: usize,
        code: u8,
        now: u32,
    ) -> bool {
        if index >= self.count().min(records.len()) {
            return false;
        }
        if LogicState::from_raw(self.state[index]) != LogicState::Ready {
            return false;
        }
        if !self.master_satisfied(records, records[index].master) {
            return false;
        }
        self.activate(records, index, code, now, 0);
        true
    }

    /// True when any fired mark is pending (the drain loop's cheap
    /// front gate: two word loads at MAX_LOGIC = 64).
    pub fn any_fired(&self) -> bool {
        let mut word = 0usize;
        while word < self.fired_marks.len() {
            if self.fired_marks[word] != 0 {
                return true;
            }
            word += 1;
        }
        false
    }

    /// Advance one 60 Hz tick: drain due events, re-arm waiting
    /// records, and run the player-touch scan over trigger volumes in
    /// active rooms.
    pub fn tick(
        &mut self,
        records: &'static [LevelLogicRecord],
        input: LogicTickInput<'_>,
        now: u32,
    ) {
        if self.count == 0 {
            return;
        }
        self.process_events(records, now);
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            match LogicState::from_raw(self.state[index]) {
                LogicState::Removed => {
                    index += 1;
                    continue;
                }
                LogicState::Waiting => {
                    if now >= self.rearm_at[index] {
                        self.state[index] = LogicState::Ready as u8;
                    }
                    index += 1;
                    continue;
                }
                LogicState::Ready => {}
            }
            // Touch requires the player IN the volume's room (cooked
            // bounds are room-local; a raw AABB test aliases across
            // rooms) plus the active-room gate for scan cost.
            let spatially_active = index < 64 && self.spatial_active_mask & (1u64 << index) != 0;
            let activation_allows = if self.spatial_activation_enabled {
                spatially_active
            } else {
                room_is_active(record.room, input.active_rooms)
            };
            if record.kind == logic_kind::TRIGGER_VOLUME
                && input.player_room == record.room
                && activation_allows
                && point_in_aabb(input.player, record.min, record.max)
                && self.master_satisfied(records, record.master)
            {
                self.activate(records, index, use_type::TOGGLE, now, 0);
            }
            index += 1;
        }
    }

    /// Queue-drain: fire every due event at depth zero (hl parity --
    /// a delayed fire restarts the depth budget).
    fn process_events(&mut self, records: &'static [LevelLogicRecord], now: u32) {
        let mut i = 0usize;
        while i < MAX_EVENTS {
            let event = self.events[i];
            if event.active() && now >= event.at {
                self.events[i].meta = LogicEvent::meta_for(false, event.use_code());
                self.kill_targets(records, event.killtarget);
                self.fire_targets(records, event.target, event.use_code(), now, 0);
            }
            i += 1;
        }
    }

    fn enqueue(&mut self, at: u32, target: u16, killtarget: u16, code: u8) {
        if target == LOGIC_NAME_NONE && killtarget == LOGIC_NAME_NONE {
            return;
        }
        let mut i = 0usize;
        while i < MAX_EVENTS {
            if !self.events[i].active() {
                self.events[i] = LogicEvent {
                    at,
                    target,
                    killtarget,
                    meta: LogicEvent::meta_for(true, code),
                };
                return;
            }
            i += 1;
        }
        self.queue_drops = self.queue_drops.saturating_add(1);
    }

    /// Remove every record named `target` (hl killtarget semantics).
    fn kill_targets(&mut self, records: &'static [LevelLogicRecord], target: u16) {
        if target == LOGIC_NAME_NONE {
            return;
        }
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            if records[index].targetname == target {
                self.state[index] = LogicState::Removed as u8;
            }
            index += 1;
        }
    }

    /// Depth-limited fan-out: use every live record named `target`.
    fn fire_targets(
        &mut self,
        records: &'static [LevelLogicRecord],
        target: u16,
        code: u8,
        now: u32,
        depth: u8,
    ) {
        if target == LOGIC_NAME_NONE {
            return;
        }
        if depth > LOGIC_FIRE_DEPTH_MAX {
            self.depth_drops = self.depth_drops.saturating_add(1);
            return;
        }
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            if records[index].targetname == target
                && LogicState::from_raw(self.state[index]) != LogicState::Removed
                && self.master_satisfied(records, records[index].master)
            {
                self.activate(records, index, code, now, depth);
            }
            index += 1;
        }
    }

    /// React record `index` to a use: kind-specific state change,
    /// then fire/kill its own targets (immediately at `depth + 1`, or
    /// through the delay queue when `delay_ticks` > 0), then apply
    /// wait/once re-arm semantics.
    fn activate(
        &mut self,
        records: &'static [LevelLogicRecord],
        index: usize,
        code: u8,
        now: u32,
        depth: u8,
    ) {
        let record = &records[index];
        match record.kind {
            logic_kind::MULTISOURCE => {
                // AND gate: inputs push the satisfied count around;
                // becoming satisfied fires the gate's own target once.
                let required = i16::try_from(record.arg0.max(1)).unwrap_or(i16::MAX);
                let was_satisfied = self.counter[index] >= required;
                self.counter[index] = match code {
                    use_type::ON => self.counter[index].saturating_add(1).min(required),
                    use_type::OFF => self.counter[index].saturating_sub(1).max(0),
                    _ => {
                        if was_satisfied {
                            0
                        } else {
                            required
                        }
                    }
                };
                let satisfied = self.counter[index] >= required;
                if satisfied && !was_satisfied {
                    self.mark_fired(index);
                    self.dispatch_outputs(records, record, now, depth);
                }
                return;
            }
            logic_kind::DOOR => {
                let open = self.counter[index] != 0;
                let next = match code {
                    use_type::ON => true,
                    use_type::OFF => false,
                    _ => !open,
                };
                if next == open {
                    return;
                }
                self.counter[index] = next as i16;
            }
            // MESSAGE / CHECKPOINT / TRIGGER_VOLUME / RELAY: the
            // reaction is the fire mark plus output dispatch.
            _ => {}
        }
        self.mark_fired(index);
        self.dispatch_outputs(records, record, now, depth);
        if record.wait_ticks < 0 {
            self.state[index] = LogicState::Removed as u8;
        } else if record.wait_ticks > 0 {
            self.state[index] = LogicState::Waiting as u8;
            self.rearm_at[index] = now.saturating_add(record.wait_ticks as u32);
        }
    }

    /// Fire/kill a reacting record's own outputs: through the delay
    /// queue when `delay_ticks` > 0, otherwise immediately at
    /// `depth + 1` (same-tick relay chains run to the depth cap).
    fn dispatch_outputs(
        &mut self,
        records: &'static [LevelLogicRecord],
        record: &LevelLogicRecord,
        now: u32,
        depth: u8,
    ) {
        if record.target == LOGIC_NAME_NONE && record.killtarget == LOGIC_NAME_NONE {
            return;
        }
        if record.delay_ticks > 0 {
            self.enqueue(
                now.saturating_add(u32::from(record.delay_ticks)),
                record.target,
                record.killtarget,
                use_type::TOGGLE,
            );
            return;
        }
        self.kill_targets(records, record.killtarget);
        self.fire_targets(records, record.target, use_type::TOGGLE, now, depth + 1);
    }

    fn mark_fired(&mut self, index: usize) {
        self.fired = self.fired.saturating_add(1);
        let word = index / 32;
        if word < self.fired_marks.len() {
            self.fired_marks[word] |= 1u32 << (index % 32);
        }
    }

    /// hl `UTIL_IsMasterTriggered`: no master, or a master name that
    /// resolves to nothing, is satisfied (fail open -- content typos
    /// can never hard-lock a gate). Otherwise the named MULTISOURCE
    /// must currently be satisfied.
    fn master_satisfied(&self, records: &'static [LevelLogicRecord], master: u16) -> bool {
        if master == LOGIC_NAME_NONE {
            return true;
        }
        let count = self.count().min(records.len());
        let mut index = 0usize;
        while index < count {
            let record = &records[index];
            if record.kind == logic_kind::MULTISOURCE && record.targetname == master {
                let required = i16::try_from(record.arg0.max(1)).unwrap_or(i16::MAX);
                return self.counter[index] >= required;
            }
            index += 1;
        }
        true
    }
}

/// See [`GameEntities`]' twin: fail-safe active test.
///
/// [`GameEntities`]: crate::entities::GameEntities
fn room_is_active(room: RoomIndex, active_rooms: &[RoomIndex]) -> bool {
    if room.raw() == u16::MAX {
        return true;
    }
    let mut i = 0usize;
    while i < active_rooms.len() {
        if active_rooms[i] == room {
            return true;
        }
        i += 1;
    }
    false
}

/// Inclusive AABB containment. Zero-height boxes (cooked from
/// XZ-radius interactables) compare Y as equal-only, which the touch
/// scan never reaches (those kinds are use-fired, not touch-fired).
fn point_in_aabb(point: [i32; 3], min: [i32; 3], max: [i32; 3]) -> bool {
    point[0] >= min[0]
        && point[0] <= max[0]
        && point[1] >= min[1]
        && point[1] <= max[1]
        && point[2] >= min[2]
        && point[2] <= max[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestLogic = LogicRuntime<8, 1, 4>;

    const fn blank(kind: u8) -> LevelLogicRecord {
        LevelLogicRecord {
            room: RoomIndex(0),
            kind,
            spawnflags: 0,
            targetname: LOGIC_NAME_NONE,
            target: LOGIC_NAME_NONE,
            killtarget: LOGIC_NAME_NONE,
            master: LOGIC_NAME_NONE,
            delay_ticks: 0,
            wait_ticks: 0,
            arg0: 0,
            arg1: 0,
            link: psx_level::LOGIC_LINK_NONE,
            message: psx_level::INTERACTABLE_MESSAGE_NONE,
            x: 0,
            y: 0,
            z: 0,
            min: [0; 3],
            max: [0; 3],
            flags: logic_flags::ENABLED,
        }
    }

    const ACTIVE: [RoomIndex; 1] = [RoomIndex(0)];

    fn input_at(pos: [i32; 3]) -> LogicTickInput<'static> {
        LogicTickInput {
            player: pos,
            player_room: RoomIndex(0),
            active_rooms: &ACTIVE,
        }
    }

    #[test]
    fn empty_records_tick_is_inert() {
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&[]);
        logic.tick(&[], input_at([0, 0, 0]), 1);
        assert_eq!(logic.stats(), LogicStats::default());
    }

    // Names: 1 = trigger, 2 = relay, 3 = door, 4 = gate, 5 = message.
    static TRIGGER_DOOR_CHAIN: [LevelLogicRecord; 3] = [
        LevelLogicRecord {
            targetname: 1,
            target: 2,
            min: [-100, -100, -100],
            max: [100, 100, 100],
            wait_ticks: 30,
            ..blank(logic_kind::TRIGGER_VOLUME)
        },
        LevelLogicRecord {
            targetname: 2,
            target: 3,
            delay_ticks: 10,
            ..blank(logic_kind::RELAY)
        },
        LevelLogicRecord {
            targetname: 3,
            ..blank(logic_kind::DOOR)
        },
    ];

    #[test]
    fn trigger_relay_door_chain_delays_through_the_queue() {
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TRIGGER_DOOR_CHAIN);
        // Player outside the volume: nothing fires.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([500, 0, 0]), 1);
        assert!(!logic.door_open(2));
        // Player enters: trigger fires the relay, which queues the
        // door for now + 10.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([0, 0, 0]), 2);
        assert!(logic.take_fired(0), "trigger fired");
        assert!(logic.take_fired(1), "relay fired");
        assert!(!logic.door_open(2), "door waits on the delay queue");
        assert_eq!(logic.stats().queued, 1);
        // Before the deadline: still closed.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([500, 0, 0]), 11);
        assert!(!logic.door_open(2));
        // At the deadline: the queued event opens the door.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([500, 0, 0]), 12);
        assert!(logic.door_open(2));
        assert!(logic.take_fired(2));
        assert_eq!(logic.stats().queued, 0);
    }

    #[test]
    fn trigger_wait_rearms_and_negative_wait_fires_once() {
        static ONCE: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 3,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                wait_ticks: -1,
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&ONCE);
        logic.tick(&ONCE, input_at([0, 0, 0]), 1);
        assert!(logic.door_open(1));
        assert!(logic.is_removed(0), "wait -1 retires after one fire");
        // Standing in it again toggles nothing.
        logic.tick(&ONCE, input_at([0, 0, 0]), 2);
        assert!(logic.door_open(1));

        // The wait=30 trigger from the chain re-arms after 30 ticks.
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TRIGGER_DOOR_CHAIN);
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([0, 0, 0]), 2);
        assert!(logic.take_fired(0));
        // Still inside during the wait: no second fire.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([0, 0, 0]), 10);
        assert!(!logic.take_fired(0));
        // Past the re-arm deadline: one tick re-arms, the next fires.
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([0, 0, 0]), 33);
        logic.tick(&TRIGGER_DOOR_CHAIN, input_at([0, 0, 0]), 34);
        assert!(logic.take_fired(0));
    }

    #[test]
    fn master_gates_until_multisource_satisfied() {
        // Trigger 1 gated by multisource 4 needing 2 inputs; door 3.
        static GATED: [LevelLogicRecord; 3] = [
            LevelLogicRecord {
                targetname: 1,
                target: 3,
                master: 4,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 4,
                arg0: 2,
                ..blank(logic_kind::MULTISOURCE)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&GATED);
        // Locked: standing in the trigger does nothing.
        logic.tick(&GATED, input_at([0, 0, 0]), 1);
        assert!(!logic.door_open(2));
        // One input on: still locked.
        logic.fire_by_name(&GATED, 4, use_type::ON, 2);
        logic.tick(&GATED, input_at([0, 0, 0]), 3);
        assert!(!logic.door_open(2));
        // Second input satisfies the gate: the trigger passes.
        logic.fire_by_name(&GATED, 4, use_type::ON, 4);
        logic.tick(&GATED, input_at([0, 0, 0]), 5);
        assert!(logic.door_open(2));
        // Unknown master fails open (hl parity).
        static TYPO: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 3,
                master: 9,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TYPO);
        logic.tick(&TYPO, input_at([0, 0, 0]), 1);
        assert!(logic.door_open(1));
    }

    #[test]
    fn fan_out_depth_is_limited_and_counted() {
        // Relay ring 1 -> 2 -> 1 with zero delay recurses same-tick
        // until the depth cap trips.
        static RING: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 2,
                ..blank(logic_kind::RELAY)
            },
            LevelLogicRecord {
                targetname: 2,
                target: 1,
                ..blank(logic_kind::RELAY)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&RING);
        logic.fire_by_name(&RING, 1, use_type::TOGGLE, 1);
        let stats = logic.stats();
        assert!(stats.depth_drops > 0, "ring must trip the depth cap");
        assert!(stats.fired <= u16::from(LOGIC_FIRE_DEPTH_MAX) + 2);
    }

    #[test]
    fn killtarget_removes_and_queue_overflow_counts() {
        static KILL: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                killtarget: 3,
                ..blank(logic_kind::RELAY)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&KILL);
        logic.fire_by_name(&KILL, 1, use_type::TOGGLE, 1);
        assert!(logic.is_removed(1), "killtarget retired the door");

        // Queue overflow: 4 slots, 5 delayed enqueues.
        static DELAYED: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 1,
            target: 2,
            delay_ticks: 100,
            ..blank(logic_kind::RELAY)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&DELAYED);
        for now in 0..5 {
            logic.fire_by_name(&DELAYED, 1, use_type::TOGGLE, now);
        }
        let stats = logic.stats();
        assert_eq!(stats.queued, 4);
        assert_eq!(stats.queue_drops, 1);
    }

    #[test]
    fn touch_scan_gates_on_active_rooms() {
        static FAR_TRIGGER: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                room: RoomIndex(7),
                targetname: 1,
                target: 3,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&FAR_TRIGGER);
        // Room 7 inactive: the volume does not test.
        logic.tick(&FAR_TRIGGER, input_at([0, 0, 0]), 1);
        assert!(!logic.door_open(1));
        // Room 7 active: it fires.
        let both = [RoomIndex(0), RoomIndex(7)];
        let input = LogicTickInput {
            player: [0, 0, 0],
            player_room: RoomIndex(7),
            active_rooms: &both,
        };
        logic.tick(&FAR_TRIGGER, input, 2);
        assert!(logic.door_open(1));
    }

    #[test]
    fn owner_spatial_mask_replaces_room_gate_for_touch_scan() {
        static TRIGGER: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                target: 3,
                min: [-100, -100, -100],
                max: [100, 100, 100],
                ..blank(logic_kind::TRIGGER_VOLUME)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TRIGGER);
        logic.set_spatial_active_mask(Some(0));
        logic.tick(&TRIGGER, input_at([0, 0, 0]), 1);
        assert!(!logic.door_open(1));

        logic.set_spatial_active_mask(Some(1));
        logic.tick(&TRIGGER, input_at([0, 0, 0]), 2);
        assert!(logic.door_open(1));
    }

    #[test]
    fn fire_index_fires_exactly_one_record_and_respects_state() {
        // Two doors SHARING a name: fire_index opens only its record
        // (fire_by_name would toggle both).
        static TWINS: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
            LevelLogicRecord {
                targetname: 3,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&TWINS);
        assert!(!logic.any_fired());
        assert!(logic.fire_index(&TWINS, 1, use_type::TOGGLE, 1));
        assert!(!logic.door_open(0));
        assert!(logic.door_open(1));
        assert!(logic.any_fired());
        assert!(!logic.take_fired(0));
        assert!(logic.take_fired(1));
        assert!(!logic.any_fired());
        // Out of range: refused.
        assert!(!logic.fire_index(&TWINS, 2, use_type::TOGGLE, 2));

        // A retired (fire-once) record refuses an indexed fire.
        static ONCE: [LevelLogicRecord; 1] = [LevelLogicRecord {
            targetname: 5,
            wait_ticks: -1,
            ..blank(logic_kind::MESSAGE)
        }];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&ONCE);
        assert!(logic.fire_index(&ONCE, 0, use_type::TOGGLE, 1));
        assert!(logic.is_removed(0));
        assert!(!logic.fire_index(&ONCE, 0, use_type::TOGGLE, 2));
    }

    #[test]
    fn fire_index_respects_master_gate() {
        static GATED: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 1,
                master: 4,
                ..blank(logic_kind::MESSAGE)
            },
            LevelLogicRecord {
                targetname: 4,
                arg0: 1,
                ..blank(logic_kind::MULTISOURCE)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&GATED);
        assert!(!logic.fire_index(&GATED, 0, use_type::TOGGLE, 1));
        logic.fire_by_name(&GATED, 4, use_type::ON, 2);
        assert!(logic.fire_index(&GATED, 0, use_type::TOGGLE, 3));
    }

    #[test]
    fn disabled_records_init_removed_and_start_on_seeds_counters() {
        static FLAGGED: [LevelLogicRecord; 2] = [
            LevelLogicRecord {
                targetname: 3,
                flags: 0,
                ..blank(logic_kind::DOOR)
            },
            LevelLogicRecord {
                targetname: 5,
                flags: logic_flags::ENABLED | logic_flags::START_ON,
                ..blank(logic_kind::DOOR)
            },
        ];
        let mut logic = TestLogic::EMPTY;
        logic.init_from_records(&FLAGGED);
        assert!(logic.is_removed(0), "flag-disabled record retires");
        assert!(logic.door_open(1), "START_ON door begins open");
    }
}
